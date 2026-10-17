//! Split virtqueue (virtio 1.x, section 2.7).

use std::sync::atomic::{fence, Ordering};

use crate::memory::GuestMemory;

pub const VIRTQ_DESC_F_NEXT: u16 = 1;
pub const VIRTQ_DESC_F_WRITE: u16 = 2;
pub const VIRTQ_DESC_F_INDIRECT: u16 = 4;

#[derive(Clone, Copy, Debug, Default)]
pub struct Descriptor {
    pub addr: u64,
    pub len: u32,
    /// Device writes into this buffer (VIRTQ_DESC_F_WRITE).
    pub write_only: bool,
}

#[derive(Clone, Debug, Default)]
pub struct DescriptorChain {
    /// Head index, passed back to add_used().
    pub head: u16,
    /// Flattened chain (indirect tables already expanded).
    pub descs: Vec<Descriptor>,
}

#[derive(Clone, Debug, Default)]
pub struct Queue {
    pub max_size: u16,
    pub size: u16,
    pub ready: bool,
    pub desc_table: u64,
    pub avail_ring: u64,
    pub used_ring: u64,
    pub next_avail: u16,
    pub next_used: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RawDesc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

/// Concatenates the driver-readable buffers of a chain, refusing chains whose
/// readable part is larger than `cap` (a broken or hostile guest could
/// otherwise make the VMM allocate gigabytes). Err carries the total size.
pub fn read_readable(mem: &GuestMemory, descs: &[Descriptor], cap: usize) -> Result<Vec<u8>, usize> {
    let total: usize = descs.iter().filter(|d| !d.write_only).map(|d| d.len as usize).sum();
    if total > cap {
        return Err(total);
    }
    let mut out = vec![0u8; total];
    let mut off = 0;
    for d in descs.iter().filter(|d| !d.write_only) {
        mem.read(d.addr, &mut out[off..off + d.len as usize]);
        off += d.len as usize;
    }
    Ok(out)
}

impl Queue {
    pub fn new(max_size: u16) -> Self {
        Self { max_size, size: max_size, ..Default::default() }
    }

    /// Next available chain, or None if the driver has published nothing new
    /// (or the queue isn't usable).
    pub fn pop(&mut self, mem: &GuestMemory) -> Option<DescriptorChain> {
        if !self.ready || self.size == 0 {
            return None;
        }
        let avail_idx: u16 = mem.read_obj(self.avail_ring + 2)?;
        if avail_idx == self.next_avail {
            return None;
        }
        // Read ring contents only after observing the index.
        fence(Ordering::Acquire);
        let slot = self.next_avail % self.size;
        let head: u16 = mem.read_obj(self.avail_ring + 4 + 2 * slot as u64)?;
        self.next_avail = self.next_avail.wrapping_add(1);

        let mut descs = Vec::new();
        if !self.walk(mem, self.desc_table, self.size, head, &mut descs, true) {
            // Malformed chain: hand it back empty so the driver doesn't wait forever.
            descs.clear();
        }
        Some(DescriptorChain { head, descs })
    }

    fn walk(
        &self,
        mem: &GuestMemory,
        table: u64,
        table_len: u16,
        first: u16,
        out: &mut Vec<Descriptor>,
        allow_indirect: bool,
    ) -> bool {
        let mut index = first;
        for _ in 0..table_len {
            if index >= table_len {
                return false;
            }
            let Some(d) = mem.read_obj::<RawDesc>(table + 16 * index as u64) else {
                return false;
            };
            if d.flags & VIRTQ_DESC_F_INDIRECT != 0 {
                if !allow_indirect || d.len % 16 != 0 {
                    return false;
                }
                return self.walk(mem, d.addr, (d.len / 16) as u16, 0, out, false);
            }
            out.push(Descriptor { addr: d.addr, len: d.len, write_only: d.flags & VIRTQ_DESC_F_WRITE != 0 });
            if d.flags & VIRTQ_DESC_F_NEXT == 0 {
                return true;
            }
            index = d.next;
        }
        false // loop in the chain
    }

    /// Return a chain to the driver with `len` bytes written.
    pub fn add_used(&mut self, mem: &GuestMemory, head: u16, len: u32) {
        if self.size == 0 {
            return;
        }
        let slot = self.next_used % self.size;
        let elem = self.used_ring + 4 + 8 * slot as u64;
        mem.write_obj(elem, head as u32);
        mem.write_obj(elem + 4, len);
        self.next_used = self.next_used.wrapping_add(1);
        // The element must be visible before the index that publishes it.
        fence(Ordering::Release);
        mem.write_obj(self.used_ring + 2, self.next_used);
        fence(Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guest(size: usize) -> (Vec<u8>, GuestMemory) {
        let mut buf = vec![0u8; size];
        let mem = unsafe { GuestMemory::from_raw(buf.as_mut_ptr(), size as u64) };
        (buf, mem)
    }

    #[test]
    fn pop_chain_and_add_used() {
        let (_buf, mem) = guest(1 << 20);
        let base = crate::memory::RAM_BASE;
        let mut q = Queue::new(8);
        q.ready = true;
        q.desc_table = base;
        q.avail_ring = base + 0x1000;
        q.used_ring = base + 0x2000;
        // desc 0 -> desc 1 (write-only)
        mem.write_obj(base, RawDesc { addr: base + 0x10000, len: 16, flags: VIRTQ_DESC_F_NEXT, next: 1 });
        mem.write_obj(base + 16, RawDesc { addr: base + 0x20000, len: 64, flags: VIRTQ_DESC_F_WRITE, next: 0 });
        mem.write_obj::<u16>(q.avail_ring + 4, 0); // ring[0] = head 0
        mem.write_obj::<u16>(q.avail_ring + 2, 1); // idx = 1

        let chain = q.pop(&mem).expect("chain");
        assert_eq!(chain.head, 0);
        assert_eq!(chain.descs.len(), 2);
        assert!(!chain.descs[0].write_only && chain.descs[1].write_only);
        assert!(q.pop(&mem).is_none());

        q.add_used(&mem, chain.head, 64);
        assert_eq!(mem.read_obj::<u16>(q.used_ring + 2), Some(1));
        assert_eq!(mem.read_obj::<u32>(q.used_ring + 4), Some(0));
        assert_eq!(mem.read_obj::<u32>(q.used_ring + 8), Some(64));
    }
}
