#!/bin/bash
# Builds a test initramfs for orbit-vmm: Alpine minirootfs + kernel modules +
# guest/init-<name>. Usage: build-initramfs.sh <name> <out.cpio.gz>
# Uses the downloads cached by ../../scripts/fetch-guest.sh in ~/.orbit/cache.
set -euo pipefail
NAME=$1 OUT=$2
HERE="$(cd "$(dirname "$0")" && pwd)"
CACHE="${ORBIT_HOME:-$HOME/.orbit}/cache"
MINI=$(ls "$CACHE"/alpine-minirootfs-*-aarch64.tar.gz | tail -1)
KAPK=$(ls "$CACHE"/linux-virt-*.apk | tail -1)

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
chmod 755 "$WORK"  # becomes the guest's /; mktemp -d makes it 0700
tar xzf "$MINI" -C "$WORK"
cp "$KAPK" "$WORK/linux-virt.apk"
cp "$HERE/init-$NAME" "$WORK/init"
# Host trust roots (corporate TLS inspection) for HTTPS from the guest.
python3 -I "$HERE/../../scripts/export-host-ca.py" >> "$WORK/etc/ssl/certs/ca-certificates.crt"
chmod 755 "$WORK/init"
(cd "$WORK" && find . | cpio -o -H newc --quiet | gzip -1 > "$OUT")
echo "built $OUT ($(du -h "$OUT" | cut -f1))"
