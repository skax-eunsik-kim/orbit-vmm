#!/bin/bash
# How much freed-but-fragmented guest memory stays on the Mac.
# 24 containers hold 150 MiB each, every other one exits (freeing memory in an
# interleaved pattern), then all exit. At each step: Mac phys_footprint vs.
# guest used+cache; "gap" is memory the Mac still holds beyond what the guest
# uses. Usage: memfrag.sh <label>
set -u
LABEL=$1
export DOCKER_HOST=unix://$HOME/.orbit/vmm/docker.sock
P=$(cat ~/.orbit/vmm/vmm.pid)
fp() { footprint -p $P 2>/dev/null | awk '/phys_footprint:/{v=$2; if($3=="GB")v*=1024; printf "%d", v; exit}'; }
guest() { python3 -I -c "
import socket
s=socket.socket(socket.AF_UNIX); s.settimeout(3); s.connect('$HOME/.orbit/vmm/control.sock'); s.sendall(b'meminfo\n')
d=dict(l.split() for l in s.recv(4096).decode().split('\n') if l)
print((int(d['MemTotal:'])-int(d['MemAvailable:']))//1024)"; }
row() { local g=$(guest) f=$(fp); printf '  %-28s mac=%5s MiB  guest_used=%5s MiB  gap=%5s MiB\n' "$1" "$f" "$g" "$((f - g))"; }
echo "== $LABEL"
for i in $(seq 1 24); do docker rm -f frag$i >/dev/null 2>&1; done
sleep 20; row "start"
for i in $(seq 1 24); do
  docker run -d --name frag$i alpine:3.22 sh -c 'v=$(head -c 150m /dev/zero | tr "\\0" x); sleep 3600; :' >/dev/null
done
sleep 30; row "24 x 150 MiB held"
for i in $(seq 1 2 24); do docker rm -f frag$i >/dev/null; done
sleep 60; row "every other freed (+60s)"
for i in $(seq 2 2 24); do docker rm -f frag$i >/dev/null; done
sleep 60; row "all freed (+60s)"
sleep 60; row "all freed (+120s)"
