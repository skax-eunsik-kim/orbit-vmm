# Changelog

All notable changes are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Fixed

- A full Mac disk no longer corrupts the guest. The VMM warns in
  `console.log` when the Mac's free space drops below 5 GiB (or 2%), holds
  guest disk writes below 1 GiB or on ENOSPC (for up to 10 minutes) instead
  of failing them, and resumes once space is freed. `vmm-up.sh` refuses to
  start that low and prints the data disk's real and logical size;
  `healthcheck.py` checks free space and no longer counts informational
  `orbit-vmm:` lines as errors.

### Performance

- The guest installs Docker 29.5 (Alpine v3.23) instead of 29.8 (v3.24).
  Docker 29.8 rebuilt every layer on top of a locally built base image on
  each warm build, because provenance changes the base image's digest.
  On a sample monorepo the second warm build took 347 s on 29.8 and 14 s on
  OrbStack.
- `ORBIT_CPUS` defaults to the Mac's core count instead of 4. Idle vCPUs
  park, and with 4 vCPUs parallel container start-up queued on CPU.
- Published ports are picked up every 250 ms instead of every second.
- The VMM follows Docker's container events and forwards a `-p` publish as
  soon as the container starts, including ports published on a specific host
  IP (`-p 127.0.0.1:8080:80`), which were never forwarded before. The runc
  wrapper that adds the Mac's CAs forks fewer processes (10 ms → 7 ms per create).
- `memd` keeps the guest page cache under 1 GiB at all times
  (`orbit.memd_cache_mb`, 0 turns it off), not only once the VM goes idle.
  During a monorepo build the cache had grown to 7 GB and the VM to 15 GB on
  the Mac; with the cap the build peaked at 7.7 GB on the Mac and took no
  longer (337 s vs 358 s). The Mac caches `data.img` itself, so the guest
  copy was redundant.
- The data disk supports discard (`F_PUNCHHOLE` on the image), and `memd`
  runs `fstrim /data` once after boot and every 10 minutes when the VM is
  quiet (`orbit.memd_trim`, 0 turns it off). Space Docker frees (pruned
  images, build cache) now goes back to the Mac: a 35 GB image with 13 GB of
  pruned build cache shrank to 16 GB, and the data stayed intact.
- The VMM passes macOS memory pressure on to the guest (`pressure.rs`,
  `pressure <level>` on the control vsock). On warn `memd` caps the page
  cache at 256 MiB instead of 1 GiB, on critical at 64 MiB after dropping all
  clean caches, and reclaims right away, so macOS has less guest RAM to
  compress. With the Mac at warn, 30 s after writing and reading a 2 GB file
  the VM took 911 MB on the Mac instead of 1866 MB (1579 MiB of it
  compressed or swapped, now 481 MiB). A working set that is re-read
  constantly still stays resident. `ORBIT_PRESSURE=off` turns it off.
- `ORBIT_GUEST` picks the kernel/initramfs directory, so a second instance
  can boot a guest built elsewhere.

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
- The guest can no longer rename, remove or replace the allowed directories
  themselves. Before, it could swap one for a symlink and get the link's
  target shared on the next start.
- The VMM checks allowlist entries itself:
  - They must be real paths, with no symlinks and exact case.
  - Your home directory, anything above it, hidden directories and
    `~/Library` are refused.
- `make up` only runs the installed copy and builds nothing.
- More bounds on what the guest can make the VMM hold:
  - the directory listing cache (512 MiB in total, 2M entries per directory)
  - the node table (4M entries)
  - the guest-to-network frame queue (4096 frames)
  - console output (256 MiB)
- CI uses a pinned Rust toolchain.
- Nested allowed directories are refused by the VMM; `vmm-up.sh` keeps only
  the outermost. The guest can't rename paths above an allowed directory,
  in any letter case.
- Guest paths below a share are capped at 4096 bytes, and node paths at
  256 MiB in total.
- RENAME_EXCHANGE swaps node paths directly.
- The "port in use" warning is printed once per port.
- The balloon range uses checked math.
- The VMM resolves allowlist entries itself and refuses any entry that
  resolves through a symlink inside a shared directory. A container could
  repoint such a link to widen what gets shared on the next start.
- Renames are checked against the path-length and node-memory limits for
  every remembered path they move.
- The symlink check compares real paths, so Unicode normalisation (NFC vs
  NFD) and firmlink spellings can't slip past it.
- READDIR reply size is capped.
- Container `chmod` and create modes no longer reach the Mac file as given.
  The Mac file never gets setuid/setgid or group/other write, and the guest
  still sees the mode it set (`user.orbit.mode` xattr). Allowed directories
  keep their Mac mode. Before, a container could hand other local users a
  setuid binary or world-writable project files.
- The VMM refuses a share that exposes the home directory when there is no
  allowlist, and checks the `/System/Volumes/Data` spelling of home too.
- The health check has an overall deadline on control-socket replies and
  needs Python 3.11+.
- Mac files keep owner read/write (rwx for directories), so the xattrs that
  hold the guest's view stay readable. A guest `chmod 000` can be undone, and
  write-only files can be created.
- A failed CREATE no longer leaks its host fd. `O_CREAT` opens count as
  writes.
- `~/bin` and `~/Applications` can't be shared.
- CREATE sets the guest's owner and mode only on files it really created,
  and it can't target an allowed directory. OPEN never creates. The
  temporary owner unlock in SETATTR only ever applies safe bits.

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
