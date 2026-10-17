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
  meminfo) awk '/^(MemTotal|MemAvailable):/{print $1, $2}' /proc/meminfo ;;
  shutdown) echo ok; poweroff ;;
  *) echo "unknown: $cmd" ;;
esac
