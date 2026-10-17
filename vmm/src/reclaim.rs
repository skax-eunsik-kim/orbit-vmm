//! Returning guest pages to macOS.
//!
//! Guest RAM comes from hv_vm_allocate (anonymous memory with accurate
//! accounting). For a range the guest reported free we tell the kernel the
//! pages' contents are disposable. Strategies (ORBIT_RECLAIM env var):
//!   reusable (default)  madvise(MADV_FREE_REUSABLE): pages leave the
//!                       footprint right away; the guest touching them again
//!                       just faults in (possibly zeroed) pages.
//!   free                madvise(MADV_FREE): reclaimed only under pressure.
//!   none                don't release (baseline for measurements).

use std::sync::OnceLock;

use crate::memory::{GuestMemory, HOST_PAGE_SIZE};
use crate::vm::hvf::{self, hv};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Strategy {
    Fresh,
    Reusable,
    Remap,
    Free,
    None,
}

fn strategy() -> Strategy {
    static S: OnceLock<Strategy> = OnceLock::new();
    *S.get_or_init(|| match std::env::var("ORBIT_RECLAIM").as_deref() {
        Ok("reusable") => Strategy::Reusable,
        Ok("remap") => Strategy::Remap,
        Ok("free") => Strategy::Free,
        Ok("none") => Strategy::None,
        _ => Strategy::Fresh,
    })
}

/// Swap the host pages behind `[gpa, gpa+size)` for untouched anonymous
/// memory: drop the stage-2 mapping, mmap(MAP_FIXED) fresh zero-fill-on-demand
/// pages over the range (freeing the old ones immediately), map it back.
/// Unlike MADV_FREE_REUSABLE there is no window where the kernel could later
/// discard data the guest wrote after reusing the pages. Measured with
/// --reclaim-selftest: this is the only method that frees hv_vm_allocate memory.
fn replace_with_fresh_pages(ptr: *mut u8, gpa: u64, size: usize) -> u64 {
    if hv!(hv_vm_unmap(gpa, size)).is_err() {
        return 0;
    }
    let fresh = unsafe {
        libc::mmap(
            ptr.cast(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_FIXED | libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        )
    };
    let flags = hvf::HV_MEMORY_READ | hvf::HV_MEMORY_WRITE | hvf::HV_MEMORY_EXEC;
    if fresh == libc::MAP_FAILED || hv!(hv_vm_map(ptr.cast(), gpa, size, flags)).is_err() {
        // A hole in guest RAM would corrupt the guest; stop instead.
        eprintln!("orbit-vmm: failed to restore guest RAM at 0x{gpa:x}");
        std::process::abort();
    }
    size as u64
}

/// This process's phys_footprint in MiB (what Activity Monitor calls Memory).
pub fn footprint_mib() -> u64 {
    let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::proc_pid_rusage(libc::getpid(), libc::RUSAGE_INFO_V2, &mut info as *mut _ as *mut libc::rusage_info_t)
    };
    if rc == 0 {
        info.ri_phys_footprint >> 20
    } else {
        0
    }
}

/// `orbit-vmm --reclaim-selftest`: with no guest involved, fill hypervisor
/// memory and check which release method actually lowers the footprint.
pub fn selftest() -> i32 {
    const SIZE: usize = 256 << 20;
    const IPA: u64 = 0x8000_0000;
    let flags = hvf::HV_MEMORY_READ | hvf::HV_MEMORY_WRITE | hvf::HV_MEMORY_EXEC;
    if let Err(e) = hv!(hv_vm_create(std::ptr::null_mut())) {
        eprintln!("{e}");
        return 1;
    }
    let fill = |p: *mut u8| unsafe { std::ptr::write_bytes(p, 0xa5, SIZE) };
    let report = |what: &str, before: u64| println!("{what:<44} {before:>4} -> {:>4} MiB", footprint_mib());

    type Method = (&'static str, bool, fn(*mut u8, usize) -> i32);
    let methods: [Method; 4] = [
        ("madvise(FREE_REUSABLE) while mapped", false, |p, n| unsafe {
            libc::madvise(p.cast(), n, libc::MADV_FREE_REUSABLE)
        }),
        ("unmap + madvise(FREE_REUSABLE) + map", true, |p, n| unsafe {
            libc::madvise(p.cast(), n, libc::MADV_FREE_REUSABLE)
        }),
        ("unmap + madvise(DONTNEED) + map", true, |p, n| unsafe { libc::madvise(p.cast(), n, libc::MADV_DONTNEED) }),
        ("unmap + mmap(MAP_FIXED anon) + map", true, |p, n| unsafe {
            let r = libc::mmap(
                p.cast(),
                n,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_FIXED | libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            );
            if r == libc::MAP_FAILED {
                -1
            } else {
                0
            }
        }),
    ];
    for (allocator, use_hv_alloc) in [("hv_vm_allocate", true), ("mmap", false)] {
        println!("== guest RAM from {allocator}");
        for (name, remap, release) in methods {
            let mut p: *mut libc::c_void = std::ptr::null_mut();
            if use_hv_alloc {
                if hv!(hv_vm_allocate(&mut p, SIZE, 0)).is_err() {
                    return 1;
                }
            } else {
                p = unsafe {
                    libc::mmap(
                        std::ptr::null_mut(),
                        SIZE,
                        libc::PROT_READ | libc::PROT_WRITE,
                        libc::MAP_PRIVATE | libc::MAP_ANON,
                        -1,
                        0,
                    )
                };
            }
            let _ = hv!(hv_vm_map(p, IPA, SIZE, flags));
            let base = footprint_mib();
            fill(p.cast());
            let filled = footprint_mib();
            if remap {
                let _ = hv!(hv_vm_unmap(IPA, SIZE));
            }
            let rc = release(p.cast(), SIZE);
            if remap {
                let _ = hv!(hv_vm_map(p, IPA, SIZE, flags));
            }
            println!("{name:<44} {base:>4} -> filled {filled:>4} -> released {:>4} MiB (rc={rc})", footprint_mib());
            let _ = hv!(hv_vm_unmap(IPA, SIZE));
            if use_hv_alloc {
                let _ = hv!(hv_vm_deallocate(p, SIZE));
            } else {
                unsafe { libc::munmap(p, SIZE) };
            }
        }
    }
    let _ = report;
    0
}

/// Releases the whole host pages inside `[gpa, gpa+len)`. Partial host pages at
/// either end stay resident (a 16 KiB host page may still hold live 4 KiB guest
/// pages). Returns the number of bytes released.
pub fn release_range(mem: &GuestMemory, gpa: u64, len: u64) -> u64 {
    let Some(start) = gpa.checked_next_multiple_of(HOST_PAGE_SIZE) else {
        return 0;
    };
    let Some(end) = gpa.checked_add(len).map(|e| e / HOST_PAGE_SIZE * HOST_PAGE_SIZE) else {
        return 0; // guest-supplied range wraps
    };
    if end <= start {
        return 0;
    }
    let Some(ptr) = mem.ptr(start, end - start) else {
        return 0;
    };
    let size = (end - start) as usize;
    if strategy() == Strategy::Fresh {
        return replace_with_fresh_pages(ptr, start, size);
    }
    let advice = match strategy() {
        Strategy::Fresh => unreachable!(),
        Strategy::Reusable | Strategy::Remap => libc::MADV_FREE_REUSABLE,
        Strategy::Free => libc::MADV_FREE,
        Strategy::None => return 0,
    };
    if strategy() == Strategy::Remap {
        // Drop the stage-2 mapping first so the hypervisor stops holding the
        // pages; the guest won't touch them until the report completes.
        if hv!(hv_vm_unmap(start, size)).is_err() {
            return 0;
        }
    }
    let rc = unsafe { libc::madvise(ptr as *mut libc::c_void, size, advice) };
    if strategy() == Strategy::Remap {
        let flags = hvf::HV_MEMORY_READ | hvf::HV_MEMORY_WRITE | hvf::HV_MEMORY_EXEC;
        if let Err(e) = hv!(hv_vm_map(ptr as *mut libc::c_void, start, size, flags)) {
            // Losing guest RAM is fatal; better to stop loudly than corrupt the guest.
            eprintln!("orbit-vmm: remapping guest RAM at 0x{start:x}: {e}");
            std::process::abort();
        }
    }
    if rc != 0 {
        return 0;
    }
    end - start
}
