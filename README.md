# orbit-vmm

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
memory, so freed memory can actually be returned.

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
  Mac edits fire inotify events in containers, so `uvicorn --reload`, vite and
  nodemon hot reload works. `chown` and `chmod` inside a container work too.
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
- **Small and auditable.** One Rust binary of about 5.5k lines. It depends on
  `libc`, `vm-fdt` and libslirp, and nothing else.

## Requirements

- Apple Silicon Mac, macOS 15 or later (the in-kernel GIC API needs 15+).
- Rust 1.87+ and libslirp: `brew install rust libslirp`.
- Docker CLI with the compose plugin: `brew install docker docker-compose`.

## Quick start

```bash
git clone https://github.com/skax-eunsik-kim/orbit-vmm ~/src/orbit-vmm
cd ~/src/orbit-vmm
make guest      # download pinned Alpine kernel + rootfs (sha256-checked) into ~/.orbit/guest
make install    # build, sign, and copy the VMM and scripts to ~/.orbit/bin
make up         # start the VM from ~/.orbit/bin; first boot installs Docker (~1 min), later boots 2-4 s
export DOCKER_HOST=unix://$HOME/.orbit/vmm/docker.sock
docker run --rm hello-world
```

Put the `DOCKER_HOST` line in your shell profile, or create a context:

```bash
docker context create orbit --docker host=unix://$HOME/.orbit/vmm/docker.sock
docker context use orbit
```

`make up` (or `~/.orbit/bin/vmm-up.sh`) restarts a running VM, which stops all
of its containers. The VM and the health check run only from the installed
copy in `~/.orbit/bin`. Containers can write to your shared directories, and
this checkout is often one of them, so `make up` builds and runs nothing from
it. Only `make install` and `make guest` do. Before running those, check that
the checkout hasn't changed unexpectedly (`git status`). Better still, keep the
checkout outside your shared directories.

## Configuration

`scripts/vmm-up.sh` (what `make up` runs) reads these environment variables:

| Variable | Default | Meaning |
|---|---|---|
| `ORBIT_MEM_MIB` | `8192` | Guest RAM ceiling. Freed memory returns to macOS, so a high ceiling costs little. |
| `ORBIT_CPUS` | Mac core count (`sysctl -n hw.ncpu`) | vCPUs. Idle vCPUs park and cost no CPU; with only a few, parallel container start-up (e.g. agent sandboxes) queues on CPU. |
| `ORBIT_GUEST` | `~/.orbit/guest` | Directory with the guest `kernel` and `initramfs` (e.g. one built with `ORBIT_HOME=… scripts/fetch-guest.sh`). |
| `ORBIT_DISK_GB` | `64` | Size of a **new** data disk (sparse file). |
| `ORBIT_SHARE_ALLOW` | see below | Colon-separated directories that containers may bind-mount. |
| `ORBIT_HOST_LOOPBACK` | `1` | `0` stops containers from reaching services on the Mac's `127.0.0.1` (and `host.docker.internal`). |
| `ORBIT_INSTANCE` | (unset) | Run a separate VM with its own disk and sockets under `~/.orbit/vmm-<name>`. |

### File sharing

By default the VM can see whichever of `~/work`, `~/src`, `~/code`, `~/dev`,
`~/projects` and `~/Developer` exist. Everything else in `/Users` is
invisible. To add directories:

```bash
ORBIT_SHARE_ALLOW="$HOME/src:$HOME/clients" make up
```

Shared directories are **writable by every container**. orbit-vmm refuses to
share your home directory, anything above it, hidden directories (`~/.ssh`,
`~/.orbit`, …), `~/Library`, `~/bin` and `~/Applications`. A directory inside another shared one is
dropped, since the outer one already covers it. Containers can't rename or
remove the shared directories themselves.

- Mac edits are visible in containers within about 0.5 s.
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

## Files

| Path | Contents |
|---|---|
| `~/.orbit/guest/` | Kernel and initramfs built by `make guest`. |
| `~/.orbit/vmm/data.img` | Data disk (Docker images, volumes). |
| `~/.orbit/vmm/docker.sock` | Docker API. |
| `~/.orbit/vmm/console.log` | Guest console; look here first when something fails. |

**Don't share `~/.orbit/guest/*` or `data.img`.** They contain GPL binaries
and your machine's trusted root certificates
([THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)).

To grow the data disk, stop the VM, enlarge the file, and start the VM again.
The guest resizes its filesystem on boot:

```bash
python3 -c "import os;p=os.path.expanduser('~/.orbit/vmm/data.img');os.truncate(p,max(os.path.getsize(p),128<<30))"
```

## Upgrading

```bash
git pull && make guest install up
```

The first boot after a guest upgrade reinstalls guest packages (about 1 min).

## Health check (optional)

After `make install`, `scripts/install-healthcheck.sh` installs a launchd
agent that runs the installed `~/.orbit/bin/healthcheck.py`. It checks Docker,
ports, bind mounts, inotify and the guest clock, and records VMM memory, thread
and fd counts to `~/.orbit/vmm/health.jsonl`.

The check runs every hour. After three healthy runs in a row it backs off to
every 2 h, then every 4 h. Any problem resets it to hourly. To remove it, run
`--uninstall`.

## Known limitations

- **inotify:** deletions, and files inside *newly created* directories, don't
  raise inotify events. Restart the watcher, or create the directory from
  inside the container.
- **testcontainers** mounts the socket path from `DOCKER_HOST`, which doesn't
  exist in the VM. Set
  `TESTCONTAINERS_DOCKER_SOCKET_OVERRIDE=/var/run/docker.sock`.
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
- **Missing pieces:** no installer, GUI, auto-start or Kubernetes.

## How it works

See [docs/architecture.md](docs/architecture.md). In short:

```
docker CLI ──unix socket── orbit-vmm ──vsock── guest: dockerd
                              │
          Hypervisor.framework (vCPUs, in-kernel GICv3)
          virtio-mmio: blk · net (libslirp) · vsock · fs · balloon (free page reporting)
```

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
