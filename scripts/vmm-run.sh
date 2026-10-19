#!/bin/bash
# Runs the orbit-vmm VM in the foreground and watches it. vmm-up.sh starts
# this (in the background, or through the launchd watchdog when
# install-watchdog.sh installed it); it is not meant to be run by hand.
#
#  - Hang detection: once the guest has answered, it is pinged over the control
#    socket every 30 s. After 3 missed pings in a row the VMM gets SIGABRT
#    (macOS writes a crash report with every thread's stack), then SIGKILL.
#  - Disk guard: the data disk is a file on the Mac. Below ORBIT_WARN_FREE_GB
#    (10) free on its volume you get a notification; below ORBIT_STOP_FREE_GB
#    (3) the guest is shut down cleanly before the disk fills (a full disk
#    corrupts the guest's ext4), and it isn't restarted.
#  - Every abnormal exit is recorded under <instance dir>/crashes/<time>/
#    (console log, crash report, watchdog log). The 10 newest are kept.
#  - Exit status: 0 = stopped on purpose (guest powered off, `make down`, or
#    nothing to do), so launchd leaves it alone. Anything else makes launchd
#    restart it. More than 3 starts within 10 min stop the restarts.
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
if [ -x "$HERE/orbit-vmm" ]; then VMM="$HERE/orbit-vmm"; else VMM="$HERE/../vmm/target/release/orbit-vmm"; fi
D="$HOME/.orbit/vmm${ORBIT_INSTANCE:+-$ORBIT_INSTANCE}"
# Settings vmm-up.sh saved (launchd starts us without the user's environment).
# shellcheck source=/dev/null
[ -f "$D/config.env" ] && . "$D/config.env"
WLOG="$D/watchdog.log"
log() { echo "[$(date '+%F %T')] $*" >> "$WLOG"; }
# Text goes in as arguments, never into the AppleScript source.
notify() {
  osascript -e 'on run argv' -e 'display notification (item 1 of argv) with title (item 2 of argv)' -e 'end run' \
    "$1" "orbit-vmm${ORBIT_INSTANCE:+ ($ORBIT_INSTANCE)}" >/dev/null 2>&1 || true
}
# A setup problem won't fix itself by restarting: report it and stay stopped.
fail() { log "not starting: $*"; echo "$*" >&2; notify "Not starting: $*"; rm -f "$D/want-running"; exit 0; }

# launchd also runs us at login: start only if the VM was meant to be running.
[ -f "$D/want-running" ] || exit 0
# One supervisor per instance: a second one (say, the agent loading while a
# background VM runs) must not touch the first one's pid files and logs.
old=$(cat "$D/run.pid" 2>/dev/null)
if [ -n "$old" ] && [ "$old" != $$ ] && ps -o command= -p "$old" 2>/dev/null | grep -q 'vmm-run\.sh'; then
  log "another supervisor ($old) is running this instance; exiting"
  exit 0
fi
echo $$ > "$D/run.pid"
: > "$D/warnings"

free_gb() { df -k "$D" | awk 'NR == 2 { print int($4 / 1048576) }'; }
WARN_GB=${ORBIT_WARN_FREE_GB:-10}
STOP_GB=${ORBIT_STOP_FREE_GB:-3}
# The VMM's own guard logs at the same level we notify at, and holds guest
# writes below ORBIT_DISK_FLOOR_MIB (1 GiB), a last resort under STOP_GB.
export ORBIT_DISK_WARN_MIB=${ORBIT_DISK_WARN_MIB:-$(( WARN_GB * 1024 ))}
[ "$(free_gb)" -ge "$STOP_GB" ] || fail "only $(free_gb) GB free on the Mac disk (need ${STOP_GB}+)"

# Crash-loop guard. A start requested by vmm-up.sh resets the count.
now=$(date +%s)
[ -f "$D/manual-start" ] && rm -f "$D/manual-start" "$D/starts"
echo "$now" >> "$D/starts"
awk -v n="$now" '$1 > n - 600' "$D/starts" > "$D/starts.tmp" && mv -f "$D/starts.tmp" "$D/starts"
if [ "$(wc -l < "$D/starts")" -gt 3 ]; then
  log "crash loop: more than 3 starts in 10 min; giving up (see $D/crashes)"
  notify "Crashed repeatedly; not restarting. Run make up after checking ~/.orbit/vmm/crashes."
  rm -f "$D/want-running"
  exit 0
fi

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
  r="$(realpath "$d" 2>/dev/null)" && [ -d "$r" ] || { fail "shared directory $d does not exist"; }
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
  case "$VMM" in "$r"/*) echo "warning: running $VMM from inside shared $r; containers could replace it (use make install)" >> "$D/warnings" ;; esac
done
[ ${#ALLOW[@]} -gt 0 ] || fail "no shared directories (set ORBIT_SHARE_ALLOW)"
printf '%s\n' "${SHARES[@]}" > "$D/shares"

NET_OPTS=()
[ "${ORBIT_HOST_LOOPBACK:-1}" = 0 ] && NET_OPTS+=(--no-host-loopback)
S="$D/docker.sock"
# `make down` may have raced this start.
[ -f "$D/want-running" ] || { log "stop requested before start"; exit 0; }
# Keep the previous run's console (vmm-up.sh used to overwrite it).
[ -s "$D/console.log" ] && mv -f "$D/console.log" "$D/console.prev.log"
G="${ORBIT_GUEST:-$HOME/.orbit/guest}"
"$VMM" --kernel "$G/kernel" --initrd "$G/initramfs" \
  --mem "${ORBIT_MEM_MIB:-8192}" --cpus "${ORBIT_CPUS:-$(sysctl -n hw.ncpu)}" --net ${NET_OPTS[@]+"${NET_OPTS[@]}"} --balloon --disk "$D/data.img" \
  --vsock "$S:2375" --vsock "$D/control.sock:1025" --vsock "$D/fsnotify.sock:1026" --share "/Users:users" "${ALLOW[@]}" \
  --cmdline "console=ttyAMA0 panic=-1 page_reporting.page_reporting_order=2 orbit.time=$(date +%s)" \
  > "$D/console.log" 2>&1 < /dev/null &
P=$!
echo "$P" > "$D/vmm.pid"
log "started VMM $P"

# Bounded in size and time (the guest is untrusted): never plain nc here.
ctl() { "$VMM" --control "$D/control.sock" "$1" 2>/dev/null; }
ping_ok() { [ "$(ctl ping)" = pong ]; }

# Stopped by launchd (unload) or a signal: shut the guest down cleanly first.
stopping=0
stop() {
  stopping=1
  log "stop requested; shutting the guest down"
  ctl shutdown >/dev/null
  for _ in $(seq 1 60); do kill -0 "$P" 2>/dev/null || return; sleep 0.5; done
  kill -TERM "$P" 2>/dev/null
}
trap stop TERM INT

watch() {
  # Grace period: the first boot installs packages and can take minutes.
  for _ in $(seq 1 120); do ping_ok && break; kill -0 "$P" 2>/dev/null || return; sleep 5; done
  local misses=0 last now gb warned=0
  last=$(date +%s)
  while kill -0 "$P" 2>/dev/null; do
    sleep 30
    gb=$(free_gb)
    if [ "$gb" -lt "$STOP_GB" ]; then
      log "Mac disk has ${gb} GB free (< ${STOP_GB}); shutting the guest down"
      notify "Mac disk almost full (${gb} GB free): the VM was shut down to protect its data. Free some space, then make up."
      ctl shutdown >/dev/null
      return
    elif [ "$gb" -lt "$WARN_GB" ]; then
      [ $warned = 1 ] || { log "Mac disk has ${gb} GB free"; notify "Mac disk is getting full (${gb} GB free). docker system prune frees space."; }
      warned=1
    else
      warned=0
    fi
    now=$(date +%s)
    # A long gap means the Mac slept; the guest needs a moment after waking.
    [ $((now - last)) -gt 90 ] && misses=0
    last=$now
    if ping_ok; then misses=0; continue; fi
    misses=$((misses + 1))
    log "guest did not answer ping ($misses/3)"
    if [ $misses -ge 3 ]; then
      log "guest hung; aborting VMM $P for a crash report"
      kill -ABRT "$P" 2>/dev/null; sleep 10; kill -KILL "$P" 2>/dev/null
      return
    fi
  done
}
watch &
W=$!

# `wait` returns early when a trapped signal arrives; keep waiting for the VMM.
while :; do
  wait "$P"; rc=$?
  kill -0 "$P" 2>/dev/null || break
done
touch "$D/.exited"   # this run's crash report is the one written after this
kill "$W" 2>/dev/null; wait "$W" 2>/dev/null

if [ $stopping = 1 ] || [ $rc = 0 ]; then
  log "VMM $P stopped (status $rc)"
  rm -f "$D/want-running"
  exit 0
fi

# Unexpected: keep the evidence, then let launchd restart us.
C="$D/crashes/$(date +%Y%m%d-%H%M%S)"
mkdir -p "$C"
cp "$D/console.log" "$C/console.log" 2>/dev/null
tail -n 50 "$WLOG" > "$C/watchdog.log" 2>/dev/null
# macOS writes the crash report (.ips) a few seconds after the process dies.
if [ "$rc" -gt 128 ]; then
  for _ in $(seq 1 30); do
    ips=$(find "$HOME/Library/Logs/DiagnosticReports" -name 'orbit-vmm*.ips' -newer "$D/.exited" 2>/dev/null | head -1)
    [ -n "$ips" ] && { cp "$ips" "$C/"; break; }
    sleep 0.5
  done
fi
find "$D/crashes" -mindepth 1 -maxdepth 1 -type d | sort -r | tail -n +11 | while read -r old; do rm -rf "$old"; done
case $rc in
  3) why="the guest rebooted (kernel panic?)" ;;
  13[0-9]|14[0-9]) why="the VMM was killed by signal $((rc - 128))" ;;
  *) why="the VMM exited with status $rc" ;;
esac
log "unexpected stop: $why; details in $C"
if [ -n "${ORBIT_WATCHDOG:-}" ]; then
  notify "Stopped unexpectedly: $why. Restarting; containers were stopped."
else
  notify "Stopped unexpectedly: $why. Run make up to start it again."
fi
exit "$rc"
