# AGENTS.md

Rules for coding agents (and humans) working on orbit-vmm. Setup, layout,
checks and style are in [CONTRIBUTING.md](CONTRIBUTING.md); this file adds what
is easy to get wrong. `CLAUDE.md` only imports this file.

## Goals that decide trade-offs

- Memory handed back to macOS is the point. Measure before and after any change
  that could raise the VM's footprint, and put both numbers in the PR.
- Few dependencies: plain std and `libc`. A new crate needs a stated reason.
- People run this as their only Docker engine. A change that can stop their
  containers, corrupt their data disk or block `make guest` is a release
  blocker, however small the diff.

## Workflow

- Issue first for anything beyond a small fix. Branch from `main`, open a PR,
  fill in the PR template, and paste the local check results.
- `main` accepts changes only through a PR with the `vmm` CI job passing
  (repository ruleset). Never push to `main` directly and never force-push a
  branch under review; bring it up to date by merging `main` into it.
- `v*` tags are releases and only the owner can create them (tag ruleset).
  Agents never create, move or delete tags.
- Merge only when the owner asks. Each request covers the PRs it names, not
  later ones.

## Verifying a change

CI builds, lints and unit-tests but cannot boot a VM. Run the checks yourself,
in an **isolated instance**, never on the shared VM someone is using:

```bash
make build
ORBIT_BIN=$HOME/.orbit-x/bin ./scripts/install.sh          # skips the watchdog
ORBIT_HOME=$HOME/.orbit-x ./scripts/fetch-guest.sh          # guest changes only
ORBIT_INSTANCE=x ORBIT_GUEST=$HOME/.orbit-x/guest ORBIT_MEM_MIB=4096 ORBIT_CPUS=4 \
  ORBIT_DISK_GB=20 $HOME/.orbit-x/bin/vmm-up.sh
bench/devflow/devflow.sh x -H unix://$HOME/.orbit/vmm-x/docker.sock   # 25 checks, 0 fail
vmm/bench/fssec.sh                                                    # virtio-fs: 23/23
ORBIT_INSTANCE=x $HOME/.orbit-x/bin/vmm-up.sh --stop; rm -rf ~/.orbit-x ~/.orbit/vmm-x
```

- Copy `~/.orbit/cache/*` into `~/.orbit-x/cache/` first: Alpine removes old
  package versions, so a fresh download of a pinned kernel can 404.
- The shared VM (`~/.orbit/vmm`, `~/.orbit/bin`, `~/.orbit/guest`) belongs to
  whoever is using it. Don't stop, restart, upgrade or hot-patch it unless asked.
  Restarting stops every container in it: list them first
  (`docker ps --format '{{.Names}}'`) and start again the ones without a
  `restart:` policy afterwards.
- A green test proves little until it fails without the fix: revert the fix,
  watch the test fail, restore it. Commit before doing this.
- Guest scripts can be tried quickly in the isolated instance with
  `docker run --rm -i --privileged --pid=host alpine nsenter -t 1 -m -- …`.
  For the PR, rebuild the guest and reboot, so the result comes from the real
  initramfs.

## Known traps

- **Guest clock.** It can step back (the VMM corrects it, `console.log` says
  so). Never decide "did this change?" from mtimes (`find -newer`); compare
  contents.
- **Network.** TCP and TLS timeouts to registries are often the corporate
  network, not orbit. Before blaming the VMM, run the same parallel load from
  the Mac itself at the same time. DNS is different: the guest's dnsmasq is
  orbit's, so check its limits too (`--dns-forward-max`).
- **Root certificates.** `make guest` builds the Mac's extra trusted roots,
  usually a company TLS-inspection CA, into the guest. Never commit
  `host-ca.pem`, a built guest, `data.img`, logs or crash records that contain
  them. Any change to `guest/etc/orbit/orbit-runc` must show that image layers
  still hold no Mac root: count the certificates in a built image.
- **BuildKit.** It runs `runc run --keep`, then removes the bundle and rootfs
  before `runc delete`, so OCI `poststop` hooks are too late to touch the rootfs.
- **Alpine pins.** `scripts/fetch-guest.sh` pins the minirootfs and
  `linux-virt` with sha256. When the weekly `guest-pins` workflow fails, bump
  the pin: read the version from `APKINDEX`, download the file, record its sha256.
- **macOS memory numbers.** Activity Monitor shows `phys_footprint`, which
  counts compressed guest memory. Check with `ORBIT_MEMDUMP` +
  `vmm/bench/memxref.py` before calling something a leak.
- **`pkill -f`.** A broad pattern can match `orbit-vmm` and kill a VM. Kill only
  the PIDs you started.
- **Docker context.** Use `DOCKER_HOST=unix://…/docker.sock` per command, never
  `docker context use`: that changes every shell on the Mac.

## Docs

- Every user-visible change gets a line under `## [Unreleased]` in
  [CHANGELOG.md](CHANGELOG.md): what changed for the user, and why.
- The English `README.md` and `docs/*.md` are the source of truth;
  translations are in `docs/i18n/<lang>/`. When you change an English document,
  update the translations or say in the PR which ones lag.
  `scripts/readme-langs.py` writes the language selectors and
  `scripts/check-doc-links.py` checks links and anchors (both run in CI). See
  CONTRIBUTING for the translation rules.
- Each top-level directory has a README listing its files; keep it in step when
  you add, remove or repurpose a file.
- Docs must state measured numbers with how they were measured, and known
  limitations plainly.

## Using orbit from an agent

For agents that only run Docker on a Mac where orbit is the engine (a coding
session building and starting a project's stack):

- Point each command at the engine: `DOCKER_HOST=unix://$HOME/.orbit/vmm/docker.sock`,
  or a context created for it (README "Quick start"). Don't switch the global
  context with `docker context use`, and don't start
  Docker Desktop or OrbStack.
- The VM is shared with the person and their other sessions. Don't run
  `make up`, `make down`, `make update`, `vmm-up.sh` or `docker system prune`,
  and don't stop containers you didn't start. Name your compose project
  (`-p <name>`) and remove only what it created.
- Bind mounts work only under shared directories (`~/work`, `~/src`, …; see
  README "File sharing"). A path under `/tmp` or `/private/tmp` fails with
  "bind source path does not exist".
- Ports published by your containers appear on the Mac's `127.0.0.1`. If a port
  is taken, the forward fails and `~/.orbit/vmm/console.log` says so.
- If Docker stops answering, read `~/.orbit/vmm/console.log` and
  `~/.orbit/vmm/watchdog.log` before anything else. The watchdog restarts a
  crashed VM within seconds; containers without a `restart:` policy then need
  `docker start`.
- When orbit itself misbehaves (not your Dockerfile, not the network), report
  it as an issue: what you ran, what you expected and saw, the relevant
  `console.log` lines, `sw_vers -productVersion`, and `cat ~/.orbit/bin/VERSION`.

## Releases

Owner-initiated only:

1. PR: cut `## [Unreleased]` into `## [X.Y.Z] - <date>` in CHANGELOG, and set
   `version` in `vmm/Cargo.toml` and `vmm/Cargo.lock`.
2. After merge: `gh release create vX.Y.Z --target <merge commit> --notes-file …`
   with short user-facing notes: highlights, how to upgrade, known limitations.
3. Users get it with `make update`; the health check announces it. Both follow
   tags only, so nothing on `main` reaches users before it is released.
