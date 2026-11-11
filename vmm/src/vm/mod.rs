//! The machine: Hypervisor.framework VM, guest RAM, boot, vCPU threads and
//! the interrupt controller. Everything below is architecture specific: the
//! arm64 machine (in-kernel GICv3, FDT, PSCI, PL011) and the x86-64 machine
//! (VMX, LAPIC/IOAPIC, MP table, 8250). The devices (`crate::virtio`) and
//! the host side (slirp, ports, reclaim) are shared.

use std::path::PathBuf;

use crate::Config;

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "aarch64")]
use aarch64 as arch;
#[cfg(target_arch = "x86_64")]
mod x86_64;
#[cfg(target_arch = "x86_64")]
use x86_64 as arch;

pub use arch::hvf;

const USAGE: &str = "usage: orbit-vmm --kernel <Image> [--initrd <cpio.gz>] [--mem <MiB>] [--cpus <n>] \
[--cmdline \"<args>\"] [--disk <raw.img>] [--balloon] [--net] [--fwd tcp:HOST:GUEST] [--vsock PATH:PORT] [--share DIR:TAG] [--share-allow DIR]...";

pub fn parse_args(args: impl Iterator<Item = String>) -> Config {
    let mut config = Config {
        kernel: PathBuf::new(),
        initrd: None,
        mem_mib: 1024,
        cpus: 1,
        cmdline: arch::DEFAULT_CMDLINE.into(),
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
                        reveal: None,
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
    arch::setup_and_run(config)
}
