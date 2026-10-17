#!/bin/bash
# C: long-running churn against one VM, sampling health/resource metrics.
# Each round: 2 worktree stacks up+down (multi.sh), 20 throwaway containers,
# 2000 files created and deleted in a bind mount, then a metrics sample.
# Usage: soak.sh <hours> [out.jsonl]
set -u
HOURS=${1:?hours}; OUT=${2:-$HOME/.orbit/vmm/soak.jsonl}
HERE="$(cd "$(dirname "$0")" && pwd)"
export DOCKER_HOST=unix://$HOME/.orbit/vmm/docker.sock
END=$(( $(date +%s) + HOURS * 3600 ))
CH="${ORBIT_SCRATCH:-$HERE/../tmp-multi}/churn"; mkdir -p "$CH"
round=0
while [ "$(date +%s)" -lt $END ]; do
  round=$((round + 1))
  "$HERE/multi.sh" 2 >/tmp/soak-multi.log 2>&1 || true
  for i in $(seq 1 20); do docker run --rm alpine:3.22 true >/dev/null 2>&1; done
  docker run --rm -v "$CH:/c" alpine:3.22 sh -c 'cd /c && i=0; while [ $i -lt 2000 ]; do echo $i > f$i; i=$((i+1)); done; find . -name "f*" -delete' >/dev/null 2>&1
  python3 -I "$HERE/../../scripts/healthcheck.py" --record "$OUT" > /tmp/soak-health.log 2>&1
  echo "round $round $(tail -1 /tmp/soak-health.log | cut -c1-200) multi=$(tail -1 /tmp/soak-multi.log)"
done
