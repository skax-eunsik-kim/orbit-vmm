//! virtio-blk backed by a raw image file (virtio 1.x section 5.2).
//! Requests are served synchronously on the notifying vCPU thread.

use std::fs::File;
use std::os::unix::fs::FileExt;
use std::sync::Arc;

use super::{Queue, VirtioDevice, VirtioInterrupt, DEVICE_ID_BLOCK, VIRTIO_F_VERSION_1};
use crate::memory::GuestMemory;

const VIRTIO_BLK_F_SEG_MAX: u64 = 1 << 2;
const VIRTIO_BLK_F_FLUSH: u64 = 1 << 9;

const VIRTIO_BLK_T_IN: u32 = 0;
const VIRTIO_BLK_T_OUT: u32 = 1;
const VIRTIO_BLK_T_FLUSH: u32 = 4;
const VIRTIO_BLK_T_GET_ID: u32 = 8;

const VIRTIO_BLK_S_OK: u8 = 0;
const VIRTIO_BLK_S_IOERR: u8 = 1;
const VIRTIO_BLK_S_UNSUPP: u8 = 2;

const SECTOR: u64 = 512;
const SEG_MAX: u32 = 126;

pub struct Block {
    file: File,
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
        Ok(Self { file, sectors: len / SECTOR, mem: None, queue: None, interrupt: None })
    }

    /// Serves one request; returns (status, bytes written into guest buffers).
    fn handle(&self, mem: &GuestMemory, descs: &[super::Descriptor]) -> (u8, u32) {
        if descs.len() < 2 || descs[0].len < 16 {
            return (VIRTIO_BLK_S_IOERR, 0);
        }
        let kind: u32 = mem.read_obj(descs[0].addr).unwrap_or(u32::MAX);
        let sector: u64 = mem.read_obj(descs[0].addr + 8).unwrap_or(0);
        let data = &descs[1..descs.len() - 1];
        let mut offset = sector * SECTOR;
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
                    written += d.len;
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
                    if self.file.write_all_at(buf, offset).is_err() {
                        return (VIRTIO_BLK_S_IOERR, 0);
                    }
                    offset += d.len as u64;
                }
                (VIRTIO_BLK_S_OK, 0)
            }
            VIRTIO_BLK_T_FLUSH => match self.file.sync_data() {
                Ok(()) => (VIRTIO_BLK_S_OK, 0),
                Err(_) => (VIRTIO_BLK_S_IOERR, 0),
            },
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

impl VirtioDevice for Block {
    fn device_id(&self) -> u32 {
        DEVICE_ID_BLOCK
    }

    fn features(&self) -> u64 {
        VIRTIO_F_VERSION_1 | VIRTIO_BLK_F_FLUSH | VIRTIO_BLK_F_SEG_MAX
    }

    fn num_queues(&self) -> usize {
        1
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        // struct virtio_blk_config { le64 capacity; le32 size_max; le32 seg_max; ... }
        let mut cfg = [0u8; 16];
        cfg[0..8].copy_from_slice(&self.sectors.to_le_bytes());
        cfg[12..16].copy_from_slice(&SEG_MAX.to_le_bytes());
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
            queue.add_used(&mem, chain.head, written + 1);
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
