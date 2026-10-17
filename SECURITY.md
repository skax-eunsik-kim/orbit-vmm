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
- Other local users reaching the VMM's Unix sockets (they are created `0600`).
- Supply-chain issues in `scripts/fetch-guest.sh`, such as checksum or pinning
  bypasses.

Out of scope: anything a container can already do through the Docker socket.
That includes root in the guest, privileged containers, and access to the
shared directories, which are writable by design.

## Supported versions

Only the latest `main` gets fixes.
