//! The arm64 machine: Hypervisor.framework VM + in-kernel GICv3 (macOS 15+),
//! guest RAM, FDT, Linux arm64 boot, vCPU threads, PL011 console, device wiring.

mod boot;
mod fdt;
mod gic;
pub mod hvf;
mod pl011;
mod vcpu;

use std::sync::Arc;

use crate::devices::{DeviceKind, MmioBus, MmioRegion};
use crate::memory::{GuestMemory, HOST_PAGE_SIZE, RAM_BASE};
use crate::Config;
use hvf::hv;

pub const DEFAULT_CMDLINE: &str = "console=ttyAMA0 earlycon=pl011,mmio32,0x09000000 panic=-1";

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

pub fn setup_and_run(config: Config) -> Result<i32, String> {
    hvf::vm_create().map_err(|e| e.to_string())?;

    // Guest RAM via hv_vm_allocate so its footprint is accounted accurately.
    let mem_size = (config.mem_mib << 20).div_ceil(HOST_PAGE_SIZE) * HOST_PAGE_SIZE;
    let mut host: *mut std::ffi::c_void = std::ptr::null_mut();
    hv!(hv_vm_allocate(&mut host, mem_size as usize, 0)).map_err(|e| e.to_string())?;
    hv!(hv_vm_map(host, RAM_BASE, mem_size as usize, hvf::HV_MEMORY_READ | hvf::HV_MEMORY_WRITE | hvf::HV_MEMORY_EXEC))
        .map_err(|e| e.to_string())?;
    let mem = Arc::new(unsafe { GuestMemory::from_raw(host as *mut u8, mem_size) });
    crate::memdump::spawn(mem.clone());
    crate::pressure::spawn(&config);

    let gic = gic::create().map_err(|e| e.to_string())?;
    let kicker = Arc::new(vcpu::Kicker::new());
    let spi = |n: u32| -> Arc<dyn crate::devices::IrqLine> { Arc::new(gic::SpiLine::new(&gic, n, kicker.clone())) };

    let mut bus = MmioBus::default();
    bus.add(MmioRegion {
        base: layout::UART_BASE,
        size: 0x1000,
        irq: Some(layout::UART_SPI),
        kind: DeviceKind::Pl011,
        device: Arc::new(pl011::Pl011::new(spi(layout::UART_SPI))),
    });
    for (slot, device) in crate::virtio_devices(&config, &mem).into_iter().enumerate() {
        let spi_n = layout::VIRTIO_SPI_BASE + slot as u32;
        bus.add(MmioRegion {
            base: layout::VIRTIO_BASE + slot as u64 * layout::VIRTIO_STRIDE,
            size: layout::VIRTIO_STRIDE,
            irq: Some(spi_n),
            kind: DeviceKind::VirtioMmio,
            device: Arc::new(crate::virtio::mmio::VirtioMmio::new(device, mem.clone(), spi(spi_n))),
        });
    }

    let kernel = boot::load_kernel(&mem, &config.kernel)?;
    let initrd = match &config.initrd {
        Some(p) => Some(std::fs::read(p).map_err(|e| format!("reading initrd {}: {e}", p.display()))?),
        None => None,
    };
    let place = boot::place(&mem, kernel.end, initrd.as_ref().map(|d| d.len() as u64))?;
    if let (Some(data), Some((start, _))) = (&initrd, place.initrd) {
        boot::load_initrd(&mem, start, data)?;
    }
    let dtb = fdt::build(&fdt::FdtParams {
        mem_size,
        cpus: config.cpus,
        cmdline: &config.cmdline,
        initrd: place.initrd,
        gic: &gic,
        bus: &bus,
    })
    .map_err(|e| format!("building device tree: {e:?}"))?;
    boot::load_dtb(&mem, place.dtb, &dtb)?;

    let machine = Arc::new(vcpu::Machine::new(Arc::new(bus), kicker, config.cpus));
    Ok(vcpu::run_all(machine, kernel.entry, place.dtb))
}
