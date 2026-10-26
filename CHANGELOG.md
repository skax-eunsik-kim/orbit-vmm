# Changelog

All notable changes are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Security

- The CA wrapper's build-step write-back (`orbit-runc`) can no longer reach
  guest paths outside the build step when an image, or the step itself, makes
  a parent of a CA directory a symlink: such directories are not bound, and a
  directory swapped during the step is not written back (#24).
- Mac roots no longer end up in an image layer when a build step appends to or
  edits the CA bundle, or copies it within the directory: they are removed
  from every file before the write-back (#24).
- The write-back replaces the directory only after a complete copy, and a
  failed write-back fails the build step instead of committing an empty or
  partial directory (#24).
- The built initramfs, which holds the Mac's extra roots, is readable only by
  its owner. The host roots are no longer appended to the guest bundle twice
  when booting from the root snapshot.

### Changed

- `vmm-up.sh` waits up to 5 minutes, not 2, for Docker on the first boot,
  which installs packages and can be slow on a slow link; when it gives up it
  says the VM keeps booting (#23).
- An installed `orbit` command (`~/.orbit/bin/orbit`) runs orbit: `orbit up`,
  `down`, `status`, `update`, `rollback` and `uninstall`. `make up`,
  `make down`, `make update` and `make rollback` now only call it, so nothing
  in a checkout runs when you operate orbit. Before, `make update` ran the
  checkout's `update.sh`, and a checkout in a shared directory can be changed
  by containers (#22).
- `orbit update` clones and fetches from the repository recorded at install
  time, not from the checkout's `origin` (#22).
- `make install` warns when the checkout is inside a shared directory, and the
  README's quick start clones to `~/.local/src`, outside them (#22).

### Added

- `orbit uninstall`: stops every orbit VM and removes the launchd agents, the
  health check's scratch directories and `~/.orbit` (data disks included),
  after asking (#22).
- `AGENTS.md` (and a `CLAUDE.md` that imports it): rules for coding agents on
  verifying changes in an isolated instance, known traps, docs and releases.
- README in Japanese, Simplified Chinese, Spanish, Brazilian Portuguese,
  French, German and Hindi, next to English and Korean, with one language
  selector on all of them (`scripts/readme-langs.py`). CI checks the selector
  and warns when a PR changes `README.md` but leaves a translation behind.

### Fixed

- Settings saved for the watchdog that the VMM itself reads
  (`ORBIT_PRESSURE`, `ORBIT_DISK_FLOOR_MIB`, `ORBIT_DISK_PAUSE_SECS`) were
  ignored when launchd started the VM: `vmm-run.sh` loaded `config.env`
  without exporting it. `ORBIT_FS_OWNERSHIP`, `ORBIT_FS_TTL` and
  `ORBIT_FS_ATTR_TTL_MS` are now saved too (#23).

### Fixed

- A first boot whose package install failed (offline, behind a proxy) no
  longer saves a root snapshot without Docker that every later boot reused.
  It saves no snapshot and retries next boot, and stage 1 ignores an existing
  snapshot without `dockerd`, so Macs already stuck recover on their own (#21).
- A VM that was running at logout or shutdown starts again at login. The
  supervisor treated launchd's SIGTERM as "stop for good" (#21).
- Containers get time to stop on `make up`, `make down` and updates. The guest
  gave dockerd 10 s before killing everything; now dockerd has
  `shutdown-timeout` 30 s and 45 s in total, and every host-side wait is 75 s
  (launchd: 90 s) (#21).

### Docs

- README: complete prerequisites (Xcode Command Line Tools, Homebrew,
  docker-buildx and the `cliPluginsExtraDirs` setting, Python 3.11+, free
  disk space), only macOS 26.4 tested so far, every user-facing variable,
  no explicit-proxy support, unpinned guest packages, moving from Docker
  Desktop, how to report a bug, and the "not affiliated with Docker, Inc.,
  OrbStack or Apple" sentence again. Translations updated to match (#23).
- Corrected: the disk guard checks every 2 s, not 5 s; only the watchdog
  restarts a crashed VM; the share refusal list includes `~/bin` and
  `~/Applications`; which benchmark scripts take a Docker host; SECURITY.md
  supports the latest release, not `main` (#23).
- `docs/architecture.md` describes the current design again: the data disk
  (discard, disk guard), signals, the directory-fd cache, Docker-event port
  forwarding, DNS, the CA wrapper's build-step handling, and the Mac-side
  supervisor, health check and updates.
- Architecture and benchmarks are translated into the same eight languages as
  the README. All translations moved from the repository root to
  `docs/i18n/<lang>/`, with one language selector on every document
  (`scripts/readme-langs.py`).
- Each top-level directory (`bench/`, `docs/`, `guest/`, `scripts/`, `vmm/`)
  has a README listing what is in it.
- CI checks every relative link and in-page anchor in the Markdown
  (`scripts/check-doc-links.py`).

## [0.2.1] - 2026-10-23

### Fixed

- The health check's release notice works behind a shared company egress
  address. It used the unauthenticated GitHub REST API, whose 60 calls an
  hour per IP were already used up there (403); it now reads the tag from the
  `releases/latest` page redirect.

## [0.2.0] - 2026-10-23

### Added

- `make update` installs the latest release (or `TO=<tag>`), building it in
  `~/.orbit/src` and the new guest in a staging directory first, so a failed
  update changes nothing. `make rollback` swaps back. The VM restarts only
  with no containers running, or with `NOW=1`. The health check shows one
  notification per newer release (daily lookup). `~/.orbit/bin/VERSION`
  records what is installed (#9).
- `make install` installs the watchdog too (`ORBIT_WATCHDOG=0` skips it).
  Running it again leaves a running VM alone (it used to shut the VM down);
  `install-watchdog.sh --reload` applies a changed agent now (#8).
- `README.ko.md`, and a language selector on top of both READMEs (#8).
- README: what to check on a macOS version orbit hasn't been tested on
  (`--reclaim-selftest`, `make devflow`, how Activity Monitor's number
  relates to what the guest uses).

- Watchdog: `scripts/install-watchdog.sh` installs a launchd agent that
  restarts the VM after a crash, a guest reboot or panic, or a hang. A hang
  is three missed control-socket pings; the VMM is aborted first so macOS
  writes a crash report.
  - It stops restarting after more than three starts in 10 minutes.
  - Each unexpected stop is recorded in `crashes/<time>/` with a
    notification.
- `scripts/vmm-run.sh` supervises the VM in both modes. `vmm-up.sh --stop`
  (`make down`) stops it on purpose.
- The VMM exits with status 3 when the guest asks for a reset (reboot or
  kernel panic) instead of 0.
- `console.log` from the previous run is kept as `console.prev.log`.

### Fixed

- `make guest` works again on a Mac without a cached kernel: Alpine removed
  the pinned `linux-virt-6.18.56-r0` from its mirrors (404). Pinned to
  6.18.58-r0. A weekly CI job now checks that both pinned downloads still
  exist.
- Parallel builds no longer fail with `Temporary failure in name resolution`.
  The guest's dnsmasq let at most 150 uncached queries be in flight and
  answered every extra one with SERVFAIL. With 200 parallel lookups of new
  names, 708 of 800 failed; now none do (#10). TLS and TCP timeouts seen in
  the same builds also happened on the Mac itself: they come from the network,
  not orbit.
- The health check no longer reports UNHEALTHY all day for fd count. The
  directory-fd cache kept expired fds open until it reached 1024 entries; it
  now closes them on the next insert (#8).
- `console.log` no longer repeats `macOS memory pressure is warn/normal`
  dozens of times: a change is logged at most every 10 minutes, with the
  number of changes in between. The guest is still told every time (#8).
- Build steps on images with CA bundles in nested directories (both
  `/etc/ssl/certs/ca-certificates.crt` and `/etc/ssl/ca-bundle.pem`) see the
  Mac's roots in both again (a regression from #5).
- A mount on the CA directory (`RUN --mount=...,target=/etc/ssl/certs`, `-v`)
  is left alone. It used to be hidden by orbit's copy, or the step failed
  with `make mountpoint ... read-only file system`.
- `ORBIT_HOST_CA=0 make guest` builds a guest that adds no root certificates.
  README now explains what is added and where it comes from.
- Build `RUN` steps that regenerate the CA bundle work again (#5).
  - `update-ca-certificates`, or an `apt-get upgrade` that updates
    `ca-certificates`, used to fail with `mv: ... Device or resource busy`.
    The cause was the bind mount that adds the Mac's roots: it made the bundle
    file a mount point.
  - Build steps now get a read-write copy of the bundle's directory. The
    step's changes are written back into the layer afterwards. The Mac's roots
    are never written back.
  - Running containers keep the file mount. Regenerating the bundle inside a
    running container still fails.
  - Within one `RUN`, commands after the regeneration no longer see the Mac's
    roots. Under TLS inspection, put HTTPS downloads in a separate `RUN`.

- A SIGTERM or SIGINT sent to `orbit-vmm` itself no longer kills the VM on the
  spot. The VMM logs the sender's pid and name to `console.log`, powers the
  guest off cleanly (30 s at most), then exits as killed by that signal so the
  supervisor still restarts it. A second signal exits at once. A VM had been
  lost to a stray SIGTERM whose sender could not be found.
- `healthcheck.py` checks free space and no longer counts informational
  `orbit-vmm:` lines as errors.
- Files and directories created, renamed or deleted on the Mac now raise
  `IN_CREATE`, `IN_MOVED_FROM`/`IN_MOVED_TO` and `IN_DELETE` in containers,
  including files inside new subdirectories, so hot reload survives
  `git checkout` and code generators. The guest agent repeats the change
  itself under the reserved gid; the VMM answers those requests without
  touching the Mac, and only for paths it queued while the Mac still matches.
- testcontainers works without `TESTCONTAINERS_DOCKER_SOCKET_OVERRIDE`. The
  VMM shows the docker socket's directory (`~/.orbit/vmm/`) to the guest as
  an empty, read-only mount point, and the guest links `docker.sock` there to
  its own socket, so Ryuk's bind mount of the `DOCKER_HOST` path works.

### Performance

A metadata-heavy bind-mount workload is 10–20x faster:

| Operation | Before | After |
|---|---|---|
| Create 5000 files | 46–83 s | 3.8 s |
| stat 5000 files | 9–10 s | 0.6 s |
| 1000 renames | 18–25 s | 1.7 s |

- virtio-fs caches directory fds from its O_NOFOLLOW walks (1 s lifetime;
  dropped at once on the guest's renames and rmdirs). Before, every
  operation re-opened every directory from `/Users` down.
- Missing paths are cached by the guest for 1 s (`ORBIT_FS_NEG_TTL_MS`).
- Ownership and mode xattrs are read through one open.
- Renames touch only the moved subtree (ordered node map), not every node.
- The echo-suppression map is pruned at most once a second.
- IRQ lines change only on transitions (virtio-mmio, PL011).
- The net RX path copies slices.
- Debug environment variables are read once.

- The guest installs Docker 29.5 (Alpine v3.23) instead of 29.8 (v3.24).
  Docker 29.8 rebuilt every layer on top of a locally built base image on
  each warm build, because provenance changes the base image's digest.
  On a sample monorepo the second warm build went from 347 s to 30 s
  (OrbStack: 14 s).
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

### Disk

- The build cache cap works again. Docker 28+ reads `defaultKeepStorage`
  as a reserved floor, not a cap, so the cache grew without limit (33 GB
  seen). daemon.json now sets `defaultMaxUsedSpace` 20 GB,
  `defaultMinFreeSpace` 10 GB and `defaultReservedSpace` 2 GB.
- Disk guard in the supervisor:
  - Below `ORBIT_WARN_FREE_GB` (10) free on the Mac you get a notification.
  - Below `ORBIT_STOP_FREE_GB` (3) the guest shuts down cleanly and stays
    down. A full Mac disk corrupted a guest's ext4 in testing.
  - The VM doesn't start below that threshold either.
- Disk guard in the VMM, a last resort under the supervisor's: on ENOSPC or
  below `ORBIT_DISK_FLOOR_MIB` (1 GiB) free it holds guest disk writes (for up
  to 10 minutes) instead of failing them, and resumes once space is freed, so
  ext4 never sees write errors. It logs to `console.log` at the supervisor's
  warning level (`ORBIT_WARN_FREE_GB`). `vmm-up.sh` prints the data disk's
  real and logical size.

### Security

- The supervisor and `vmm-up.sh` talk to the guest control socket only
  through `orbit-vmm --control`, which is bounded in size (64 KiB) and time
  (3 s), instead of `nc`. A guest could stall the watchdog or stream
  unbounded data into it.
- One supervisor per instance. `make down` can't race a restart.
- `vmm-up.sh` prints warnings, such as running the VMM from a shared
  directory, to the terminal again.

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
