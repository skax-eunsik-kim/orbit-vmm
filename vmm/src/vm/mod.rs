//! Core VMM: Hypervisor.framework VM + in-kernel GICv3, guest RAM, FDT,
//! Linux arm64 boot, vCPU threads, PL011 console, device wiring.

mod boot;
mod fdt;
mod gic;
pub mod hvf;
mod pl011;
mod vcpu;

use std::path::PathBuf;
use std::sync::Arc;

use crate::devices::{layout, DeviceKind, MmioBus, MmioRegion};
use crate::memory::{GuestMemory, HOST_PAGE_SIZE, RAM_BASE};
use crate::Config;
use hvf::hv;

const USAGE: &str = "usage: orbit-vmm --kernel <Image> [--initrd <cpio.gz>] [--mem <MiB>] [--cpus <n>] \
[--cmdline \"<args>\"] [--disk <raw.img>] [--balloon] [--net] [--fwd tcp:HOST:GUEST] [--vsock PATH:PORT] [--share DIR:TAG] [--share-allow DIR]...";

pub fn parse_args(args: impl Iterator<Item = String>) -> Config {
    let mut config = Config {
        kernel: PathBuf::new(),
        initrd: None,
        mem_mib: 1024,
        cpus: 1,
        cmdline: "console=ttyAMA0 earlycon=pl011,mmio32,0x09000000 panic=-1".into(),
        disk: None,
        balloon: false,
        net: false,
        host_loopback: true,
        forwards: Vec::new(),
        vsock: Vec::new(),
        shares: Vec::new(),
        share_allow: Vec::new(),
    };
    let mut args = args.peekable();
    let die = |msg: &str| -> ! {
        eprintln!("orbit-vmm: {msg}\n{USAGE}");
        std::process::exit(2)
    };
    while let Some(arg) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| die(&format!("{arg} needs a value")));
        match arg.as_str() {
            "--kernel" => config.kernel = value().into(),
            "--initrd" => config.initrd = Some(value().into()),
            "--mem" => config.mem_mib = value().parse().unwrap_or_else(|_| die("--mem needs MiB")),
            "--cpus" => config.cpus = value().parse().unwrap_or_else(|_| die("--cpus needs a number")),
            "--cmdline" => config.cmdline = value(),
            "--disk" => config.disk = Some(value().into()),
            "--balloon" => config.balloon = true,
            "--net" => config.net = true,
            "--no-host-loopback" => config.host_loopback = false,
            // Restrict every share to these directories (and the path down to them).
            "--share-allow" => config.share_allow.push(value().into()),
            "--share" => {
                let spec = value();
                match spec.rsplit_once(':') {
                    Some((path, tag)) if !tag.is_empty() => config.shares.push(crate::virtio::fs::Share {
                        path: path.into(),
                        tag: tag.into(),
                        notify: None,
                        allow: Vec::new(),
                    }),
                    _ => die(&format!("bad --share {spec}, want DIR:TAG")),
                }
            }
            "--vsock" => {
                let spec = value();
                match spec.rsplit_once(':').and_then(|(p, port)| Some((p, port.parse::<u32>().ok()?))) {
                    Some((path, guest_port)) => {
                        config.vsock.push(crate::virtio::vsock::VsockListen { path: path.into(), guest_port })
                    }
                    None => die(&format!("bad --vsock {spec}, want PATH:PORT")),
                }
            }
            "--fwd" => {
                let spec = value();
                let parts: Vec<&str> = spec.split(':').collect();
                let parse = |s: &str| s.parse::<u16>().ok();
                match parts.as_slice() {
                    [proto @ ("tcp" | "udp"), h, g] if parse(h).is_some() && parse(g).is_some() => {
                        config.net = true;
                        config.forwards.push(crate::slirp::Forward {
                            udp: *proto == "udp",
                            host_port: parse(h).unwrap(),
                            guest_port: parse(g).unwrap(),
                        });
                    }
                    _ => die(&format!("bad --fwd {spec}, want tcp:HOST:GUEST")),
                }
            }
            "-h" | "--help" => die("help"),
            _ => die(&format!("unknown argument {arg}")),
        }
    }
    if config.kernel.as_os_str().is_empty() {
        die("--kernel is required");
    }
    if config.mem_mib < 256 {
        die("--mem must be at least 256");
    }
    config
}

pub fn run(config: Config) -> i32 {
    match setup_and_run(config) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("orbit-vmm: {e}");
            1
        }
    }
}

fn setup_and_run(config: Config) -> Result<i32, String> {
    hv!(hv_vm_create(std::ptr::null_mut())).map_err(|e| e.to_string())?;

    // Guest RAM via hv_vm_allocate so its footprint is accounted accurately.
    let mem_size = (config.mem_mib << 20).div_ceil(HOST_PAGE_SIZE) * HOST_PAGE_SIZE;
    let mut host: *mut std::ffi::c_void = std::ptr::null_mut();
    hv!(hv_vm_allocate(&mut host, mem_size as usize, 0)).map_err(|e| e.to_string())?;
    hv!(hv_vm_map(host, RAM_BASE, mem_size as usize, hvf::HV_MEMORY_READ | hvf::HV_MEMORY_WRITE | hvf::HV_MEMORY_EXEC))
        .map_err(|e| e.to_string())?;
    let mem = Arc::new(unsafe { GuestMemory::from_raw(host as *mut u8, mem_size) });
    crate::memdump::spawn(mem.clone());

    let gic = gic::create().map_err(|e| e.to_string())?;
    let kicker = Arc::new(vcpu::Kicker::new());
    let spi = |n: u32| -> Arc<dyn crate::devices::IrqLine> { Arc::new(gic::SpiLine::new(&gic, n, kicker.clone())) };

    let mut bus = MmioBus::default();
    bus.add(MmioRegion {
        base: layout::UART_BASE,
        size: 0x1000,
        spi: Some(layout::UART_SPI),
        kind: DeviceKind::Pl011,
        device: Arc::new(pl011::Pl011::new(spi(layout::UART_SPI))),
    });
    for (slot, device) in crate::virtio_devices(&config, &mem).into_iter().enumerate() {
        let spi_n = layout::VIRTIO_SPI_BASE + slot as u32;
        bus.add(MmioRegion {
            base: layout::VIRTIO_BASE + slot as u64 * layout::VIRTIO_STRIDE,
            size: layout::VIRTIO_STRIDE,
            spi: Some(spi_n),
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
