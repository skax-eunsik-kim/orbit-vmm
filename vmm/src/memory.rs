//! Guest RAM: one contiguous region at `RAM_BASE` in guest physical space,
//! backed by host anonymous memory from `hv_vm_allocate` (owner: core / agent A
//! for allocation + mapping, agent C for `release`).

/// Guest physical address where RAM starts (below it: MMIO devices).
pub const RAM_BASE: u64 = 0x8000_0000;

/// Host page size on Apple Silicon. Stage-2 mappings and madvise work at this granularity.
pub const HOST_PAGE_SIZE: u64 = 16 * 1024;

pub struct GuestMemory {
    host: *mut u8,
    size: u64,
}

// The region is shared by vCPU and device threads; accesses are raw byte copies.
unsafe impl Send for GuestMemory {}
unsafe impl Sync for GuestMemory {}

impl GuestMemory {
    /// # Safety
    /// `host` must point to `size` bytes that stay mapped for the VM's lifetime.
    pub unsafe fn from_raw(host: *mut u8, size: u64) -> Self {
        Self { host, size }
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    /// Host pointer for `[gpa, gpa+len)`, or None if any byte is outside RAM.
    pub fn ptr(&self, gpa: u64, len: u64) -> Option<*mut u8> {
        let off = gpa.checked_sub(RAM_BASE)?;
        if off.checked_add(len)? > self.size {
            return None;
        }
        Some(unsafe { self.host.add(off as usize) })
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
    /// are released. Returns bytes released. Implemented by agent C.
    pub fn release(&self, gpa: u64, len: u64) -> u64 {
        crate::reclaim::release_range(self, gpa, len)
    }
}
