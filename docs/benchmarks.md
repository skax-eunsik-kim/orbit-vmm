# Benchmarks

**English** | [한국어](i18n/ko/benchmarks.md) | [日本語](i18n/ja/benchmarks.md) | [简体中文](i18n/zh-CN/benchmarks.md) | [Español](i18n/es/benchmarks.md) | [Português (Brasil)](i18n/pt-BR/benchmarks.md) | [Français](i18n/fr/benchmarks.md) | [Deutsch](i18n/de/benchmarks.md) | [हिन्दी](i18n/hi/benchmarks.md)

All numbers are from one M-series MacBook (macOS 26) in October 2026, using
the scripts in `bench/`. They show orders of magnitude, not precise results.
Run the scripts yourself to compare on your machine.

## Memory returned to macOS

`bench/memtest.sh` measures the VM process's `phys_footprint` at four points:
idle, during a container that dirties about 3.5 GB (page cache plus anonymous
memory), and 30 s and 90 s after the container exits. The table shows idle and after
90 s.

| Engine | Idle | 90 s after exit |
|---|---|---|
| orbit-vmm | 0.66 GB | 0.69 GB |
| OrbStack | 1.25 GB | 1.11 GB |
| Colima (QEMU/vz) | 1.59 GB | 3.61 GB |

`bench/memfrag.sh` stresses fragmentation. It starts 24 containers holding
150 MiB each, frees every other one, then frees the rest:

| Step | Mac footprint | Guest used |
|---|---|---|
| Start | 849 MiB | 304 MiB |
| 24 × 150 MiB held | 4854 MiB | 4165 MiB |
| Every other freed (+60 s) | 2917 MiB | 2206 MiB |
| All freed (+60 s) | 1043 MiB | 334 MiB |

## Developer workflow

`bench/devflow/devflow.sh` runs 25 checks:

- compose up with healthchecks, and published ports from the Mac
- service DNS and `host.docker.internal`
- named volumes, bind-mount visibility and inotify, postgres `initdb` on a
  bind mount, `chown`
- HTTPS behind a TLS proxy and an amd64 image
- a BuildKit build (`# syntax`, ghcr, apt/uv cache mounts) and an alembic
  migration
- logs, exec, stats and clock skew

Current result: 25/25 pass.

## Stability

`bench/stress/multi.sh N` runs N worktree stacks in one VM at the same time
and checks that edits in one worktree never reach another. `bench/stress/soak.sh <hours>`
repeats that in a loop together with container and file churn, and samples
the VMM's threads, fds and memory.

A 4 h soak passed 295 of 295 rounds, and thread and fd counts stayed flat.
