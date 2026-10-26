#!/bin/bash
# (Re)starts the orbit-vmm Docker VM and waits until Docker answers.
#   DOCKER_HOST=unix://$HOME/.orbit/vmm/docker.sock
# `vmm-up.sh --stop` shuts it down (and keeps the watchdog from restarting it).
# Normally run as ~/.orbit/bin/vmm-up.sh (make install / orbit up). The VM runs
# under vmm-run.sh, through the launchd watchdog if install-watchdog.sh set it up.
# Env: ORBIT_MEM_MIB (default 8192; unused memory is returned to macOS), ORBIT_CPUS (default: the Mac's core count), ORBIT_DISK_GB,
#      ORBIT_GUEST (kernel + initramfs directory, default ~/.orbit/guest),
#      ORBIT_SHARE_ALLOW, ORBIT_HOST_LOOPBACK=0 (containers can't reach the Mac's
#      127.0.0.1 services), ORBIT_INSTANCE (run more than one VM: each instance
#      gets its own ~/.orbit/vmm-<name> with its own disk and sockets).
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
if [ -x "$HERE/orbit-vmm" ]; then VMM="$HERE/orbit-vmm"; else VMM="$HERE/../vmm/target/release/orbit-vmm"; fi
D="$HOME/.orbit/vmm${ORBIT_INSTANCE:+-$ORBIT_INSTANCE}"
LABEL="com.orbit.vmm${ORBIT_INSTANCE:+.$ORBIT_INSTANCE}"
# Owner-only: the sockets here are root in the VM.
mkdir -p "$HOME/.orbit" "$D"
chmod 700 "$HOME/.orbit" "$D"

# Ask a running VM to power off cleanly; never start a second one on the same disk.
stop_vm() {
  rm -f "$D/want-running"   # tells the watchdog this stop is on purpose
  if [ -S "$D/control.sock" ]; then "$VMM" --control "$D/control.sock" shutdown >/dev/null 2>&1; fi
  local old sup
  old=$(cat "$D/vmm.pid" 2>/dev/null)
  sup=$(cat "$D/run.pid" 2>/dev/null)
  # The guest gives dockerd 45 s; allow 75 s for the whole power-off.
  for _ in $(seq 1 150); do kill -0 "$old" 2>/dev/null || break; sleep 0.5; done
  if [ -n "$old" ] && kill -0 "$old" 2>/dev/null; then echo "VM $old is still running" >&2; return 1; fi
  # ...and its supervisor finishing up (a new start must not race it).
  for _ in $(seq 1 40); do kill -0 "$sup" 2>/dev/null || return 0; sleep 0.25; done
}
if [ "${1:-}" = --stop ]; then stop_vm && echo "stopped"; exit; fi

# Sparse: only what Docker actually stores takes space on the Mac.
[ -f "$D/data.img" ] || mkfile -n "${ORBIT_DISK_GB:-64}g" "$D/data.img"
read -r free_k < <(df -k "$D" | awk 'NR == 2 { print $4 }')
used_k=$(du -k "$D/data.img" | awk '{ print $1 }')
echo "data disk $D/data.img: $(( used_k >> 20 )) GiB used of $(( $(stat -f %z "$D/data.img") >> 30 )) GiB; Mac disk has $(( free_k >> 20 )) GiB free"

# Settings for vmm-run.sh, which may be started later by launchd without them.
if [ -n "${ORBIT_SHARE_ALLOW:-}" ]; then
  abs=""
  IFS=: read -ra dirs <<< "$ORBIT_SHARE_ALLOW"
  for d in "${dirs[@]}"; do
    [ -n "$d" ] || continue
    case "$d" in /*) ;; *) d="$PWD/$d" ;; esac
    abs="${abs:+$abs:}$d"
  done
  ORBIT_SHARE_ALLOW=$abs
fi
for v in ORBIT_MEM_MIB ORBIT_CPUS ORBIT_GUEST ORBIT_SHARE_ALLOW ORBIT_HOST_LOOPBACK ORBIT_WARN_FREE_GB ORBIT_STOP_FREE_GB ORBIT_DISK_WARN_MIB ORBIT_DISK_FLOOR_MIB ORBIT_DISK_PAUSE_SECS ORBIT_PRESSURE; do
  [ -n "${!v:-}" ] && printf '%s=%q\n' "$v" "${!v}"
done > "$D/config.env"

stop_vm || exit 1
touch "$D/want-running" "$D/manual-start"
if launchctl print "gui/$(id -u)/$LABEL" >/dev/null 2>&1; then
  launchctl kickstart "gui/$(id -u)/$LABEL"
else
  nohup "$HERE/vmm-run.sh" >/dev/null 2>&1 &
fi

S="$D/docker.sock"
for _ in $(seq 1 600); do
  if curl -s -m 2 --unix-socket "$S" http://d/_ping 2>/dev/null | grep -q OK; then
    cat "$D/warnings" >&2 2>/dev/null
    echo "docker ready: export DOCKER_HOST=unix://$S"
    exit 0
  fi
  [ -f "$D/want-running" ] || { echo "VM did not start:" >&2; tail -n 3 "$D/watchdog.log" >&2; echo "see $D/console.log" >&2; exit 1; }
  sleep 0.2
done
echo "docker did not come up; see $D/console.log" >&2; exit 1
