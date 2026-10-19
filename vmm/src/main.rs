mod devices;
mod fsevents;
mod memdump;
mod memory;
mod ports;
mod pressure;
mod reclaim;
mod slirp;
mod virtio;
mod vm; // hvf bindings, GIC, vCPU loop, PSCI, FDT, PL011, boot

use std::path::{Path, PathBuf};

/// Command line:
///   orbit-vmm --kernel <Image> [--initrd <cpio.gz>] [--mem <MiB>] [--cpus <n>]
///             [--cmdline "<kernel args>"] [--disk <raw.img>] [--balloon]
///             [--net] [--no-host-loopback] [--fwd tcp:<host port>:<guest port>]...
///             [--vsock <unix socket path>:<guest port>]...
///             [--share <host dir>:<virtiofs tag>]... [--share-allow <dir>]...
///
/// Exit status: 0 when the guest powers off, 3 when it reboots (or its kernel
/// panics), 1 on a VMM error, 2 for bad arguments. Signals as usual.
pub struct Config {
    pub kernel: PathBuf,
    pub initrd: Option<PathBuf>,
    pub mem_mib: u64,
    pub cpus: u32,
    pub cmdline: String,
    pub disk: Option<PathBuf>,
    pub balloon: bool,
    pub net: bool,
    /// Let the guest reach the Mac's 127.0.0.1 through the gateway (host.docker.internal).
    pub host_loopback: bool,
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
        let net = virtio::net::Net::new(config.forwards.clone(), config.host_loopback);
        // Auto-publish guest ports when the guest control channel is available.
        if let Some(ctl) = config.vsock.iter().find(|v| v.guest_port == 1025) {
            let fixed = config.forwards.iter().map(|f| f.host_port).collect();
            let docker = config.vsock.iter().find(|v| v.guest_port == 2375).map(|v| v.path.clone());
            ports::spawn(ctl.path.clone(), docker, net.port_control(), fixed);
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
    let reveal = docker_socket(config).and_then(|s| s.parent().map(Path::to_path_buf));
    for share in &config.shares {
        let share = virtio::fs::Share {
            notify: notify.clone(),
            allow: config.share_allow.clone(),
            reveal: reveal.clone(),
            ..share.clone()
        };
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

/// The Mac-side docker socket (the vsock listener for the guest's dockerd).
/// Clients that bind-mount DOCKER_HOST's path into a container (testcontainers'
/// Ryuk) ask the daemon for exactly this path, so the guest's boot.sh puts a
/// link to its own socket there (orbit.dockersock= on the kernel command line).
fn docker_socket(config: &Config) -> Option<PathBuf> {
    let s = &config.vsock.iter().find(|v| v.guest_port == 2375)?.path;
    let s = s.parent()?.canonicalize().ok()?.join(s.file_name()?);
    s.to_str().is_some_and(|t| !t.contains(char::is_whitespace)).then_some(s)
}

/// macOS starts processes with a soft limit of 256 fds; every file the guest
/// holds open on a share is one host fd. Raise it to the hard limit (capped).
fn raise_fd_limit() {
    let mut rl = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut rl) } == 0 {
        rl.rlim_cur = rl.rlim_max.min(65536);
        unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &rl) };
    }
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--reclaim-selftest") {
        std::process::exit(reclaim::selftest());
    }
    // `orbit-vmm --control <socket> <command>`: one bounded request on the guest
    // control channel (64 KiB, 3 s), for scripts. The guest is untrusted, so
    // they must not read its replies with plain nc.
    if std::env::args().nth(1).as_deref() == Some("--control") {
        let a: Vec<String> = std::env::args().skip(2).collect();
        let [sock, cmd] = a.as_slice() else {
            eprintln!("usage: orbit-vmm --control <socket> <command>");
            std::process::exit(2);
        };
        match ports::ask(&PathBuf::from(sock), &format!("{cmd}\n")) {
            Some(reply) => {
                print!("{reply}");
                std::process::exit(0);
            }
            None => std::process::exit(1),
        }
    }
    raise_fd_limit();
    let mut config = vm::parse_args(std::env::args().skip(1));
    if let Some(s) = docker_socket(&config) {
        config.cmdline.push_str(&format!(" orbit.dockersock={}", s.display()));
    }
    std::process::exit(vm::run(config));
}
