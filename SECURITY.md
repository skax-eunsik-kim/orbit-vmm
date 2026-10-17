# Security policy

## Reporting a vulnerability

Please **don't open a public issue** for security problems. Report them
privately through GitHub instead. Go to
[Security → Report a vulnerability](https://github.com/skax-eunsik-kim/orbit-vmm/security/advisories/new).

Include steps to reproduce and the impact you see. We aim to reply within a
week.

## Scope

orbit-vmm is a development tool that runs on your own Mac. These are in scope:

- Escaping the file-sharing allowlist, for example through symlinks, `..`,
  rename races or xattrs.
- A guest or container reaching host memory or host files outside shared
  directories, or crashing the VMM from inside the guest.
- Other local users reaching the VMM's Unix sockets (they are created `0600`),
  or using files a container made setuid or world-writable on the Mac.
- Supply-chain issues in `scripts/fetch-guest.sh`, such as checksum or pinning
  bypasses.

Out of scope: anything a container can already do through the Docker socket.
That includes root in the guest, privileged containers, and access to the
shared directories, which are writable by design.

## Accepted risks

These are deliberate trade-offs, documented so you can decide for yourself:

- **Shared directories are writable from the guest.** A container can modify
  anything in them, including source code and build scripts you later run on
  the Mac. orbit-vmm itself runs from `~/.orbit/bin` (`make install`), not from
  the checkout, and `make up` builds nothing. The VMM refuses to share your
  home directory or anything above it, hidden directories, `~/Library`,
  `~/bin`, or `~/Applications`. It
  also never lets the guest rename, remove or replace the shared directories
  themselves, and refuses entries that resolve through a symlink located
  inside a shared directory. That check covers the directories shared by
  this start. If you narrow the allowlist later, or run a second instance
  with a different one, review symlinks that containers may have left in
  directories you shared before.
- **Containers can reach the Mac's localhost services**
  (`host.docker.internal`, like Docker Desktop). Turn this off with
  `ORBIT_HOST_LOOPBACK=0`.
- **Guest listeners appear on the Mac's 127.0.0.1.** A guest process can take
  a free localhost port before a Mac service does.
- **The guest can stop the VM.** A guest kernel fault or a malformed MMIO access
  ends the VMM process, which is the same as the guest powering off.
- **Containers trust the Mac's extra TLS roots.**
  `scripts/export-host-ca.py` exports only CA certificates that are trusted
  for TLS.

## Supported versions

Only the latest `main` gets fixes.
