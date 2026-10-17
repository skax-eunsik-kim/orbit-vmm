//! Flattened device tree for the guest, modelled on QEMU's `virt` machine.

use vm_fdt::{Error as FdtError, FdtWriter};

use super::gic::GicLayout;
use crate::devices::{DeviceKind, MmioBus};
use crate::memory::RAM_BASE;

const PHANDLE_GIC: u32 = 1;
const PHANDLE_APB_PCLK: u32 = 2;

const GIC_FDT_IRQ_TYPE_SPI: u32 = 0;
const GIC_FDT_IRQ_TYPE_PPI: u32 = 1;
const IRQ_TYPE_EDGE_RISING: u32 = 1;
const IRQ_TYPE_LEVEL_HIGH: u32 = 4;

pub struct FdtParams<'a> {
    pub mem_size: u64,
    pub cpus: u32,
    pub cmdline: &'a str,
    pub initrd: Option<(u64, u64)>, // (start gpa, end gpa)
    pub gic: &'a GicLayout,
    pub bus: &'a MmioBus,
}

/// MPIDR_EL1 affinity value for vCPU `i` (Aff0 = i, up to 16 per cluster).
pub fn mpidr_for(i: u32) -> u64 {
    ((i as u64 / 16) << 8) | (i as u64 % 16)
}

pub fn build(p: &FdtParams) -> Result<Vec<u8>, FdtError> {
    let mut f = FdtWriter::new()?;
    let root = f.begin_node("")?;
    f.property_string("compatible", "linux,dummy-virt")?;
    f.property_u32("#address-cells", 2)?;
    f.property_u32("#size-cells", 2)?;
    f.property_u32("interrupt-parent", PHANDLE_GIC)?;

    // chosen
    let chosen = f.begin_node("chosen")?;
    f.property_string("bootargs", p.cmdline)?;
    if let Some((start, end)) = p.initrd {
        f.property_u64("linux,initrd-start", start)?;
        f.property_u64("linux,initrd-end", end)?;
    }
    if let Some(uart) = p.bus.regions.iter().find(|r| r.kind == DeviceKind::Pl011) {
        f.property_string("stdout-path", &format!("/pl011@{:x}", uart.base))?;
    }
    f.end_node(chosen)?;

    // memory
    let mem = f.begin_node(&format!("memory@{RAM_BASE:x}"))?;
    f.property_string("device_type", "memory")?;
    f.property_array_u64("reg", &[RAM_BASE, p.mem_size])?;
    f.end_node(mem)?;

    // cpus
    let cpus = f.begin_node("cpus")?;
    f.property_u32("#address-cells", 1)?;
    f.property_u32("#size-cells", 0)?;
    for i in 0..p.cpus {
        let mpidr = mpidr_for(i) as u32;
        let cpu = f.begin_node(&format!("cpu@{mpidr:x}"))?;
        f.property_string("device_type", "cpu")?;
        f.property_string("compatible", "arm,arm-v8")?;
        f.property_u32("reg", mpidr)?;
        if p.cpus > 1 {
            f.property_string("enable-method", "psci")?;
        }
        f.end_node(cpu)?;
    }
    f.end_node(cpus)?;

    // psci
    let psci = f.begin_node("psci")?;
    f.property_string_list("compatible", vec!["arm,psci-1.0".into(), "arm,psci-0.2".into()])?;
    f.property_string("method", "hvc")?;
    f.end_node(psci)?;

    // GICv3
    let g = p.gic;
    let redist_used = (g.redist_size * p.cpus as u64).min(g.redist_region_size);
    let intc = f.begin_node(&format!("intc@{:x}", g.dist_base))?;
    f.property_string("compatible", "arm,gic-v3")?;
    f.property_u32("#interrupt-cells", 3)?;
    f.property_u32("#address-cells", 2)?;
    f.property_u32("#size-cells", 2)?;
    f.property_null("ranges")?;
    f.property_null("interrupt-controller")?;
    f.property_u32("#redistributor-regions", 1)?;
    f.property_array_u64("reg", &[g.dist_base, g.dist_size, g.redist_base, redist_used])?;
    f.property_phandle(PHANDLE_GIC)?;
    f.end_node(intc)?;

    // Architected timer: secure phys, non-secure phys, virt, hyp (PPI numbers).
    let ppi = |intid: u32| intid - 16;
    let timer = f.begin_node("timer")?;
    f.property_string_list("compatible", vec!["arm,armv8-timer".into(), "arm,armv7-timer".into()])?;
    let t = IRQ_TYPE_LEVEL_HIGH;
    f.property_array_u32(
        "interrupts",
        &[
            GIC_FDT_IRQ_TYPE_PPI,
            13,
            t,
            GIC_FDT_IRQ_TYPE_PPI,
            ppi(g.ptimer_intid),
            t,
            GIC_FDT_IRQ_TYPE_PPI,
            ppi(g.vtimer_intid),
            t,
            GIC_FDT_IRQ_TYPE_PPI,
            10,
            t,
        ],
    )?;
    f.property_null("always-on")?;
    f.end_node(timer)?;

    // apb_pclk for the PL011
    let clk = f.begin_node("apb-pclk")?;
    f.property_string("compatible", "fixed-clock")?;
    f.property_u32("#clock-cells", 0)?;
    f.property_u32("clock-frequency", 24_000_000)?;
    f.property_string("clock-output-names", "clk24mhz")?;
    f.property_phandle(PHANDLE_APB_PCLK)?;
    f.end_node(clk)?;

    for r in &p.bus.regions {
        match r.kind {
            DeviceKind::Pl011 => {
                let n = f.begin_node(&format!("pl011@{:x}", r.base))?;
                f.property_string_list("compatible", vec!["arm,pl011".into(), "arm,primecell".into()])?;
                f.property_array_u64("reg", &[r.base, r.size])?;
                if let Some(spi) = r.spi {
                    f.property_array_u32("interrupts", &[GIC_FDT_IRQ_TYPE_SPI, spi, IRQ_TYPE_LEVEL_HIGH])?;
                }
                f.property_array_u32("clocks", &[PHANDLE_APB_PCLK, PHANDLE_APB_PCLK])?;
                f.property_string_list("clock-names", vec!["uartclk".into(), "apb_pclk".into()])?;
                f.end_node(n)?;
            }
            DeviceKind::VirtioMmio => {
                let n = f.begin_node(&format!("virtio_mmio@{:x}", r.base))?;
                f.property_string("compatible", "virtio,mmio")?;
                f.property_array_u64("reg", &[r.base, r.size])?;
                if let Some(spi) = r.spi {
                    // Level-high: the transport holds the line while InterruptStatus != 0.
                    f.property_array_u32("interrupts", &[GIC_FDT_IRQ_TYPE_SPI, spi, IRQ_TYPE_LEVEL_HIGH])?;
                }
                f.property_null("dma-coherent")?;
                f.end_node(n)?;
            }
        }
    }
    let _ = IRQ_TYPE_EDGE_RISING;

    f.end_node(root)?;
    f.finish()
}
