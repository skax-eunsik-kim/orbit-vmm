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
echo "installed to $BIN"
