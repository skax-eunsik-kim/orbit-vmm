#!/bin/bash
# A: N worktrees, each with its own compose stack, all in one VM at once.
# Checks per worktree: stack healthy, its ports on the Mac, its DB, and that
# edits in one worktree reach only that worktree's watcher (no cross-talk).
# Usage: multi.sh [N] [docker host flag...]   (default N=3, orbit-vmm)
set -u
N=${1:-3}; shift || true
[ $# -gt 0 ] || set -- -H "unix://$HOME/.orbit/vmm/docker.sock"
HERE="$(cd "$(dirname "$0")" && pwd)"
BASE="${ORBIT_SCRATCH:-$HERE/../tmp-multi}"   # must be inside a shared directory
now() { python3 -c 'import time;print(time.time())'; }
mkdir -p "$BASE"
for i in $(seq 1 $N); do
  mkdir -p "$BASE/wt$i"; echo "print('wt$i v0')" > "$BASE/wt$i/app.py"; : > "$BASE/wt$i/.events"
done
t0=$(now)
for i in $(seq 1 $N); do
  WT="$BASE/wt$i" PG_PORT=$((25430 + i)) REDIS_PORT=$((26370 + i)) \
    docker "$@" compose -p "mw$i" -f "$HERE/compose.yml" up -d --wait --wait-timeout 180 >/tmp/mw$i.log 2>&1 &
done
wait
echo "A: $N stacks up in $(python3 -c "print(round($(now)-$t0,1))")s (in parallel)"
fail=0
for i in $(seq 1 $N); do
  ok=1
  python3 -c "import socket;socket.create_connection(('127.0.0.1',$((25430+i))),3).close()" 2>/dev/null || { echo "  wt$i: pg port $((25430+i)) unreachable"; ok=0; }
  printf 'PING\r\n' | nc -w 3 127.0.0.1 $((26370+i)) | grep -q PONG || { echo "  wt$i: redis port unreachable"; ok=0; }
  # postgres:16's first start runs a temporary init server that pg_isready can
  # see before the real one is up; give psql a few seconds.
  pq=0; for r in $(seq 1 10); do
    docker "$@" compose -p "mw$i" -f "$HERE/compose.yml" exec -T postgres psql -U postgres -d app -qAtc "create table if not exists t(v int); insert into t values ($i); select sum(v) from t" >/dev/null 2>&1 && { pq=1; break; }
    sleep 1
  done
  [ $pq = 1 ] || { echo "  wt$i: psql failed"; ok=0; }
  [ $ok = 1 ] || fail=1
done
sleep 2
for i in $(seq 1 $N); do : > "$BASE/wt$i/.events"; done
# Edit every worktree at the same moment with a distinct token.
for i in $(seq 1 $N); do echo "print('wt$i v1')" > "$BASE/wt$i/app.py" & done; wait
sleep 4
for i in $(seq 1 $N); do
  # The watcher rewrites .seen every 0.2 s (truncate+write); retry an empty read.
  seen=""; for r in 1 2 3 4 5 6 7 8 9 10; do seen=$(cat "$BASE/wt$i/.seen" 2>/dev/null); [ -n "$seen" ] && break; sleep 0.1; done
  ev=$(grep -c 'app.py' "$BASE/wt$i/.events" 2>/dev/null)
  cross=$(grep -l "wt$i v1" "$BASE"/wt*/.seen 2>/dev/null | grep -vc "/wt$i/")
  echo "  wt$i: sees=[$seen] inotify_events=$ev cross_talk=$cross"
  { [ "$seen" = "print('wt$i v1')" ] && [ "$ev" -ge 1 ] && [ "$cross" = 0 ]; } || fail=1
done
for i in $(seq 1 $N); do WT="$BASE/wt$i" PG_PORT=1 REDIS_PORT=1 docker "$@" compose -p "mw$i" -f "$HERE/compose.yml" down -v >/dev/null 2>&1 & done; wait
[ $fail = 0 ] && echo "A: PASS" || echo "A: FAIL"
