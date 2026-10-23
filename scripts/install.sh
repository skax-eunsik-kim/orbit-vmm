#!/bin/bash
# Copies what runs unattended (the VMM, its close() interposer, the start and
# health-check scripts) to ~/.orbit/bin. The checkout usually lives in a shared
# directory that containers can write to, so nothing should execute from it
# without you rebuilding on purpose.
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${ORBIT_BIN:-$HOME/.orbit/bin}"
REL="$ROOT/vmm/target/release"
[ -x "$REL/orbit-vmm" ] || { echo "build first: make build" >&2; exit 1; }
mkdir -p "$HOME/.orbit" "$BIN"
chmod 700 "$HOME/.orbit" "$BIN"
for f in "$REL/orbit-vmm" "$REL/liborbitclose.dylib" "$ROOT/scripts/vmm-up.sh" "$ROOT/scripts/vmm-run.sh" "$ROOT/scripts/healthcheck.py"; do
  # Replace via rename so a running VM keeps its (unchanged) binary.
  install -m 700 "$f" "$BIN/.$(basename "$f").new"
  mv -f "$BIN/.$(basename "$f").new" "$BIN/$(basename "$f")"
done
# What was installed, for make update / make rollback and the health check's
# update notice: the release tag (or commit) and the GitHub repository.
{
  echo "version=$(git -C "$ROOT" describe --tags --always --dirty 2>/dev/null || echo unknown)"
  echo "repo=$(git -C "$ROOT" remote get-url origin 2>/dev/null | sed -E 's#^(git@github\.com:|https://github\.com/)##; s#\.git$##')"
} > "$BIN/VERSION"
echo "installed $(sed -n 's/^version=//p' "$BIN/VERSION") to $BIN"
# The watchdog restarts a VM that crashes or hangs, and starts it at login.
# Only for the default install: a test install (ORBIT_BIN=...) must not take
# over the launchd agent. ORBIT_WATCHDOG=0 skips it.
if [ "$BIN" = "$HOME/.orbit/bin" ] && [ "${ORBIT_WATCHDOG:-1}" != 0 ]; then
  "$ROOT/scripts/install-watchdog.sh"
fi
