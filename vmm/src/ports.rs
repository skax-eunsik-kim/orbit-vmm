//! Publishes guest ports on the Mac's localhost, like Docker Desktop/OrbStack.
//!
//! Two sources, merged:
//! - Docker itself: the VMM follows its own docker.sock (`/events` for
//!   container start/die) and re-reads `/containers/json` on every event, so a
//!   `-p` publish is forwarded as soon as the container starts. That includes
//!   ports published on a specific host IP (`-p 127.0.0.1:8080:80`), which the
//!   guest's docker-proxy wrapper DNATs from eth0 (guest/etc/orbit/bin/docker-proxy).
//! - Every 250 ms (fallback, and for `--network host` services), the guest
//!   control channel (vsock port 1025, `ports`) lists TCP ports listening on
//!   all addresses.
//!
//! The union is kept as slirp host forwards from the Mac's 127.0.0.1.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::virtio::net::PortControl;

/// Ports never published on the Mac (guest-internal services).
const RESERVED: &[u16] = &[53];
/// Upper bound on published ports, so a misbehaving guest can't make the VMM
/// open tens of thousands of listeners.
const MAX_PORTS: usize = 1024;
/// Replies on the control channel are a few lines; anything bigger is junk.
const MAX_REPLY: u64 = 64 << 10;
/// `/containers/json` is ~1-2 KiB per running container.
const MAX_DOCKER_REPLY: u64 = 4 << 20;
const POLL: Duration = Duration::from_millis(250);
/// Without events, re-read the container list this often anyway (an event
/// that raced the subscription is never missed for long).
const DOCKER_IDLE: Duration = Duration::from_secs(5);

/// One request/reply on a guest socket, bounded in size and total time
/// (the guest is untrusted: it must not be able to stall or bloat us).
fn ask(path: &PathBuf, line: &str) -> Option<String> {
    ask_bounded(path, line, MAX_REPLY)
}

fn ask_bounded(path: &PathBuf, line: &str, max: u64) -> Option<String> {
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
                if out.len() as u64 > max {
                    return None;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    String::from_utf8(out).ok()
}

pub fn spawn(control_socket: PathBuf, docker_socket: Option<PathBuf>, net: PortControl, fixed: Vec<u16>) {
    let (tx, rx) = mpsc::channel::<BTreeSet<u16>>();
    if let Some(docker) = docker_socket {
        let tx = tx.clone();
        std::thread::Builder::new()
            .name("ports-docker".into())
            .spawn(move || follow_docker(&docker, &tx))
            .expect("spawn ports-docker thread");
    }
    std::thread::Builder::new()
        .name("ports".into())
        .spawn(move || {
            let _keep = tx; // no docker thread: recv_timeout must still time out, not disconnect
            let mut active: BTreeSet<u16> = BTreeSet::new();
            let (mut listening, mut published) = (BTreeSet::new(), BTreeSet::new());
            let mut next_poll = Instant::now() + POLL;
            loop {
                match rx.recv_timeout(next_poll.saturating_duration_since(Instant::now())) {
                    Ok(p) => published = p,
                    Err(_) => {
                        if let Some(l) = query(&control_socket) {
                            listening = l;
                        }
                        next_poll = Instant::now() + POLL;
                    }
                }
                let wanted: BTreeSet<u16> = listening
                    .union(&published)
                    .copied()
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

fn query(path: &PathBuf) -> Option<BTreeSet<u16>> {
    Some(ask(path, "ports\n")?.lines().filter_map(|l| l.trim().parse().ok()).collect())
}

/// Subscribes to container start/die events and sends the published TCP
/// ports after each one (and after subscribing, and when idle). The event
/// bodies are not parsed: any bytes on the stream mean "look again".
fn follow_docker(path: &PathBuf, tx: &mpsc::Sender<BTreeSet<u16>>) {
    const EVENTS: &str = "GET /events?filters=%7B%22type%22%3A%5B%22container%22%5D%2C%22event%22%3A%5B%22start%22%2C%22die%22%5D%7D HTTP/1.1\r\nHost: orbit\r\n\r\n";
    let refresh = || {
        // A failed read (dockerd busy or restarting) is retried briefly; the
        // idle refresh catches anything later.
        for _ in 0..3 {
            if let Some(p) = published_ports(path) {
                let _ = tx.send(p);
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    let mut buf = [0u8; 4096];
    loop {
        let Ok(mut s) = UnixStream::connect(path) else {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        };
        let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
        let _ = s.set_read_timeout(Some(DOCKER_IDLE));
        if s.write_all(EVENTS.as_bytes()).is_err() {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        refresh();
        loop {
            match s.read(&mut buf) {
                Ok(0) => break,
                Ok(_) => refresh(),
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                    refresh()
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        // Daemon restarting or the guest rebooting.
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn published_ports(path: &PathBuf) -> Option<BTreeSet<u16>> {
    let reply = ask_bounded(path, "GET /containers/json HTTP/1.0\r\nHost: orbit\r\n\r\n", MAX_DOCKER_REPLY)?;
    let (head, body) = reply.split_once("\r\n\r\n")?;
    let status = head.lines().next()?;
    if !status.starts_with("HTTP/1.") || status.split_whitespace().nth(1) != Some("200") {
        return None;
    }
    Some(parse_ports(body))
}

/// TCP `PublicPort`s in a `/containers/json` body, whatever their `IP`. Port
/// entries are the only flat objects with that key, e.g.
/// `{"IP":"127.0.0.1","PrivatePort":80,"PublicPort":8080,"Type":"tcp"}`
/// (quotes inside JSON strings are escaped, so labels can't fake one).
fn parse_ports(body: &str) -> BTreeSet<u16> {
    const KEY: &str = "\"PublicPort\":";
    let mut out = BTreeSet::new();
    let mut from = 0;
    while let Some(i) = body[from..].find(KEY).map(|i| from + i) {
        from = i + KEY.len();
        let start = body[..i].rfind('{').map_or(0, |s| s + 1);
        let end = body[from..].find('}').map_or(body.len(), |e| from + e);
        let obj = &body[start..end];
        if obj.contains('{') || !obj.contains("\"Type\":\"tcp\"") {
            continue;
        }
        let digits: String = body[from..end].chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(p) = digits.parse::<u16>() {
            out.insert(p);
            if out.len() >= MAX_PORTS {
                break;
            }
        }
    }
    out
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

#[cfg(test)]
mod tests {
    use super::parse_ports;

    #[test]
    fn parses_published_tcp_ports_on_any_ip() {
        let body = r#"[{"Id":"a","Names":["/x"],"Labels":{"l":"{\"PublicPort\":22,\"Type\":\"tcp\"}"},"Ports":[{"IP":"127.0.0.1","PrivatePort":8080,"PublicPort":32768,"Type":"tcp"},{"IP":"0.0.0.0","PrivatePort":80,"PublicPort":8081,"Type":"tcp"},{"IP":"::","PrivatePort":80,"PublicPort":8081,"Type":"tcp"},{"IP":"0.0.0.0","PrivatePort":53,"PublicPort":5353,"Type":"udp"},{"PrivatePort":9000,"Type":"tcp"}]}]"#;
        assert_eq!(parse_ports(body).into_iter().collect::<Vec<_>>(), vec![8081, 32768]);
        assert!(parse_ports(r#"{"PublicPort":99999,"Type":"tcp"}"#).is_empty());
        assert!(parse_ports("").is_empty());
    }
}
