#!/bin/sh
# dockerd gives containers shutdown-timeout (30 s, daemon.json) to stop, then
# needs a moment itself. The host waits 75 s for the whole power-off.
killall dockerd 2>/dev/null
i=0; while [ $i -lt 45 ] && pidof dockerd >/dev/null; do sleep 1; i=$((i + 1)); done
sync
umount /var/lib/docker /data /Users 2>/dev/null
