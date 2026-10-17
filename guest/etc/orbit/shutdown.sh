#!/bin/sh
killall dockerd 2>/dev/null
for i in 1 2 3 4 5 6 7 8 9 10; do pidof dockerd >/dev/null || break; sleep 1; done
sync
umount /var/lib/docker /data /Users 2>/dev/null
