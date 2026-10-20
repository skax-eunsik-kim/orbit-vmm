//! SIGTERM / SIGINT sent to the VMM itself. Without a handler the process
//! died on the spot: the guest's filesystems were never unmounted, and nothing
//! said who sent it (a VM was lost that way and the sender couldn't be found).
//! Now the VMM logs the sender's pid and name, asks the guest to power off over
//! the control channel, and exits with 128 + signal once it has (or after 30 s),
//! so the supervisor still treats it as an unexpected stop and restarts it.
//! A second signal exits at once.

use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

static SENDER: AtomicI32 = AtomicI32::new(0);
static RECEIVED: AtomicI32 = AtomicI32::new(0);
static PIPE: AtomicI32 = AtomicI32::new(-1);
const GRACE: Duration = Duration::from_secs(30);

extern "C" fn on_signal(sig: libc::c_int, info: *mut libc::siginfo_t, _: *mut libc::c_void) {
    // Async-signal-safe only: atomics and write(2).
    if !info.is_null() {
        SENDER.store(unsafe { (*info).si_pid }, Ordering::Relaxed);
    }
    RECEIVED.store(sig, Ordering::Relaxed);
    let b = sig as u8;
    unsafe { libc::write(PIPE.load(Ordering::Relaxed), (&b as *const u8).cast(), 1) };
}

fn process_name(pid: i32) -> String {
    let mut buf = [0u8; 256];
    let n = unsafe { libc::proc_name(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n <= 0 {
        return "?".into();
    }
    String::from_utf8_lossy(&buf[..n as usize]).into_owned()
}

/// The signal that started a shutdown, if any (for the exit status).
pub fn received() -> Option<i32> {
    Some(RECEIVED.load(Ordering::Relaxed)).filter(|s| *s != 0)
}

/// `control`: the guest control socket (vsock port 1025), if there is one.
pub fn install(control: Option<PathBuf>) {
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return;
    }
    PIPE.store(fds[1], Ordering::Relaxed);
    for sig in [libc::SIGTERM, libc::SIGINT] {
        let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
        sa.sa_sigaction = on_signal as extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void) as usize;
        sa.sa_flags = libc::SA_SIGINFO;
        unsafe { libc::sigaction(sig, &sa, std::ptr::null_mut()) };
    }
    let read_fd = fds[0];
    std::thread::Builder::new()
        .name("signals".into())
        .spawn(move || {
            let mut seen = 0;
            loop {
                let mut b = [0u8; 1];
                if unsafe { libc::read(read_fd, b.as_mut_ptr().cast(), 1) } != 1 {
                    continue;
                }
                seen += 1;
                let sig = b[0] as i32;
                let pid = SENDER.load(Ordering::Relaxed);
                let name = if sig == libc::SIGTERM { "SIGTERM" } else { "SIGINT" };
                let Some(ctl) = control.as_ref().filter(|_| seen == 1) else {
                    eprintln!("orbit-vmm: {name} from pid {pid} ({}); exiting now", process_name(pid));
                    std::process::exit(128 + sig);
                };
                eprintln!("orbit-vmm: {name} from pid {pid} ({}); shutting the guest down", process_name(pid));
                if crate::ports::ask(ctl, "shutdown\n").is_none() {
                    eprintln!("orbit-vmm: the guest did not take the shutdown request; exiting");
                    std::process::exit(128 + sig);
                }
                // The guest powering off ends vm::run; main then exits 128 + sig.
                std::thread::spawn(move || {
                    std::thread::sleep(GRACE);
                    eprintln!("orbit-vmm: the guest did not power off within {}s; exiting", GRACE.as_secs());
                    std::process::exit(128 + sig);
                });
            }
        })
        .expect("spawn signals thread");
}
