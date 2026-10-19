#!/bin/sh
# One command per vsock connection on port 1025, from the host.
read -r cmd
case "$cmd" in
  ping) echo pong ;;
  # TCP ports listening on all addresses (docker-proxy for published ports);
  # the VMM forwards each one from the Mac's localhost.
  ports)
    for f in /proc/net/tcp /proc/net/tcp6; do
      [ -r $f ] || continue
      # local_address is HEXIP:HEXPORT; state 0A is LISTEN.
      sed -n 's/^ *[0-9]*: \(0*\):\([0-9A-F]*\) [0-9A-F:]* 0A .*/\2/p' $f
    done | while read -r hex; do echo $((0x$hex)); done | sort -un ;;
  # Guest wall clock as <sec> <usec>; the VMM brackets this with its own
  # clock to measure the offset (and the round trip, to ignore slow answers).
  now) adjtimex | awk '/tv_sec/{s=$2} /tv_usec/{u=$2} END{print s, u}' ;;
  # Sent by the VMM exactly on a host second boundary, only after "now" showed
  # a real offset. busybox date only sets whole seconds, which is why.
  time\ *) date -s "@${cmd#time }" >/dev/null && echo ok ;;
  # macOS memory pressure (normal|warn|critical), sent by the VMM when it
  # changes and every 30 s while it lasts. memd keeps the page cache under a
  # lower cap meanwhile; on a change, also give memory back right away
  # (detached, so the reply isn't held up).
  pressure\ normal | pressure\ warn | pressure\ critical)
    level=${cmd#pressure }
    old=$(cat /run/orbit-pressure 2>/dev/null || echo normal)
    if [ "$level" = normal ]; then rm -f /run/orbit-pressure; else echo "$level" > /run/orbit-pressure; fi
    [ "$level" = "$old" ] || setsid /etc/orbit/memd.sh reclaim "$level" </dev/null >/dev/null 2>&1 &
    echo ok ;;
  meminfo) awk '/^(MemTotal|MemAvailable):/{print $1, $2}' /proc/meminfo ;;
  shutdown) echo ok; poweroff ;;
  *) echo "unknown: $cmd" ;;
esac
