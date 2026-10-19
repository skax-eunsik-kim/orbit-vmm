#!/bin/bash
# (Re)starts the orbit-vmm Docker VM and waits until Docker answers.
#   DOCKER_HOST=unix://$HOME/.orbit/vmm/docker.sock
# Normally run as ~/.orbit/bin/vmm-up.sh (make install / make up), next to the
# installed orbit-vmm; run from a checkout it uses that checkout's build.
# Env: ORBIT_MEM_MIB (default 8192; unused memory is returned to macOS), ORBIT_CPUS (default: the Mac's core count), ORBIT_DISK_GB,
#      ORBIT_GUEST (kernel + initramfs directory, default ~/.orbit/guest),
#      ORBIT_SHARE_ALLOW, ORBIT_HOST_LOOPBACK=0 (containers can't reach the Mac's
#      127.0.0.1 services), ORBIT_INSTANCE (run more than one VM: each instance
#      gets its own ~/.orbit/vmm-<name> with its own disk and sockets),
#      ORBIT_DISK (data disk image, default <instance dir>/data.img),
#      ORBIT_DISK_WARN_MIB / ORBIT_DISK_FLOOR_MIB / ORBIT_DISK_PAUSE_SECS (see
#      README "Data disk"), ORBIT_IGNORE_DISK_SPACE=1 (start even when the Mac's
#      disk is nearly full).
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
if [ -x "$HERE/orbit-vmm" ]; then VMM="$HERE/orbit-vmm"; else VMM="$HERE/../vmm/target/release/orbit-vmm"; fi
D="$HOME/.orbit/vmm${ORBIT_INSTANCE:+-$ORBIT_INSTANCE}"
# Owner-only: the sockets here are root in the VM.
mkdir -p "$HOME/.orbit" "$D"
chmod 700 "$HOME/.orbit" "$D"
# Sparse: only what Docker actually stores takes space on the Mac.
IMG="${ORBIT_DISK:-$D/data.img}"
[ -f "$IMG" ] || mkfile -n "${ORBIT_DISK_GB:-64}g" "$IMG"

# The image grows as the guest writes; a full Mac disk stalls (and after
# ORBIT_DISK_PAUSE_SECS corrupts) the VM, so don't start close to that.
read -r total_k free_k < <(df -k "$IMG" | awk 'NR == 2 { print $2, $4 }')
used_k=$(du -k "$IMG" | awk '{ print $1 }')
size_k=$(( $(stat -f %z "$IMG") / 1024 ))
warn_k=$(( ${ORBIT_DISK_WARN_MIB:-0} * 1024 ))
if [ "$warn_k" = 0 ]; then
  warn_k=$(( total_k / 50 )); [ $warn_k -gt 5242880 ] && warn_k=5242880
  floor2_k=$(( ${ORBIT_DISK_FLOOR_MIB:-1024} * 2048 )); [ $warn_k -lt $floor2_k ] && warn_k=$floor2_k
fi
echo "data disk $IMG: $(( used_k >> 20 )).$(( (used_k * 10 >> 20) % 10 )) GiB used of $(( size_k >> 20 )) GiB; Mac disk has $(( free_k >> 20 )).$(( (free_k * 10 >> 20) % 10 )) GiB free"
if [ "$free_k" -lt "$warn_k" ]; then
  echo "the Mac's disk has only $(( free_k >> 10 )) MiB free (need $(( warn_k >> 10 )) MiB); the data disk can grow up to $(( (size_k - used_k) >> 20 )) GiB more." >&2
  echo "free space on the Mac (or set ORBIT_IGNORE_DISK_SPACE=1)" >&2
  [ "${ORBIT_IGNORE_DISK_SPACE:-0}" = 1 ] || exit 1
fi

# Ask a running VM to power off cleanly, and never start a second one on the same disk.
if [ -S "$D/control.sock" ]; then echo shutdown | nc -U -w 3 "$D/control.sock" >/dev/null 2>&1; fi
OLD=$(cat "$D/vmm.pid" 2>/dev/null)
for i in $(seq 1 120); do kill -0 "$OLD" 2>/dev/null || break; sleep 0.5; done
if [ -n "$OLD" ] && kill -0 "$OLD" 2>/dev/null; then echo "previous VM $OLD still running; not starting a second one" >&2; exit 1; fi

# Directories containers may bind-mount (everything else in /Users is hidden).
if [ -z "${ORBIT_SHARE_ALLOW:-}" ]; then
  for d in work src code dev projects Developer; do [ -d "$HOME/$d" ] && ORBIT_SHARE_ALLOW="${ORBIT_SHARE_ALLOW:+$ORBIT_SHARE_ALLOW:}$HOME/$d"; done
fi
# The VMM resolves each entry itself and refuses your home directory, hidden
# directories, ~/Library, nested entries, and paths that resolve through a
# symlink inside a shared directory (containers can write to those).
ALLOW=(); LIT=(); REAL=()
IFS=: read -ra dirs <<< "${ORBIT_SHARE_ALLOW:-}"
for d in "${dirs[@]}"; do
  [ -n "$d" ] || continue
  case "$d" in /*) ;; *) d="$PWD/$d" ;; esac
  r="$(realpath "$d" 2>/dev/null)" && [ -d "$r" ] || { echo "shared directory $d does not exist" >&2; exit 1; }
  LIT+=("$d"); REAL+=("$r")
done
# One directory inside another is already shared by the outer one (and the VMM
# refuses nested entries), so keep only the outermost.
SHARES=()
for ((k = 0; k < ${#REAL[@]}; k++)); do
  r=${REAL[k]}
  inner=0
  for o in "${REAL[@]}"; do
    lr=$(printf %s "$r" | tr '[:upper:]' '[:lower:]'); lo=$(printf %s "$o" | tr '[:upper:]' '[:lower:]')
    case "$lr" in "$lo"/*) inner=1 ;; esac
  done
  [ $inner = 1 ] && continue
  case " ${SHARES[*]-} " in *" $r "*) continue ;; esac
  ALLOW+=(--share-allow "${LIT[k]}"); SHARES+=("$r")
  case "$VMM" in "$r"/*) echo "warning: running $VMM from inside shared $r; containers could replace it (use make install)" >&2 ;; esac
done
if [ ${#ALLOW[@]} -eq 0 ]; then echo "no shared directories (set ORBIT_SHARE_ALLOW)" >&2; exit 1; fi
printf '%s\n' "${SHARES[@]}" > "$D/shares"
printf '%s\n' "$IMG" > "$D/disk"

NET_OPTS=()
[ "${ORBIT_HOST_LOOPBACK:-1}" = 0 ] && NET_OPTS+=(--no-host-loopback)
S="$D/docker.sock"
G="${ORBIT_GUEST:-$HOME/.orbit/guest}"
nohup "$VMM" --kernel "$G/kernel" --initrd "$G/initramfs" \
  --mem "${ORBIT_MEM_MIB:-8192}" --cpus "${ORBIT_CPUS:-$(sysctl -n hw.ncpu)}" --net ${NET_OPTS[@]+"${NET_OPTS[@]}"} --balloon --disk "$IMG" \
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
