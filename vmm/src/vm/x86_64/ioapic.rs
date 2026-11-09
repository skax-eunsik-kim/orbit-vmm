//! I/O APIC, emulated here rather than in the kernel.
//!
//! Hypervisor.framework has an in-kernel IOAPIC (HV_VM_ACCEL_APIC), but its
//! level-triggered lines are reported to lose interrupts: an interrupt
//! asserted while masked is not delivered on unmask, and one still asserted
//! after EOI is not redelivered (Apple developer forums thread 760565, filed
//! as FB14425412). So this IOAPIC treats every line as edge-triggered: each
//! `set_level(true)` is an edge and is sent to the in-kernel local APICs with
//! `hv_vm_send_ioapic_intr` (which routes by the redirection entry's
//! destination), and nothing waits for an EOI. That suits our devices: the
//! virtio-mmio driver reads and acks InterruptStatus and then scans every
//! queue, and the transport re-raises when status bits are left after an ack
//! (virtio/mmio.rs). An edge on a masked line is kept and sent on unmask.

use std::sync::{Arc, Mutex};

use crate::devices::{IrqLine, MmioDevice};

pub const PINS: usize = 24;
const IOREGSEL: u64 = 0x00;
const IOWIN: u64 = 0x10;
const MASKED: u64 = 1 << 16;
/// Redirection entry bits that only describe level-triggered state.
const LEVEL_BITS: u64 = (1 << 15) | (1 << 14);

/// Where the IOAPIC sends interrupts: the in-kernel local APICs.
pub trait Deliver: Send + Sync {
    fn deliver(&self, entry: u64);
}

pub struct HvDeliver;

impl Deliver for HvDeliver {
    fn deliver(&self, entry: u64) {
        if let Err(e) = super::hvf::hv!(hv_vm_send_ioapic_intr(entry)) {
            eprintln!("orbit-vmm: {e} (entry 0x{entry:x})");
        }
    }
}

struct State {
    select: u32,
    id: u32,
    redir: [u64; PINS],
    /// An edge arrived while the pin was masked.
    pending: [bool; PINS],
}

pub struct IoApic {
    state: Mutex<State>,
    out: Box<dyn Deliver>,
}

impl IoApic {
    pub fn new(id: u32, out: Box<dyn Deliver>) -> Self {
        Self { state: Mutex::new(State { select: 0, id, redir: [MASKED; PINS], pending: [false; PINS] }), out }
    }

    fn send(&self, entry: u64) {
        // Bits 0-7 vector, 8-10 delivery mode, 11 destination mode, 56-63 destination.
        self.out.deliver(entry & !(MASKED | LEVEL_BITS));
    }

    /// A rising edge on `pin`.
    pub fn edge(&self, pin: usize) {
        let mut s = self.state.lock().unwrap();
        let Some(&entry) = s.redir.get(pin) else { return };
        if entry & MASKED != 0 {
            s.pending[pin] = true;
            return;
        }
        drop(s);
        self.send(entry);
    }

    fn read_reg(&self, s: &State) -> u32 {
        match s.select {
            0 => s.id << 24,
            1 => 0x0011 | ((PINS as u32 - 1) << 16), // version 0x11, max redirection entry
            2 => s.id << 24,
            r @ 0x10..=0x3f => {
                let entry = s.redir[((r - 0x10) / 2) as usize];
                if r % 2 == 0 {
                    entry as u32
                } else {
                    (entry >> 32) as u32
                }
            }
            _ => 0,
        }
    }

    fn write_reg(&self, s: &mut State, value: u32) -> Option<u64> {
        match s.select {
            0 => s.id = (value >> 24) & 0xf,
            r @ 0x10..=0x3f => {
                let pin = ((r - 0x10) / 2) as usize;
                let old = s.redir[pin];
                s.redir[pin] = if r % 2 == 0 {
                    // Remote IRR and delivery status are read-only.
                    (old & 0xffff_ffff_0000_0000) | (value as u64 & !((1 << 14) | (1 << 12)))
                } else {
                    (old & 0xffff_ffff) | ((value as u64) << 32)
                };
                if s.redir[pin] & MASKED == 0 && std::mem::take(&mut s.pending[pin]) {
                    return Some(s.redir[pin]);
                }
            }
            _ => {}
        }
        None
    }
}

impl MmioDevice for IoApic {
    fn read(&self, offset: u64, data: &mut [u8]) {
        let s = self.state.lock().unwrap();
        let value = match offset {
            IOREGSEL => s.select,
            IOWIN => self.read_reg(&s),
            _ => 0,
        };
        let bytes = value.to_le_bytes();
        let n = data.len().min(4);
        data[..n].copy_from_slice(&bytes[..n]);
    }

    fn write(&self, offset: u64, data: &[u8]) {
        let mut buf = [0u8; 4];
        let n = data.len().min(4);
        buf[..n].copy_from_slice(&data[..n]);
        let value = u32::from_le_bytes(buf);
        let mut s = self.state.lock().unwrap();
        let send = match offset {
            IOREGSEL => {
                s.select = value & 0xff;
                None
            }
            IOWIN => self.write_reg(&mut s, value),
            _ => None,
        };
        drop(s);
        if let Some(entry) = send {
            self.send(entry);
        }
    }
}

/// One IOAPIC input as seen by a device.
pub struct Line {
    ioapic: Arc<IoApic>,
    pin: usize,
}

impl Line {
    pub fn new(ioapic: Arc<IoApic>, pin: u32) -> Self {
        Self { ioapic, pin: pin as usize }
    }
}

impl IrqLine for Line {
    fn set_level(&self, high: bool) {
        if high {
            self.ioapic.edge(self.pin);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default, Clone)]
    struct Sent(Arc<Mutex<Vec<u64>>>);
    impl Deliver for Sent {
        fn deliver(&self, entry: u64) {
            self.0.lock().unwrap().push(entry);
        }
    }

    /// High word first, then low (which may unmask), as Linux's __ioapic_write_entry does.
    fn program(io: &IoApic, pin: u32, low: u32, high: u32) {
        io.write(IOREGSEL, &(0x11 + 2 * pin).to_le_bytes());
        io.write(IOWIN, &high.to_le_bytes());
        io.write(IOREGSEL, &(0x10 + 2 * pin).to_le_bytes());
        io.write(IOWIN, &low.to_le_bytes());
    }

    #[test]
    fn edges_are_routed_and_kept_while_masked() {
        let sent = Sent::default();
        let io = Arc::new(IoApic::new(4, Box::new(sent.clone())));
        let line = Line::new(io.clone(), 5);

        line.set_level(true); // masked out of reset: kept, not sent
        assert!(sent.0.lock().unwrap().is_empty());
        // vector 0x25, level-triggered (bit 15) as an MP-table guest might set, CPU 1
        program(&io, 5, 0x0000_8025, 0x0100_0000);
        assert_eq!(*sent.0.lock().unwrap(), vec![0x0100_0000_0000_0025], "pending edge sent on unmask, as edge");

        line.set_level(false);
        line.set_level(true);
        line.set_level(true); // a re-raise is another edge
        assert_eq!(sent.0.lock().unwrap().len(), 3);

        io.write(IOREGSEL, &0x1au32.to_le_bytes());
        let mut word = [0u8; 4];
        io.read(IOWIN, &mut word);
        assert_eq!(u32::from_le_bytes(word), 0x0000_8025, "entry reads back as written");
        io.write(IOREGSEL, &1u32.to_le_bytes());
        io.read(IOWIN, &mut word);
        assert_eq!(u32::from_le_bytes(word), 0x0017_0011, "24 pins");
    }
}
