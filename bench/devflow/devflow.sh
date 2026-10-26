#!/bin/bash
# Docker / compose developer-workflow checks against one engine.
# Usage: devflow.sh <label> <docker host flag...>
#   devflow.sh orbit-vmm -H unix://$HOME/.orbit/vmm/docker.sock
#   devflow.sh orbstack --context orbstack
# Uses project "orbittest", ports 15444/16379/20000/19000 and orbittest/* image
# tags only, so other stacks on the same engine are left alone.
set -u
LABEL=$1; shift
D=(docker "$@")
C=("${D[@]}" compose -p orbittest -f "$(dirname "$0")/compose.yml")
HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="$(dirname "$0")/results-$LABEL.txt"
: > "$OUT"
now() { python3 -c 'import time;print(time.time())'; }
secs() { python3 -c "print('%.1f' % ($2-$1))"; }
report() { printf '%-5s %-34s %s\n' "$1" "$2" "$3" | tee -a "$OUT"; }
check() { # check <name> <command...> : PASS/FAIL + seconds
  local name=$1; shift
  local t0=$(now)
  if out=$("$@" 2>&1); then report PASS "$name" "$(secs $t0 $(now))s"
  else report FAIL "$name" "$(secs $t0 $(now))s | $(echo "$out" | tr '\n' ' ' | cut -c1-160)"; fi
}

"${C[@]}" down -v >/dev/null 2>&1

# 1. compose up + healthchecks
check "compose up (3 svc healthy)" "${C[@]}" up -d --wait --wait-timeout 180

# 2. published ports reachable from macOS
check "host->pg  localhost:15444" python3 -c "import socket;s=socket.create_connection(('127.0.0.1',15444),3);s.close()"
check "host->redis PING :16379" bash -c "printf 'PING\r\n' | nc -w 3 127.0.0.1 16379 | grep -q PONG"
check "host->azurite :20000" python3 -c "import socket;s=socket.create_connection(('127.0.0.1',20000),3);s.close()"

# 3. service DNS inside the compose network
check "svc DNS (pg -> redis)" "${C[@]}" exec -T postgres sh -c "nc -z redis 6379"

# 4. container -> service on the Mac (host.docker.internal)
python3 -m http.server 18765 --bind 127.0.0.1 >/dev/null 2>&1 & HTTP=$!
sleep 1
check "ctr->host host.docker.internal" "${D[@]}" run --rm alpine:3.22 wget -q -T 5 -O /dev/null http://host.docker.internal:18765/
kill $HTTP 2>/dev/null

# 5. named volume survives down/up
"${C[@]}" exec -T postgres psql -U postgres -d app -qc "create table if not exists t(v text); insert into t values ('persist')" >/dev/null 2>&1
"${C[@]}" down >/dev/null 2>&1
"${C[@]}" up -d --wait --wait-timeout 120 >/dev/null 2>&1
check "named volume persists" bash -c "${C[*]} exec -T postgres psql -U postgres -d app -tAc 'select v from t' | grep -q persist"

# 6. bind mount: host edit visible in container (delay)
W=$(mktemp -d "$HERE/tmp.XXXX")   # the checkout must be inside a shared directory
echo v1 > "$W/f"
"${D[@]}" rm -f orbittest-watch >/dev/null 2>&1
"${D[@]}" run -d --name orbittest-watch -v "$W:/w" alpine:3.22 sh -c 'while :; do cat /w/f > /w/seen; sleep 0.1; done' >/dev/null
sleep 1
t0=$(now); echo v2 > "$W/f"
for i in $(seq 1 150); do grep -q v2 "$W/seen" 2>/dev/null && break; sleep 0.1; done
if grep -q v2 "$W/seen"; then report PASS "bind: host edit visible" "$(secs $t0 $(now))s after edit"; else report FAIL "bind: host edit visible" "not seen in 15s"; fi
"${D[@]}" rm -f orbittest-watch >/dev/null 2>&1

# 7. inotify on bind mount (hot reload: uvicorn --reload, vite, nodemon)
"${D[@]}" run -d --name orbittest-inotify -v "$W:/w" alpine:3.22 sh -c 'inotifyd - /w:wcM > /w/event 2>&1' >/dev/null
sleep 2
echo change >> "$W/f"
for i in $(seq 1 40); do [ -s "$W/event" ] && break; sleep 0.25; done
if [ -s "$W/event" ]; then report PASS "bind: inotify on host edit" "$(head -c 40 "$W/event" | tr '\t\n' '  ')"; else report FAIL "bind: inotify on host edit" "no event (hot reload won't trigger)"; fi
"${D[@]}" rm -f orbittest-inotify >/dev/null 2>&1

# 7b. HTTPS from inside a container (corporate TLS inspection needs the host's roots)
check "ctr HTTPS (apk via TLS proxy)" "${D[@]}" run --rm alpine:3.22 wget -q -T 10 -O /dev/null https://dl-cdn.alpinelinux.org/alpine/latest-stable/releases/aarch64/latest-releases.yaml

# 8. bind mount ownership: postgres initdb on a bind-mounted data dir
mkdir -p "$W/pgdata"
check "bind: postgres initdb on bind dir" "${D[@]}" run --rm -e POSTGRES_PASSWORD=x -v "$W/pgdata:/var/lib/postgresql/data" postgres:16-alpine sh -c 'docker-entrypoint.sh postgres -c listen_addresses= & for i in $(seq 1 60); do pg_isready -q && exit 0; sleep 1; done; exit 1'
check "bind: chown in container" "${D[@]}" run --rm -v "$W:/w" alpine:3.22 sh -c 'touch /w/o && chown 1234:1234 /w/o && [ "$(stat -c %u /w/o)" = 1234 ]'
rm -rf "$W"

# 9. amd64-only image (Rosetta / emulation)
check "linux/amd64 container" bash -c "${D[*]} run --rm --platform linux/amd64 alpine:3.22 uname -m | grep -q x86_64"

# 10. BuildKit build of the sample app (# syntax frontend, ghcr, apt, uv, cache mounts)
check "build sample app" "${D[@]}" build -q -t orbittest/app "$HERE/app"
check "rebuild (cached)" "${D[@]}" build -q -t orbittest/app "$HERE/app"

# 10b. A RUN step that regenerates the CA bundle (apt-get upgrade of ca-certificates
# does this) must work, and the Mac's roots must stay out of the committed image (#5).
check "build: RUN regenerates CA bundle" bash -c "printf 'FROM python:3.12-slim\nRUN update-ca-certificates --fresh\n' | ${D[*]} build -q --no-cache -t orbittest/ca -"
check "build: host CA kept out of image" bash -c "c=\$(${D[*]} create orbittest/ca) && ${D[*]} cp \$c:/etc/ssl/certs/ca-certificates.crt - | tar -xO > '$OUT.ca' && ${D[*]} rm \$c >/dev/null &&
  python3 -I '$HERE/../../scripts/export-host-ca.py' 2>/dev/null | python3 -I -c \"import re,sys; c=lambda t: set(re.findall(r'-----BEGIN CERTIFICATE-----[^-]+', t)); h=c(sys.stdin.read()); sys.exit(1 if h & c(open('$OUT.ca').read()) else 0)\""
rm -f "$OUT.ca"
# Bundles in nested directories (/etc/ssl/certs and /etc/ssl) both get the
# Mac's roots, and a mount the build puts on the CA directory is left alone.
check "build: nested CA bundle dirs" bash -c "printf 'FROM python:3.12-slim\nRUN touch /etc/ssl/ca-bundle.pem\nRUN tail -c \$(stat -c %%s /etc/ssl/ca-bundle.pem) /etc/ssl/certs/ca-certificates.crt | cmp - /etc/ssl/ca-bundle.pem\n' | ${D[*]} build -q --no-cache -"
check "build: RUN --mount on CA dir kept" bash -c "printf 'FROM python:3.12-slim\nRUN --mount=type=tmpfs,target=/etc/ssl/certs test -z \"\$(ls -A /etc/ssl/certs)\"\n' | ${D[*]} build -q --no-cache -"
# The guest's own wrapper against hostile build steps: symlinked or swapped CA
# directories, a full disk, host roots copied around (#24).
check "orbit-runc: hostile build steps" bash -c "${D[*]} run --rm -i --privileged --pid=host alpine:3.22 nsenter -t 1 -m -- sh -s /etc/orbit/orbit-runc < '$HERE/orbit-runc-test.sh'"

# 11. alembic upgrade head against the compose postgres
check "alembic upgrade head" "${D[@]}" run --rm --network orbittest_default \
  -e DATABASE_URL=postgresql+psycopg://postgres:postgres@postgres:5432/app orbittest/app
check "migrated table exists" bash -c "${C[*]} exec -T postgres psql -U postgres -d app -tAc \"select to_regclass('items')\" | grep -q items"

# 12. everyday commands
check "compose logs" bash -c "${C[*]} logs --tail 5 postgres | grep -q ."
check "compose exec" "${C[@]}" exec -T redis redis-cli ping
check "docker stats" "${D[@]}" stats --no-stream
"${D[@]}" rm -f orbittest-clock >/dev/null 2>&1
"${D[@]}" run -d --name orbittest-clock alpine:3.22 sleep 60 >/dev/null
t0=$(now); g=$("${D[@]}" exec orbittest-clock date +%s); t1=$(now)
skew=$(python3 -c "print(abs($g - ($t0+$t1)/2) // 1)")
"${D[@]}" rm -f orbittest-clock >/dev/null 2>&1
if python3 -c "import sys; sys.exit(0 if $skew <= 1 else 1)"; then report PASS "guest clock skew" "${skew}s"; else report FAIL "guest clock skew" "${skew}s"; fi

"${C[@]}" down -v >/dev/null 2>&1
"${D[@]}" rmi orbittest/app orbittest/ca >/dev/null 2>&1
echo "== $LABEL: $(grep -c ^PASS "$OUT") pass, $(grep -c ^FAIL "$OUT") fail"
