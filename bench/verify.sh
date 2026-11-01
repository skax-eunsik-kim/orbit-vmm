#!/bin/bash
# Every local check for a PR, on a throwaway instance: CI can't boot a VM.
# Usage: make verify (ORBIT_VERIFY_MIN_FREE sets the memory floor, 25%).
# Builds the checkout, boots it, runs devflow, reverts each imagegc.sh guard and
# requires the test to fail, then runs the low-disk path through the supervisor.
# The shared VM isn't touched, and the instance is removed however this ends.
# Free swap says little (macOS keeps it full long after memory is back), so the
# floor goes by memory_pressure.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT" || exit 1
INST=verify
X="$HOME/.orbit-$INST"
D="$HOME/.orbit/vmm-$INST"
MIN_FREE=${ORBIT_VERIFY_MIN_FREE:-25}
OUT="$ROOT/bench/devflow/results-verify-summary.txt"
export ORBIT_INSTANCE=$INST ORBIT_GUEST="$X/guest" ORBIT_MEM_MIB=4096 ORBIT_CPUS=4 ORBIT_DISK_GB=20 ORBIT_NOTIFY=0
export DOCKER_HOST="unix://$D/docker.sock"
GUEST=(docker run --rm -i --privileged --pid=host alpine:3.22 nsenter -t 1 -m --)
failed=0

free_pct() { memory_pressure | awk '/free percentage/ { print int($NF) }'; }
say() { echo "$*" | tee -a "$OUT"; }
result() { # result <PASS|FAIL> <what>
  say "$1  $2"
  [ "$1" = PASS ] || failed=1
}
up() { "$X/bin/vmm-up.sh" 2>&1 | tail -1; }
stop() { "$X/bin/vmm-up.sh" --stop >/dev/null 2>&1; }

now=$(free_pct)
[ "$now" -ge "$MIN_FREE" ] || { echo "only ${now}% of memory free (need ${MIN_FREE}%+); not starting" >&2; exit 1; }
[ ! -e "$D" ] && [ ! -e "$X" ] || { echo "$D or $X exists: is another run going?" >&2; exit 1; }

guard=
cleanup() {
  [ -z "$guard" ] || kill "$guard" 2>/dev/null
  [ ! -x "$X/bin/vmm-up.sh" ] || stop
  rm -rf "$X" "$D"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

: > "$OUT"
say "commit $(git rev-parse --short HEAD)$(git diff --quiet HEAD -- . || echo ' (with uncommitted changes)')"
make lint test >/dev/null 2>&1 && result PASS "make lint test" || result FAIL "make lint test"
[ "$failed" = 0 ] || exit 1

mkdir -p "$X/cache"
cp "$HOME"/.orbit/cache/* "$X/cache/" 2>/dev/null # Alpine drops old packages, so a pinned kernel can 404
make build >/dev/null 2>&1 || { result FAIL "make build"; exit 1; }
ORBIT_BIN="$X/bin" ./scripts/install.sh >/dev/null 2>&1 || { result FAIL "install"; exit 1; }
ORBIT_HOME="$X" ./scripts/fetch-guest.sh >/dev/null 2>&1 || { result FAIL "guest build"; exit 1; }
up

( while sleep 15; do
    pct=$(free_pct)
    [ "$pct" -lt "$MIN_FREE" ] || continue
    echo "memory free fell to ${pct}%; stopping the VM" >&2
    touch "$X/out-of-memory"
    stop
    exit
  done ) &
guard=$!

# devflow: its own lines go to the terminal, the total to the summary.
bench/devflow/devflow.sh "$INST" -H "$DOCKER_HOST" | tee "$X/devflow.out"
total=$(tail -1 "$X/devflow.out")
case $total in *" 0 fail"*) result PASS "devflow $total" ;; *) result FAIL "devflow $total" ;; esac
grep '^FAIL' "$X/devflow.out" >> "$OUT"

# Each guard must be what keeps the test green.
mutant() { # mutant <what> <sed expression>
  "${GUEST[@]}" sh -c "sed '$2' /etc/orbit/imagegc.sh > /run/gc-m; chmod +x /run/gc-m
    ! cmp -s /etc/orbit/imagegc.sh /run/gc-m" || { result FAIL "mutant has no effect: $1"; return; }
  if "${GUEST[@]}" sh -s /run/gc-m < bench/devflow/imagegc-test.sh >/dev/null 2>&1; then
    result FAIL "imagegc-test misses: $1"
  else result PASS "imagegc-test catches: $1"; fi
}
mutant "no in-use check" '/grep -qxF/d'
mutant "no kept tags" '/\*:latest | \*:local/d'
mutant "no build date check" '/tr -dc 0-9/d'
mutant "no arrival date check" 's/--filter "until=${hours}h" \${ref/${ref/'
mutant "no argument check" '/^case /d'

# The low-disk path, through the real supervisor. Its first disk check is 30 s
# after the guest answers. Above the Mac's free space counts as low.
old_image() {
  printf 'FROM scratch\nCOPY <<EOF /gc\nverify\nEOF\n' |
    docker build -q --no-cache --build-arg SOURCE_DATE_EPOCH=1000000000 -t orbittest/verify:old - >/dev/null
}
has_old() { docker image inspect orbittest/verify:old >/dev/null 2>&1; }
cleaned() { grep -c 'removing unused images' "$D/watchdog.log"; }
low=$(( $(df -g "$D" | awk 'NR == 2 { print $4 }') + 1 ))
old_image
stop
ORBIT_WARN_FREE_GB=$low ORBIT_IMAGE_GC_DAYS=0 up
sleep 45
if has_old && [ "$(cleaned)" = 0 ]; then result PASS "low disk, ORBIT_IMAGE_GC_DAYS=0: nothing removed"
else result FAIL "low disk, ORBIT_IMAGE_GC_DAYS=0: something ran"; fi
stop
ORBIT_WARN_FREE_GB=$low up
for _ in $(seq 1 30); do has_old || break; sleep 2; done
if has_old; then result FAIL "low disk: old image still there after 60 s"
else result PASS "low disk: old image removed by the supervisor"; fi
sleep 35 # one more disk check
[ "$(cleaned)" = 1 ] && result PASS "low disk: ran once, not on the next check" ||
  result FAIL "low disk: ran $(cleaned) times"
grep -q 'fstrim' "$D/console.log" && result PASS "low disk: trimmed" || result FAIL "low disk: no trim"

[ ! -e "$X/out-of-memory" ] || result FAIL "stopped early: memory ran out"
[ "$failed" = 0 ] && say "== verify: pass" || say "== verify: FAIL"
echo "paste into the PR: $OUT"
exit "$failed"
