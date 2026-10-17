#!/bin/sh
# Minimal udhcpc hook: configure address, default route and DNS.
case "$1" in
  bound|renew)
    ip addr flush dev "$interface"
    ip addr add "$ip/${mask:-24}" dev "$interface"
    for r in $router; do ip route replace default via "$r" dev "$interface"; break; done
    : > /etc/resolv.conf
    for d in ${dns:-$router}; do echo "nameserver $d" >> /etc/resolv.conf; done
    ;;
esac
