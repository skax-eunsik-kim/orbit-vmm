#!/bin/bash
# Copies what runs (the VMM, its close() interposer, the orbit command, the
# start, update and health-check scripts) to ~/.orbit/bin. A checkout in a
# shared directory can be changed by containers, so after this nothing runs
# from the checkout: `orbit` uses only the installed copies.
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${ORBIT_BIN:-$HOME/.orbit/bin}"
REL="$ROOT/vmm/target/release"
[ -x "$REL/orbit-vmm" ] || { echo "build first: make build" >&2; exit 1; }
mkdir -p "$HOME/.orbit" "$BIN"
chmod 700 "$HOME/.orbit" "$BIN"
# Warn when the checkout itself is shared: containers could change what the
# next `make install` builds. (Default shares, as vmm-run.sh picks them, and
# the ones a running VM recorded.)
here=$(realpath "$ROOT")
for d in "$HOME"/{work,src,code,dev,projects,Developer} $(cat "$HOME"/.orbit/vmm*/shares 2>/dev/null) ${ORBIT_SHARE_ALLOW//:/ }; do
  r=$(realpath "$d" 2>/dev/null) || continue
  case "$here/" in "$r"/*)
    echo "warning: this checkout is inside $r, which the VM shares with containers;" >&2
    echo "         clone orbit outside shared directories (e.g. ~/.local/src/orbit-vmm) and install from there" >&2
    break ;;
  esac
done
for f in "$REL/orbit-vmm" "$REL/liborbitclose.dylib" "$ROOT/scripts/orbit" "$ROOT/scripts/vmm-up.sh" "$ROOT/scripts/vmm-run.sh" "$ROOT/scripts/update.sh" "$ROOT/scripts/healthcheck.py"; do
  # Replace via rename so a running VM keeps its (unchanged) binary.
  install -m 700 "$f" "$BIN/.$(basename "$f").new"
  mv -f "$BIN/.$(basename "$f").new" "$BIN/$(basename "$f")"
done
# What was installed, for orbit update / orbit rollback and the health check's
# update notice: the release tag (or commit) and the GitHub repository.
{
  echo "version=$(git -C "$ROOT" describe --tags --always --dirty 2>/dev/null || echo unknown)"
  echo "repo=$(git -C "$ROOT" remote get-url origin 2>/dev/null | sed -E 's#^(git@github\.com:|https://github\.com/)##; s#\.git$##')"
} > "$BIN/VERSION"
echo "installed $(sed -n 's/^version=//p' "$BIN/VERSION") to $BIN; run orbit as $BIN/orbit (add $BIN to PATH)"
# The watchdog restarts a VM that crashes or hangs, and starts it at login.
# Only for the default install: a test install (ORBIT_BIN=...) must not take
# over the launchd agent. ORBIT_WATCHDOG=0 skips it.
if [ "$BIN" = "$HOME/.orbit/bin" ] && [ "${ORBIT_WATCHDOG:-1}" != 0 ]; then
  "$ROOT/scripts/install-watchdog.sh"
fi
