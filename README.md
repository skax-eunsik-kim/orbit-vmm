# orbit-vmm

**English** | [한국어](docs/i18n/ko/README.md) | [日本語](docs/i18n/ja/README.md) | [简体中文](docs/i18n/zh-CN/README.md) | [Español](docs/i18n/es/README.md) | [Português (Brasil)](docs/i18n/pt-BR/README.md) | [Français](docs/i18n/fr/README.md) | [Deutsch](docs/i18n/de/README.md) | [हिन्दी](docs/i18n/hi/README.md)

[![CI](https://github.com/skax-eunsik-kim/orbit-vmm/actions/workflows/ci.yml/badge.svg)](https://github.com/skax-eunsik-kim/orbit-vmm/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

A memory-frugal Docker engine for Apple Silicon Macs.

orbit-vmm is a small virtual machine monitor written in Rust on Apple's
Hypervisor.framework. It boots an Alpine Linux guest that runs Docker, and the
`docker` / `docker compose` CLI on your Mac talks to it through a Unix socket.

Most Docker VMs on macOS grow to their peak memory use and stay there.
orbit-vmm hands memory back. The guest reports the pages it frees
(virtio-balloon *free page reporting*), and the VMM releases them to macOS
within seconds. A small guest daemon also drops idle page cache and compacts
memory (defragments it, so whole free blocks can be reported), so freed memory
can actually be returned.

> **Status: early.** It runs real compose stacks every day, but it is young,
> has one maintainer, and has no installer or GUI. Read
> [Known limitations](#known-limitations) before you depend on it.

## Features

- **Memory goes back to macOS.** Free page reporting at 16 KiB granularity,
  plus idle page cache trimming. After a workload exits, the VM's footprint on
  the Mac drops back to about 1 GB within a minute
  ([benchmarks](docs/benchmarks.md)).
- **Bind mounts over virtio-fs.** `-v $PWD:/app` works at the same path as on
  the Mac. It has multiple request queues, attribute caching and readdirplus.
  Mac edits, new files and directories, renames and deletions fire inotify
  events in containers, so `uvicorn --reload`, vite and nodemon hot reload
  keeps working through `git checkout` and code generators. `chown` and `chmod` inside a container work too.
  Both are emulated with xattrs. The real file stays yours and never becomes
  setuid or group- or world-writable on the Mac.
- **Opt-in sharing.** Only allowed directories are visible to the VM, and
  path walks are symlink-safe. A container with the Docker socket can't read
  `~/.ssh`.
- **Networking that behaves like Docker Desktop:**
  - Published ports appear on `127.0.0.1` automatically.
  - `host.docker.internal` and `host-gateway` resolve to the Mac.
  - Containers trust the root CAs your Mac trusts, so `pip`, `npm`, `apk` and
    `curl` work behind TLS-inspecting proxies.
- **amd64 images** run via qemu user emulation (binfmt).
- **Several VMs side by side** with `ORBIT_INSTANCE=<name>`.
- **Small and auditable.** One Rust binary of about 6.3k lines of code. It depends on
  `libc`, `vm-fdt` and libslirp, and nothing else.

## Requirements

- Apple Silicon Mac, macOS 15 or later (the in-kernel GIC API needs 15+).
  Only macOS 26.4 has been tested so far; on another version, run the checks
  below first.
- Xcode Command Line Tools (`xcode-select --install`) and
  [Homebrew](https://brew.sh).
- Rust 1.87+ and libslirp: `brew install rust libslirp`.
- Docker CLI with the compose and buildx plugins:
  `brew install docker docker-compose docker-buildx`. Homebrew puts the plugins
  where the CLI doesn't look by default, so add them to `~/.docker/config.json`:

  ```json
  { "cliPluginsExtraDirs": ["/opt/homebrew/lib/docker/cli-plugins"] }
  ```

  Without buildx, `docker build` falls back to the legacy builder and the
  BuildKit checks in `make devflow` fail.
- Python 3.11+ for the optional health check (`brew install python`).
- Free disk space: the VM won't start with less than 3 GB free on the Mac and
  warns below 10 GB (`ORBIT_STOP_FREE_GB` / `ORBIT_WARN_FREE_GB`). Docker's
  data lives in a sparse `~/.orbit/vmm/data.img` that grows as you pull and
  build.

### On a macOS version orbit hasn't been tested on

Tested so far on macOS 26.4. How Hypervisor.framework memory can be handed
back to macOS, and how Activity Monitor counts it, are up to macOS, so check
these three things on a new version before relying on it:

1. **Memory release.** With no VM running:

   ```bash
   vmm/target/release/orbit-vmm --reclaim-selftest
   ```

   In the `guest RAM from hv_vm_allocate` block, the
   `unmap + mmap(MAP_FIXED anon) + map` line must end near `released 1 MiB`.
   That is the method the VMM uses (`vmm/src/reclaim.rs`). If it stays at
   ~257 MiB, freed guest memory isn't returned on this version: the VM only
   grows on the Mac.
2. **Developer workflow.** With the VM up: `make devflow` must report
   0 fail (25 checks).
3. **Activity Monitor.** Its Memory column is the VMM's `phys_footprint`.
   When the Mac is short on memory, macOS compresses part of guest RAM and
   that number grows past what the guest uses. In testing the gap was near
   zero with nothing compressed and about a third to a half of the
   compressed amount under pressure. A different gap on another macOS
   version is a change in how macOS counts, not a leak, as long as
   `ORBIT_MEMDUMP` + `vmm/bench/memxref.py` show the VMM holding about what
   the guest uses (see [docs/architecture.md](docs/architecture.md)).

## Quick start

```bash
git clone https://github.com/skax-eunsik-kim/orbit-vmm ~/.local/src/orbit-vmm
cd ~/.local/src/orbit-vmm
make guest      # download Alpine kernel + rootfs (pinned, sha256-checked) into ~/.orbit/guest
make install    # build, sign, copy the VMM, the orbit command and scripts to ~/.orbit/bin, install the watchdog
export PATH="$HOME/.orbit/bin:$PATH"   # put this in your shell profile
orbit up        # start the VM; first boot installs Docker (~1 min, waits up to 5), later boots 2-4 s
export DOCKER_HOST=unix://$HOME/.orbit/vmm/docker.sock
docker run --rm hello-world
```

Put the `DOCKER_HOST` line in your shell profile, or create a context:

```bash
docker context create orbit --docker host=unix://$HOME/.orbit/vmm/docker.sock
docker context use orbit
```

`orbit status` shows the installed version and whether the VM is running.
`orbit up` restarts a running VM, which stops all of its containers.

Clone orbit **outside your shared directories** (see
[File sharing](#file-sharing)), as above. Containers can write to shared
directories, so a checkout in one can be changed behind your back, and the
next `make install` or `make guest` would build and run that code on the Mac.
`make install` warns when the checkout is in a shared directory. After
installing, `orbit` runs only the installed copies in `~/.orbit/bin` and never
the checkout; `make up`, `make down`, `make update` and `make rollback` just
call it.

## Configuration

`scripts/vmm-up.sh` (what `orbit up` runs) reads these environment variables:

| Variable | Default | Meaning |
|---|---|---|
| `ORBIT_MEM_MIB` | `8192` | Guest RAM ceiling. Freed memory returns to macOS, so a high ceiling costs little. |
| `ORBIT_CPUS` | Mac core count (`sysctl -n hw.ncpu`) | vCPUs. Idle vCPUs park and cost no CPU; with only a few, parallel container start-up (e.g. agent sandboxes) queues on CPU. |
| `ORBIT_GUEST` | `~/.orbit/guest` | Directory with the guest `kernel` and `initramfs` (e.g. one built with `ORBIT_HOME=… scripts/fetch-guest.sh`). |
| `ORBIT_DISK_GB` | `64` | Size of a **new** data disk (sparse file). |
| `ORBIT_DISK_WARN_MIB` | `ORBIT_WARN_FREE_GB` in MiB | Below this much free space on the Mac, the VMM also warns in `console.log`. |
| `ORBIT_DISK_FLOOR_MIB` | `1024` | Last resort under `ORBIT_STOP_FREE_GB`: below this, guest disk writes are held until space is freed (see [Data disk](#data-disk)). |
| `ORBIT_DISK_PAUSE_SECS` | `600` | How long a held write waits before it fails. |
| `ORBIT_SHARE_ALLOW` | see below | Colon-separated directories that containers may bind-mount. |
| `ORBIT_HOST_LOOPBACK` | `1` | `0` stops containers from reaching services on the Mac's `127.0.0.1` (and `host.docker.internal`). |
| `ORBIT_WARN_FREE_GB` / `ORBIT_STOP_FREE_GB` | `10` / `3` | Mac disk free space at which the VM warns you, and at which it shuts down cleanly before its disk fills. |
| `ORBIT_INSTANCE` | (unset) | Run a separate VM with its own disk and sockets under `~/.orbit/vmm-<name>`. |
| `ORBIT_PRESSURE` | (follow the Mac) | `off` stops passing macOS memory pressure to the guest; `warn` or `critical` pins the level (for measurements). |
| `ORBIT_FS_OWNERSHIP` | (emulated) | `host` shows bind-mounted files with their real Mac owner and mode instead of the emulated ones. |
| `ORBIT_FS_TTL` / `ORBIT_FS_ATTR_TTL_MS` | `5` / `500` | How long containers cache names (s) and attributes (ms) of bind-mounted files. Lower means Mac edits show sooner, at some speed cost. |

`make guest` (`scripts/fetch-guest.sh`) reads `ALPINE_MIRROR` (default
`https://dl-cdn.alpinelinux.org/alpine`; the sha256 pins still apply),
`ORBIT_HOST_CA=0` (see [Root certificates](#root-certificates-tls-inspecting-proxies))
and `ORBIT_HOME` (build into `<dir>/guest` instead of `~/.orbit/guest`).

### File sharing

By default the VM can see whichever of `~/work`, `~/src`, `~/code`, `~/dev`,
`~/projects` and `~/Developer` exist. Everything else in `/Users` is
invisible. To add directories:

```bash
ORBIT_SHARE_ALLOW="$HOME/src:$HOME/clients" orbit up
```

Shared directories are **writable by every container**. orbit-vmm refuses to
share your home directory, anything above it, hidden directories (`~/.ssh`,
`~/.orbit`, …), `~/Library`, `~/bin` and `~/Applications`. A directory inside another shared one is
dropped, since the outer one already covers it. Containers can't rename or
remove the shared directories themselves.

- Mac edits to files are visible in containers within about 0.5 s. New files
  and renamed directories show up within about 1 s.
- Mac edits and new files raise `IN_MODIFY` / `IN_CLOSE_WRITE` in containers
  about 1 s later.

### Networking

- `-p 8080:80` and compose `ports:` are forwarded to `127.0.0.1:8080` on the
  Mac within about 1 s. If the Mac port is taken, the forward fails, and the
  error goes to `~/.orbit/vmm/console.log`.
- Containers reach services on the Mac's localhost via `host.docker.internal`.
  That includes anything listening only on `127.0.0.1` (databases, notebooks,
  local model servers). Set `ORBIT_HOST_LOOPBACK=0` if you don't want that.
- Any process listening on all addresses in the VM gets the same port on the
  Mac's `127.0.0.1`, not only `-p` publishes (as with `--network host`).

### Root certificates (TLS-inspecting proxies)

- `make guest` reads the roots your Mac trusts beyond Apple's built-in set
  (the ones marked "Always Trust" for SSL, typically a corporate
  TLS-inspection CA) and builds them into your guest. On a Mac without such
  roots nothing is added, and containers see their images' own bundles.
  The repository and releases contain no certificates.
- Containers and build steps see those roots appended to the image's CA
  bundle, through a mount. A build step gets a copy of the bundle's
  directory, so `update-ca-certificates` and `apt-get upgrade` work. Neither
  writes the Mac's roots into the image.
- `ORBIT_HOST_CA=0 make guest && orbit up` builds a guest without them.
- There is no explicit-proxy support: the guest doesn't use the Mac's
  HTTP(S) proxy settings or PAC file, and Docker pulls go out directly through
  the Mac's network. Networks that only allow traffic through an explicit
  proxy don't work yet; a transparent TLS-inspecting proxy does.
- Your guest (`~/.orbit/guest`) and data disk then hold your organisation's
  CAs: don't share them outside it.

## Files

| Path | Contents |
|---|---|
| `~/.orbit/guest/` | Kernel and initramfs built by `make guest`. |
| `~/.orbit/vmm/data.img` | Data disk (Docker images, volumes). |
| `~/.orbit/vmm/docker.sock` | Docker API. |
| `~/.orbit/vmm/console.log` | Guest console; look here first when something fails (`console.prev.log` is the previous run). |
| `~/.orbit/vmm/watchdog.log`, `crashes/` | Supervisor log and records of unexpected stops. |

**Don't share `~/.orbit/guest/*` or `data.img`.** They contain GPL binaries
and your machine's trusted root certificates
([THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)).

To grow the data disk, stop the VM, enlarge the file, and start the VM again.
The guest resizes its filesystem on boot:

```bash
python3 -c "import os;p=os.path.expanduser('~/.orbit/vmm/data.img');os.truncate(p,max(os.path.getsize(p),128<<30))"
```

### Data disk

`data.img` is sparse: it takes as much space on the Mac as Docker has
written, up to its logical size (`vmm-up.sh` prints both). It grows silently,
so two guards watch the Mac's free space. The supervisor (`vmm-run.sh`)
notifies you below `ORBIT_WARN_FREE_GB`, and shuts the guest down cleanly below
`ORBIT_STOP_FREE_GB` (it won't start there either). Under that, as a last
resort, the VMM checks every 2 seconds:

- Below `ORBIT_DISK_WARN_MIB`, it logs `orbit-vmm: disk space low` to
  `console.log` (again every 10 minutes while it stays low), and
  `scripts/healthcheck.py` reports a warning.
- Below `ORBIT_DISK_FLOOR_MIB`, or when a write fails because the disk is
  full, it holds guest disk writes and logs `orbit-vmm: DISK FULL` once.
  Docker stalls (the guest may log hung-task warnings), but nothing fails.
  Free space on the Mac and writes resume where they stopped
  (`disk writes resumed`).
- If no space is freed within `ORBIT_DISK_PAUSE_SECS`, writes fail. ext4 then
  reports I/O errors and may go read-only, so free space before that and
  restart the VM if it did.

Holding writes instead of failing them is what QEMU does by default
(`werror=enospc`): a failed write is data ext4 has already promised to store,
and losing it is how a full Mac disk corrupts the guest.

## Upgrading

```bash
orbit update           # the latest release; --to v0.2.1 for a given one
orbit rollback         # back to what was installed before the last update
```

`orbit update` builds the release in its own clone (`~/.orbit/src`) of the
repository recorded at install time, not in your checkout, and builds the new
guest before replacing anything, so a failed download or build leaves the
installed version as it was. It keeps the previous binaries and guest for
`orbit rollback`. It restarts the VM only when no containers are running;
otherwise the VM keeps the old version until `orbit up` (or run
`orbit update --now`). The first boot after a guest upgrade reinstalls guest
packages (about 1 min).

If the health check is installed, it looks up the latest release once a day
and shows one macOS notification per newer release. Nothing is installed
without `orbit update`.

To follow `main` instead of releases: `git pull && make guest install && orbit up`
in your checkout.

Installed v0.2.1 or older? Run `make update` once from your checkout; that
installs the `orbit` command.

## Uninstall

```bash
orbit uninstall
```

It stops every orbit VM, removes the launchd agents (watchdog and health
check), the health check's scratch directories in your shared directories, and
`~/.orbit`, **including the data disks with every image, container and
volume**. It asks first (`--yes` skips the question). It leaves your checkout,
and a Docker context named `orbit` if you created one
(`docker context rm orbit`).

## Watchdog

`make install` also installs a launchd agent, `com.orbit.vmm`, that runs the
VM and recovers it. It is there because nothing else is: orbit has no app that
stays running, so without it a VMM that dies or hangs stays down.

```bash
ORBIT_WATCHDOG=0 make install          # install without it
./scripts/install-watchdog.sh --uninstall
./scripts/install-watchdog.sh --reload # apply a changed agent now (restarts the VM)
```

Installing again leaves a running VM alone.

- **Crash:** if the VMM dies, or the guest reboots or panics, launchd starts
  it again within seconds.
- **Hang:** the guest is pinged every 30 s. After three missed pings the VMM
  is aborted, so macOS writes a crash report, and then it is restarted. That
  takes about 90 s.
- **Crash loop:** after more than three starts in 10 minutes it stops
  restarting and tells you.
- **Crash records:** each unexpected stop is kept in
  `~/.orbit/vmm/crashes/<time>/` (console log, macOS crash report, watchdog
  log). The 10 newest are kept, and you get a macOS notification. Containers
  stop with the VM, so ones without a `restart:` policy have to be started
  again.
- **On purpose:** `orbit down` (`vmm-up.sh --stop`) and a guest `poweroff`
  count as intended stops and are not restarted. A VM that was running when
  you logged out starts again at login.

Without the watchdog, `orbit up` runs the same supervisor in the background,
so hangs are still detected and crashes still recorded, but nothing is
restarted.

## Health check (optional)

After `make install`, `scripts/install-healthcheck.sh` installs a launchd
agent that runs the installed `~/.orbit/bin/healthcheck.py`. It checks Docker,
ports, bind mounts, inotify and the guest clock, and records VMM memory, thread
and fd counts to `~/.orbit/vmm/health.jsonl`.

The check runs every hour. After three healthy runs in a row it backs off to
every 2 h, then every 4 h. Any problem resets it to hourly. To remove it, run
`--uninstall`.

## Known limitations

- **inotify:** Mac-side changes reach containers about a second later. A new
  directory tree with more than 256 entries (a fresh worktree, an
  `npm install` on the Mac) raises one `IN_CREATE` for its top directory;
  recursive watchers scan it from there. Renames pair up only when the guest
  had already seen the old name; otherwise they show up as delete + create.
- **Port forwarding:** UDP publishes aren't forwarded. Publishes on a specific
  host IP (`-p 127.0.0.1:8080:80`) are forwarded to the Mac's `127.0.0.1` like
  the rest; the same port published on two such IPs reaches only the newest.
  If two VMs publish the same port, the second one's forward fails (logged).
- **Bind mounts:** no file locks are shared between the Mac and containers.
  Unix sockets and FIFOs can't be created on bind mounts.
- **Data disk:** space Docker frees goes back to the Mac only when the guest
  trims (`fstrim`, every 10 minutes when the VM is quiet, and once after boot),
  so `docker system prune` shows up on the Mac a few minutes later. The image
  never shrinks below what the guest has written but not freed.
- **Root certificates:** a build step that copies the CA bundle somewhere else
  (`cp /etc/ssl/certs/ca-certificates.crt /app/`) copies the Mac's roots with
  it, into the image. Regenerating the bundle works in build steps but not in
  running containers, and later commands in the same `RUN` don't see the Mac's
  roots. If something is mounted on the CA directory (`RUN --mount`, `-v`),
  the roots aren't added there.
- **amd64 images** run under qemu user emulation, not Rosetta, so they are
  noticeably slower than arm64 ones.
- **Guest packages:** the kernel and Alpine root filesystem are pinned by
  sha256, but Docker, qemu and the other guest packages are installed from
  Alpine's repositories on the first boot after each guest build, so they move
  with Alpine's updates.
- **Coming from Docker Desktop or OrbStack:** images, containers and volumes
  are not migrated. Re-pull images, or move them with `docker save` /
  `docker load`; copy volume contents with a container that mounts both.
- **Missing pieces:** no GUI or Kubernetes, and no signed binary release: you
  build it yourself.

## How it works

See [docs/architecture.md](docs/architecture.md). In short:

```
docker CLI ──unix socket── orbit-vmm ──vsock── guest: dockerd
                              │
          Hypervisor.framework (vCPUs, in-kernel GICv3)
          virtio-mmio: blk · net (libslirp) · vsock · fs · balloon (free page reporting)
```

## Reporting a bug

Open an [issue](https://github.com/skax-eunsik-kim/orbit-vmm/issues) with what
you ran, what you expected and what happened, and attach:

- the end of `~/.orbit/vmm/console.log` and `~/.orbit/vmm/watchdog.log`;
- `sw_vers -productVersion` and `orbit status`;
- for a crash, the newest `~/.orbit/vmm/crashes/<time>/` directory.

Logs can contain paths and image names; check before posting. Never attach
`~/.orbit/guest`, `data.img` or `host-ca.pem`: they hold your Mac's extra root
certificates.

## Contributing

Bug reports and pull requests are welcome. See
[CONTRIBUTING.md](CONTRIBUTING.md) for the development setup and checks.
Report security issues privately ([SECURITY.md](SECURITY.md)).

## License

Apache-2.0 ([LICENSE](LICENSE)). The guest image you build downloads Alpine
Linux packages, which carry their own licences; see
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

orbit-vmm is an independent project. It is not affiliated with Docker, Inc.,
OrbStack, or Apple.
