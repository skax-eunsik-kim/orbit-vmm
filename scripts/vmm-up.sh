#!/bin/bash
# (Re)starts the orbit-vmm Docker VM and waits until Docker answers.
#   DOCKER_HOST=unix://$HOME/.orbit/vmm/docker.sock
# Normally run as ~/.orbit/bin/vmm-up.sh (make install / make up), next to the
# installed orbit-vmm; run from a checkout it uses that checkout's build.
# Env: ORBIT_MEM_MIB (default 8192; unused memory is returned to macOS), ORBIT_CPUS (default 4), ORBIT_DISK_GB,
#      ORBIT_SHARE_ALLOW, ORBIT_HOST_LOOPBACK=0 (containers can't reach the Mac's
#      127.0.0.1 services), ORBIT_INSTANCE (run more than one VM: each instance
#      gets its own ~/.orbit/vmm-<name> with its own disk and sockets).
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
if [ -x "$HERE/orbit-vmm" ]; then VMM="$HERE/orbit-vmm"; else VMM="$HERE/../vmm/target/release/orbit-vmm"; fi
D="$HOME/.orbit/vmm${ORBIT_INSTANCE:+-$ORBIT_INSTANCE}"
# Owner-only: the sockets here are root in the VM.
mkdir -p "$HOME/.orbit" "$D"
chmod 700 "$HOME/.orbit" "$D"
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
# Containers can write to shared directories. Sharing your home folder (or
# anything containing ~/.orbit, ~/Library or your shell profile) would let any
# container run code on the Mac, so refuse it.
for d in "${dirs[@]}"; do
  [ -n "$d" ] || continue
  r="$(cd "$d" 2>/dev/null && pwd -P)" || { echo "shared directory $d does not exist" >&2; exit 1; }
  case "$(cd "$HOME" && pwd -P)/" in "${r%/}"/*) echo "refusing to share $d: it contains your home directory" >&2; exit 1 ;; esac
  case "$VMM" in "$r"/*) echo "warning: running $VMM from inside shared $d; containers could replace it (use make install)" >&2 ;; esac
done
printf '%s\n' "${dirs[@]}" | grep . > "$D/shares"

NET_OPTS=()
[ "${ORBIT_HOST_LOOPBACK:-1}" = 0 ] && NET_OPTS+=(--no-host-loopback)
S="$D/docker.sock"
nohup "$VMM" --kernel "$HOME/.orbit/guest/kernel" --initrd "$HOME/.orbit/guest/initramfs" \
  --mem "${ORBIT_MEM_MIB:-8192}" --cpus "${ORBIT_CPUS:-4}" --net ${NET_OPTS[@]+"${NET_OPTS[@]}"} --balloon --disk "$D/data.img" \
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
