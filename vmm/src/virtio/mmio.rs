//! virtio-mmio transport, version 2 (virtio 1.x section 4.2.2).

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use super::{Queue, VirtioDevice, VirtioInterrupt};
use crate::devices::{IrqLine, MmioDevice};
use crate::memory::GuestMemory;

const MAGIC: u64 = 0x000;
const VERSION: u64 = 0x004;
const DEVICE_ID: u64 = 0x008;
const VENDOR_ID: u64 = 0x00c;
const DEVICE_FEATURES: u64 = 0x010;
const DEVICE_FEATURES_SEL: u64 = 0x014;
const DRIVER_FEATURES: u64 = 0x020;
const DRIVER_FEATURES_SEL: u64 = 0x024;
const QUEUE_SEL: u64 = 0x030;
const QUEUE_NUM_MAX: u64 = 0x034;
const QUEUE_NUM: u64 = 0x038;
const QUEUE_READY: u64 = 0x044;
const QUEUE_NOTIFY: u64 = 0x050;
const INTERRUPT_STATUS: u64 = 0x060;
const INTERRUPT_ACK: u64 = 0x064;
const STATUS: u64 = 0x070;
const QUEUE_DESC_LOW: u64 = 0x080;
const QUEUE_DESC_HIGH: u64 = 0x084;
const QUEUE_DRIVER_LOW: u64 = 0x090;
const QUEUE_DRIVER_HIGH: u64 = 0x094;
const QUEUE_DEVICE_LOW: u64 = 0x0a0;
const QUEUE_DEVICE_HIGH: u64 = 0x0a4;
const CONFIG_GENERATION: u64 = 0x0fc;
const CONFIG: u64 = 0x100;

const STATUS_DRIVER_OK: u32 = 4;
const INT_USED_RING: u32 = 1;
const INT_CONFIG: u32 = 2;

/// InterruptStatus shared between the transport and the device; drives a
/// level-triggered SPI that stays high until the driver ACKs every bit.
/// Devices raise from their own threads (e.g. network RX) while the driver ACKs
/// from a vCPU, so the status update and the line level change happen under
/// one lock: otherwise an ACK can lower the line right after a raise and the
/// interrupt is lost for good.
struct Interrupt {
    status: AtomicU32,
    level: Mutex<()>,
    irq: Arc<dyn IrqLine>,
}

impl Interrupt {
    fn raise(&self, bit: u32) {
        let _g = self.level.lock().unwrap();
        // The line is high exactly while status != 0: only the first bit raises it.
        if self.status.fetch_or(bit, Ordering::SeqCst) == 0 {
            self.irq.set_level(true);
        }
    }

    fn ack(&self, bits: u32) {
        let _g = self.level.lock().unwrap();
        let left = self.status.fetch_and(!bits, Ordering::SeqCst) & !bits;
        if left == 0 {
            self.irq.set_level(false);
        }
    }
}

impl VirtioInterrupt for Interrupt {
    fn signal_used_queue(&self) {
        self.raise(INT_USED_RING);
    }
    fn signal_config_change(&self) {
        self.raise(INT_CONFIG);
    }
}

struct State {
    device: Box<dyn VirtioDevice>,
    device_features_sel: u32,
    driver_features_sel: u32,
    driver_features: u64,
    queue_sel: u32,
    queues: Vec<Queue>,
    status: u32,
    activated: bool,
    config_generation: u32,
}

pub struct VirtioMmio {
    state: Mutex<State>,
    mem: Arc<GuestMemory>,
    interrupt: Arc<Interrupt>,
}

impl VirtioMmio {
    pub fn new(device: Box<dyn VirtioDevice>, mem: Arc<GuestMemory>, irq: Arc<dyn IrqLine>) -> Self {
        let queues = (0..device.num_queues()).map(|_| Queue::new(device.queue_max_size())).collect();
        Self {
            state: Mutex::new(State {
                device,
                device_features_sel: 0,
                driver_features_sel: 0,
                driver_features: 0,
                queue_sel: 0,
                queues,
                status: 0,
                activated: false,
                config_generation: 0,
            }),
            mem,
            interrupt: Arc::new(Interrupt { status: AtomicU32::new(0), level: Mutex::new(()), irq }),
        }
    }
}

fn set_low(v: &mut u64, low: u32) {
    *v = (*v & !0xffff_ffff) | low as u64;
}

fn set_high(v: &mut u64, high: u32) {
    *v = (*v & 0xffff_ffff) | ((high as u64) << 32);
}

impl State {
    fn queue(&mut self) -> Option<&mut Queue> {
        self.queues.get_mut(self.queue_sel as usize)
    }

    fn reset(&mut self) {
        self.device.reset();
        let max = self.device.queue_max_size();
        for q in &mut self.queues {
            *q = Queue::new(max);
        }
        self.driver_features = 0;
        self.status = 0;
        self.activated = false;
    }
}

impl MmioDevice for VirtioMmio {
    fn read(&self, offset: u64, data: &mut [u8]) {
        let st = self.state.lock().unwrap();
        if offset >= CONFIG {
            st.device.read_config(offset - CONFIG, data);
            return;
        }
        let value: u32 = match offset {
            MAGIC => 0x7472_6976, // "virt"
            VERSION => 2,
            DEVICE_ID => st.device.device_id(),
            VENDOR_ID => 0x4f52_4254, // "ORBT"
            DEVICE_FEATURES => {
                let f = st.device.features();
                match st.device_features_sel {
                    0 => f as u32,
                    1 => (f >> 32) as u32,
                    _ => 0,
                }
            }
            QUEUE_NUM_MAX => st.queues.get(st.queue_sel as usize).map_or(0, |q| q.max_size as u32),
            QUEUE_READY => st.queues.get(st.queue_sel as usize).map_or(0, |q| q.ready as u32),
            INTERRUPT_STATUS => self.interrupt.status.load(Ordering::SeqCst),
            STATUS => st.status,
            CONFIG_GENERATION => st.config_generation,
            _ => 0,
        };
        let bytes = value.to_le_bytes();
        let n = data.len().min(4);
        data[..n].copy_from_slice(&bytes[..n]);
    }

    fn write(&self, offset: u64, data: &[u8]) {
        let mut st = self.state.lock().unwrap();
        if offset >= CONFIG {
            st.device.write_config(offset - CONFIG, data);
            return;
        }
        let mut buf = [0u8; 4];
        let n = data.len().min(4);
        buf[..n].copy_from_slice(&data[..n]);
        let v = u32::from_le_bytes(buf);
        match offset {
            DEVICE_FEATURES_SEL => st.device_features_sel = v,
            DRIVER_FEATURES_SEL => st.driver_features_sel = v,
            DRIVER_FEATURES => {
                let sel = st.driver_features_sel;
                match sel {
                    0 => set_low(&mut st.driver_features, v),
                    1 => set_high(&mut st.driver_features, v),
                    _ => {}
                }
            }
            QUEUE_SEL => st.queue_sel = v,
            QUEUE_NUM => {
                if let Some(q) = st.queue() {
                    q.size = (v as u16).min(q.max_size);
                }
            }
            QUEUE_READY => {
                if let Some(q) = st.queue() {
                    q.ready = v == 1;
                }
            }
            QUEUE_DESC_LOW => st.queue().map_or((), |q| set_low(&mut q.desc_table, v)),
            QUEUE_DESC_HIGH => st.queue().map_or((), |q| set_high(&mut q.desc_table, v)),
            QUEUE_DRIVER_LOW => st.queue().map_or((), |q| set_low(&mut q.avail_ring, v)),
            QUEUE_DRIVER_HIGH => st.queue().map_or((), |q| set_high(&mut q.avail_ring, v)),
            QUEUE_DEVICE_LOW => st.queue().map_or((), |q| set_low(&mut q.used_ring, v)),
            QUEUE_DEVICE_HIGH => st.queue().map_or((), |q| set_high(&mut q.used_ring, v)),
            QUEUE_NOTIFY => {
                if st.activated {
                    st.device.queue_notify(v as u16);
                }
            }
            INTERRUPT_ACK => self.interrupt.ack(v),
            STATUS => {
                if v == 0 {
                    st.reset();
                    self.interrupt.ack(!0);
                    return;
                }
                st.status = v;
                if v & STATUS_DRIVER_OK != 0 && !st.activated {
                    st.activated = true;
                    let queues = st.queues.clone();
                    let features = st.driver_features;
                    let interrupt: Arc<dyn VirtioInterrupt> = self.interrupt.clone();
                    st.device.activate(self.mem.clone(), queues, interrupt, features);
                }
            }
            _ => {}
        }
    }
}
