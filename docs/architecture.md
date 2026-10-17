# Architecture

```
 macOS                                          Linux guest (Alpine)
 ─────                                          ────────────────────
 docker CLI ── docker.sock ─┐                ┌─ socat ── dockerd / containerd
 vmm-up.sh  ── control.sock ┤   vsock        ├─ control.sh  (ports, clock, meminfo, shutdown)
 FSEvents   ── fsnotify     ┘ ─────────────  └─ fsnotify.sh (Mac edits → inotify)
                         orbit-vmm
   vCPU threads ─ Hypervisor.framework ─ in-kernel GICv3, PSCI, PL011
   virtio-mmio:  blk  ─ data.img (ext4, flock'd: one VM per disk)
                 net  ─ libslirp (NAT, DNS, runtime port forwards)
                 vsock ─ Unix sockets on the Mac
                 fs   ─ FUSE passthrough of allowed dirs under /Users
                 balloon ─ free page reporting → memory back to macOS
```

## VMM (`vmm/`)

The VMM is one Rust process. `src/vm/` has the machine itself:

- Hypervisor.framework FFI (`hvf.rs`) and the in-kernel GIC (`gic.rs`).
- Boot and the device tree (`boot.rs`, `fdt.rs`).
- The PL011 console (`pl011.rs`).
- vCPU threads, PSCI and MMIO decoding (`vcpu.rs`).

`src/virtio/` has the virtio-mmio v2 transport and split virtqueues, plus one
file per device.

### Returning memory

Guest RAM is anonymous `mmap` memory mapped into the VM. When the guest
reports free pages (`VIRTIO_BALLOON_F_REPORTING`, queue 2), the VMM:

1. unmaps the range from the VM (`hv_vm_unmap`),
2. replaces it with fresh zero pages (`mmap(MAP_FIXED)`), so macOS drops the
   old pages from the process footprint,
3. maps the range back (`hv_vm_map`).

`madvise` alone doesn't shrink the footprint of memory that is mapped into a
VM.

The guest kernel boots with `page_reporting.page_reporting_order=2`, so the
reporting unit is 16 KiB, the macOS page size. Fragmented free memory still
gets returned.

Linux holds on to file data as page cache, which the balloon can't report.
`guest/etc/orbit/memd.sh` deals with this:

- After 60 s of light load, it reclaims half of the containers' page cache
  (cgroup `memory.reclaim`).
- After 30 s fully idle, it drops clean caches.
- Both times, it compacts memory so the freed pages form reportable blocks.

### File sharing (virtio-fs)

`fs.rs` is a FUSE server and `hostfs.rs` handles path access.

- Every path is walked with `openat(O_NOFOLLOW)` from a directory fd, so a
  symlink can't escape the allowed directories (`--share-allow`). Directories
  above an allowed one can be traversed but not listed.
- There are four request queues.
- Entry TTL is 5 s and attribute TTL is 500 ms. `KEEP_CACHE` and
  `AUTO_INVAL_DATA` are enabled, and so is readdirplus.
- Writeback caching is off. With it on, Mac-side edits weren't visible in the
  guest.
- Container `chown` is stored in the `user.orbit.owner` xattr and reported
  back in `stat`. Group/other permission bits mirror the owner bits, so
  non-root container users can write to your files.
- Mac edits reach the guest as inotify events. `fsevents.rs` watches the
  allowed directories with FSEvents and sends changed paths to
  `fsnotify.sh` over vsock. The guest then does a same-size `truncate` under a
  reserved gid, which raises `IN_MODIFY`. The server recognises that gid and
  doesn't apply the truncate to the Mac file.

### Networking

`slirp.rs` runs libslirp on its own thread.

- `ports.rs` polls the guest's listening sockets over the control channel and
  adds or removes host forwards at runtime.
- The same channel keeps the guest clock in sync. It measures the round-trip
  time and only steps the clock when it is off by more than 250 ms.
- `csrc/asyncclose.c` is a small dylib that interposes `close()` for internet
  sockets. Content-filter network extensions can make `close()` after FIN block
  for about 1 s, which would stall every connection on the network thread.

## Guest (`guest/`)

`scripts/fetch-guest.sh` downloads a pinned Alpine kernel and minirootfs
(sha256-verified) and builds an initramfs from them plus `guest/`.

- **First boot:** installs Docker and friends onto the data disk, then takes a
  read-only root snapshot.
- **Later boots:** use that snapshot under a tmpfs overlay, so they take
  2–4 s.

Guest pieces:

- `dockerd.sh` writes `daemon.json` (BuildKit GC at 20 GB) and starts dockerd
  with `orbit-runc`. `orbit-runc` is an OCI runtime wrapper that bind-mounts
  the host's CA bundle into containers.
- `dns.sh` runs dnsmasq, which answers `host.docker.internal`.
- `binfmt.sh` registers qemu-x86_64.
- `memd.sh`: see above.
