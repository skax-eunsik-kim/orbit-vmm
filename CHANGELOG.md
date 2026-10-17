# Changelog

All notable changes are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Security

- Run the VMM, `vmm-up.sh` and the health check from an installed copy in
  `~/.orbit/bin` (`make install`), not from the checkout. Containers can
  write to the checkout.
- `vmm-up.sh` refuses to share the home directory or any directory above it.
  It also creates `~/.orbit` with mode 0700, and sockets are created 0600
  from the start.
- The health check uses directory fds and `O_NOFOLLOW`, so a guest-planted
  symlink can't redirect its writes.
- The VMM bounds what the guest can make it hold:
  - vsock control replies
  - control-channel replies (64 KiB, 3 s)
  - published ports (1024)
  - open virtio-fs handles, with the fd limit raised at start
- virtio-blk sector math saturates instead of overflowing.
- `export-host-ca.py` exports only CA certificates trusted for TLS, so Wi-Fi,
  S/MIME and leaf certificates are no longer exported.
- New `ORBIT_HOST_LOOPBACK=0` (`--no-host-loopback`) cuts containers off
  from the Mac's localhost.
- CI actions are pinned to commit SHAs.

## [0.1.0] - 2026-10-17

First public release.

- arm64 VMM on Hypervisor.framework with an in-kernel GICv3, PSCI and a PL011
  console.
- virtio-mmio devices:
  - blk, with a single-instance disk lock
  - net (libslirp), with automatic port publishing
  - vsock
  - virtio-fs, with an allowlist, symlink-safe walks, ownership emulation and
    inotify for Mac edits
  - balloon, with free page reporting
- Alpine v3.24 guest (pinned, sha256-verified):
  - read-only root snapshot
  - memd (cache trimming and compaction)
  - `host.docker.internal`
  - host CA trust in containers
  - amd64 via binfmt/qemu
  - clock sync
- Benchmarks: developer workflow (21 checks), memory, fragmentation,
  multi-worktree, soak.
- Optional launchd health check with an adaptive 1 h → 2 h → 4 h interval.
