#!/bin/bash
# Usage: bench.sh <label> <docker-context> [vm-process-pattern]
# Measures container start latency, bind-mount (host file sharing) I/O,
# in-VM disk I/O, published-port latency, and host memory of the VM processes.
set -uo pipefail
LABEL=$1 CTX=$2 PAT=${3:-}
if [[ $CTX == unix://* ]]; then D="docker -H $CTX"; else D="docker --context $CTX"; fi; HOSTIP=${HOSTIP:-localhost}
now() { python3 -c 'import time;print(time.time())'; }
ms() { python3 -c "print(round(($2-$1)*1000))"; }
SRC="$(cd "$(dirname "$0")" && pwd)/tmp-$LABEL"
rm -rf "$SRC"; mkdir -p "$SRC"

$D info >/dev/null 2>&1 || { echo "$LABEL: docker not reachable" >&2; exit 1; }; $D pull -q alpine:3.22 >/dev/null; $D pull -q nginx:alpine >/dev/null

# 1. container start (median of 5)
starts=()
for i in 1 2 3 4 5; do t0=$(now); $D run --rm alpine:3.22 true; t1=$(now); starts+=($(ms $t0 $t1)); done
start_ms=$(printf '%s\n' "${starts[@]}" | sort -n | sed -n 3p)

# 2. bind mount: write 5000 small files, then stat+read them all
fails=0; t0=$(now)
$D run --rm -v "$SRC:/w" alpine:3.22 sh -c 'cd /w && for i in $(seq 1 5000); do echo hello-$i > f$i; done'
t1=$(now)
$D run --rm -v "$SRC:/w" alpine:3.22 sh -c 'cd /w && find . -type f | wc -l >/dev/null && cat f* > /dev/null'
t2=$(now)
bind_write=$(ms $t0 $t1); bind_read=$(ms $t1 $t2)

# 3. bind mount sequential: 512MB write + read
fails=0; t0=$(now)
$D run --rm -v "$SRC:/w" alpine:3.22 sh -c 'dd if=/dev/zero of=/w/big bs=1M count=512 conv=fsync 2>/dev/null && dd if=/w/big of=/dev/null bs=1M 2>/dev/null'
t1=$(now)
bind_seq=$(ms $t0 $t1)

# 4. in-VM filesystem: same small-file workload, no host sharing
fails=0; t0=$(now)
$D run --rm alpine:3.22 sh -c 'mkdir /w && cd /w && for i in $(seq 1 5000); do echo hello-$i > f$i; done && cat f* >/dev/null'
t1=$(now)
vm_small=$(ms $t0 $t1)

# 5. published port: 200 sequential HTTP requests to localhost (SKIP_PORT=1 to skip)
port_ms=-; fails=-
if [[ -z "${SKIP_PORT:-}" ]]; then
$D rm -f bench-http >/dev/null 2>&1
$D run -d --name bench-http -p 18080:80 nginx:alpine >/dev/null
for i in $(seq 1 50); do curl -fs -m 2 $HOSTIP:18080 >/dev/null && break; sleep 0.2; done
fails=0; t0=$(now)
for i in $(seq 1 200); do curl -fs -m 2 $HOSTIP:18080 >/dev/null || fails=$((fails+1)); done
t1=$(now)
port_ms=$(ms $t0 $t1)
$D rm -f bench-http >/dev/null
fi

# 6. host memory of VM processes (sum RSS, MiB)
mem="-"
if [[ -n "$PAT" ]]; then
  mem=$(ps -axo rss=,command= | grep -E "$PAT" | grep -v grep | awk '{s+=$1} END{printf "%d", s/1024}')
fi

rm -rf "$SRC"
printf '%-10s start=%sms bind_small_write=%sms bind_small_read=%sms bind_seq512M=%sms vm_small=%sms port200=%sms(fail=%s) rss=%sMiB\n' \
  "$LABEL" "$start_ms" "$bind_write" "$bind_read" "$bind_seq" "$vm_small" "$port_ms" "$fails" "$mem" | tee -a "$(dirname "$0")/results.txt"
