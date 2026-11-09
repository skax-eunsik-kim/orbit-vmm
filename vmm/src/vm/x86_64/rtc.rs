//! CMOS real-time clock at ports 0x70/0x71, read-only: Linux reads the wall
//! clock from it at boot (arm64 guests get it from orbit.time= instead).
//! Values are the Mac's UTC time in BCD, 24-hour mode.

use std::sync::atomic::{AtomicU8, Ordering};

pub const INDEX: u16 = 0x70;
pub const DATA: u16 = 0x71;

#[derive(Default)]
pub struct Rtc {
    index: AtomicU8,
}

fn bcd(v: i32) -> u8 {
    let v = v.rem_euclid(100) as u8;
    ((v / 10) << 4) | (v % 10)
}

impl Rtc {
    pub fn write_index(&self, v: u8) {
        self.index.store(v & 0x7f, Ordering::Relaxed); // bit 7 is the NMI mask
    }

    pub fn read_data(&self) -> u8 {
        register(self.index.load(Ordering::Relaxed), unsafe { libc::time(std::ptr::null_mut()) })
    }
}

/// CMOS register `index` at Unix time `now`.
pub fn register(index: u8, now: libc::time_t) -> u8 {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::gmtime_r(&now, &mut tm) };
    match index {
        0x00 => bcd(tm.tm_sec),
        0x02 => bcd(tm.tm_min),
        0x04 => bcd(tm.tm_hour),
        0x06 => bcd(tm.tm_wday + 1),
        0x07 => bcd(tm.tm_mday),
        0x08 => bcd(tm.tm_mon + 1),
        0x09 => bcd(tm.tm_year + 1900),
        0x32 => bcd((tm.tm_year + 1900) / 100),
        0x0a => 0x26, // no update in progress, 32.768 kHz, 1024 Hz
        0x0b => 0x02, // 24-hour, BCD
        0x0d => 0x80, // battery good
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reports_utc_in_bcd() {
        let t = 1_794_234_567; // 2026-11-09 14:29:27 UTC
        let r = |i| super::register(i, t);
        assert_eq!([r(0x32), r(0x09), r(0x08), r(0x07)], [0x20, 0x26, 0x11, 0x09]);
        assert_eq!([r(0x04), r(0x02), r(0x00)], [0x14, 0x29, 0x27]);
    }
}
