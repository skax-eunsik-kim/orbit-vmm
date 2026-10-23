# vmm

The VMM: one Rust binary, `orbit-vmm`, on Hypervisor.framework. `make build`
(from the repository root) builds and signs `target/release/orbit-vmm` with
[`hvf.entitlements`](hvf.entitlements). How it works:
[docs/architecture.md](../docs/architecture.md).

| Path | What |
|---|---|
| [`src/main.rs`](src/main.rs) | Command line, start-up, the control channel client (`--control`), self-tests. |
| [`src/vm/`](src/vm) | The machine: Hypervisor.framework FFI, in-kernel GICv3, boot and device tree, PL011 console, vCPU threads and PSCI. |
| [`src/virtio/`](src/virtio) | virtio-mmio transport and devices: `blk` (data disk, discard, disk guard), `net`, `vsock`, `fs` + `hostfs` (shared directories), `balloon` (free page reporting). |
| [`src/slirp.rs`](src/slirp.rs) | libslirp on its own thread: NAT, DNS, port forwards. |
| [`src/ports.rs`](src/ports.rs) | Published ports: Docker events plus a 250 ms poll, forwarded to the Mac's `127.0.0.1`. |
| [`src/fsevents.rs`](src/fsevents.rs) | FSEvents on shared directories, turned into inotify events in the guest. |
| [`src/pressure.rs`](src/pressure.rs) | macOS memory pressure, passed on to the guest. |
| [`src/reclaim.rs`](src/reclaim.rs), [`src/memory.rs`](src/memory.rs) | Guest RAM, and handing reported pages back to macOS. |
| [`src/memdump.rs`](src/memdump.rs) | `ORBIT_MEMDUMP`: which guest pages the Mac still holds. |
| [`src/signals.rs`](src/signals.rs) | SIGTERM / SIGINT: log the sender, power the guest off cleanly. |
| [`src/devices.rs`](src/devices.rs) | Wiring the devices into the machine. |
| [`csrc/asyncclose.c`](csrc/asyncclose.c) | A `close()` interposer: moves closing internet sockets off libslirp's thread, so content-filter extensions can't stall networking. |
| [`guest/`](guest) | Tiny test initramfs images (`init-*`) that boot one device each (`make -C vmm ~/.orbit/vmm/<name>.cpio.gz`). |
| [`bench/`](bench) | VMM-level checks: `fssec.sh` (shared-directory security, 23 checks), `memtrace.sh`, `memxref.py`, `vsock-echo.py`. |

`make lint test` runs `cargo fmt --check`, `clippy -D warnings`, `cargo test`
and shellcheck. Dependencies are `libc`, `vm-fdt` and libslirp only.
