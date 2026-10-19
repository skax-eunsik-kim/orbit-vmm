//! Debugging aid for "why is the VM bigger on the Mac than the guest uses":
//! with ORBIT_MEMDUMP=<prefix>, creating `<prefix>.req` makes the VMM write
//! `<prefix>.bin`, one mincore(2) byte per 16 KiB host page of guest RAM
//! (in core, paged out = compressed/swapped, ...). Pair it with the guest's
//! /proc/kpageflags to see which guest pages the Mac is still holding.
//! The prefix comes from the environment of whoever started the VMM, never
//! from the guest.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::memory::{GuestMemory, HOST_PAGE_SIZE, RAM_BASE};

pub fn spawn(mem: Arc<GuestMemory>) {
    let Some(prefix) = std::env::var_os("ORBIT_MEMDUMP").map(PathBuf::from) else {
        return;
    };
    let req = prefix.with_extension("req");
    let out = prefix.with_extension("bin");
    let tmp = prefix.with_extension("bin.tmp");
    std::thread::Builder::new()
        .name("memdump".into())
        .spawn(move || loop {
            std::thread::sleep(Duration::from_secs(1));
            if std::fs::remove_file(&req).is_err() {
                continue;
            }
            let Some(base) = mem.ptr(RAM_BASE, mem.size()) else {
                continue;
            };
            let mut vec = vec![0u8; (mem.size() / HOST_PAGE_SIZE) as usize];
            let rc = unsafe { libc::mincore(base.cast(), mem.size() as usize, vec.as_mut_ptr().cast()) };
            if rc != 0 {
                eprintln!("orbit-vmm: memdump: mincore failed: {}", std::io::Error::last_os_error());
                continue;
            }
            if std::fs::write(&tmp, &vec).and_then(|_| std::fs::rename(&tmp, &out)).is_err() {
                eprintln!("orbit-vmm: memdump: cannot write {}", out.display());
            }
        })
        .expect("spawn memdump thread");
}
