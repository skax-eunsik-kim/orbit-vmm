//! Device-facing interfaces shared by the core and the devices.

use std::sync::Arc;

/// A level-triggered interrupt line into the guest's interrupt controller:
/// a GIC SPI (`hv_gic_set_spi`) on arm64, an IOAPIC pin on x86-64.
pub trait IrqLine: Send + Sync {
    fn set_level(&self, high: bool);
}

/// A device occupying an MMIO window. `offset` is relative to the window base.
/// `data.len()` is 1, 2, 4 or 8. Little-endian, like the guest.
pub trait MmioDevice: Send + Sync {
    fn read(&self, offset: u64, data: &mut [u8]);
    fn write(&self, offset: u64, data: &[u8]);
}

/// What the device is, which decides how the guest learns about it (device
/// tree node on arm64, kernel command line or MP table on x86-64).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    /// ARM PL011 UART ("arm,pl011"), used as the kernel console (ttyAMA0).
    #[cfg(target_arch = "aarch64")]
    Pl011,
    /// virtio-mmio transport ("virtio,mmio"), any virtio device behind it.
    VirtioMmio,
    /// x86 I/O APIC (described by the MP table).
    #[cfg(target_arch = "x86_64")]
    IoApic,
}

pub struct MmioRegion {
    pub base: u64,
    pub size: u64,
    /// Interrupt line, or None: GIC SPI number (0-based, i.e. INTID - 32)
    /// on arm64, IOAPIC pin on x86-64.
    pub irq: Option<u32>,
    pub kind: DeviceKind,
    pub device: Arc<dyn MmioDevice>,
}

/// The MMIO bus the vCPU loop dispatches guest device accesses to (data
/// aborts on arm64, EPT violations on x86-64).
#[derive(Default)]
pub struct MmioBus {
    pub regions: Vec<MmioRegion>,
}

impl MmioBus {
    pub fn add(&mut self, region: MmioRegion) {
        self.regions.push(region);
    }

    pub fn find(&self, gpa: u64) -> Option<(&MmioRegion, u64)> {
        self.regions.iter().find(|r| gpa >= r.base && gpa < r.base + r.size).map(|r| (r, gpa - r.base))
    }
}
