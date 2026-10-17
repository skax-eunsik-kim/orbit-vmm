//! libslirp (user-mode TCP/IP NAT, as used by QEMU's `-netdev user`) bindings
//! and the network thread that drives it. Guest network: 10.0.2.0/24, host
//! (gateway) 10.0.2.2, DNS 10.0.2.3, guest via DHCP 10.0.2.15.
#![allow(non_camel_case_types)]

use std::ffi::c_void;
use std::net::Ipv4Addr;
use std::os::fd::RawFd;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Instant;

#[repr(C)]
struct SlirpConfig {
    version: u32,
    restricted: i32,
    in_enabled: bool,
    vnetwork: u32,
    vnetmask: u32,
    vhost: u32,
    in6_enabled: bool,
    vprefix_addr6: [u8; 16],
    vprefix_len: u8,
    vhost6: [u8; 16],
    vhostname: *const i8,
    tftp_server_name: *const i8,
    tftp_path: *const i8,
    bootfile: *const i8,
    vdhcp_start: u32,
    vnameserver: u32,
    vnameserver6: [u8; 16],
    vdnssearch: *const *const i8,
    vdomainname: *const i8,
    if_mtu: usize,
    if_mru: usize,
    disable_host_loopback: bool,
    enable_emu: bool,
    outbound_addr: *const c_void,
    outbound_addr6: *const c_void,
    disable_dns: bool,
    disable_dhcp: bool,
    mfr_id: u32,
    oob_eth_addr: [u8; 6],
}

type TimerCb = extern "C" fn(*mut c_void);

#[repr(C)]
struct SlirpCb {
    send_packet: extern "C" fn(*const c_void, usize, *mut c_void) -> isize,
    guest_error: extern "C" fn(*const i8, *mut c_void),
    clock_get_ns: extern "C" fn(*mut c_void) -> i64,
    timer_new: extern "C" fn(TimerCb, *mut c_void, *mut c_void) -> *mut c_void,
    timer_free: extern "C" fn(*mut c_void, *mut c_void),
    timer_mod: extern "C" fn(*mut c_void, i64, *mut c_void),
    register_poll_fd: Option<extern "C" fn(i32, *mut c_void)>,
    unregister_poll_fd: Option<extern "C" fn(i32, *mut c_void)>,
    notify: extern "C" fn(*mut c_void),
    init_completed: Option<extern "C" fn(*mut c_void, *mut c_void)>,
    timer_new_opaque: Option<extern "C" fn(u32, *mut c_void, *mut c_void) -> *mut c_void>,
    register_poll_socket: Option<extern "C" fn(i32, *mut c_void)>,
    unregister_poll_socket: Option<extern "C" fn(i32, *mut c_void)>,
}

const SLIRP_POLL_IN: i32 = 1 << 0;
const SLIRP_POLL_OUT: i32 = 1 << 1;
const SLIRP_POLL_PRI: i32 = 1 << 2;
const SLIRP_POLL_ERR: i32 = 1 << 3;
const SLIRP_POLL_HUP: i32 = 1 << 4;

#[link(name = "slirp")]
extern "C" {
    fn slirp_new(cfg: *const SlirpConfig, cb: *const SlirpCb, opaque: *mut c_void) -> *mut c_void;
    fn slirp_input(slirp: *mut c_void, pkt: *const u8, len: i32);
    fn slirp_pollfds_fill_socket(
        slirp: *mut c_void,
        timeout: *mut u32,
        add: extern "C" fn(i32, i32, *mut c_void) -> i32,
        opaque: *mut c_void,
    );
    fn slirp_pollfds_poll(
        slirp: *mut c_void,
        select_error: i32,
        get_revents: extern "C" fn(i32, *mut c_void) -> i32,
        opaque: *mut c_void,
    );
    fn slirp_add_hostfwd(slirp: *mut c_void, is_udp: i32, host: u32, hport: i32, guest: u32, gport: i32) -> i32;
    fn slirp_remove_hostfwd(slirp: *mut c_void, is_udp: i32, host: u32, hport: i32) -> i32;
}

/// Messages to the network thread.
pub enum NetMsg {
    /// Guest → network Ethernet frame.
    Frame(Vec<u8>),
    /// Start/stop forwarding the Mac's 127.0.0.1:port to the same guest port.
    Forward { port: u16, add: bool },
}

/// A host → guest port forward.
#[derive(Clone, Copy, Debug)]
pub struct Forward {
    pub udp: bool,
    pub host_port: u16,
    pub guest_port: u16,
}

pub const GUEST_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 15);

fn netdebug() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("ORBIT_NETDEBUG").is_some())
}

/// ORBIT_NETDEBUG: log TCP handshake frames with timestamps (ms since start).
fn tcp_trace(dir: &str, frame: &[u8], start: Instant) {
    if !netdebug() {
        return;
    }
    // Ethernet + IPv4 + TCP
    if frame.len() < 54 || frame[12..14] != [0x08, 0x00] || frame[23] != 6 {
        return;
    }
    let ihl = ((frame[14] & 0x0f) as usize) * 4;
    let tcp = 14 + ihl;
    if frame.len() < tcp + 14 {
        return;
    }
    let flags = frame[tcp + 13];
    if flags & 0x02 == 0 && flags & 0x04 == 0 {
        return; // only SYN / SYN-ACK / RST
    }
    let src = Ipv4Addr::new(frame[26], frame[27], frame[28], frame[29]);
    let dst = Ipv4Addr::new(frame[30], frame[31], frame[32], frame[33]);
    let sp = u16::from_be_bytes([frame[tcp], frame[tcp + 1]]);
    let dp = u16::from_be_bytes([frame[tcp + 2], frame[tcp + 3]]);
    eprintln!("netdbg {:>8} {dir} {src}:{sp} -> {dst}:{dp} flags=0x{flags:02x}", start.elapsed().as_millis());
}

fn be(ip: Ipv4Addr) -> u32 {
    u32::from_ne_bytes(ip.octets())
}

struct Timer {
    cb: TimerCb,
    cb_opaque: *mut c_void,
    expire_ms: Option<i64>,
}

/// State reachable from libslirp callbacks (all on the network thread).
type Deliver = Box<dyn FnMut(&[u8]) + Send>;

struct Ctx {
    deliver: Deliver,
    start: Instant,
    #[allow(clippy::vec_box)] // libslirp holds raw pointers to the timers: they must not move
    timers: Vec<Box<Timer>>,
    pollfds: Vec<libc::pollfd>,
}

extern "C" fn cb_send_packet(buf: *const c_void, len: usize, opaque: *mut c_void) -> isize {
    let ctx = unsafe { &mut *(opaque as *mut Ctx) };
    tcp_trace("->guest", unsafe { std::slice::from_raw_parts(buf as *const u8, len) }, ctx.start);
    (ctx.deliver)(unsafe { std::slice::from_raw_parts(buf as *const u8, len) });
    len as isize
}

extern "C" fn cb_guest_error(msg: *const i8, _: *mut c_void) {
    let msg = unsafe { std::ffi::CStr::from_ptr(msg) };
    eprintln!("orbit-vmm: slirp: guest error: {}", msg.to_string_lossy());
}

extern "C" fn cb_clock_get_ns(opaque: *mut c_void) -> i64 {
    let ctx = unsafe { &*(opaque as *const Ctx) };
    ctx.start.elapsed().as_nanos() as i64
}

extern "C" fn cb_timer_new(cb: TimerCb, cb_opaque: *mut c_void, opaque: *mut c_void) -> *mut c_void {
    let ctx = unsafe { &mut *(opaque as *mut Ctx) };
    let mut t = Box::new(Timer { cb, cb_opaque, expire_ms: None });
    let p = &mut *t as *mut Timer as *mut c_void;
    ctx.timers.push(t);
    p
}

extern "C" fn cb_timer_free(timer: *mut c_void, opaque: *mut c_void) {
    let ctx = unsafe { &mut *(opaque as *mut Ctx) };
    ctx.timers.retain(|t| &**t as *const Timer as *mut c_void != timer);
}

extern "C" fn cb_timer_mod(timer: *mut c_void, expire_ms: i64, _: *mut c_void) {
    unsafe { (*(timer as *mut Timer)).expire_ms = Some(expire_ms) };
}

extern "C" fn cb_notify(_: *mut c_void) {}

/// We rebuild the poll set every iteration, so (un)registration is a no-op;
/// libslirp calls these unconditionally when it opens sockets.
extern "C" fn cb_poll_registration(_: i32, _: *mut c_void) {}

extern "C" fn cb_add_poll(fd: i32, events: i32, opaque: *mut c_void) -> i32 {
    let ctx = unsafe { &mut *(opaque as *mut Ctx) };
    let mut ev = 0;
    if events & SLIRP_POLL_IN != 0 {
        ev |= libc::POLLIN;
    }
    if events & SLIRP_POLL_OUT != 0 {
        ev |= libc::POLLOUT;
    }
    if events & SLIRP_POLL_PRI != 0 {
        ev |= libc::POLLPRI;
    }
    ctx.pollfds.push(libc::pollfd { fd, events: ev, revents: 0 });
    (ctx.pollfds.len() - 1) as i32
}

extern "C" fn cb_get_revents(idx: i32, opaque: *mut c_void) -> i32 {
    let ctx = unsafe { &*(opaque as *const Ctx) };
    let Some(p) = ctx.pollfds.get(idx as usize) else {
        return 0;
    };
    let mut r = 0;
    if p.revents & libc::POLLIN != 0 {
        r |= SLIRP_POLL_IN;
    }
    if p.revents & libc::POLLOUT != 0 {
        r |= SLIRP_POLL_OUT;
    }
    if p.revents & libc::POLLPRI != 0 {
        r |= SLIRP_POLL_PRI;
    }
    if p.revents & libc::POLLERR != 0 {
        r |= SLIRP_POLL_ERR;
    }
    if p.revents & libc::POLLHUP != 0 {
        r |= SLIRP_POLL_HUP;
    }
    r
}

/// Runs libslirp on the current thread forever. `tx` carries guest → network
/// frames (wake the loop by writing a byte to `wake_write`); `deliver` is
/// called with network → guest frames.
pub fn run(tx: Receiver<NetMsg>, wake_read: RawFd, forwards: Vec<Forward>, deliver: Deliver) {
    let mut ctx = Box::new(Ctx { deliver, start: Instant::now(), timers: Vec::new(), pollfds: Vec::new() });
    let cfg = SlirpConfig {
        version: 4,
        restricted: 0,
        in_enabled: true,
        vnetwork: be(Ipv4Addr::new(10, 0, 2, 0)),
        vnetmask: be(Ipv4Addr::new(255, 255, 255, 0)),
        vhost: be(Ipv4Addr::new(10, 0, 2, 2)),
        in6_enabled: false,
        vprefix_addr6: [0; 16],
        vprefix_len: 0,
        vhost6: [0; 16],
        vhostname: c"orbit".as_ptr(),
        tftp_server_name: std::ptr::null(),
        tftp_path: std::ptr::null(),
        bootfile: std::ptr::null(),
        vdhcp_start: be(GUEST_IP),
        vnameserver: be(Ipv4Addr::new(10, 0, 2, 3)),
        vnameserver6: [0; 16],
        vdnssearch: std::ptr::null(),
        vdomainname: std::ptr::null(),
        if_mtu: 1500,
        if_mru: 1500,
        disable_host_loopback: false,
        enable_emu: false,
        outbound_addr: std::ptr::null(),
        outbound_addr6: std::ptr::null(),
        disable_dns: false,
        disable_dhcp: false,
        mfr_id: 0,
        oob_eth_addr: [0; 6],
    };
    let cb = SlirpCb {
        send_packet: cb_send_packet,
        guest_error: cb_guest_error,
        clock_get_ns: cb_clock_get_ns,
        timer_new: cb_timer_new,
        timer_free: cb_timer_free,
        timer_mod: cb_timer_mod,
        register_poll_fd: Some(cb_poll_registration),
        unregister_poll_fd: Some(cb_poll_registration),
        notify: cb_notify,
        init_completed: None,
        timer_new_opaque: None,
        register_poll_socket: Some(cb_poll_registration),
        unregister_poll_socket: Some(cb_poll_registration),
    };
    let opaque = &mut *ctx as *mut Ctx as *mut c_void;
    let slirp = unsafe { slirp_new(&cfg, &cb, opaque) };
    if slirp.is_null() {
        eprintln!("orbit-vmm: slirp_new failed; networking disabled");
        return;
    }
    for f in &forwards {
        let rc = unsafe {
            slirp_add_hostfwd(
                slirp,
                f.udp as i32,
                be(Ipv4Addr::LOCALHOST),
                f.host_port as i32,
                be(GUEST_IP),
                f.guest_port as i32,
            )
        };
        if rc != 0 {
            eprintln!("orbit-vmm: cannot forward host port {} (in use?)", f.host_port);
        }
    }

    let mut wake_buf = [0u8; 64];
    loop {
        loop {
            match tx.try_recv() {
                Ok(NetMsg::Frame(frame)) => {
                    tcp_trace("guest->", &frame, unsafe { &*(opaque as *const Ctx) }.start);
                    let t = Instant::now();
                    unsafe { slirp_input(slirp, frame.as_ptr(), frame.len() as i32) };
                    // Everything networking runs on this thread: a stall here stalls
                    // every guest connection (see csrc/asyncclose.c for the one we hit).
                    if t.elapsed().as_millis() > 500 {
                        eprintln!("orbit-vmm: network thread stalled {}ms in slirp_input", t.elapsed().as_millis());
                    }
                }
                Ok(NetMsg::Forward { port, add }) => {
                    let (lo, guest, p) = (be(Ipv4Addr::LOCALHOST), be(GUEST_IP), port as i32);
                    let rc = unsafe {
                        if add {
                            slirp_add_hostfwd(slirp, 0, lo, p, guest, p)
                        } else {
                            slirp_remove_hostfwd(slirp, 0, lo, p)
                        }
                    };
                    if add && rc != 0 {
                        eprintln!("orbit-vmm: cannot forward localhost:{port} (port in use on the Mac?)");
                    } else if std::env::var_os("ORBIT_DEBUG").is_some() {
                        eprintln!(
                            "orbit-vmm: {} localhost:{port}",
                            if add { "forwarding" } else { "stopped forwarding" }
                        );
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        let ctx = unsafe { &mut *(opaque as *mut Ctx) };
        ctx.pollfds.clear();
        ctx.pollfds.push(libc::pollfd { fd: wake_read, events: libc::POLLIN, revents: 0 });
        let mut timeout: u32 = 1000;
        unsafe { slirp_pollfds_fill_socket(slirp, &mut timeout, cb_add_poll, opaque) };
        let now_ms = ctx.start.elapsed().as_millis() as i64;
        for t in &ctx.timers {
            if let Some(e) = t.expire_ms {
                timeout = timeout.min((e - now_ms).max(0) as u32);
            }
        }
        let want_out = ctx.pollfds.iter().filter(|p| p.events & libc::POLLOUT != 0).count();
        let t_poll = Instant::now();
        let n = unsafe { libc::poll(ctx.pollfds.as_mut_ptr(), ctx.pollfds.len() as u32, timeout as i32) };
        if netdebug() && (want_out > 0 || t_poll.elapsed().as_millis() > 400) {
            let outs: Vec<String> = ctx
                .pollfds
                .iter()
                .filter(|p| p.revents != 0)
                .map(|p| {
                    let mut err: libc::c_int = 0;
                    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
                    if p.revents & libc::POLLHUP != 0 {
                        unsafe {
                            libc::getsockopt(
                                p.fd,
                                libc::SOL_SOCKET,
                                libc::SO_ERROR,
                                (&mut err as *mut libc::c_int).cast(),
                                &mut len,
                            )
                        };
                    }
                    format!("fd{}:{:#x} ev={:#x} so_error={}", p.fd, p.revents, p.events, err)
                })
                .collect();
            eprintln!(
                "netdbg {:>8} poll fds={} want_out={want_out} timeout={timeout} took={}ms n={n} revents={outs:?}",
                ctx.start.elapsed().as_millis(),
                ctx.pollfds.len(),
                t_poll.elapsed().as_millis()
            );
        }
        if ctx.pollfds[0].revents & libc::POLLIN != 0 {
            unsafe { libc::read(wake_read, wake_buf.as_mut_ptr().cast(), wake_buf.len()) };
        }
        let t = Instant::now();
        unsafe { slirp_pollfds_poll(slirp, (n < 0) as i32, cb_get_revents, opaque) };
        if t.elapsed().as_millis() > 500 {
            eprintln!("orbit-vmm: network thread stalled {}ms in slirp_pollfds_poll", t.elapsed().as_millis());
        }

        let now_ms = ctx.start.elapsed().as_millis() as i64;
        let due: Vec<(TimerCb, *mut c_void)> = ctx
            .timers
            .iter_mut()
            .filter(|t| t.expire_ms.is_some_and(|e| e <= now_ms))
            .map(|t| {
                t.expire_ms = None;
                (t.cb, t.cb_opaque)
            })
            .collect();
        for (cb, arg) in due {
            cb(arg);
        }
    }
}
