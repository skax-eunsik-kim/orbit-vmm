//! The x86-64 machine (Intel Macs): Hypervisor.framework VMX with the
//! in-kernel local APIC (macOS 12+), an IOAPIC and 8250 UART emulated here,
//! guest RAM below and above a 32-bit MMIO hole, an MP table instead of ACPI,
//! and the Linux 64-bit boot protocol. virtio devices are virtio-mmio,
//! announced on the kernel command line (virtio_mmio.device=).

mod boot;
mod cpuid;
mod decode;
pub mod hvf;
mod idle;
mod ioapic;
mod mptable;
mod rtc;
mod serial;
mod vcpu;

use std::sync::Arc;

use crate::devices::{DeviceKind, MmioBus, MmioRegion};
use crate::memory::{GuestMemory, HOST_PAGE_SIZE};
use crate::Config;
use hvf::hv;

pub const DEFAULT_CMDLINE: &str = "console=ttyS0 earlycon=uart8250,io,0x3f8 panic=-1";

/// Guest-physical layout.
pub mod layout {
    /// RAM runs from 0 up to this, the rest continues at HIGH_RAM_BASE.
    pub const LOW_RAM_MAX: u64 = 0xc000_0000;
    pub const HIGH_RAM_BASE: u64 = 0x1_0000_0000;
    /// virtio-mmio slot i at VIRTIO_BASE + i * VIRTIO_STRIDE, IOAPIC pin VIRTIO_IRQ_BASE + i.
    pub const VIRTIO_BASE: u64 = 0xd000_0000;
    pub const VIRTIO_STRIDE: u64 = 0x200;
    pub const VIRTIO_IRQ_BASE: u32 = 5;
    pub const IOAPIC_BASE: u64 = 0xfec0_0000;
    pub const LAPIC_BASE: u64 = 0xfee0_0000;
    /// Local APIC timer clock. 1 GHz gives Linux's clockevent (bus / 16) a
    /// 16 ns resolution; the framework accepts 16 MHz to 2 GHz.
    pub const APIC_BUS_HZ: u64 = 1_000_000_000;
}

/// What the kernel can't discover on this machine: there is no PCI, no ACPI,
/// no PIT/HPET (the TSC rate comes from CPUID), and reset goes through the
/// i8042 port. Plus one virtio_mmio.device= per virtio window.
fn machine_cmdline(bus: &MmioBus) -> String {
    let mut s = String::from(" pci=off acpi=off tsc=reliable reboot=k");
    for r in bus.regions.iter().filter(|r| r.kind == DeviceKind::VirtioMmio) {
        if let Some(irq) = r.irq {
            s.push_str(&format!(" virtio_mmio.device=0x{:x}@0x{:x}:{irq}", r.size, r.base));
        }
    }
    s
}

fn tsc_hz() -> Result<u64, String> {
    let mut hz: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    let name = c"machdep.tsc.frequency";
    let rc =
        unsafe { libc::sysctlbyname(name.as_ptr(), (&mut hz as *mut u64).cast(), &mut len, std::ptr::null_mut(), 0) };
    if rc != 0 || hz == 0 {
        return Err("cannot read machdep.tsc.frequency".into());
    }
    Ok(hz)
}

pub fn setup_and_run(config: Config) -> Result<i32, String> {
    hvf::vm_create().map_err(|e| e.to_string())?;
    hv!(hv_vm_set_apic_bus_freq(layout::APIC_BUS_HZ)).map_err(|e| e.to_string())?;
    let tsc_hz = tsc_hz()?;

    // One hv_vm_allocate block (accurate footprint accounting), mapped as RAM
    // below the MMIO hole and, for the rest, from 4 GiB up.
    let mem_size = (config.mem_mib << 20).div_ceil(HOST_PAGE_SIZE) * HOST_PAGE_SIZE;
    let low = mem_size.min(layout::LOW_RAM_MAX);
    let mut ram = vec![(0, low)];
    if mem_size > low {
        ram.push((layout::HIGH_RAM_BASE, mem_size - low));
    }
    let mut host: *mut std::ffi::c_void = std::ptr::null_mut();
    hv!(hv_vm_allocate(&mut host, mem_size as usize, 0)).map_err(|e| e.to_string())?;
    let flags = hvf::HV_MEMORY_READ | hvf::HV_MEMORY_WRITE | hvf::HV_MEMORY_EXEC;
    let mut off = 0;
    for &(gpa, size) in &ram {
        let p = unsafe { host.cast::<u8>().add(off as usize) };
        hv!(hv_vm_map(p.cast(), gpa, size as usize, flags)).map_err(|e| e.to_string())?;
        off += size;
    }
    let mem = Arc::new(unsafe { GuestMemory::from_raw_regions(host as *mut u8, &ram) });
    crate::memdump::spawn(mem.clone());
    crate::pressure::spawn(&config);

    let kicker = Arc::new(idle::Kicker::new());
    let ioapic = Arc::new(ioapic::IoApic::new(config.cpus, Box::new(ioapic::HvDeliver(kicker.clone()))));
    let line = |pin: u32| -> Arc<dyn crate::devices::IrqLine> { Arc::new(ioapic::Line::new(ioapic.clone(), pin)) };
    let serial = serial::Serial::new(line(serial::IRQ));

    let mut bus = MmioBus::default();
    bus.add(MmioRegion {
        base: layout::IOAPIC_BASE,
        size: 0x1000,
        irq: None,
        kind: DeviceKind::IoApic,
        device: ioapic.clone(),
    });
    for (slot, device) in crate::virtio_devices(&config, &mem).into_iter().enumerate() {
        let pin = layout::VIRTIO_IRQ_BASE + slot as u32;
        if pin as usize >= ioapic::PINS {
            return Err(format!("too many virtio devices (IOAPIC has {} pins)", ioapic::PINS));
        }
        bus.add(MmioRegion {
            base: layout::VIRTIO_BASE + slot as u64 * layout::VIRTIO_STRIDE,
            size: layout::VIRTIO_STRIDE,
            irq: Some(pin),
            kind: DeviceKind::VirtioMmio,
            device: Arc::new(crate::virtio::mmio::VirtioMmio::new(device, mem.clone(), line(pin))),
        });
    }

    let kernel = boot::load_kernel(&mem, &config.kernel)?;
    let initrd = match &config.initrd {
        Some(p) => {
            let data = std::fs::read(p).map_err(|e| format!("reading initrd {}: {e}", p.display()))?;
            let start = boot::place_initrd(&kernel, low, data.len() as u64)?;
            if !mem.write(start, &data) {
                return Err("initrd load failed".into());
            }
            Some((start, data.len() as u64))
        }
        None => None,
    };
    let cmdline = config.cmdline.clone() + &machine_cmdline(&bus);
    boot::write_zero_page(&mem, &kernel, &cmdline, initrd, &ram)?;
    let [signature, _, _, features] = cpuid::host(1, 0);
    let table = mptable::build(config.cpus, layout::IOAPIC_BASE as u32, ioapic::PINS as u8, signature, features);
    mptable::write(&mem, &table)?;

    let machine = Arc::new(vcpu::Machine::new(Arc::new(bus), mem, serial, kicker, tsc_hz, config.cpus, kernel.entry));
    Ok(vcpu::run_all(machine))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Nop;
    impl crate::devices::MmioDevice for Nop {
        fn read(&self, _: u64, _: &mut [u8]) {}
        fn write(&self, _: u64, _: &[u8]) {}
    }

    #[test]
    fn announces_virtio_windows_on_the_command_line() {
        let mut bus = MmioBus::default();
        for (base, irq, kind) in
            [(layout::IOAPIC_BASE, None, DeviceKind::IoApic), (layout::VIRTIO_BASE, Some(5), DeviceKind::VirtioMmio)]
        {
            bus.add(MmioRegion { base, size: 0x200, irq, kind, device: Arc::new(Nop) });
        }
        let s = machine_cmdline(&bus);
        assert!(s.ends_with(" virtio_mmio.device=0x200@0xd0000000:5"), "{s}");
        assert_eq!(s.matches("virtio_mmio.device").count(), 1);
    }
}
