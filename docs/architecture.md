# Architecture

**English** | [한국어](i18n/ko/architecture.md) | [日本語](i18n/ja/architecture.md) | [简体中文](i18n/zh-CN/architecture.md) | [Español](i18n/es/architecture.md) | [Português (Brasil)](i18n/pt-BR/architecture.md) | [Français](i18n/fr/architecture.md) | [Deutsch](i18n/de/architecture.md) | [हिन्दी](i18n/hi/architecture.md)

```
 macOS                                          Linux guest (Alpine)
 ─────                                          ────────────────────
 docker CLI ── docker.sock ─┐                ┌─ socat ── dockerd / containerd
 vmm-up.sh  ── control.sock ┤   vsock        ├─ control.sh  (ports, clock, pressure, shutdown)
 FSEvents   ── fsnotify     ┘ ─────────────  └─ fsnotify.sh (Mac edits → inotify)
                         orbit-vmm
   vCPU threads ─ Hypervisor.framework ─ in-kernel GICv3, PSCI, PL011
   virtio-mmio:  blk  ─ data.img (ext4, flock'd: one VM per disk; discard)
                 net  ─ libslirp (NAT, DNS, runtime port forwards)
                 vsock ─ Unix sockets on the Mac
                 fs   ─ FUSE passthrough of allowed dirs under /Users
                 balloon ─ free page reporting → memory back to macOS
 launchd ── vmm-run.sh (supervisor: restart, disk guard, crash records)
         └─ healthcheck.py (hourly: checks, resource use, release notice)
```

## VMM (`vmm/`)

The VMM is one Rust process. `src/vm/` has the machine itself:

- Hypervisor.framework FFI (`hvf.rs`) and the in-kernel GIC (`gic.rs`).
- Boot and the device tree (`boot.rs`, `fdt.rs`).
- The PL011 console (`pl011.rs`).
- vCPU threads, PSCI and MMIO decoding (`vcpu.rs`).

`src/virtio/` has the virtio-mmio v2 transport and split virtqueues, plus one
file per device.

`signals.rs` handles SIGTERM and SIGINT sent to the VMM itself: it logs the
sender's pid and name to `console.log`, asks the guest to power off (30 s at
most), and exits as killed by that signal, so the supervisor still restarts it.
A second signal exits at once.

### Returning memory

Guest RAM is anonymous memory from `hv_vm_allocate`, mapped into the VM. When the guest
reports free pages (`VIRTIO_BALLOON_F_REPORTING`, queue 2), the VMM:

1. unmaps the range from the VM (`hv_vm_unmap`),
2. replaces it with fresh zero pages (`mmap(MAP_FIXED)`), so macOS drops the
   old pages from the process footprint,
3. maps the range back (`hv_vm_map`).

`madvise` alone doesn't shrink the footprint of memory that is mapped into a
VM.

The guest kernel boots with `page_reporting.page_reporting_order=2`, so the
reporting unit is 16 KiB, the macOS page size. Fragmented free memory still
gets returned.

Linux holds on to file data as page cache, which the balloon can't report.
`guest/etc/orbit/memd.sh` deals with this:

- After 60 s of light load, it reclaims half of the containers' page cache
  (cgroup `memory.reclaim`).
- After 30 s fully idle, it drops clean caches.
- Both times, it compacts memory so the freed pages form reportable blocks.
- At all times, it keeps the page cache under 1 GiB through the root cgroup's
  `memory.reclaim` (builds are never idle; the Mac caches `data.img` anyway).
- While macOS reports memory pressure, it keeps the page cache under 256 MiB
  (warn) or 64 MiB (critical) instead (`orbit.memd_warn_mb`,
  `orbit.memd_crit_mb`).
- It never squeezes below the working set. When pages the cap reclaimed are
  read straight back (`workingset_refault_file`), the cap holds at the
  cache's size and loosens by a quarter each minute without refaults.
  Before this, a Kubernetes control plane under macOS "warn" pressure lost
  and re-read about 2.7 GB a minute; now about 0.1–0.2 GB.

`pressure.rs` watches macOS memory pressure (a libdispatch memory-pressure
source, plus `kern.memorystatus_vm_pressure_level` re-read every 30 s) and
sends `pressure normal|warn|critical` on the control vsock. On a change,
`control.sh` records the level in `/run/orbit-pressure` and runs
`memd.sh reclaim <level>` right away: reclaim to the new cap (on critical,
drop all clean caches first) and compact, so free page reporting hands the
pages back before macOS has to compress them. `ORBIT_PRESSURE=off` turns it
off, and `ORBIT_PRESSURE=warn|critical` pins the level, for comparisons. It
helps with cache the guest touched once (a build's inputs, a pulled image);
a working set the guest keeps re-reading stays resident whatever the cap.
The guest is told every change; `console.log` gets at most one line per
10 minutes, with the number of changes in between.

Activity Monitor's Memory column is the process's `phys_footprint`. When the
Mac is short on memory, macOS compresses or swaps part of guest RAM, and the
footprint then grows past the guest pages the VMM actually holds (by about
a third to a half of the compressed amount in measurements; it is near zero
when nothing is compressed). To see which guest pages are held, start the VMM
with `ORBIT_MEMDUMP=<prefix>`, `touch <prefix>.req`, and match `<prefix>.bin`
(one `mincore` byte per 16 KiB page of guest RAM) against the guest's
`/proc/kpageflags` with `vmm/bench/memxref.py`.

### Data disk (virtio-blk)

`blk.rs` serves `data.img`, a sparse file, and takes an `flock` on it so two
VMs can't share one disk.

- **Discard.** Guest discards become `F_PUNCHHOLE` on the image, and `memd`
  runs `fstrim /data` after boot and every 10 minutes when the VM is quiet, so
  space Docker frees goes back to the Mac.
- **Disk guard.** Every 2 s the VMM checks free space on the Mac. Below
  `ORBIT_DISK_WARN_MIB` it logs a warning. Below `ORBIT_DISK_FLOOR_MIB`, or when
  a write fails with ENOSPC, it holds guest writes instead of failing them
  (QEMU's `werror=enospc`) for up to `ORBIT_DISK_PAUSE_SECS`, and resumes once
  space is freed, so ext4 never sees a write error. The supervisor stops the
  VM cleanly earlier, below `ORBIT_STOP_FREE_GB`.

### File sharing (virtio-fs)

`fs.rs` is a FUSE server and `hostfs.rs` handles path access.

- Every path is walked with `openat(O_NOFOLLOW)` from a directory fd, so a
  symlink can't escape the allowed directories (`--share-allow`). Directories
  above an allowed one can be traversed but not listed.
- The VMM checks the allowlist itself: entries must be real paths (no
  symlinks, exact case, not nested), and the home directory, anything above
  it, hidden directories, `~/Library`, `~/bin` and `~/Applications` are
  refused. The guest can't rename
  or remove the allowed directories themselves.
- Directory fds from earlier walks are cached for 1 s, so a path doesn't
  re-open every directory from `/Users` down. The guest's own renames and
  rmdirs drop entries at once, and expired entries are closed on the next
  insert. Missing paths are cached by the guest for 1 s
  (`ORBIT_FS_NEG_TTL_MS`).
- There are four request queues.
- Entry TTL is 5 s and attribute TTL is 500 ms. `KEEP_CACHE` and
  `AUTO_INVAL_DATA` are enabled, and so is readdirplus.
- Writeback caching is off. With it on, Mac-side edits weren't visible in the
  guest.
- Container `chown` is stored in the `user.orbit.owner` xattr and reported
  back in `stat`. Group/other permission bits mirror the owner bits, so
  non-root container users can write to your files.
- Mac edits reach the guest as inotify events. `fsevents.rs` watches the
  allowed directories with FSEvents; `forward_events` turns each batch into
  edits, creations, renames and deletions (from the Mac's current state and
  what the guest has looked up) and sends them to `fsnotify.sh` over vsock.
  The guest repeats each change under a reserved gid: a same-size `truncate`
  for an edit (`IN_MODIFY`), create/mkdir, rename or unlink/rmdir for the
  rest. The server recognises that gid, never applies any of it to the Mac,
  and lets it succeed only for paths it queued while the Mac still matches.
- The docker socket's directory (`~/.orbit/vmm/`) is shown to the guest as an
  empty, read-only mount point, and the guest links `docker.sock` there, so
  tools that bind-mount the `DOCKER_HOST` path (testcontainers' Ryuk) work.

### Networking

`slirp.rs` runs libslirp on its own thread.

- `ports.rs` forwards published ports to the Mac's `127.0.0.1`. It follows
  Docker's container events (`/events` on the VMM's own docker socket) and
  forwards a `-p` publish as soon as the container starts, including ports
  published on a specific host IP. Every 250 ms it also reads the guest's
  listening sockets over the control channel, for `--network host` services.
- Containers reach the Mac's localhost through `host.docker.internal`
  (`ORBIT_HOST_LOOPBACK=0` turns that off).
- DNS: containers ask the guest's dnsmasq, which answers
  `host.docker.internal` and forwards the rest to libslirp's resolver and from
  there to the Mac's. It allows 4096 queries in flight; dnsmasq's default of
  150 made parallel builds fail with "Temporary failure in name resolution".
- The control channel also keeps the guest clock in sync. It measures the
  round-trip time and only steps the clock when it is off by more than 250 ms.
  The clock can therefore step back: guest code must not rely on mtimes to
  tell whether something changed.
- `csrc/asyncclose.c` is a small dylib that interposes `close()` for internet
  sockets. Content-filter network extensions can make `close()` after FIN block
  for about 1 s, which would stall every connection on the network thread.

## Guest (`guest/`)

`scripts/fetch-guest.sh` downloads a pinned Alpine kernel and minirootfs
(sha256-verified) and builds an initramfs from them plus `guest/`. The
packages the first boot installs (Docker, qemu, …) are not pinned: they come
from Alpine's repositories at that time. Alpine
removes old package versions, so the weekly `guest-pins` workflow checks that
both pinned downloads still exist.

- **First boot:** installs Docker and friends onto the data disk, then takes a
  read-only root snapshot. Docker comes from Alpine v3.23 (29.5): 29.8 rebuilt
  every layer on top of a locally built base image on each warm build.
- **Later boots:** use that snapshot under a tmpfs overlay, so they take
  2–4 s.

Guest pieces:

- `dockerd.sh` writes `daemon.json` (build cache capped at 20 GB with
  `defaultMaxUsedSpace`) and starts dockerd with `orbit-runc`.
- `orbit-runc` is an OCI runtime wrapper that makes containers trust the roots
  the Mac trusts beyond Apple's set. `make guest` exports those
  (`scripts/export-host-ca.py`, usually a company TLS-inspection CA; none on
  most Macs, and none with `ORBIT_HOST_CA=0`) into `/etc/orbit/host-ca.pem`.
  - A container gets a copy of the image's CA bundle with them appended,
    bind-mounted read-only over the bundle. A bundle path that is a symlink
    (RHEL, Fedora, Amazon Linux link theirs into `ca-trust/`) is resolved
    the way the container would see it, inside its rootfs only, and the copy
    goes over the file it names.
  - In a kind node made by `orbit k8s`, the node's containerd runs pods
    through the same wrapper (mounted from the guest with `host-ca.pem`, and
    set as the runc `BinaryName`), so pods get the same treatment.
  - A build `RUN` step gets a read-write copy of the bundle's directory
    instead, so tools that regenerate the bundle by rename work. runc runs as
    a child; when it returns (BuildKit passes `--keep`, so the rootfs is still
    mounted) the directory is written back into the step's rootfs with the
    image's own bundle, only if its contents changed. The host roots never
    reach a layer.
  - A directory that contains another bundle's directory or a mount gets the
    file bind instead, and a bundle at or under an existing mount
    (`RUN --mount`, `-v`) is left alone.
- `dns.sh` runs dnsmasq (see Networking).
- `binfmt.sh` registers qemu-x86_64.
- `memd.sh`: see above.

## On the Mac (`scripts/`)

- `make install` copies the VMM, its `close()` interposer, `vmm-up.sh`,
  `vmm-run.sh` and `healthcheck.py` to `~/.orbit/bin`, writes
  `~/.orbit/bin/VERSION`, and installs the launchd watchdog. The VM only ever
  runs from that copy: containers can write to the checkout.
- `vmm-run.sh` supervises the VM, with or without launchd. It loads the
  settings `vmm-up.sh` saved in `config.env` and exports them, since launchd
  starts it without the user's environment.
  - It detects a crash, a guest reboot or panic, or a hang (three missed
    control-socket pings; the VMM is aborted first so macOS writes a crash
    report). Only under launchd (the watchdog) does the VM then start again;
    without it, the supervisor records the stop and exits.
  - It stops restarting after more than three starts in 10 minutes.
  - It keeps a record of each unexpected stop in `crashes/<time>/`.
  - It warns below `ORBIT_WARN_FREE_GB` and stops the VM cleanly below
    `ORBIT_STOP_FREE_GB`.
- `healthcheck.py`, if installed, checks Docker, ports, bind mounts, inotify,
  the clock and disk space, records the VMM's memory, threads and fds, and once
  a day reads the latest release from the `releases/latest` page and shows one
  notification per newer release.
- `orbit` is the installed command: `up` and `down` run `vmm-up.sh`, `update`
  and `rollback` run `update.sh`, `status` reports, and `uninstall` removes the
  agents, the health check's scratch directories and `~/.orbit`. The
  Makefile's run targets call it, so nothing in the checkout runs.
  `orbit k8s up|down|status` creates and deletes kind clusters with the
  node configured as above; `uninstall` deletes them too, which also drops
  their kubectl contexts.
- `update.sh` (`orbit update`, `orbit rollback`) builds a release tag in its own
  clone (`~/.orbit/src`) of the repository recorded in `~/.orbit/bin/VERSION`,
  and the new guest in a staging directory, keeps the
  previous binaries and guest for rollback, and restarts the VM only when no
  containers run.
