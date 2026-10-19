#!/bin/sh
# Respawned by init. Logs go to the host console log.
# daemon.json: runc wrapper as default runtime (host CA injection, see
# orbit-runc), host-gateway → the Mac, for `extra_hosts: host-gateway`, and
# a build cache cap (it otherwise fills the disk). Docker 28+ reads
# defaultKeepStorage as a *reserved* floor, not a cap: the cap is
# defaultMaxUsedSpace, and defaultMinFreeSpace keeps the guest disk from
# filling up (the data disk is a file on your Mac).
mkdir -p /etc/docker
cat > /etc/docker/daemon.json <<JSON
{
  "runtimes": { "orbit": { "path": "/etc/orbit/orbit-runc" } },
  "default-runtime": "orbit",
  "host-gateway-ip": "$(cat /etc/orbit/host-ip 2>/dev/null || echo 10.0.2.2)",
  "group": "root",
  "builder": { "gc": { "enabled": true, "defaultReservedSpace": "2GB", "defaultMaxUsedSpace": "20GB", "defaultMinFreeSpace": "10GB" } }
}
JSON
# docker.sock is root:root 0660 like Docker Desktop/OrbStack, so compose files
# that give containers access with `group_add: ["0"]` work unchanged.
# BuildKit (RUN steps) runs whatever `runc` is first on PATH, bypassing the
# default runtime; put the CA-injecting wrapper there too.
mkdir -p /etc/orbit/bin && ln -sf /etc/orbit/orbit-runc /etc/orbit/bin/runc
export PATH=/etc/orbit/bin:$PATH
exec /usr/bin/dockerd --host unix:///var/run/docker.sock --data-root /var/lib/docker \
  > /dev/hvc0 2>&1
