//! Device-facing interfaces shared by the core (agent A) and devices (agents B, C).

use std::sync::Arc;

/// A level-triggered interrupt line into the guest's GIC (an SPI).
/// Implemented by the core on top of `hv_gic_set_spi`.
pub trait IrqLine: Send + Sync {
    fn set_level(&self, high: bool);
}

/// A device occupying an MMIO window. `offset` is relative to the window base.
/// `data.len()` is 1, 2, 4 or 8. Little-endian, like the guest.
pub trait MmioDevice: Send + Sync {
    fn read(&self, offset: u64, data: &mut [u8]);
    fn write(&self, offset: u64, data: &[u8]);
}

/// How the device appears in the device tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    /// ARM PL011 UART ("arm,pl011"), used as the kernel console (ttyAMA0).
    Pl011,
    /// virtio-mmio transport ("virtio,mmio"), any virtio device behind it.
    VirtioMmio,
}

pub struct MmioRegion {
    pub base: u64,
    pub size: u64,
    /// GIC SPI number (0-based, i.e. INTID - 32), or None.
    pub spi: Option<u32>,
    pub kind: DeviceKind,
    pub device: Arc<dyn MmioDevice>,
}

/// Fixed guest-physical layout. RAM starts at memory::RAM_BASE (0x8000_0000).
pub mod layout {
    pub const GIC_DIST_BASE: u64 = 0x0800_0000; // 64 KiB
    pub const GIC_REDIST_BASE: u64 = 0x080a_0000; // 128 KiB per vCPU
    pub const UART_BASE: u64 = 0x0900_0000;
    pub const UART_SPI: u32 = 1;
    /// virtio-mmio slot i lives at VIRTIO_BASE + i * VIRTIO_STRIDE, SPI VIRTIO_SPI_BASE + i.
    pub const VIRTIO_BASE: u64 = 0x0a00_0000;
    pub const VIRTIO_STRIDE: u64 = 0x200;
    pub const VIRTIO_SPI_BASE: u32 = 16;
}

/// The MMIO bus the vCPU loop dispatches data aborts to.
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
