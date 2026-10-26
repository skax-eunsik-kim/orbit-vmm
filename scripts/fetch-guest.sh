#!/bin/bash
# Downloads Alpine's linux-virt kernel + minirootfs and builds the guest
# boot artifacts in ~/.orbit/guest:
#   kernel     uncompressed arm64 Image (Virtualization.framework requirement)
#   initramfs  newc cpio.gz = minirootfs + guest/ overlay + linux-virt.apk (modules)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${ORBIT_HOME:-$HOME/.orbit}/guest"
MIRROR="${ALPINE_MIRROR:-https://dl-cdn.alpinelinux.org/alpine}"
ARCH=aarch64
# Pinned so every machine builds the same guest; bump all four together.
# minirootfs sha256 = Alpine's published .sha256; linux-virt sha256 recorded
# when it was pinned (Alpine publishes no per-package sha256).
BRANCH=v3.24
MINIROOT=alpine-minirootfs-3.24.2-aarch64.tar.gz
MINIROOT_SHA256=9bf70a7f18ea44094cbb5f70c58f9af129c8214745743db0e68e5502cc2ce773
KVER=6.18.58-r0
KAPK_SHA256=b46c51ddf572d85b2ba7b9adfaa64f4c38fc490c93d97a20ffd2418d51284085

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$OUT" "$WORK/dl" "$WORK/root"

echo "==> downloading (Alpine $BRANCH, linux-virt $KVER)"
CACHE="${ORBIT_HOME:-$HOME/.orbit}/cache"
mkdir -p "$CACHE"
fetch() { # fetch <url> <cache name> <sha256> <work name>
  if [[ ! -s "$CACHE/$2" ]]; then curl -fL# -o "$CACHE/$2.part" "$1" && mv "$CACHE/$2.part" "$CACHE/$2"; fi
  local got; got=$(shasum -a 256 "$CACHE/$2" | cut -d' ' -f1)
  if [[ "$got" != "$3" ]]; then
    echo "checksum mismatch for $2: got $got, want $3" >&2
    rm -f "$CACHE/$2"; exit 1
  fi
  cp "$CACHE/$2" "$WORK/dl/$4"
}
fetch "$MIRROR/$BRANCH/releases/$ARCH/$MINIROOT" "$MINIROOT" "$MINIROOT_SHA256" minirootfs.tar.gz
fetch "$MIRROR/$BRANCH/main/$ARCH/linux-virt-$KVER.apk" "linux-virt-$KVER.apk" "$KAPK_SHA256" linux-virt.apk

echo "==> extracting kernel"
tar xzf "$WORK/dl/linux-virt.apk" -C "$WORK" boot/vmlinuz-virt 2>/dev/null
python3 -I "$ROOT/scripts/extract-image.py" "$WORK/boot/vmlinuz-virt" "$OUT/kernel"

echo "==> building initramfs"
# Kernel modules stay packed inside linux-virt.apk and are unpacked by /init
# inside the guest: the module tree has names that collide on case-insensitive APFS.
tar xzf "$WORK/dl/minirootfs.tar.gz" -C "$WORK/root"
cp "$WORK/dl/linux-virt.apk" "$WORK/root/linux-virt.apk"
cp -R "$ROOT/guest/." "$WORK/root/"
# Trust the extra roots the Mac trusts (a corporate TLS-inspection CA), and
# only those: see export-host-ca.py. Nothing is added on a Mac without any.
# The result embeds your organisation's CAs, so never share the built
# initramfs or data disk outside it. ORBIT_HOST_CA=0 builds a guest without them.
if [ "${ORBIT_HOST_CA:-1}" = 0 ]; then
  : > "$WORK/root/etc/orbit/host-ca.pem"
else
  python3 -I "$ROOT/scripts/export-host-ca.py" > "$WORK/root/etc/orbit/host-ca.pem"
fi
chmod 755 "$WORK/root/init" "$WORK/root/etc/orbit/"*.sh "$WORK/root/etc/orbit/orbit-runc"
# A root snapshot saved on the data disk is only reused by the same build.
{ echo "$MINIROOT $KVER"; (cd "$ROOT/guest" && find . -type f | sort | xargs cat); cat "$WORK/root/etc/orbit/host-ca.pem"; } \
  | shasum -a 256 | cut -d' ' -f1 > "$WORK/root/etc/orbit/build-id"
(cd "$WORK/root" && find . | cpio -o -H newc --quiet | gzip -1 > "$OUT/initramfs")
chmod 600 "$OUT/initramfs"  # holds the Mac's extra roots

echo "$KVER" > "$OUT/version"
ls -lh "$OUT"
