//! virtio-vsock (virtio 1.x section 5.10), stream sockets only.
//!
//! Host side: each `--vsock <unix path>:<guest port>` listens on a unix
//! socket; every accepted connection becomes a vsock stream to that guest
//! port (this is how `docker` on macOS reaches dockerd in the guest).
//! Guest-initiated connections to the host are refused (RST).
//!
//! Queues: rx = 0 (host → guest), tx = 1 (guest → host), event = 2.
//! Flow control follows the spec's credit scheme: we may have at most
//! peer.buf_alloc - (tx_cnt - peer.fwd_cnt) bytes in flight to the guest.

use std::collections::{HashMap, VecDeque};
use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::{Queue, VirtioDevice, VirtioInterrupt, VIRTIO_F_VERSION_1};
use crate::memory::GuestMemory;

pub const DEVICE_ID_VSOCK: u32 = 19;

fn debug() -> bool {
    static D: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *D.get_or_init(|| std::env::var_os("ORBIT_DEBUG").is_some())
}
pub const GUEST_CID: u64 = 3;
const HOST_CID: u64 = 2;

const HDR_LEN: usize = 44;
const TYPE_STREAM: u16 = 1;
const OP_REQUEST: u16 = 1;
const OP_RESPONSE: u16 = 2;
const OP_RST: u16 = 3;
const OP_SHUTDOWN: u16 = 4;
const OP_RW: u16 = 5;
const OP_CREDIT_UPDATE: u16 = 6;
const OP_CREDIT_REQUEST: u16 = 7;
const SHUTDOWN_RCV: u32 = 1;
const SHUTDOWN_SEND: u32 = 2;

/// Receive buffer we advertise to the guest per connection.
const BUF_ALLOC: u32 = 256 * 1024;
const MAX_PAYLOAD: usize = 64 * 1024;

#[derive(Clone, Debug)]
pub struct VsockListen {
    pub path: PathBuf,
    pub guest_port: u32,
}

#[derive(Clone, Copy, Default)]
struct Hdr {
    src_cid: u64,
    dst_cid: u64,
    src_port: u32,
    dst_port: u32,
    len: u32,
    kind: u16,
    op: u16,
    flags: u32,
    buf_alloc: u32,
    fwd_cnt: u32,
}

impl Hdr {
    fn parse(b: &[u8]) -> Hdr {
        let u64_at = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
        let u32_at = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        let u16_at = |o: usize| u16::from_le_bytes(b[o..o + 2].try_into().unwrap());
        Hdr {
            src_cid: u64_at(0),
            dst_cid: u64_at(8),
            src_port: u32_at(16),
            dst_port: u32_at(20),
            len: u32_at(24),
            kind: u16_at(28),
            op: u16_at(30),
            flags: u32_at(32),
            buf_alloc: u32_at(36),
            fwd_cnt: u32_at(40),
        }
    }

    fn bytes(&self) -> [u8; HDR_LEN] {
        let mut b = [0u8; HDR_LEN];
        b[0..8].copy_from_slice(&self.src_cid.to_le_bytes());
        b[8..16].copy_from_slice(&self.dst_cid.to_le_bytes());
        b[16..20].copy_from_slice(&self.src_port.to_le_bytes());
        b[20..24].copy_from_slice(&self.dst_port.to_le_bytes());
        b[24..28].copy_from_slice(&self.len.to_le_bytes());
        b[28..30].copy_from_slice(&self.kind.to_le_bytes());
        b[30..32].copy_from_slice(&self.op.to_le_bytes());
        b[32..36].copy_from_slice(&self.flags.to_le_bytes());
        b[36..40].copy_from_slice(&self.buf_alloc.to_le_bytes());
        b[40..44].copy_from_slice(&self.fwd_cnt.to_le_bytes());
        b
    }
}

/// Connection key: (host port, guest port).
type Key = (u32, u32);

struct Conn {
    stream: UnixStream,
    connected: bool,
    /// Guest → host bytes not yet accepted by the unix socket.
    out: VecDeque<u8>,
    /// Host → guest accounting.
    tx_cnt: u32,
    peer_buf_alloc: u32,
    peer_fwd_cnt: u32,
    /// Guest → host accounting (what we've consumed and reported).
    fwd_cnt: u32,
    last_reported_fwd: u32,
    host_eof: bool,
    guest_done_sending: bool,
}

impl Conn {
    fn credit(&self) -> usize {
        self.peer_buf_alloc.saturating_sub(self.tx_cnt.wrapping_sub(self.peer_fwd_cnt)) as usize
    }
}

#[derive(Default)]
struct Inner {
    mem: Option<Arc<GuestMemory>>,
    rx: Option<Queue>,
    tx: Option<Queue>,
    interrupt: Option<Arc<dyn VirtioInterrupt>>,
    conns: HashMap<Key, Conn>,
    /// Control/data packets waiting for an RX buffer.
    pending: VecDeque<Vec<u8>>,
    next_host_port: u32,
}

impl Inner {
    fn hdr(&self, key: Key, op: u16, len: u32, flags: u32) -> Hdr {
        let fwd = self.conns.get(&key).map_or(0, |c| c.fwd_cnt);
        Hdr {
            src_cid: HOST_CID,
            dst_cid: GUEST_CID,
            src_port: key.0,
            dst_port: key.1,
            len,
            kind: TYPE_STREAM,
            op,
            flags,
            buf_alloc: BUF_ALLOC,
            fwd_cnt: fwd,
        }
    }

    fn queue_ctrl(&mut self, key: Key, op: u16, flags: u32) {
        if self.pending.len() > 4096 {
            return; // guest isn't draining RX; drop control replies rather than grow
        }
        if debug() {
            eprintln!("vsock rx: op={op} host:{} -> guest:{} flags={flags}", key.0, key.1);
        }
        let h = self.hdr(key, op, 0, flags);
        self.pending.push_back(h.bytes().to_vec());
    }

    /// Moves pending packets into guest RX buffers; returns true if any went in.
    fn flush_rx(&mut self) -> bool {
        let Some(mem) = self.mem.clone() else {
            return false;
        };
        let Some(rx) = self.rx.as_mut() else {
            return false;
        };
        let mut used = false;
        while !self.pending.is_empty() {
            let Some(chain) = rx.pop(&mem) else { break };
            let mut pkt = self.pending.pop_front().unwrap();
            // A stream can be cut anywhere: split RW packets that don't fit this buffer.
            let cap: usize = chain.descs.iter().filter(|d| d.write_only).map(|d| d.len as usize).sum();
            if pkt.len() > cap && cap > HDR_LEN {
                let mut h = Hdr::parse(&pkt);
                if h.op == OP_RW {
                    let first = cap - HDR_LEN;
                    let rest_payload = pkt.split_off(HDR_LEN + first);
                    let mut rest = h;
                    rest.len = rest_payload.len() as u32;
                    let mut tail = rest.bytes().to_vec();
                    tail.extend_from_slice(&rest_payload);
                    self.pending.push_front(tail);
                    h.len = first as u32;
                    pkt[..HDR_LEN].copy_from_slice(&h.bytes());
                }
            }
            let mut off = 0;
            for d in chain.descs.iter().filter(|d| d.write_only) {
                if off >= pkt.len() {
                    break;
                }
                let n = (d.len as usize).min(pkt.len() - off);
                mem.write(d.addr, &pkt[off..off + n]);
                off += n;
            }
            rx.add_used(&mem, chain.head, off as u32);
            used = true;
        }
        if used {
            self.signal();
        }
        used
    }

    /// Whether the driver has an RX queue up (flush_rx splits packets to fit
    /// whatever buffers it posts).
    fn rx_room(&self) -> usize {
        if self.rx.as_ref().is_some_and(|q| q.ready) {
            MAX_PAYLOAD
        } else {
            0
        }
    }

    fn signal(&self) {
        if let Some(i) = &self.interrupt {
            i.signal_used_queue();
        }
    }

    fn reset_conn(&mut self, key: Key) {
        if self.conns.remove(&key).is_some() {
            self.queue_ctrl(key, OP_RST, 0);
        }
    }

    /// One guest → host packet from the TX queue.
    fn handle_tx(&mut self, h: Hdr, payload: &[u8]) {
        if debug() {
            eprintln!(
                "vsock tx: op={} {}:{} -> {}:{} len={} flags={} buf_alloc={} fwd={}",
                h.op, h.src_cid, h.src_port, h.dst_cid, h.dst_port, h.len, h.flags, h.buf_alloc, h.fwd_cnt
            );
        }
        if h.kind != TYPE_STREAM || h.dst_cid != HOST_CID {
            return;
        }
        let key = (h.dst_port, h.src_port);
        let Some(conn) = self.conns.get_mut(&key) else {
            if h.op != OP_RST {
                // Unknown connection, or a guest-initiated connect: refuse.
                let rst = Hdr {
                    src_cid: HOST_CID,
                    dst_cid: GUEST_CID,
                    src_port: h.dst_port,
                    dst_port: h.src_port,
                    kind: TYPE_STREAM,
                    op: OP_RST,
                    ..Default::default()
                };
                self.pending.push_back(rst.bytes().to_vec());
            }
            return;
        };
        conn.peer_buf_alloc = h.buf_alloc;
        conn.peer_fwd_cnt = h.fwd_cnt;
        match h.op {
            OP_RESPONSE => conn.connected = true,
            OP_RW => {
                // A guest that ignores our credit could grow this forever.
                if conn.out.len() + payload.len() > 4 * BUF_ALLOC as usize {
                    self.reset_conn(key);
                    return;
                }
                conn.out.extend(payload);
                flush_out(conn);
            }
            OP_CREDIT_REQUEST => self.queue_ctrl(key, OP_CREDIT_UPDATE, 0),
            OP_CREDIT_UPDATE => {}
            OP_SHUTDOWN => {
                if h.flags & SHUTDOWN_SEND != 0 {
                    conn.guest_done_sending = true;
                    if conn.out.is_empty() {
                        let _ = conn.stream.shutdown(std::net::Shutdown::Write);
                    }
                }
                if h.flags & SHUTDOWN_RCV != 0 && h.flags & SHUTDOWN_SEND != 0 {
                    self.reset_conn(key);
                }
            }
            OP_RST => {
                self.conns.remove(&key);
            }
            _ => {}
        }
        self.maybe_credit_update(key);
    }

    /// Tell the guest we've drained its data once enough has piled up.
    fn maybe_credit_update(&mut self, key: Key) {
        if let Some(c) = self.conns.get_mut(&key) {
            if c.fwd_cnt.wrapping_sub(c.last_reported_fwd) >= BUF_ALLOC / 4 {
                c.last_reported_fwd = c.fwd_cnt;
                self.queue_ctrl(key, OP_CREDIT_UPDATE, 0);
            }
        }
    }

    /// Read from host sockets into RW packets while the guest has credit and
    /// buffers; returns true if anything was queued.
    fn pump_host(&mut self, key: Key) -> bool {
        let mut queued = false;
        loop {
            let room = self.rx_room();
            let Some(conn) = self.conns.get_mut(&key) else {
                return queued;
            };
            if !conn.connected || conn.host_eof || room == 0 || self.pending.len() > 64 {
                return queued;
            }
            let want = conn.credit().min(MAX_PAYLOAD).min(room);
            if want == 0 {
                return queued;
            }
            let mut buf = vec![0u8; want];
            let r = conn.stream.read(&mut buf);
            if debug() {
                eprintln!("vsock host read({want}) on {key:?} -> {r:?}");
            }
            match r {
                Ok(0) => {
                    conn.host_eof = true;
                    self.queue_ctrl(key, OP_SHUTDOWN, SHUTDOWN_SEND);
                    return true;
                }
                Ok(n) => {
                    conn.tx_cnt = conn.tx_cnt.wrapping_add(n as u32);
                    let h = self.hdr(key, OP_RW, n as u32, 0);
                    let mut pkt = h.bytes().to_vec();
                    pkt.extend_from_slice(&buf[..n]);
                    self.pending.push_back(pkt);
                    queued = true;
                    if !self.flush_rx() {
                        return queued;
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => return queued,
                Err(_) => {
                    self.reset_conn(key);
                    return true;
                }
            }
        }
    }
}

/// Writes as much buffered guest data to the unix socket as it takes now.
fn flush_out(conn: &mut Conn) {
    while !conn.out.is_empty() {
        let (a, _) = conn.out.as_slices();
        match conn.stream.write(a) {
            Ok(0) => break,
            Ok(n) => {
                conn.out.drain(..n);
                conn.fwd_cnt = conn.fwd_cnt.wrapping_add(n as u32);
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => break,
            Err(_) => {
                conn.out.clear();
                break;
            }
        }
    }
    if conn.out.is_empty() && conn.guest_done_sending {
        let _ = conn.stream.shutdown(std::net::Shutdown::Write);
    }
}

pub struct Vsock {
    inner: Arc<Mutex<Inner>>,
    wake_write: RawFd,
}

impl Vsock {
    pub fn new(listens: Vec<VsockListen>) -> Result<Self, String> {
        let mut listeners = Vec::new();
        for l in &listens {
            // Bind next to the target and rename over it, so a socket in use is
            // never left missing, and make it owner-only (docker.sock = your account).
            let tmp = l.path.with_extension(format!("tmp-{}", std::process::id()));
            let _ = std::fs::remove_file(&tmp);
            let sock = UnixListener::bind(&tmp).map_err(|e| format!("binding {}: {e}", tmp.display()))?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
            std::fs::rename(&tmp, &l.path).map_err(|e| format!("installing {}: {e}", l.path.display()))?;
            sock.set_nonblocking(true).map_err(|e| e.to_string())?;
            listeners.push((sock, l.guest_port));
        }
        let mut fds = [0; 2];
        unsafe { libc::pipe(fds.as_mut_ptr()) };
        for fd in fds {
            unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) };
        }
        let inner = Arc::new(Mutex::new(Inner { next_host_port: 1024, ..Default::default() }));
        let shared = inner.clone();
        std::thread::Builder::new()
            .name("vsock".into())
            .spawn(move || host_loop(shared, listeners, fds[0]))
            .map_err(|e| e.to_string())?;
        Ok(Self { inner, wake_write: fds[1] })
    }

    fn wake(&self) {
        unsafe { libc::write(self.wake_write, [1u8].as_ptr().cast(), 1) };
    }
}

/// Host side: accept unix connections, move host → guest data, retry writes.
fn host_loop(inner: Arc<Mutex<Inner>>, listeners: Vec<(UnixListener, u32)>, wake_read: RawFd) {
    let mut wake_buf = [0u8; 256];
    loop {
        let mut fds = vec![libc::pollfd { fd: wake_read, events: libc::POLLIN, revents: 0 }];
        for (l, _) in &listeners {
            fds.push(libc::pollfd { fd: l.as_raw_fd(), events: libc::POLLIN, revents: 0 });
        }
        let mut keys = Vec::new();
        {
            let st = inner.lock().unwrap();
            let can_rx = st.rx_room() > 0 && st.pending.len() <= 64;
            for (key, c) in &st.conns {
                let mut ev = 0;
                if c.connected && !c.host_eof && can_rx && c.credit() > 0 {
                    ev |= libc::POLLIN;
                }
                if !c.out.is_empty() {
                    ev |= libc::POLLOUT;
                }
                fds.push(libc::pollfd { fd: c.stream.as_raw_fd(), events: ev, revents: 0 });
                keys.push(*key);
            }
        }
        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as u32, 200) };
        if fds[0].revents & libc::POLLIN != 0 {
            unsafe { libc::read(wake_read, wake_buf.as_mut_ptr().cast(), wake_buf.len()) };
        }

        let mut st = inner.lock().unwrap();
        if st.mem.is_none() {
            continue; // driver not up yet; leave connections in the backlog
        }
        for (i, (l, guest_port)) in listeners.iter().enumerate() {
            if fds[1 + i].revents & libc::POLLIN == 0 {
                continue;
            }
            while let Ok((stream, _)) = l.accept() {
                let _ = stream.set_nonblocking(true);
                let host_port = st.next_host_port;
                st.next_host_port = st.next_host_port.wrapping_add(1).max(1024);
                let key = (host_port, *guest_port);
                st.conns.insert(
                    key,
                    Conn {
                        stream,
                        connected: false,
                        out: VecDeque::new(),
                        tx_cnt: 0,
                        peer_buf_alloc: 0,
                        peer_fwd_cnt: 0,
                        fwd_cnt: 0,
                        last_reported_fwd: 0,
                        host_eof: false,
                        guest_done_sending: false,
                    },
                );
                st.queue_ctrl(key, OP_REQUEST, 0);
            }
        }
        let base = 1 + listeners.len();
        for (i, key) in keys.iter().enumerate() {
            let rev = fds[base + i].revents;
            if rev & libc::POLLOUT != 0 {
                if let Some(c) = st.conns.get_mut(key) {
                    flush_out(c);
                }
                st.maybe_credit_update(*key);
            }
            if rev & (libc::POLLIN | libc::POLLHUP) != 0 {
                st.pump_host(*key);
            }
        }
        if st.flush_rx() {
            st.signal();
        }
    }
}

impl VirtioDevice for Vsock {
    fn device_id(&self) -> u32 {
        DEVICE_ID_VSOCK
    }

    fn features(&self) -> u64 {
        VIRTIO_F_VERSION_1
    }

    fn num_queues(&self) -> usize {
        3
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        let cfg = GUEST_CID.to_le_bytes();
        for (i, b) in data.iter_mut().enumerate() {
            *b = cfg.get(offset as usize + i).copied().unwrap_or(0);
        }
    }

    fn write_config(&mut self, _offset: u64, _data: &[u8]) {}

    fn activate(
        &mut self,
        mem: Arc<GuestMemory>,
        mut queues: Vec<Queue>,
        interrupt: Arc<dyn VirtioInterrupt>,
        _acked: u64,
    ) {
        let mut st = self.inner.lock().unwrap();
        queues.truncate(2); // the event queue is unused
        st.tx = queues.pop();
        st.rx = queues.pop();
        st.mem = Some(mem);
        st.interrupt = Some(interrupt);
        drop(st);
        self.wake();
    }

    fn queue_notify(&mut self, index: u16) {
        let mut st = self.inner.lock().unwrap();
        let Some(mem) = st.mem.clone() else { return };
        match index {
            0 => {
                // New RX buffers: deliver backlog, and let the host loop read more.
                if st.flush_rx() {
                    st.signal();
                }
            }
            1 => {
                let mut tx = st.tx.take();
                let mut used = false;
                if let Some(q) = tx.as_mut() {
                    while let Some(chain) = q.pop(&mem) {
                        // Linux sends at most 64 KiB of payload per packet.
                        let pkt =
                            super::queue::read_readable(&mem, &chain.descs, HDR_LEN + MAX_PAYLOAD).unwrap_or_default();
                        if pkt.len() >= HDR_LEN {
                            let h = Hdr::parse(&pkt);
                            let end = (HDR_LEN + h.len as usize).min(pkt.len());
                            st.handle_tx(h, &pkt[HDR_LEN..end]);
                        }
                        q.add_used(&mem, chain.head, 0);
                        used = true;
                    }
                }
                st.tx = tx;
                let rx_used = st.flush_rx();
                if used || rx_used {
                    st.signal();
                }
            }
            _ => {}
        }
        drop(st);
        self.wake();
    }

    fn reset(&mut self) {
        let mut st = self.inner.lock().unwrap();
        st.conns.clear();
        st.pending.clear();
        st.mem = None;
        st.rx = None;
        st.tx = None;
        st.interrupt = None;
    }
}
