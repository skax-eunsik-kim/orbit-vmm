mod devices;
mod fsevents;
mod memory;
mod ports;
mod reclaim;
mod slirp;
mod virtio;
mod vm; // agent A: hvf bindings, GIC, vCPU loop, PSCI, FDT, PL011, boot

use std::path::PathBuf;

/// Command line contract (agent A implements parsing details, keep these flags):
///   orbit-vmm --kernel <Image> [--initrd <cpio.gz>] [--mem <MiB>] [--cpus <n>]
///             [--cmdline "<kernel args>"] [--disk <raw.img>] [--balloon]
///             [--net] [--fwd tcp:<host port>:<guest port>]...
///             [--vsock <unix socket path>:<guest port>]...
///             [--share <host dir>:<virtiofs tag>]... [--share-allow <dir>]...
pub struct Config {
    pub kernel: PathBuf,
    pub initrd: Option<PathBuf>,
    pub mem_mib: u64,
    pub cpus: u32,
    pub cmdline: String,
    pub disk: Option<PathBuf>,
    pub balloon: bool,
    pub net: bool,
    pub forwards: Vec<slirp::Forward>,
    pub vsock: Vec<virtio::vsock::VsockListen>,
    pub shares: Vec<virtio::fs::Share>,
    pub share_allow: Vec<PathBuf>,
}

/// virtio devices to attach, in slot order (slot i = virtio-mmio window i).
fn virtio_devices(config: &Config, mem: &std::sync::Arc<memory::GuestMemory>) -> Vec<Box<dyn virtio::VirtioDevice>> {
    let _ = mem;
    let mut devices: Vec<Box<dyn virtio::VirtioDevice>> = Vec::new();
    // The disk goes first: its exclusive flock is what stops a second VMM on
    // the same instance, and it must win before any socket below is replaced.
    if let Some(path) = &config.disk {
        match virtio::blk::Block::open(path) {
            Ok(disk) => devices.push(Box::new(disk)),
            Err(e) => {
                eprintln!("orbit-vmm: {e}");
                std::process::exit(1);
            }
        }
    }
    if config.net {
        let net = virtio::net::Net::new(config.forwards.clone());
        // Auto-publish guest ports when the guest control channel is available.
        if let Some(ctl) = config.vsock.iter().find(|v| v.guest_port == 1025) {
            let fixed = config.forwards.iter().map(|f| f.host_port).collect();
            ports::spawn(ctl.path.clone(), net.port_control(), fixed);
            ports::spawn_time_sync(ctl.path.clone());
        }
        devices.push(Box::new(net));
    }
    if !config.vsock.is_empty() {
        match virtio::vsock::Vsock::new(config.vsock.clone()) {
            Ok(v) => devices.push(Box::new(v)),
            Err(e) => {
                eprintln!("orbit-vmm: vsock: {e}");
                std::process::exit(1);
            }
        }
    }
    let notify = config.vsock.iter().find(|v| v.guest_port == 1026).map(|v| v.path.clone());
    for share in &config.shares {
        let share = virtio::fs::Share { notify: notify.clone(), allow: config.share_allow.clone(), ..share.clone() };
        match virtio::fs::Fs::new(share) {
            Ok(fs) => devices.push(Box::new(fs)),
            Err(e) => {
                eprintln!("orbit-vmm: virtiofs: {e}");
                std::process::exit(1);
            }
        }
    }
    if config.balloon {
        devices.push(Box::new(virtio::balloon::Balloon::new()));
    }
    devices
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--reclaim-selftest") {
        std::process::exit(reclaim::selftest());
    }
    let config = vm::parse_args(std::env::args().skip(1));
    std::process::exit(vm::run(config));
}
