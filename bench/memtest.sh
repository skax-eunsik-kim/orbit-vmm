#!/bin/bash
# Usage: memtest.sh <label> <docker-context> <vm-process-pattern>
# Host memory (phys_footprint) of the VM processes: idle, while a container
# holds 1.5GB anonymous memory + 2GB page cache, and 30s / 90s after it exits.
set -uo pipefail
LABEL=$1 CTX=$2 PAT=$3
if [[ $CTX == unix://* ]]; then D="docker -H $CTX"; else D="docker --context $CTX"; fi; HOSTIP=${HOSTIP:-localhost}

mem() {
  local total=0
  for pid in $(pgrep -f "$PAT"); do
    kb=$(footprint -p "$pid" 2>/dev/null | awk '/phys_footprint:/{v=$2; u=$3} END{if(u=="GB")v*=1048576; else if(u=="MB")v*=1024; printf "%d", v}')
    total=$((total + ${kb:-0}))
  done
  echo $((total / 1024))
}

$D info >/dev/null 2>&1 || { echo "$LABEL: docker not reachable" >&2; exit 1; }; $D pull -q alpine:3.22 >/dev/null
$D rm -f memhog >/dev/null 2>&1
idle=$(mem)
# tail keeps the whole newline-free stream in memory -> 1.5GB anonymous RSS.
$D run -d --name memhog alpine:3.22 sh -c 'dd if=/dev/zero of=/tmp/cache bs=1M count=2048 2>/dev/null; rm /tmp/cache; head -c 1536m /dev/zero | tail > /dev/null; sleep 5' >/dev/null
sleep 25
peak=$(mem)
$D wait memhog >/dev/null; $D rm memhog >/dev/null
sleep 30; after30=$(mem)
sleep 60; after90=$(mem)
printf '%-10s idle=%sMiB peak=%sMiB after30s=%sMiB after90s=%sMiB\n' "$LABEL" "$idle" "$peak" "$after30" "$after90" \
  | tee -a "$(dirname "$0")/mem-results.txt"
