#!/bin/bash
# Downloads Alpine's linux-virt kernel + minirootfs for this Mac's architecture
# and builds the guest boot artifacts in ~/.orbit/guest:
#   kernel     arm64: uncompressed Image; x86-64: bzImage (the VMM loads it as is)
#   initramfs  newc cpio.gz = minirootfs + guest/ overlay + linux-virt.apk (modules)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${ORBIT_HOME:-$HOME/.orbit}/guest"
MIRROR="${ALPINE_MIRROR:-https://dl-cdn.alpinelinux.org/alpine}"
# Pinned so every machine builds the same guest; bump all four of an
# architecture together. minirootfs sha256 = Alpine's published .sha256;
# linux-virt sha256 recorded when it was pinned (Alpine publishes no
# per-package sha256).
BRANCH=v3.24
# The Mac's architecture, not the shell's: under Rosetta `uname -m` says x86_64.
if [ "$(sysctl -n hw.optional.arm64 2>/dev/null)" = 1 ]; then HOST_ARCH=arm64; else HOST_ARCH=$(uname -m); fi
GUEST_ARCH=${ORBIT_GUEST_ARCH:-$HOST_ARCH}
case "$GUEST_ARCH" in
  arm64 | aarch64)
    ARCH=aarch64
    MINIROOT=alpine-minirootfs-3.24.2-aarch64.tar.gz
    MINIROOT_SHA256=9bf70a7f18ea44094cbb5f70c58f9af129c8214745743db0e68e5502cc2ce773
    KVER=6.18.62-r0
    KAPK_SHA256=3a9a0f16adaabee0e4e05e373682249e2a99ab000faebf6aaaf682ae575c661d
    KCACHE=linux-virt-$KVER.apk
    ;;
  x86_64)
    ARCH=x86_64
    MINIROOT=alpine-minirootfs-3.24.4-x86_64.tar.gz
    MINIROOT_SHA256=cdaa38cb8de3537967ee5b6c9b6cd00cc962da2b018e6eea48696bb274923012
    KVER=6.18.62-r0
    KAPK_SHA256=e121b547d9b86194a641718b52ae1c3e605085c59f31525b1649a59b3d4db0c7
    KCACHE=linux-virt-$KVER-x86_64.apk
    ;;
  *) echo "unsupported architecture: $GUEST_ARCH" >&2; exit 1 ;;
esac
# For the guest-pins workflow: the two pinned downloads, nothing else.
if [ "${1:-}" = --urls ]; then
  echo "$MIRROR/$BRANCH/releases/$ARCH/$MINIROOT"
  echo "$MIRROR/$BRANCH/main/$ARCH/linux-virt-$KVER.apk"
  exit 0
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$OUT" "$WORK/dl" "$WORK/root"

echo "==> downloading (Alpine $BRANCH $ARCH, linux-virt $KVER)"
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
fetch "$MIRROR/$BRANCH/main/$ARCH/linux-virt-$KVER.apk" "$KCACHE" "$KAPK_SHA256" linux-virt.apk

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
