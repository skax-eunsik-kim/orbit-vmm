//! Publishes guest ports on the Mac's localhost, like Docker Desktop/OrbStack.
//!
//! Every 250 ms, ask the guest (control vsock port 1025, `ports`) which TCP
//! ports are listening on all addresses — docker-proxy does that for every
//! `-p`/`ports:` mapping — and add or remove the matching slirp host forwards.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::virtio::net::PortControl;

/// Ports never published on the Mac (guest-internal services).
const RESERVED: &[u16] = &[53];
/// Upper bound on published ports, so a misbehaving guest can't make the VMM
/// open tens of thousands of listeners.
const MAX_PORTS: usize = 1024;
/// Replies on the control channel are a few lines; anything bigger is junk.
const MAX_REPLY: u64 = 64 << 10;

/// One request/reply on the control socket, bounded in size and total time
/// (the guest is untrusted: it must not be able to stall or bloat us).
fn ask(path: &PathBuf, line: &str) -> Option<String> {
    let mut s = UnixStream::connect(path).ok()?;
    s.set_write_timeout(Some(Duration::from_secs(2))).ok()?;
    s.write_all(line.as_bytes()).ok()?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let left = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero())?;
        s.set_read_timeout(Some(left)).ok()?;
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                out.extend_from_slice(&buf[..n]);
                if out.len() as u64 > MAX_REPLY {
                    return None;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    String::from_utf8(out).ok()
}

pub fn spawn(control_socket: PathBuf, net: PortControl, fixed: Vec<u16>) {
    std::thread::Builder::new()
        .name("ports".into())
        .spawn(move || {
            let mut active: BTreeSet<u16> = BTreeSet::new();
            loop {
                std::thread::sleep(Duration::from_millis(250));
                let Some(listening) = query(&control_socket) else {
                    continue;
                };
                let wanted: BTreeSet<u16> = listening
                    .into_iter()
                    .filter(|p| *p != 0 && !RESERVED.contains(p) && !fixed.contains(p))
                    .take(MAX_PORTS)
                    .collect();
                for &p in wanted.difference(&active) {
                    net.set(p, true);
                }
                for &p in active.difference(&wanted) {
                    net.set(p, false);
                }
                active = wanted;
            }
        })
        .expect("spawn ports thread");
}

fn query(path: &PathBuf) -> Option<Vec<u16>> {
    Some(ask(path, "ports\n")?.lines().filter_map(|l| l.trim().parse().ok()).collect())
}

/// Keeps the guest clock on the Mac's. Every 5 s, ask the guest for its time
/// (`now`) and bracket the answer with the host clock: offset = guest - midpoint,
/// trusted only when the round trip was short (a busy guest answering late
/// would otherwise look like a skewed clock and get its time yanked around).
/// If it is more than 250 ms off, set it on the next whole host second.
/// This also catches up after the Mac sleeps (the guest's counter stops).
pub fn spawn_time_sync(control_socket: PathBuf) {
    fn now() -> f64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
    }
    std::thread::Builder::new()
        .name("timesync".into())
        .spawn(move || loop {
            std::thread::sleep(Duration::from_secs(5));
            let t0 = now();
            let Some(reply) = ask(&control_socket, "now\n") else {
                continue;
            };
            let t1 = now();
            let mut it = reply.split_whitespace().filter_map(|x| x.parse::<f64>().ok());
            let (Some(sec), Some(usec)) = (it.next(), it.next()) else {
                continue;
            };
            if t1 - t0 > 0.05 {
                continue; // too slow to measure reliably; try again later
            }
            let offset = sec + usec / 1e6 - (t0 + t1) / 2.0;
            if offset.abs() < 0.25 {
                continue;
            }
            let wait = 1.0 - now().fract();
            std::thread::sleep(Duration::from_secs_f64(wait));
            let target = now().round() as u64;
            if ask(&control_socket, &format!("time {target}\n")).is_some() {
                eprintln!("orbit-vmm: guest clock was off by {:+.0} ms; corrected", offset * 1000.0);
            }
        })
        .expect("spawn timesync thread");
}
