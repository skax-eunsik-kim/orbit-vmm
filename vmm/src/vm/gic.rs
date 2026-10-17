//! In-kernel GICv3 (hv_gic, macOS 15+) and SPI interrupt lines.

use std::sync::Arc;

use super::hvf::{self, hv, HvResult};
use super::vcpu::Kicker;
use crate::devices::{layout, IrqLine};

/// Guest-physical placement of the GIC, as created.
#[derive(Clone, Copy, Debug)]
pub struct GicLayout {
    pub dist_base: u64,
    pub dist_size: u64,
    pub redist_base: u64,
    /// Size of one vCPU's redistributor (two 64 KiB frames).
    pub redist_size: u64,
    /// Total redistributor region size reserved by the framework.
    pub redist_region_size: u64,
    pub spi_base: u32,
    pub spi_count: u32,
    /// INTIDs of the PPIs the framework reserves for the timers.
    pub vtimer_intid: u32,
    pub ptimer_intid: u32,
}

fn align_up(v: u64, a: u64) -> u64 {
    if a <= 1 {
        v
    } else {
        v.div_ceil(a) * a
    }
}

/// Create the GIC. Must run after hv_vm_create and before any hv_vcpu_create.
pub fn create() -> HvResult<GicLayout> {
    let mut dist_size = 0usize;
    let mut dist_align = 0usize;
    let mut redist_region_size = 0usize;
    let mut redist_size = 0usize;
    let mut redist_align = 0usize;
    let (mut spi_base, mut spi_count) = (0u32, 0u32);
    hv!(hv_gic_get_distributor_size(&mut dist_size))?;
    hv!(hv_gic_get_distributor_base_alignment(&mut dist_align))?;
    hv!(hv_gic_get_redistributor_region_size(&mut redist_region_size))?;
    hv!(hv_gic_get_redistributor_size(&mut redist_size))?;
    hv!(hv_gic_get_redistributor_base_alignment(&mut redist_align))?;
    hv!(hv_gic_get_spi_interrupt_range(&mut spi_base, &mut spi_count))?;
    let mut vtimer_intid = 0u32;
    let mut ptimer_intid = 0u32;
    hv!(hv_gic_get_intid(hvf::HV_GIC_INT_EL1_VIRTUAL_TIMER, &mut vtimer_intid))?;
    hv!(hv_gic_get_intid(hvf::HV_GIC_INT_EL1_PHYSICAL_TIMER, &mut ptimer_intid))?;

    let dist_base = align_up(layout::GIC_DIST_BASE, dist_align as u64);
    let mut redist_base = align_up(layout::GIC_REDIST_BASE.max(dist_base + dist_size as u64), redist_align as u64);
    // The redistributor region covers every vCPU the framework supports; if that
    // would run into the UART/virtio window, move it to a free hole below RAM.
    if redist_base + redist_region_size as u64 > layout::UART_BASE {
        redist_base = align_up(0x1000_0000, redist_align as u64);
        assert!(redist_base + (redist_region_size as u64) <= crate::memory::RAM_BASE);
    }

    let config = unsafe { hvf::hv_gic_config_create() };
    if config.is_null() {
        return Err(hvf::HvError { call: "hv_gic_config_create", code: hvf::HV_ERROR });
    }
    let r = (|| {
        hv!(hv_gic_config_set_distributor_base(config, dist_base))?;
        hv!(hv_gic_config_set_redistributor_base(config, redist_base))?;
        hv!(hv_gic_create(config))
    })();
    unsafe { hvf::os_release(config) };
    r?;

    Ok(GicLayout {
        dist_base,
        dist_size: dist_size as u64,
        redist_base,
        redist_size: redist_size as u64,
        redist_region_size: redist_region_size as u64,
        spi_base,
        spi_count,
        vtimer_intid,
        ptimer_intid,
    })
}

/// A level-sensitive SPI driven through hv_gic_set_spi.
pub struct SpiLine {
    intid: u32,
    kicker: Arc<Kicker>,
}

impl SpiLine {
    /// `spi` is the 0-based SPI number (INTID - 32), as in the device tree.
    pub fn new(gic: &GicLayout, spi: u32, kicker: Arc<Kicker>) -> Self {
        let intid = 32 + spi;
        assert!(intid >= gic.spi_base && intid < gic.spi_base + gic.spi_count, "SPI {spi} outside the GIC's range");
        Self { intid, kicker }
    }
}

impl IrqLine for SpiLine {
    fn set_level(&self, high: bool) {
        if let Err(e) = hv!(hv_gic_set_spi(self.intid, high)) {
            eprintln!("orbit-vmm: {e} (intid {})", self.intid);
        }
        if high {
            // Wake vCPUs parked in WFI so they re-enter the guest and take it.
            self.kicker.kick_all();
        }
    }
}
