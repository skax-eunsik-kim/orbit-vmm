//! Passes macOS memory pressure on to the guest.
//!
//! When the Mac runs short, macOS compresses or swaps whatever guest RAM the
//! VMM holds, and a compressed page still counts in Activity Monitor (by
//! about a third to a half of its size). Page cache the guest could simply
//! drop is the cheapest thing to give back, so on WARN/CRITICAL the guest is
//! told `pressure <level>` on the control vsock (port 1025): memd lowers its
//! page-cache cap and reclaims right away, and free page reporting returns
//! the pages. `pressure normal` restores the usual cap.
//!
//! The level is the system-wide `kern.memorystatus_vm_pressure_level`. A
//! libdispatch memory-pressure source wakes us when it changes; it is also
//! re-read every 30 s (and re-sent while not normal, so a restarted guest
//! catches up). `ORBIT_PRESSURE=off` turns this off and `ORBIT_PRESSURE=warn`
//! or `critical` pins the level, for comparisons that shouldn't depend on
//! how loaded the Mac happens to be.

use std::ffi::c_void;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::OnceLock;
use std::time::Duration;

#[repr(C)]
struct DispatchSourceType {
    _private: [u8; 0],
}

extern "C" {
    static _dispatch_source_type_memorypressure: DispatchSourceType;
    fn dispatch_get_global_queue(identifier: isize, flags: usize) -> *mut c_void;
    fn dispatch_source_create(
        ty: *const DispatchSourceType,
        handle: usize,
        mask: usize,
        queue: *mut c_void,
    ) -> *mut c_void;
    fn dispatch_source_set_event_handler_f(source: *mut c_void, handler: extern "C" fn(*mut c_void));
    fn dispatch_resume(object: *mut c_void);
}

const DISPATCH_MEMORYPRESSURE_ALL: usize = 0x1 | 0x2 | 0x4; // NORMAL | WARN | CRITICAL
const QOS_CLASS_UTILITY: isize = 0x11;

static WAKE: OnceLock<SyncSender<()>> = OnceLock::new();

extern "C" fn on_pressure(_ctx: *mut c_void) {
    if let Some(tx) = WAKE.get() {
        let _ = tx.try_send(());
    }
}

/// macOS pressure level: 1 normal, 2 warn, 4 critical.
fn level() -> &'static str {
    let mut v: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>();
    let rc = unsafe {
        libc::sysctlbyname(
            c"kern.memorystatus_vm_pressure_level".as_ptr(),
            (&mut v as *mut libc::c_int).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    match (rc, v) {
        (0, 4) => "critical",
        (0, 2) => "warn",
        _ => "normal",
    }
}

/// One command on the control socket; true when the guest answered `ok`.
/// The reply is a single short line, read with a deadline and a size bound.
fn tell(path: &PathBuf, level: &str) -> bool {
    let Ok(mut s) = UnixStream::connect(path) else {
        return false;
    };
    let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
    let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
    if s.write_all(format!("pressure {level}\n").as_bytes()).is_err() {
        return false;
    }
    let mut buf = [0u8; 16];
    let mut n = 0;
    while n < buf.len() {
        match s.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
    &buf[..n] == b"ok\n"
}

pub fn spawn(config: &crate::Config) {
    let pinned: Option<&'static str> = match std::env::var("ORBIT_PRESSURE").as_deref() {
        Ok("off") => return,
        Ok("warn") => Some("warn"),
        Ok("critical") => Some("critical"),
        _ => None,
    };
    let Some(path) = config.vsock.iter().find(|v| v.guest_port == 1025).map(|v| v.path.clone()) else {
        return;
    };
    let (tx, rx) = sync_channel(1);
    if WAKE.set(tx).is_ok() {
        unsafe {
            let queue = dispatch_get_global_queue(QOS_CLASS_UTILITY, 0);
            let source =
                dispatch_source_create(&_dispatch_source_type_memorypressure, 0, DISPATCH_MEMORYPRESSURE_ALL, queue);
            if !source.is_null() {
                dispatch_source_set_event_handler_f(source, on_pressure);
                dispatch_resume(source); // lives as long as the process
            }
        }
    }
    std::thread::Builder::new()
        .name("pressure".into())
        .spawn(move || {
            let mut told: Option<&str> = None;
            loop {
                let now = pinned.unwrap_or_else(level);
                if (told != Some(now) || now != "normal") && tell(&path, now) {
                    if told.is_some_and(|t| t != now) || (told.is_none() && now != "normal") {
                        eprintln!("orbit-vmm: macOS memory pressure is {now}, told the guest");
                    }
                    told = Some(now);
                }
                // Retry soon while the guest isn't answering yet (boot).
                let wait = if told == Some(now) { 30 } else { 2 };
                let _ = rx.recv_timeout(Duration::from_secs(wait));
            }
        })
        .expect("spawn pressure thread");
}
