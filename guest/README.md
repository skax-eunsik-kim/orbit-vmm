# guest

Files baked into the Linux guest's initramfs by `scripts/fetch-guest.sh`
(`make guest`). A change here reaches a VM only after `make guest` and a
restart. Scripts run under busybox `ash`: keep them POSIX.

| Path | Role |
|---|---|
| [`init`](init) | Stage 1, PID 1 in the initramfs: picks the root (the saved snapshot on the data disk, or the initramfs on first boot) and hands over to busybox init. |
| [`etc/inittab`](etc/inittab) | What busybox init starts and respawns. |
| [`etc/orbit/boot.sh`](etc/orbit/boot.sh) | Stage 2: mounts, kernel modules, network, data disk, packages (Docker from Alpine v3.23 on first boot). |
| [`etc/orbit/snapshot.sh`](etc/orbit/snapshot.sh) | Saves the installed root to the data disk, so later boots take 2–4 s. |
| [`etc/orbit/udhcpc.sh`](etc/orbit/udhcpc.sh) | DHCP hook: address, route and upstream DNS. |
| [`etc/orbit/dns.sh`](etc/orbit/dns.sh) | dnsmasq for the guest and containers (`host.docker.internal`, 4096 queries in flight). |
| [`etc/orbit/dockerd.sh`](etc/orbit/dockerd.sh) | Writes `daemon.json` and runs dockerd with `orbit-runc`. |
| [`etc/orbit/orbit-runc`](etc/orbit/orbit-runc) | OCI runtime wrapper that adds the Mac's extra root certificates to containers and build steps without writing them into images. |
| [`etc/orbit/bin/docker-proxy`](etc/orbit/bin/docker-proxy) | Wrapper dockerd runs for each published port, so ports published on `127.0.0.1` answer the VMM's forwards. |
| [`etc/orbit/control.sh`](etc/orbit/control.sh) | One command per host connection on vsock port 1025: ports, clock, memory pressure, shutdown. |
| [`etc/orbit/fsnotify.sh`](etc/orbit/fsnotify.sh) | Turns Mac file changes into inotify events in the guest. |
| [`etc/orbit/memd.sh`](etc/orbit/memd.sh) | Hands memory back: page-cache caps, reclaim on pressure, compaction, periodic `fstrim`. |
| [`etc/orbit/binfmt.sh`](etc/orbit/binfmt.sh) | Registers qemu-x86_64 for `linux/amd64` images. |
| [`etc/orbit/shutdown.sh`](etc/orbit/shutdown.sh) | Clean shutdown: stop dockerd, sync, unmount the data disk and shares. |

How the pieces fit: [docs/architecture.md](../docs/architecture.md#guest-guest).
