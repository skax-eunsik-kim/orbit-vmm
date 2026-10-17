#!/bin/sh
# Saves the fully installed root (Alpine + Docker + kernel modules) to the
# data disk. Next boot, stage 1 mounts it read-only under a tmpfs overlay, so
# root files live in ordinary, reclaimable page cache instead of tmpfs (which
# can never be dropped or returned to the host), and package installs are skipped.
set -e
set -o pipefail
dst=/data/rootfs
rm -rf "$dst.tmp"
mkdir -p "$dst.tmp"
# Copy the root without runtime state. /var/lib/docker is a bind mount of the
# data disk's Docker store and must never be walked: copying it first and
# deleting it afterwards once filled the disk with a copy of every image.
dirs=""
for d in bin etc home lib media mnt opt root sbin srv usr var; do [ -e "/$d" ] && dirs="$dirs $d"; done
tar -C / -cf - \
  --exclude=./var/lib/docker --exclude=./var/run --exclude=./var/cache/apk --exclude=./etc/apk/cache \
  $(for d in $dirs; do echo "./$d"; done) | tar -C "$dst.tmp" -xf -
mkdir -p "$dst.tmp/var/lib/docker"
ln -s /run "$dst.tmp/var/run"
ln -s /data/apk-cache "$dst.tmp/etc/apk/cache"
sync
rm -rf "$dst"
mv "$dst.tmp" "$dst"
sync
