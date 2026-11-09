//! 16550A UART at I/O port 0x3f8 (ttyS0, IRQ 4): enough for earlycon and the
//! Linux 8250 driver, including its probe (scratch register, FIFO and
//! loopback tests) and the transmit-empty interrupt the tty layer waits for.
//! Output goes to the console log; there is no input.

use std::sync::{Arc, Mutex};

use crate::devices::IrqLine;

pub const BASE: u16 = 0x3f8;
pub const IRQ: u32 = 4;

const IER_THRI: u8 = 0x02;
const IIR_NO_INT: u8 = 0x01;
const IIR_THRI: u8 = 0x02;
const IIR_FIFO: u8 = 0xc0;
const LCR_DLAB: u8 = 0x80;
const MCR_LOOP: u8 = 0x10;
const LSR_IDLE: u8 = 0x60; // THR empty, transmitter empty

#[derive(Default)]
struct Regs {
    ier: u8,
    lcr: u8,
    mcr: u8,
    scr: u8,
    dll: u8,
    dlm: u8,
    fifo: bool,
    /// Transmit-empty interrupt raised and not yet read from IIR.
    thri: bool,
}

pub struct Serial {
    regs: Mutex<Regs>,
    irq: Arc<dyn IrqLine>,
}

impl Serial {
    pub fn new(irq: Arc<dyn IrqLine>) -> Self {
        Self { regs: Mutex::new(Regs { dll: 12, ..Default::default() }), irq }
    }

    /// THR is always empty again at once; signal it if the driver asked.
    fn raise_thri(&self, r: &mut Regs) {
        if r.ier & IER_THRI != 0 && !r.thri {
            r.thri = true;
            self.irq.set_level(true);
        }
    }

    pub fn read(&self, offset: u16) -> u8 {
        let mut r = self.regs.lock().unwrap();
        let dlab = r.lcr & LCR_DLAB != 0;
        match offset {
            0 if dlab => r.dll,
            0 => 0,
            1 if dlab => r.dlm,
            1 => r.ier,
            2 => {
                let fifo = if r.fifo { IIR_FIFO } else { 0 };
                if r.thri && r.ier & IER_THRI != 0 {
                    r.thri = false; // reading IIR acknowledges THRE
                    IIR_THRI | fifo
                } else {
                    IIR_NO_INT | fifo
                }
            }
            3 => r.lcr,
            4 => r.mcr,
            5 => LSR_IDLE,
            6 => {
                if r.mcr & MCR_LOOP != 0 {
                    // DTR->DSR, RTS->CTS, OUT1->RI, OUT2->DCD
                    ((r.mcr & 1) << 5) | ((r.mcr & 2) << 3) | ((r.mcr & 4) << 4) | ((r.mcr & 8) << 4)
                } else {
                    0xb0 // DCD, DSR, CTS
                }
            }
            7 => r.scr,
            _ => 0xff,
        }
    }

    pub fn write(&self, offset: u16, value: u8) {
        let mut r = self.regs.lock().unwrap();
        let dlab = r.lcr & LCR_DLAB != 0;
        match offset {
            0 if dlab => r.dll = value,
            0 => {
                if r.mcr & MCR_LOOP == 0 {
                    crate::console::put(value);
                }
                r.thri = false;
                self.raise_thri(&mut r);
            }
            1 if dlab => r.dlm = value,
            1 => {
                let enabling = value & IER_THRI != 0 && r.ier & IER_THRI == 0;
                r.ier = value & 0x0f;
                if enabling {
                    r.thri = false;
                    self.raise_thri(&mut r);
                }
            }
            2 => r.fifo = value & 1 != 0,
            3 => r.lcr = value,
            4 => r.mcr = value & 0x1f,
            7 => r.scr = value,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct Count(AtomicUsize);
    impl IrqLine for Count {
        fn set_level(&self, high: bool) {
            if high {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    #[test]
    fn passes_the_8250_probe_and_signals_transmit_empty() {
        let irq = Arc::new(Count::default());
        let s = Serial::new(irq.clone());
        s.write(7, 0xa5);
        assert_eq!(s.read(7), 0xa5, "scratch register");
        s.write(2, 1);
        assert_eq!(s.read(2) & 0xc0, 0xc0, "FIFO reported: a 16550A");
        s.write(4, MCR_LOOP | 0x0a);
        assert_eq!(s.read(6) & 0xf0, 0x90, "loopback test: RTS->CTS, OUT2->DCD");
        s.write(4, 0);

        s.write(1, IER_THRI);
        assert_eq!(irq.0.load(Ordering::SeqCst), 1, "enabling THRI interrupts at once");
        assert_eq!(s.read(2) & 0x0f, IIR_THRI);
        assert_eq!(s.read(2) & 0x0f, IIR_NO_INT, "reading IIR acknowledged it");
        s.write(0, b'x');
        assert_eq!(irq.0.load(Ordering::SeqCst), 2, "each transmit empties THR again");
        assert_eq!(s.read(5), LSR_IDLE);
    }
}
