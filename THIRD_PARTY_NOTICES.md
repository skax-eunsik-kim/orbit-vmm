# Third-party components

orbit-vmm's own code (Apache-2.0) is in `vmm/`, `guest/`, `scripts/`, `bench/` and `docs/`.
It links or downloads the components below; their licences apply to them.

## Linked into `orbit-vmm`

| Component | Licence | How it is used |
|---|---|---|
| [libslirp](https://gitlab.freedesktop.org/slirp/libslirp) | BSD-3-Clause | User-mode NAT networking, dynamically linked from Homebrew (`brew install libslirp`). |
| [libc](https://crates.io/crates/libc) crate | MIT OR Apache-2.0 | FFI declarations. |
| [vm-fdt](https://crates.io/crates/vm-fdt) crate | Apache-2.0 OR BSD-3-Clause | Device tree generation. |
| Apple Hypervisor.framework, CoreServices, CoreFoundation | Apple SDK terms | System frameworks, not redistributed. |

## Downloaded at build time by `scripts/fetch-guest.sh`

Pinned versions are in that script (Alpine v3.24; checksums verified).

| Component | Licence |
|---|---|
| Linux kernel (Alpine `linux-virt`) and its modules | GPL-2.0 |
| Alpine minirootfs (busybox, musl, apk-tools, …) | GPL-2.0 (busybox), MIT (musl), various |

## Installed inside the guest from Alpine's repositories (first boot)

docker / containerd / runc (Apache-2.0), socat (GPL-2.0), iptables (GPL-2.0),
e2fsprogs (GPL-2.0), dnsmasq (GPL-2.0/3.0), jq (MIT), qemu-x86_64 (GPL-2.0),
plus their dependencies.

## Redistribution

The repository contains only orbit's source. Everyone builds their own guest
image and data disk locally, which involves no redistribution.

**Do not hand out built artifacts** (`~/.orbit/guest/kernel`, `initramfs`,
`~/.orbit/vmm/data.img`):

- They contain GPL software, so distributing them would carry source-offer
  obligations. Alpine's sources are at https://gitlab.alpinelinux.org/alpine/aports
  and https://dl-cdn.alpinelinux.org/alpine/v3.24/.
- They embed your Mac's trusted organisation root certificates
  (`/etc/orbit/host-ca.pem`).

If `orbit-vmm` binaries are ever distributed, ship this file and the libslirp
BSD-3-Clause notice with them.
