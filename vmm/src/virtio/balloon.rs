//! virtio-balloon with free page reporting (VIRTIO_BALLOON_F_REPORTING).
//!
//! With reporting, the guest kernel's page allocator hands us ranges of free
//! memory (in 2 MiB-ish chunks, CONFIG_PAGE_REPORTING) on the reporting
//! queue; we give the host pages behind them back to macOS. When the guest
//! allocates them again they fault back in as fresh pages. This is what
//! Virtualization.framework's balloon can't do.
//!
//! Queue layout: Linux only numbers the queues whose features were
//! negotiated, so with REPORTING (and no STATS / FREE_PAGE_HINT) the queues
//! are inflate=0, deflate=1, reporting=2.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use super::{Queue, VirtioDevice, VirtioInterrupt, DEVICE_ID_BALLOON, VIRTIO_F_VERSION_1};
use crate::memory::GuestMemory;

pub const VIRTIO_BALLOON_F_REPORTING: u64 = 1 << 5;

const QUEUE_INFLATE: u16 = 0;
const QUEUE_DEFLATE: u16 = 1;
const QUEUE_REPORTING: u16 = 2;

/// Balloon PFNs are always in 4 KiB units, whatever the page sizes.
const BALLOON_PAGE: u64 = 4096;

/// Counters exported for logging/benchmarks.
#[derive(Default)]
pub struct BalloonStats {
    pub reports: AtomicU64,
    pub reported_bytes: AtomicU64,
    pub released_bytes: AtomicU64,
}

pub struct Balloon {
    mem: Option<Arc<GuestMemory>>,
    queues: Vec<Queue>,
    interrupt: Option<Arc<dyn VirtioInterrupt>>,
    /// Config space: num_pages (target, written by us) and actual (by the driver).
    num_pages: u32,
    actual: u32,
    pub stats: Arc<BalloonStats>,
}

impl Balloon {
    pub fn new() -> Self {
        Self { mem: None, queues: Vec::new(), interrupt: None, num_pages: 0, actual: 0, stats: Arc::default() }
    }

    fn process(&mut self, index: u16) {
        let (Some(mem), Some(interrupt)) = (self.mem.clone(), self.interrupt.clone()) else {
            return;
        };
        let Some(queue) = self.queues.get_mut(index as usize) else {
            return;
        };
        let mut used = false;
        while let Some(chain) = queue.pop(&mem) {
            match index {
                QUEUE_REPORTING => {
                    let mut bytes = 0;
                    let mut released = 0;
                    for d in &chain.descs {
                        bytes += d.len as u64;
                        released += mem.release(d.addr, d.len as u64);
                    }
                    let n = self.stats.reports.fetch_add(1, Ordering::Relaxed) + 1;
                    let total = self.stats.reported_bytes.fetch_add(bytes, Ordering::Relaxed) + bytes;
                    let freed = self.stats.released_bytes.fetch_add(released, Ordering::Relaxed) + released;
                    if std::env::var_os("ORBIT_DEBUG").is_some() && n.is_multiple_of(16) {
                        eprintln!(
                            "orbit-vmm: balloon: {n} reports, {} MiB reported, {} MiB released",
                            total >> 20,
                            freed >> 20
                        );
                    }
                }
                QUEUE_INFLATE => {
                    // Driver-readable array of little-endian u32 PFNs.
                    for d in chain.descs.iter().filter(|d| !d.write_only) {
                        for i in 0..(d.len / 4) as u64 {
                            if let Some(pfn) = mem.read_obj::<u32>(d.addr + 4 * i) {
                                mem.release(pfn as u64 * BALLOON_PAGE, BALLOON_PAGE);
                            }
                        }
                    }
                }
                _ => {} // deflate: pages fault back in on use, nothing to do
            }
            queue.add_used(&mem, chain.head, 0);
            used = true;
        }
        if used {
            interrupt.signal_used_queue();
        }
    }
}

impl VirtioDevice for Balloon {
    fn device_id(&self) -> u32 {
        DEVICE_ID_BALLOON
    }

    fn features(&self) -> u64 {
        VIRTIO_F_VERSION_1 | VIRTIO_BALLOON_F_REPORTING
    }

    fn num_queues(&self) -> usize {
        3
    }

    fn queue_max_size(&self) -> u16 {
        // The reporting queue carries up to 32 ranges per report.
        256
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        let mut cfg = [0u8; 8];
        cfg[0..4].copy_from_slice(&self.num_pages.to_le_bytes());
        cfg[4..8].copy_from_slice(&self.actual.to_le_bytes());
        for (i, b) in data.iter_mut().enumerate() {
            *b = cfg.get(offset as usize + i).copied().unwrap_or(0);
        }
    }

    fn write_config(&mut self, offset: u64, data: &[u8]) {
        // Only `actual` (offset 4) is driver-writable.
        if offset == 4 && data.len() == 4 {
            self.actual = u32::from_le_bytes(data.try_into().unwrap());
        }
    }

    fn activate(&mut self, mem: Arc<GuestMemory>, queues: Vec<Queue>, interrupt: Arc<dyn VirtioInterrupt>, acked: u64) {
        if acked & VIRTIO_BALLOON_F_REPORTING == 0 {
            eprintln!("orbit-vmm: balloon: guest did not accept free page reporting");
        }
        self.mem = Some(mem);
        self.queues = queues;
        self.interrupt = Some(interrupt);
        for q in [QUEUE_INFLATE, QUEUE_DEFLATE, QUEUE_REPORTING] {
            self.process(q);
        }
    }

    fn queue_notify(&mut self, index: u16) {
        self.process(index);
    }

    fn reset(&mut self) {
        self.mem = None;
        self.queues.clear();
        self.interrupt = None;
        self.actual = 0;
    }
}
