#!/bin/sh
# Stage 2 (busybox init sysinit): mounts, modules, network, data disk, packages.
export PATH=/usr/sbin:/usr/bin:/sbin:/bin
log() { echo "[orbit] $*" > /dev/hvc0; }

mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev
# Console: hvc0 under Virtualization.framework, the PL011 (ttyAMA0) under orbit-vmm.
[ -e /dev/hvc0 ] || ln -sf ttyAMA0 /dev/hvc0
mkdir -p /dev/pts /dev/shm
mount -t devpts devpts /dev/pts
mount -t tmpfs -o mode=1777 tmpfs /dev/shm
mount -t tmpfs -o mode=0755 tmpfs /run
mount -t cgroup2 cgroup2 /sys/fs/cgroup
ln -sf /run /var/run

# Host clock is passed on the kernel command line (orbit.time=<epoch>).
for arg in $(cat /proc/cmdline); do
  case "$arg" in orbit.time=*) date -s "@${arg#orbit.time=}" >/dev/null ;; esac
done

hostname orbit
log "loading modules"
for m in virtio_mmio virtio_net virtio_blk virtio_console virtio_balloon virtiofs \
         vmw_vsock_virtio_transport ext4 overlay \
         bridge br_netfilter veth nf_tables nft_compat nf_nat nf_conntrack \
         xt_conntrack xt_MASQUERADE xt_addrtype iptable_nat iptable_filter \
         ip6table_nat ip6table_filter; do
  modprobe -q "$m" || log "modprobe $m failed"
done
sysctl -qw net.ipv4.ip_forward=1

log "network"
ip link set lo up
ip link set eth0 up
udhcpc -i eth0 -q -t 10 -s /etc/orbit/udhcpc.sh >/dev/null 2>&1 || log "dhcp failed"
log "$(ip -4 -o addr show eth0 | awk '{print $4}') via $(ip route | awk '/default/{print $3}'), dns $(awk '{print $2}' /etc/resolv.conf | tr '\n' ' ')"
cat /etc/orbit/host-ca.pem >> /etc/ssl/certs/ca-certificates.crt
# Containers resolve through dnsmasq on the guest's own address (dns.sh), which
# adds host.docker.internal; keep the DHCP-provided server as its upstream.
awk '/^nameserver/{print $2; exit}' /etc/resolv.conf > /etc/orbit/upstream-dns
ip -4 -o addr show eth0 | awk '{split($4, a, "/"); print a[1]}' > /etc/orbit/guest-ip
ip route | awk '/default/{print $3; exit}' > /etc/orbit/host-ip

log "data disk"
mount_data() { mount -t ext4 -o noatime /dev/vda /data 2>/dev/null; }
# Stage 1 already mounted it when booting from the root snapshot. Only ever
# format a disk that has no filesystem at all.
if ! mountpoint -q /data && ! mount_data; then
  if blkid /dev/vda 2>/dev/null | grep -q TYPE=; then
    log "data disk has a filesystem but will not mount; refusing to format"
    exit 1
  fi
  apk add -q e2fsprogs > /dev/hvc0 2>&1 || { log "cannot install e2fsprogs"; exit 1; }
  mkfs.ext4 -q -L orbit-data -E lazy_itable_init=1,lazy_journal_init=1 /dev/vda
  mount_data || { log "data disk mount failed"; exit 1; }
fi
# The data disk image can be grown on the Mac (see README); take the space
# before anything (packages, the root snapshot) needs it.
command -v resize2fs >/dev/null && resize2fs /dev/vda >/dev/null 2>&1
mkdir -p /data/apk-cache /data/docker /var/lib/docker
ln -sfn /data/apk-cache /etc/apk/cache
mount --bind /data/docker /var/lib/docker

if [ -e /etc/orbit/booted-from-snapshot ]; then
  log "packages: from read-only root snapshot"
else
  log "packages"
  # Docker comes from v3.23 (29.5.x): 29.8.x in v3.24 keys BuildKit's cache on the
  # base image's index digest, which provenance changes on every build, so warm
  # rebuilds redo every layer on top of a locally built base (29.4/29.5 do not).
  grep -q '^@docker ' /etc/apk/repositories \
    || sed -n 's|^\(.*/alpine/\)v[0-9.]*/main$|@docker \1v3.23/community|p' /etc/apk/repositories | head -1 >> /etc/apk/repositories
  # Served from /data/apk-cache after the first boot, so later boots work offline.
  apk add -q docker@docker docker-engine@docker docker-cli@docker socat iptables ip6tables e2fsprogs e2fsprogs-extra setpriv dnsmasq jq qemu-x86_64 > /dev/hvc0 2>&1 || log "apk add failed"
  # ca-certificates (pulled in by docker) regenerates the bundle; persist the host roots through it.
  cp /etc/orbit/host-ca.pem /usr/local/share/ca-certificates/host-ca.crt
  update-ca-certificates >/dev/null 2>&1
  grep -q "$(sed -n 2p /etc/orbit/host-ca.pem)" /etc/ssl/certs/ca-certificates.crt \
    || cat /etc/orbit/host-ca.pem >> /etc/ssl/certs/ca-certificates.crt
  resize2fs /dev/vda >/dev/null 2>&1  # first boot: e2fsprogs only now exists
  /etc/orbit/snapshot.sh && log "saved read-only root snapshot for next boot"
fi

# Compressed swap in RAM: under memory pressure the guest swaps cold pages
# instead of thrashing page cache (which froze the VM in testing). Half of RAM.
if modprobe -q zram && [ -e /sys/block/zram0 ]; then
  echo lz4 > /sys/block/zram0/comp_algorithm 2>/dev/null
  awk '/MemTotal/{printf "%dK", $2/2}' /proc/meminfo > /sys/block/zram0/disksize
  mkswap /dev/zram0 >/dev/null && swapon -p 100 /dev/zram0 && sysctl -qw vm.swappiness=60
fi
/etc/orbit/binfmt.sh && [ -e /proc/sys/fs/binfmt_misc/qemu-x86_64 ] && log "amd64 emulation: qemu-x86_64"
mount -t virtiofs users /Users || log "virtiofs /Users mount failed"
# From here on resolve through dnsmasq (dns.sh, started by init right after this).
echo "nameserver $(cat /etc/orbit/guest-ip)" > /etc/resolv.conf
log "ready"
