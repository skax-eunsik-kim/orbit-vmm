//! virtio-net (virtio 1.x section 5.1) backed by the libslirp network thread.
//! Queues: rx = 0, tx = 1. Every buffer starts with a 12-byte virtio_net_hdr_v1.

use std::collections::VecDeque;
use std::os::fd::RawFd;
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex};

use super::{Queue, VirtioDevice, VirtioInterrupt, VIRTIO_F_VERSION_1};
use crate::memory::GuestMemory;
use crate::slirp::{self, Forward, NetMsg};

pub const DEVICE_ID_NET: u32 = 1;
const VIRTIO_NET_F_MAC: u64 = 1 << 5;
const HDR_LEN: usize = 12;
const BACKLOG: usize = 512;

#[derive(Default)]
struct Rx {
    mem: Option<Arc<GuestMemory>>,
    queue: Option<Queue>,
    interrupt: Option<Arc<dyn VirtioInterrupt>>,
    backlog: VecDeque<Vec<u8>>,
}

impl Rx {
    /// Copies queued frames into guest RX buffers while there are any.
    fn flush(&mut self) {
        let (Some(mem), Some(interrupt)) = (self.mem.clone(), self.interrupt.clone()) else {
            return;
        };
        let Some(queue) = self.queue.as_mut() else {
            return;
        };
        let mut used = false;
        while let Some(frame) = self.backlog.front() {
            let Some(chain) = queue.pop(&mem) else { break };
            let mut hdr = [0u8; HDR_LEN];
            hdr[10] = 1; // num_buffers
                         // Header then frame, spread over the chain's buffers (slice copies).
            let mut parts = [&hdr[..], &frame[..]];
            let mut written = 0u32;
            for d in chain.descs.iter().filter(|d| d.write_only) {
                let mut room = d.len as usize;
                let mut at = d.addr;
                while room > 0 {
                    let Some(part) = parts.iter_mut().find(|p| !p.is_empty()) else { break };
                    let n = part.len().min(room);
                    mem.write(at, &part[..n]);
                    *part = &part[n..];
                    at += n as u64;
                    room -= n;
                    written += n as u32;
                }
                if parts.iter().all(|p| p.is_empty()) {
                    break;
                }
            }
            queue.add_used(&mem, chain.head, written);
            self.backlog.pop_front();
            used = true;
        }
        if used {
            interrupt.signal_used_queue();
        }
    }
}

#[derive(Clone)]
pub struct PortControl {
    tx: SyncSender<NetMsg>,
    wake: RawFd,
}

impl PortControl {
    pub fn set(&self, port: u16, add: bool) {
        let _ = self.tx.send(NetMsg::Forward { port, add });
        unsafe { libc::write(self.wake, [1u8].as_ptr().cast(), 1) };
    }
}

pub struct Net {
    mac: [u8; 6],
    rx: Arc<Mutex<Rx>>,
    tx_queue: Option<Queue>,
    tx_mem: Option<Arc<GuestMemory>>,
    tx_interrupt: Option<Arc<dyn VirtioInterrupt>>,
    to_slirp: SyncSender<NetMsg>,
    wake_write: RawFd,
}

impl Net {
    /// Starts the slirp network thread and returns the device.
    pub fn new(forwards: Vec<Forward>, host_loopback: bool) -> Self {
        let rx = Arc::new(Mutex::new(Rx::default()));
        // Bounded: if the network thread falls behind, guest frames are dropped
        // (like a full NIC queue) instead of piling up in memory.
        let (to_slirp, from_guest) = sync_channel::<NetMsg>(4096);
        let mut fds = [0; 2];
        unsafe { libc::pipe(fds.as_mut_ptr()) };
        for fd in fds {
            unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) };
        }
        let rx_for_thread = rx.clone();
        std::thread::Builder::new()
            .name("net".into())
            .spawn(move || {
                let deliver = Box::new(move |frame: &[u8]| {
                    let mut rx = rx_for_thread.lock().unwrap();
                    if rx.backlog.len() < BACKLOG {
                        rx.backlog.push_back(frame.to_vec());
                    }
                    rx.flush();
                });
                slirp::run(from_guest, fds[0], forwards, host_loopback, deliver);
            })
            .expect("spawn net thread");
        Self {
            mac: [0x52, 0x54, 0x00, 0x12, 0x34, 0x56],
            rx,
            tx_queue: None,
            tx_mem: None,
            tx_interrupt: None,
            to_slirp,
            wake_write: fds[1],
        }
    }

    /// Handle for adding/removing localhost port forwards at runtime.
    pub fn port_control(&self) -> PortControl {
        PortControl { tx: self.to_slirp.clone(), wake: self.wake_write }
    }

    fn transmit(&mut self) {
        let (Some(mem), Some(interrupt)) = (self.tx_mem.clone(), self.tx_interrupt.clone()) else {
            return;
        };
        let Some(queue) = self.tx_queue.as_mut() else {
            return;
        };
        let mut used = false;
        while let Some(chain) = queue.pop(&mem) {
            // No offloads are negotiated, so frames are at most MTU-sized; allow GSO-sized slack.
            let mut frame = super::queue::read_readable(&mem, &chain.descs, HDR_LEN + 65_550).unwrap_or_default();
            if frame.len() > HDR_LEN {
                let _ = self.to_slirp.try_send(NetMsg::Frame(frame.split_off(HDR_LEN)));
            }
            queue.add_used(&mem, chain.head, 0);
            used = true;
        }
        if used {
            unsafe { libc::write(self.wake_write, [1u8].as_ptr().cast(), 1) };
            interrupt.signal_used_queue();
        }
    }
}

impl VirtioDevice for Net {
    fn device_id(&self) -> u32 {
        DEVICE_ID_NET
    }

    fn features(&self) -> u64 {
        VIRTIO_F_VERSION_1 | VIRTIO_NET_F_MAC
    }

    fn num_queues(&self) -> usize {
        2
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        let mut cfg = [0u8; 8];
        cfg[..6].copy_from_slice(&self.mac);
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
        let tx = queues.pop();
        let rxq = queues.pop();
        {
            let mut rx = self.rx.lock().unwrap();
            rx.mem = Some(mem.clone());
            rx.queue = rxq;
            rx.interrupt = Some(interrupt.clone());
            rx.flush();
        }
        self.tx_queue = tx;
        self.tx_mem = Some(mem);
        self.tx_interrupt = Some(interrupt);
        self.transmit();
    }

    fn queue_notify(&mut self, index: u16) {
        match index {
            0 => self.rx.lock().unwrap().flush(),
            1 => self.transmit(),
            _ => {}
        }
    }

    fn reset(&mut self) {
        let mut rx = self.rx.lock().unwrap();
        *rx = Rx::default();
        self.tx_queue = None;
        self.tx_mem = None;
        self.tx_interrupt = None;
    }
}
