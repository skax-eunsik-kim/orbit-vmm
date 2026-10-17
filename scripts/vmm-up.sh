#!/bin/bash
# (Re)starts the orbit-vmm Docker VM and waits until Docker answers.
#   DOCKER_HOST=unix://$HOME/.orbit/vmm/docker.sock
# Env: ORBIT_MEM_MIB (default 8192; unused memory is returned to macOS), ORBIT_CPUS (default 4), ORBIT_DISK_GB,
#      ORBIT_SHARE_ALLOW, ORBIT_INSTANCE (run more than one VM: each instance
#      gets its own ~/.orbit/vmm-<name> with its own disk and sockets).
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
D="$HOME/.orbit/vmm${ORBIT_INSTANCE:+-$ORBIT_INSTANCE}"
mkdir -p "$D"
# Sparse: only what Docker actually stores takes space on the Mac.
[ -f "$D/data.img" ] || mkfile -n "${ORBIT_DISK_GB:-64}g" "$D/data.img"

# Ask a running VM to power off cleanly, and never start a second one on the same disk.
if [ -S "$D/control.sock" ]; then echo shutdown | nc -U -w 3 "$D/control.sock" >/dev/null 2>&1; fi
OLD=$(cat "$D/vmm.pid" 2>/dev/null)
for i in $(seq 1 120); do kill -0 "$OLD" 2>/dev/null || break; sleep 0.5; done
if [ -n "$OLD" ] && kill -0 "$OLD" 2>/dev/null; then echo "previous VM $OLD still running; not starting a second one" >&2; exit 1; fi

# Directories containers may bind-mount (everything else in /Users is hidden).
if [ -z "${ORBIT_SHARE_ALLOW:-}" ]; then
  for d in work src code dev projects Developer; do [ -d "$HOME/$d" ] && ORBIT_SHARE_ALLOW="${ORBIT_SHARE_ALLOW:+$ORBIT_SHARE_ALLOW:}$HOME/$d"; done
fi
ALLOW=()
IFS=: read -ra dirs <<< "${ORBIT_SHARE_ALLOW:-}"
for d in "${dirs[@]}"; do [ -n "$d" ] && ALLOW+=(--share-allow "$d"); done
if [ ${#ALLOW[@]} -eq 0 ]; then echo "no shared directories (set ORBIT_SHARE_ALLOW)" >&2; exit 1; fi

S="$D/docker.sock"
nohup "$ROOT/vmm/target/release/orbit-vmm" --kernel "$HOME/.orbit/guest/kernel" --initrd "$HOME/.orbit/guest/initramfs" \
  --mem "${ORBIT_MEM_MIB:-8192}" --cpus "${ORBIT_CPUS:-4}" --net --balloon --disk "$D/data.img" \
  --vsock "$S:2375" --vsock "$D/control.sock:1025" --vsock "$D/fsnotify.sock:1026" --share "/Users:users" "${ALLOW[@]}" \
  --cmdline "console=ttyAMA0 panic=-1 page_reporting.page_reporting_order=2 orbit.time=$(date +%s)" \
  > "$D/console.log" 2>&1 < /dev/null &
echo $! > "$D/vmm.pid"
for i in $(seq 1 600); do
  curl -s --unix-socket "$S" http://d/_ping 2>/dev/null | grep -q OK && { echo "docker ready: export DOCKER_HOST=unix://$S"; exit 0; }
  kill -0 $! 2>/dev/null || { echo "VM exited; see $D/console.log" >&2; exit 1; }
  sleep 0.2
done
echo "docker did not come up; see $D/console.log" >&2; exit 1
