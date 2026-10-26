# scripts

Host-side scripts. Everything except the build and install steps (`orbit`,
`vmm-up.sh`, `vmm-run.sh`, `update.sh`, `healthcheck.py`) runs from the
installed copy in `~/.orbit/bin`, never from this checkout: containers can
write to a checkout in a shared directory.

| Script | Run by | What it does |
|---|---|---|
| [`fetch-guest.sh`](fetch-guest.sh) | `make guest` | Downloads the pinned Alpine kernel and minirootfs (sha256-checked) and builds `~/.orbit/guest`. `ORBIT_HOST_CA=0` leaves out the Mac's extra root certificates. |
| [`extract-image.py`](extract-image.py) | `fetch-guest.sh` | Extracts the uncompressed arm64 kernel `Image` from Alpine's `vmlinuz`. |
| [`export-host-ca.py`](export-host-ca.py) | `fetch-guest.sh` | Prints the roots this Mac trusts for TLS beyond Apple's set (usually a company TLS-inspection CA). |
| [`install.sh`](install.sh) | `make install` | Copies the VMM and these scripts to `~/.orbit/bin`, writes `VERSION` (version and repository), installs the watchdog (`ORBIT_WATCHDOG=0` skips it). Warns when the checkout is in a shared directory. |
| [`orbit`](orbit) | you (`make up` etc. call it) | The installed command: `up`, `down`, `status`, `update`, `rollback`, `uninstall`. |
| [`install-watchdog.sh`](install-watchdog.sh) | `install.sh` | The launchd agent `com.orbit.vmm`. Re-running leaves a running VM alone; `--reload` applies a changed agent, `--uninstall` removes it. |
| [`install-healthcheck.sh`](install-healthcheck.sh) | you | The optional hourly health check agent. |
| [`vmm-up.sh`](vmm-up.sh) | `orbit up` / `orbit down` | (Re)starts the VM and waits for Docker; `--stop` shuts it down on purpose. |
| [`vmm-run.sh`](vmm-run.sh) | `vmm-up.sh`, launchd | The supervisor: restarts after crashes and hangs, records crashes, guards Mac disk space. |
| [`healthcheck.py`](healthcheck.py) | launchd | Checks Docker, ports, bind mounts, inotify, clock and disk; records resource use; daily release notice. |
| [`update.sh`](update.sh) | `orbit update` / `orbit rollback` | Builds a release tag in `~/.orbit/src`, from the repository recorded in `VERSION`, and installs it, keeping the previous version for rollback. |
| [`readme-langs.py`](readme-langs.py) | you, CI | Writes the language selector into every README and doc translation, and checks translations keep up. |
