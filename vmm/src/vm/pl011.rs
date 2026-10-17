//! Minimal ARM PL011 UART: enough for earlycon + the Linux amba-pl011 console
//! driver. Output goes to stdout; there is no input.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// The guest controls what it prints; stop copying it to console.log past this
/// much, so it can't fill the Mac's disk.
const MAX_CONSOLE_BYTES: u64 = 256 << 20;
static CONSOLE_BYTES: AtomicU64 = AtomicU64::new(0);

use crate::devices::{IrqLine, MmioDevice};

const DR: u64 = 0x00;
const FR: u64 = 0x18;
const IBRD: u64 = 0x24;
const FBRD: u64 = 0x28;
const LCR_H: u64 = 0x2c;
const CR: u64 = 0x30;
const IFLS: u64 = 0x34;
const IMSC: u64 = 0x38;
const RIS: u64 = 0x3c;
const MIS: u64 = 0x40;
const ICR: u64 = 0x44;

const FR_RXFE: u32 = 1 << 4;
const FR_TXFE: u32 = 1 << 7;
const INT_TX: u32 = 1 << 5;

/// PrimeCell peripheral/cell IDs at 0xfe0..0xffc (PL011 r1p5).
const ID: [u32; 8] = [0x11, 0x10, 0x14, 0x00, 0x0d, 0xf0, 0x05, 0xb1];

#[derive(Default)]
struct Regs {
    ibrd: u32,
    fbrd: u32,
    lcr_h: u32,
    cr: u32,
    ifls: u32,
    imsc: u32,
    ris: u32,
    /// Last level driven on the IRQ line (each change is a GIC call and a kick).
    level: bool,
}

pub struct Pl011 {
    regs: Mutex<Regs>,
    irq: Arc<dyn IrqLine>,
}

impl Pl011 {
    pub fn new(irq: Arc<dyn IrqLine>) -> Self {
        Self { regs: Mutex::new(Regs { cr: 0x300, ifls: 0x12, ..Default::default() }), irq }
    }

    fn update_irq(&self, regs: &mut Regs) {
        let level = regs.ris & regs.imsc != 0;
        if level != regs.level {
            regs.level = level;
            self.irq.set_level(level);
        }
    }
}

impl MmioDevice for Pl011 {
    fn read(&self, offset: u64, data: &mut [u8]) {
        let regs = self.regs.lock().unwrap();
        let value = match offset {
            DR => 0,
            FR => FR_TXFE | FR_RXFE,
            IBRD => regs.ibrd,
            FBRD => regs.fbrd,
            LCR_H => regs.lcr_h,
            CR => regs.cr,
            IFLS => regs.ifls,
            IMSC => regs.imsc,
            RIS => regs.ris,
            MIS => regs.ris & regs.imsc,
            0xfe0..=0xffc => ID[((offset - 0xfe0) / 4) as usize],
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
        let mut regs = self.regs.lock().unwrap();
        match offset {
            DR => {
                let n = CONSOLE_BYTES.fetch_add(1, Ordering::Relaxed);
                if n < MAX_CONSOLE_BYTES {
                    let mut out = std::io::stdout().lock();
                    let _ = out.write_all(&[value as u8]);
                    if value as u8 == b'\n' {
                        let _ = out.flush();
                    }
                } else if n == MAX_CONSOLE_BYTES {
                    println!("\norbit-vmm: console output limit reached; further guest output dropped");
                }
                // The transmit FIFO drains instantly.
                regs.ris |= INT_TX;
            }
            IBRD => regs.ibrd = value,
            FBRD => regs.fbrd = value,
            LCR_H => regs.lcr_h = value,
            CR => regs.cr = value,
            IFLS => regs.ifls = value,
            IMSC => {
                regs.imsc = value;
                // A freshly enabled TX interrupt fires because the FIFO is empty.
                if value & INT_TX != 0 {
                    regs.ris |= INT_TX;
                }
            }
            ICR => regs.ris &= !value,
            _ => {}
        }
        self.update_irq(&mut regs);
    }
}
