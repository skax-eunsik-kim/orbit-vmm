//! The guest console (PL011 on arm64, 8250 on x86-64) goes to stdout, which
//! the supervisor appends to console.log.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};

/// The guest controls what it prints; stop copying it to console.log past this
/// much, so it can't fill the Mac's disk.
const MAX_CONSOLE_BYTES: u64 = 256 << 20;
static CONSOLE_BYTES: AtomicU64 = AtomicU64::new(0);

/// One byte the guest transmitted.
pub fn put(byte: u8) {
    let n = CONSOLE_BYTES.fetch_add(1, Ordering::Relaxed);
    if n < MAX_CONSOLE_BYTES {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(&[byte]);
        if byte == b'\n' {
            let _ = out.flush();
        }
    } else if n == MAX_CONSOLE_BYTES {
        println!("\norbit-vmm: console output limit reached; further guest output dropped");
    }
}
