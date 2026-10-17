#!/bin/bash
# Runs the memtest guest and samples the VMM's phys_footprint every 2s.
# Usage: memtrace.sh <label> [extra orbit-vmm args...]   (env: ORBIT_RECLAIM)
set -uo pipefail
LABEL=$1; shift
HERE="$(cd "$(dirname "$0")/.." && pwd)"
LOG="$(mktemp)"
"$HERE/target/release/orbit-vmm" --kernel "$HOME/.orbit/guest/kernel" \
  --initrd "$HOME/.orbit/vmm/memtest.cpio.gz" --mem 1024 --cpus 2 "$@" > "$LOG" 2>&1 &
PID=$!
start=$(date +%s)
peak=0
while kill -0 $PID 2>/dev/null; do
  mb=$(footprint -p $PID 2>/dev/null | awk '/phys_footprint:/{v=$2; if($3=="GB")v*=1024; else if($3=="KB")v/=1024; printf "%d", v; exit}')
  [[ -n "$mb" ]] && { echo "  +$(( $(date +%s) - start ))s footprint=${mb}MiB"; (( mb > peak )) && peak=$mb; last=$mb; }
  sleep 2
done
grep -E 'MEMTEST|orbit-vmm' "$LOG"
echo "$LABEL: peak=${peak}MiB final=${last:-?}MiB" | tee -a "$HERE/bench/memtrace-results.txt"
rm -f "$LOG"
