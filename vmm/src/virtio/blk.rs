//! virtio-blk backed by a raw image file (virtio 1.x section 5.2).
//! Requests are served synchronously on the notifying vCPU thread.
//!
//! The image is sparse, so it grows on the Mac as the guest writes. When the
//! Mac's volume runs out, a failed write reaches ext4 as an I/O error, and ext4
//! answers by dropping the write ("failed to convert unwritten extents ...
//! potential data loss") and going read-only. So writes are held instead, the
//! way QEMU's default werror=enospc pauses the VM: once the volume is below a
//! floor (or a write fails with ENOSPC), the write waits and retries until
//! space comes back, and only fails after a bounded time. The guest sees a
//! slow disk (and may log hung-task or RCU-stall warnings), not lost data.
//! A watcher thread warns on stderr (console.log) well before that point.

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{Queue, VirtioDevice, VirtioInterrupt, DEVICE_ID_BLOCK, VIRTIO_F_VERSION_1};
use crate::memory::GuestMemory;

const VIRTIO_BLK_F_SEG_MAX: u64 = 1 << 2;
const VIRTIO_BLK_F_FLUSH: u64 = 1 << 9;
const VIRTIO_BLK_F_DISCARD: u64 = 1 << 13;

const VIRTIO_BLK_T_IN: u32 = 0;
const VIRTIO_BLK_T_OUT: u32 = 1;
const VIRTIO_BLK_T_FLUSH: u32 = 4;
const VIRTIO_BLK_T_GET_ID: u32 = 8;
const VIRTIO_BLK_T_DISCARD: u32 = 11;

const VIRTIO_BLK_S_OK: u8 = 0;
const VIRTIO_BLK_S_IOERR: u8 = 1;
const VIRTIO_BLK_S_UNSUPP: u8 = 2;

const SECTOR: u64 = 512;
const SEG_MAX: u32 = 126;
/// Discard (fstrim) punches holes in the image so space the guest freed goes
/// back to the Mac. APFS punches whole 4 KiB blocks; the guest is told to
/// align to that, and anything partial at the edges is left allocated.
const PUNCH_ALIGN: u64 = 4096;
const MAX_DISCARD_SECTORS: u32 = u32::MAX / 2;
const MAX_DISCARD_SEG: u32 = 256;
/// struct virtio_blk_discard_write_zeroes { le64 sector; le32 num_sectors; le32 flags; }
const DISCARD_SEG_SIZE: u64 = 16;

const MIB: u64 = 1 << 20;
/// Warn below 5 GiB or 2% of the volume, whichever is smaller.
const WARN_BYTES: u64 = 5 << 30;
const WARN_PERCENT: u64 = 2;
/// Hold writes below this much free space, leaving room for macOS itself.
const FLOOR_BYTES: u64 = 1 << 30;
/// How long a held write waits for space before it fails.
const PAUSE: Duration = Duration::from_secs(600);
const WATCH_EVERY: Duration = Duration::from_secs(2);
const REWARN_EVERY: Duration = Duration::from_secs(600);

fn env_mib(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.parse::<u64>().ok().map(|m| m * MIB)
}

/// (available, total) bytes on the volume holding `file`.
fn statfs(file: &File) -> Option<(u64, u64)> {
    use std::os::fd::AsRawFd;
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatfs(file.as_raw_fd(), &mut st) } != 0 {
        return None;
    }
    let bs = st.f_bsize as u64;
    Some((st.f_bavail.saturating_mul(bs), st.f_blocks.saturating_mul(bs)))
}

/// Free space on the Mac's volume that holds the image. The thresholds come
/// from the environment of whoever started the VMM, never from the guest.
struct SpaceGuard {
    file: File,
    warn: u64,
    floor: u64,
    pause: Duration,
    free: AtomicU64,
    /// Writes are being held for space (logged once per shortage).
    holding: AtomicBool,
    /// A held write timed out: fail at once until space comes back.
    failing: AtomicBool,
}

impl SpaceGuard {
    fn new(file: File) -> Arc<Self> {
        let (free, total) = statfs(&file).unwrap_or((u64::MAX, 0));
        let floor = env_mib("ORBIT_DISK_FLOOR_MIB").unwrap_or(FLOOR_BYTES);
        let warn = env_mib("ORBIT_DISK_WARN_MIB")
            .unwrap_or_else(|| WARN_BYTES.min(total / 100 * WARN_PERCENT).max(floor.saturating_mul(2)));
        let pause =
            std::env::var("ORBIT_DISK_PAUSE_SECS").ok().and_then(|v| v.parse().ok()).map_or(PAUSE, Duration::from_secs);
        let g = Arc::new(Self {
            file,
            warn,
            floor,
            pause,
            free: AtomicU64::new(free),
            holding: AtomicBool::new(false),
            failing: AtomicBool::new(false),
        });
        let w = g.clone();
        let _ = std::thread::Builder::new().name("disk-space".into()).spawn(move || w.watch());
        g
    }

    fn refresh(&self) -> u64 {
        let free = statfs(&self.file).map_or(u64::MAX, |(f, _)| f);
        self.free.store(free, Ordering::Relaxed);
        free
    }

    fn watch(&self) {
        let mut warned: Option<Instant> = None;
        loop {
            let free = self.refresh();
            if free < self.warn {
                if warned.is_none_or(|t| t.elapsed() >= REWARN_EVERY) {
                    eprintln!(
                        "orbit-vmm: disk space low: {} MiB free on the Mac's disk (warning below {} MiB; \
                         guest writes are held below {} MiB). Free space on the Mac or run \
                         `docker system prune` / `fstrim -a` in the VM.",
                        free / MIB,
                        self.warn / MIB,
                        self.floor / MIB
                    );
                    warned = Some(Instant::now());
                }
            } else if warned.is_some() && free >= self.warn.saturating_add(256 * MIB) {
                eprintln!("orbit-vmm: disk space ok again: {} MiB free on the Mac's disk", free / MIB);
                warned = None;
            }
            std::thread::sleep(WATCH_EVERY);
        }
    }

    /// Runs `op` (a write of `bytes`, or a flush); while the Mac's disk is
    /// full, holds it and retries with backoff for up to `pause` instead of
    /// failing.
    fn run(&self, bytes: u64, mut op: impl FnMut() -> io::Result<()>) -> io::Result<()> {
        let mut since: Option<Instant> = None;
        let mut backoff = Duration::from_millis(50);
        loop {
            // The guest writes faster than the watcher samples, so each write
            // counts against the last sample; near the floor, ask the Mac.
            let need = self.floor.saturating_add(bytes);
            let low = self.free.load(Ordering::Relaxed) < need && self.refresh() < need;
            if !low {
                match op() {
                    Ok(()) => {
                        let _ = self
                            .free
                            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |f| Some(f.saturating_sub(bytes)));
                        self.failing.store(false, Ordering::Relaxed);
                        if self.holding.swap(false, Ordering::Relaxed) {
                            eprintln!(
                                "orbit-vmm: disk writes resumed after {:.1} s ({} MiB free on the Mac's disk)",
                                since.map_or(0.0, |t| t.elapsed().as_secs_f64()),
                                self.refresh() / MIB
                            );
                        }
                        return Ok(());
                    }
                    Err(e) if !matches!(e.raw_os_error(), Some(libc::ENOSPC | libc::EDQUOT)) => return Err(e),
                    Err(_) => {}
                }
            }
            if self.failing.load(Ordering::Relaxed) {
                return Err(io::Error::from_raw_os_error(libc::ENOSPC));
            }
            let start = *since.get_or_insert_with(Instant::now);
            if !self.holding.swap(true, Ordering::Relaxed) {
                eprintln!(
                    "orbit-vmm: DISK FULL: the Mac's disk has {} MiB free; holding guest disk writes \
                     for up to {} s until space is freed (the VM may stall). Free space on the Mac now.",
                    self.refresh() / MIB,
                    self.pause.as_secs()
                );
            }
            if start.elapsed() >= self.pause {
                eprintln!(
                    "orbit-vmm: DISK FULL: no space freed in {} s; failing guest writes until there is \
                     (ext4 in the VM will report I/O errors)",
                    self.pause.as_secs()
                );
                self.failing.store(true, Ordering::Relaxed);
                self.holding.store(false, Ordering::Relaxed);
                return Err(io::Error::from_raw_os_error(libc::ENOSPC));
            }
            std::thread::sleep(backoff);
            backoff = (backoff * 2).min(Duration::from_secs(2));
        }
    }
}

pub struct Block {
    file: File,
    space: Arc<SpaceGuard>,
    sectors: u64,
    mem: Option<Arc<GuestMemory>>,
    queue: Option<Queue>,
    interrupt: Option<Arc<dyn VirtioInterrupt>>,
}

impl Block {
    pub fn open(path: &std::path::Path) -> Result<Self, String> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| format!("opening disk {}: {e}", path.display()))?;
        // Two VMs writing the same ext4 image corrupt it; refuse to share.
        use std::os::fd::AsRawFd;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(format!("disk {} is in use by another VM", path.display()));
        }
        let len = file.metadata().map_err(|e| e.to_string())?.len();
        let space = SpaceGuard::new(file.try_clone().map_err(|e| e.to_string())?);
        Ok(Self { file, space, sectors: len / SECTOR, mem: None, queue: None, interrupt: None })
    }

    /// Frees the whole 4 KiB blocks of `[off, off+len)` in the image file
    /// (F_PUNCHHOLE: they read back as zeros and stop taking space on the Mac).
    fn punch(&self, off: u64, len: u64) -> bool {
        use std::os::fd::AsRawFd;
        let Some(start) = off.checked_next_multiple_of(PUNCH_ALIGN) else {
            return false;
        };
        let end = (off + len) / PUNCH_ALIGN * PUNCH_ALIGN;
        if end <= start {
            return true; // nothing whole to free; discard is only a hint
        }
        let arg = libc::fpunchhole_t {
            fp_flags: 0,
            reserved: 0,
            fp_offset: start as libc::off_t,
            fp_length: (end - start) as libc::off_t,
        };
        unsafe { libc::fcntl(self.file.as_raw_fd(), libc::F_PUNCHHOLE, &arg) == 0 }
    }

    /// Serves one request; returns (status, bytes written into guest buffers).
    fn handle(&self, mem: &GuestMemory, descs: &[super::Descriptor]) -> (u8, u32) {
        if descs.len() < 2 || descs[0].len < 16 {
            return (VIRTIO_BLK_S_IOERR, 0);
        }
        let kind: u32 = mem.read_obj(descs[0].addr).unwrap_or(u32::MAX);
        let sector: u64 = mem.read_obj(descs[0].addr.wrapping_add(8)).unwrap_or(0);
        let data = &descs[1..descs.len() - 1];
        // Guest-controlled: an out-of-range sector fails in_range below.
        let mut offset = sector.saturating_mul(SECTOR);
        let mut written = 0u32;
        let in_range = |off: u64, len: u64| off.checked_add(len).is_some_and(|end| end <= self.sectors * SECTOR);
        match kind {
            VIRTIO_BLK_T_IN => {
                for d in data {
                    let Some(p) = mem.ptr(d.addr, d.len as u64) else {
                        return (VIRTIO_BLK_S_IOERR, written);
                    };
                    if !d.write_only || !in_range(offset, d.len as u64) {
                        return (VIRTIO_BLK_S_IOERR, written);
                    }
                    let buf = unsafe { std::slice::from_raw_parts_mut(p, d.len as usize) };
                    if self.file.read_exact_at(buf, offset).is_err() {
                        return (VIRTIO_BLK_S_IOERR, written);
                    }
                    offset += d.len as u64;
                    written = written.saturating_add(d.len);
                }
                (VIRTIO_BLK_S_OK, written)
            }
            VIRTIO_BLK_T_OUT => {
                for d in data {
                    let Some(p) = mem.ptr(d.addr, d.len as u64) else {
                        return (VIRTIO_BLK_S_IOERR, 0);
                    };
                    if d.write_only || !in_range(offset, d.len as u64) {
                        return (VIRTIO_BLK_S_IOERR, 0);
                    }
                    let buf = unsafe { std::slice::from_raw_parts(p, d.len as usize) };
                    if let Err(e) = self.space.run(d.len as u64, || self.file.write_all_at(buf, offset)) {
                        log_io_error("write", &e);
                        return (VIRTIO_BLK_S_IOERR, 0);
                    }
                    offset += d.len as u64;
                }
                (VIRTIO_BLK_S_OK, 0)
            }
            VIRTIO_BLK_T_FLUSH => match self.space.run(0, || self.file.sync_data()) {
                Ok(()) => (VIRTIO_BLK_S_OK, 0),
                Err(e) => {
                    log_io_error("flush", &e);
                    (VIRTIO_BLK_S_IOERR, 0)
                }
            },
            VIRTIO_BLK_T_DISCARD => {
                let mut segs = 0u32;
                for d in data {
                    if d.write_only {
                        return (VIRTIO_BLK_S_IOERR, 0);
                    }
                    for i in 0..(d.len as u64 / DISCARD_SEG_SIZE) {
                        segs += 1;
                        if segs > MAX_DISCARD_SEG {
                            return (VIRTIO_BLK_S_IOERR, 0);
                        }
                        let at = d.addr.wrapping_add(i * DISCARD_SEG_SIZE);
                        let (Some(start), Some(count)) =
                            (mem.read_obj::<u64>(at), mem.read_obj::<u32>(at.wrapping_add(8)))
                        else {
                            return (VIRTIO_BLK_S_IOERR, 0);
                        };
                        let off = start.saturating_mul(SECTOR);
                        let len = count as u64 * SECTOR;
                        if count > MAX_DISCARD_SECTORS || !in_range(off, len) || !self.punch(off, len) {
                            return (VIRTIO_BLK_S_IOERR, 0);
                        }
                    }
                }
                (VIRTIO_BLK_S_OK, 0)
            }
            VIRTIO_BLK_T_GET_ID => {
                let id = b"orbit-vmm-disk\0\0\0\0\0\0";
                match data.first() {
                    Some(d) if d.write_only && mem.write(d.addr, &id[..(d.len as usize).min(20)]) => {
                        (VIRTIO_BLK_S_OK, d.len.min(20))
                    }
                    _ => (VIRTIO_BLK_S_IOERR, 0),
                }
            }
            _ => (VIRTIO_BLK_S_UNSUPP, 0),
        }
    }
}

/// The guest only sees "I/O error"; say once on the Mac side what it was.
fn log_io_error(what: &str, e: &io::Error) {
    static ONCE: AtomicBool = AtomicBool::new(false);
    if !ONCE.swap(true, Ordering::Relaxed) {
        eprintln!("orbit-vmm: disk {what} failed: {e} (further disk errors are not logged)");
    }
}

impl VirtioDevice for Block {
    fn device_id(&self) -> u32 {
        DEVICE_ID_BLOCK
    }

    fn features(&self) -> u64 {
        VIRTIO_F_VERSION_1 | VIRTIO_BLK_F_FLUSH | VIRTIO_BLK_F_SEG_MAX | VIRTIO_BLK_F_DISCARD
    }

    fn num_queues(&self) -> usize {
        1
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        // struct virtio_blk_config { le64 capacity; le32 size_max; le32 seg_max;
        //   geometry (4); le32 blk_size; topology (8); u8 writeback; u8; le16 num_queues;
        //   le32 max_discard_sectors @36; le32 max_discard_seg @40;
        //   le32 discard_sector_alignment @44; ... }
        let mut cfg = [0u8; 48];
        cfg[0..8].copy_from_slice(&self.sectors.to_le_bytes());
        cfg[12..16].copy_from_slice(&SEG_MAX.to_le_bytes());
        cfg[36..40].copy_from_slice(&MAX_DISCARD_SECTORS.to_le_bytes());
        cfg[40..44].copy_from_slice(&MAX_DISCARD_SEG.to_le_bytes());
        cfg[44..48].copy_from_slice(&((PUNCH_ALIGN / SECTOR) as u32).to_le_bytes());
        for (i, b) in data.iter_mut().enumerate() {
            *b = cfg.get(offset as usize + i).copied().unwrap_or(0);
        }
    }

    fn write_config(&mut self, _offset: u64, _data: &[u8]) {}

    fn activate(
        &mut self,
        mem: Arc<GuestMemory>,
        mut queues: Vec<Queue>,
        interrupt: Arc<dyn VirtioInterrupt>,
        _acked: u64,
    ) {
        self.mem = Some(mem);
        self.queue = queues.drain(..).next();
        self.interrupt = Some(interrupt);
        self.queue_notify(0);
    }

    fn queue_notify(&mut self, _index: u16) {
        let (Some(mem), Some(interrupt)) = (self.mem.clone(), self.interrupt.clone()) else {
            return;
        };
        let Some(mut queue) = self.queue.take() else {
            return;
        };
        let mut used = false;
        while let Some(chain) = queue.pop(&mem) {
            let (status, written) = self.handle(&mem, &chain.descs);
            if let Some(last) = chain.descs.last().filter(|d| d.write_only && d.len >= 1) {
                mem.write_obj(last.addr, status);
            }
            queue.add_used(&mem, chain.head, written.saturating_add(1));
            used = true;
        }
        self.queue = Some(queue);
        if used {
            interrupt.signal_used_queue();
        }
    }

    fn reset(&mut self) {
        self.mem = None;
        self.queue = None;
        self.interrupt = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard(floor: u64, pause: Duration) -> SpaceGuard {
        SpaceGuard {
            file: File::open("/").unwrap(),
            warn: 0,
            floor,
            pause,
            free: AtomicU64::new(u64::MAX),
            holding: AtomicBool::new(false),
            failing: AtomicBool::new(false),
        }
    }

    fn enospc() -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::ENOSPC))
    }

    #[test]
    fn enospc_is_retried_until_it_succeeds() {
        let g = guard(0, Duration::from_secs(10));
        let mut n = 0;
        let r = g.run(0, || {
            n += 1;
            if n < 3 {
                enospc()
            } else {
                Ok(())
            }
        });
        assert!(r.is_ok());
        assert_eq!(n, 3);
        assert!(!g.holding.load(Ordering::Relaxed));
    }

    #[test]
    fn enospc_fails_after_the_pause_and_then_fails_fast() {
        let g = guard(0, Duration::ZERO);
        assert!(g.run(0, enospc).is_err());
        let mut called = false;
        // Still failing: the op runs once (space may be back), then fails fast.
        assert!(g
            .run(0, || {
                called = true;
                enospc()
            })
            .is_err());
        assert!(called);
        assert!(g.run(0, || Ok(())).is_ok());
        assert!(!g.failing.load(Ordering::Relaxed));
    }

    #[test]
    fn other_errors_are_not_retried() {
        let g = guard(0, Duration::from_secs(10));
        let mut n = 0;
        let r = g.run(0, || {
            n += 1;
            Err(io::Error::from_raw_os_error(libc::EIO))
        });
        assert!(r.is_err());
        assert_eq!(n, 1);
    }

    #[test]
    fn below_the_floor_writes_are_held_not_attempted() {
        // The root volume has less than u64::MAX free, so this is always "low".
        let g = guard(u64::MAX, Duration::ZERO);
        g.free.store(0, Ordering::Relaxed);
        let mut called = false;
        assert!(g
            .run(0, || {
                called = true;
                Ok(())
            })
            .is_err());
        assert!(!called);
    }
}
