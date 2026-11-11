//! Idle vCPUs. The kernel runs a guest HLT inside VMX: the vCPU thread keeps
//! its core (hv_vcpu_get_idle_time stays 0) and only comes back to us on the
//! next host interrupt, so an idle 4-vCPU guest showed as 400% CPU. When an
//! exit finds the guest halted, the thread instead sleeps here until the guest
//! has something to do: an IOAPIC interrupt or an IPI (both pass through this
//! VMM and kick), or its local APIC timer (whose remaining time we read from
//! the in-kernel APIC). This is the x86 counterpart of WFI parking on arm64.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

/// Wakes parked vCPUs when an interrupt may have become pending for them.
pub struct Kicker {
    generation: AtomicU64,
    lock: Mutex<()>,
    cond: Condvar,
}

impl Kicker {
    pub fn new() -> Self {
        Self { generation: AtomicU64::new(0), lock: Mutex::new(()), cond: Condvar::new() }
    }

    pub fn kick(&self) {
        let _g = self.lock.lock().unwrap();
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.cond.notify_all();
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// Sleep until kicked (generation moves past `seen`) or `timeout` passes.
    pub fn park(&self, seen: u64, timeout: Duration) {
        let guard = self.lock.lock().unwrap();
        if self.generation() != seen {
            return;
        }
        let _ = self.cond.wait_timeout_while(guard, timeout, |_| self.generation() == seen);
    }
}

/// Local APIC register offsets.
pub const APIC_ICR_LOW: u32 = 0x300;
pub const APIC_IRR: u32 = 0x200;
pub const APIC_LVT_TIMER: u32 = 0x320;
pub const APIC_TIMER_CURRENT: u32 = 0x390;
pub const APIC_TIMER_DIVIDE: u32 = 0x3e0;

/// Time until the local APIC timer fires, from its LVT entry, current count
/// and divide configuration; None if it is masked or stopped.
pub fn timer_remaining(lvt: u32, current: u32, divide: u32, bus_hz: u64) -> Option<Duration> {
    if lvt & (1 << 16) != 0 || current == 0 {
        return None;
    }
    if (lvt >> 17) & 3 == 2 {
        // TSC-deadline mode isn't offered in CPUID; poll rather than guess.
        return Some(Duration::from_millis(1));
    }
    let code = ((divide >> 1) & 4) | (divide & 3);
    let div: u128 = if code == 7 { 1 } else { 2 << code };
    let ns = current as u128 * div * 1_000_000_000 / bus_hz as u128;
    Some(Duration::from_nanos(ns.min(u64::MAX as u128) as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_remaining_follows_count_and_divider() {
        let ghz = 1_000_000_000;
        // Linux's setup: divide by 16 (code 0b011), one-shot.
        assert_eq!(timer_remaining(0xec, 62_500, 0x3, ghz), Some(Duration::from_millis(1)));
        assert_eq!(timer_remaining(0xec, 1_000, 0xb, ghz), Some(Duration::from_micros(1)), "0b1011: divide by 1");
        assert_eq!(timer_remaining(0xec, 1_000, 0x0, ghz), Some(Duration::from_micros(2)), "0b0000: divide by 2");
        assert_eq!(timer_remaining(0x1_00ec, 1_000, 0x3, ghz), None, "masked");
        assert_eq!(timer_remaining(0xec, 0, 0x3, ghz), None, "stopped");
    }

    #[test]
    fn park_returns_at_once_after_a_kick() {
        let k = Kicker::new();
        let seen = k.generation();
        k.kick();
        let t = std::time::Instant::now();
        k.park(seen, Duration::from_secs(5));
        assert!(t.elapsed() < Duration::from_secs(1));
    }
}
