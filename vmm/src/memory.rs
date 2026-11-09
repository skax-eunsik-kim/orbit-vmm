//! Guest RAM: one host allocation mapped into guest physical space as one
//! region (arm64: at `RAM_BASE`) or two (x86-64: below the 32-bit MMIO hole
//! and from 4 GiB up).

/// Guest physical address where RAM starts (arm64: below it, MMIO devices).
#[cfg(target_arch = "aarch64")]
pub const RAM_BASE: u64 = 0x8000_0000;
#[cfg(target_arch = "x86_64")]
pub const RAM_BASE: u64 = 0;

/// Host page size: 16 KiB on Apple Silicon, 4 KiB on Intel. Stage-2/EPT
/// mappings and madvise work at this granularity.
#[cfg(target_arch = "aarch64")]
pub const HOST_PAGE_SIZE: u64 = 16 * 1024;
#[cfg(target_arch = "x86_64")]
pub const HOST_PAGE_SIZE: u64 = 4 * 1024;

/// One guest-physical range backed by `[host + host_off, +size)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Region {
    gpa: u64,
    size: u64,
    host_off: u64,
}

pub struct GuestMemory {
    host: *mut u8,
    size: u64,
    regions: Vec<Region>,
}

// The region is shared by vCPU and device threads; accesses are raw byte copies.
unsafe impl Send for GuestMemory {}
unsafe impl Sync for GuestMemory {}

impl GuestMemory {
    /// RAM as one region at `RAM_BASE` (arm64; on x86-64 only tests use it).
    ///
    /// # Safety
    /// `host` must point to `size` bytes that stay mapped for the VM's lifetime.
    #[cfg_attr(target_arch = "x86_64", allow(dead_code))]
    pub unsafe fn from_raw(host: *mut u8, size: u64) -> Self {
        unsafe { Self::from_raw_regions(host, &[(RAM_BASE, size)]) }
    }

    /// RAM as consecutive slices of one host allocation: `layout[i] = (gpa, size)`,
    /// in host order.
    ///
    /// # Safety
    /// `host` must point to the sum of the sizes in bytes, mapped for the VM's lifetime.
    pub unsafe fn from_raw_regions(host: *mut u8, layout: &[(u64, u64)]) -> Self {
        let mut host_off = 0;
        let regions = layout
            .iter()
            .map(|&(gpa, size)| {
                let r = Region { gpa, size, host_off };
                host_off += size;
                r
            })
            .collect();
        Self { host, size: host_off, regions }
    }

    /// Start of the host allocation behind all regions (`size()` bytes).
    pub fn host_base(&self) -> *mut u8 {
        self.host
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    /// Host pointer for `[gpa, gpa+len)`, or None if any byte is outside RAM
    /// (including a range that runs from one region into the next).
    pub fn ptr(&self, gpa: u64, len: u64) -> Option<*mut u8> {
        let r = self.regions.iter().find(|r| gpa >= r.gpa && gpa - r.gpa < r.size)?;
        let off = gpa - r.gpa;
        if off.checked_add(len)? > r.size {
            return None;
        }
        Some(unsafe { self.host.add((r.host_off + off) as usize) })
    }

    pub fn read(&self, gpa: u64, buf: &mut [u8]) -> bool {
        match self.ptr(gpa, buf.len() as u64) {
            Some(p) => {
                unsafe { std::ptr::copy_nonoverlapping(p, buf.as_mut_ptr(), buf.len()) };
                true
            }
            None => false,
        }
    }

    pub fn write(&self, gpa: u64, buf: &[u8]) -> bool {
        match self.ptr(gpa, buf.len() as u64) {
            Some(p) => {
                unsafe { std::ptr::copy_nonoverlapping(buf.as_ptr(), p, buf.len()) };
                true
            }
            None => false,
        }
    }

    pub fn read_obj<T: Copy + Default>(&self, gpa: u64) -> Option<T> {
        let mut v = T::default();
        let bytes = unsafe { std::slice::from_raw_parts_mut(&mut v as *mut T as *mut u8, std::mem::size_of::<T>()) };
        self.read(gpa, bytes).then_some(v)
    }

    pub fn write_obj<T: Copy>(&self, gpa: u64, v: T) -> bool {
        let bytes = unsafe { std::slice::from_raw_parts(&v as *const T as *const u8, std::mem::size_of::<T>()) };
        self.write(gpa, bytes)
    }

    /// Give the host pages backing `[gpa, gpa+len)` back to macOS. The guest has
    /// promised not to rely on their contents; later guest accesses must still work
    /// (and see either old data or zeros). Only whole host pages inside the range
    /// are released. Returns bytes released.
    pub fn release(&self, gpa: u64, len: u64) -> u64 {
        crate::reclaim::release_range(self, gpa, len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ptr_maps_each_region_and_rejects_the_hole() {
        let mut buf = vec![0u8; 0x3000];
        let mem = unsafe { GuestMemory::from_raw_regions(buf.as_mut_ptr(), &[(0, 0x1000), (0x10_0000, 0x2000)]) };
        let host = buf.as_mut_ptr();
        assert_eq!(mem.size(), 0x3000);
        assert_eq!(mem.ptr(0x0ff0, 0x10), Some(host.wrapping_add(0x0ff0)));
        assert_eq!(mem.ptr(0x10_0000, 0x2000), Some(host.wrapping_add(0x1000)));
        assert_eq!(mem.ptr(0x0ff0, 0x11), None, "runs past the first region");
        assert_eq!(mem.ptr(0x8000, 1), None, "in the hole");
        assert_eq!(mem.ptr(0x10_1fff, 2), None, "past the end");
        assert!(mem.write(0x10_0000, &[7]));
        assert_eq!(buf[0x1000], 7);
    }
}
