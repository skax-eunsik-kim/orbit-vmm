#!/bin/sh
# Respawned by init: DNS for the guest and its containers. Answers
# host.docker.internal / gateway.docker.internal with the Mac's address
# (the NAT gateway: slirp's 10.0.2.2 or Virtualization.framework's 192.168.64.1)
# and forwards everything else to the DHCP-provided resolver.
# --dns-forward-max: dnsmasq's default of 150 uncached queries in flight is
# below what parallel builds send (#10); past it every extra query fails at
# once with SERVFAIL, which resolvers report as "Temporary failure in name
# resolution". 4096 is far above what a busy Mac sends.
host=$(cat /etc/orbit/host-ip 2>/dev/null)
guest=$(cat /etc/orbit/guest-ip 2>/dev/null)
upstream=$(cat /etc/orbit/upstream-dns 2>/dev/null)
[ -n "$host" ] && [ -n "$guest" ] && [ -n "$upstream" ] || { sleep 5; exit 1; }
exec /usr/sbin/dnsmasq --keep-in-foreground --user=root --listen-address="$guest" --bind-interfaces \
  --no-resolv --no-hosts --server="$upstream" --cache-size=1000 --dns-forward-max=4096 \
  --address=/host.docker.internal/"$host" --address=/gateway.docker.internal/"$host" \
  > /dev/hvc0 2>&1
