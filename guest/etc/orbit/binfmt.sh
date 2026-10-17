#!/bin/sh
# Lets linux/amd64 images run on this arm64 guest: registers Alpine's
# qemu-x86_64 (user-mode emulation) for x86-64 ELF binaries. Flags:
#   F  the kernel opens the interpreter now, so it works inside containers
#      that don't have /usr/bin/qemu-x86_64 themselves
#   C  credentials/setuid from the emulated binary, O  pass an open fd
[ -x /usr/bin/qemu-x86_64 ] || exit 0
modprobe -q binfmt_misc
mountpoint -q /proc/sys/fs/binfmt_misc || mount -t binfmt_misc binfmt_misc /proc/sys/fs/binfmt_misc
[ -e /proc/sys/fs/binfmt_misc/qemu-x86_64 ] && exit 0
printf '%s' ':qemu-x86_64:M::\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x02\x00\x3e\x00:\xff\xff\xff\xff\xff\xfe\xfe\x00\xff\xff\xff\xff\xff\xff\xff\xff\xfe\xff\xff\xff:/usr/bin/qemu-x86_64:FCO' \
  > /proc/sys/fs/binfmt_misc/register
