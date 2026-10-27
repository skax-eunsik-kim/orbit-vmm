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

`bench/devflow/devflow.sh` runs 28 checks:

- compose up with healthchecks, and published ports from the Mac
- service DNS and `host.docker.internal`
- named volumes, bind-mount visibility and inotify, postgres `initdb` on a
  bind mount, `chown`
- HTTPS behind a TLS proxy and an amd64 image
- a BuildKit build (`# syntax`, ghcr, apt/uv cache mounts) and an alembic
  migration
- the guest's CA wrapper against hostile build steps (symlinked or swapped
  CA directories, a full disk), and a RHEL (UBI) build whose bundle is a
  symlink
- logs, exec, stats and clock skew

Current result: 28/28 pass.

## Kubernetes

`bench/k8s/k8s.sh` checks `orbit k8s` (kind v0.33, Kubernetes v1.37, one
node) and records the VMM's `phys_footprint`. Three runs on an isolated
instance (8 GiB, 6 vCPUs) on macOS 26.4, M-series, 2026-10-27:

| | Run 1 | Run 2 | Run 3 |
|---|---|---|---|
| `orbit k8s up` (node Ready) | 43 s | 45 s | 40 s |
| VM before the cluster | 2019 MiB | 2797 MiB | 2300 MiB |
| Cluster idle (60 s) | 3267 MiB | 3757 MiB | 4036 MiB |
| 20 nginx pods | 4417 MiB | 5247 MiB | 4598 MiB |
| 60 s after `orbit k8s down` | 2685 MiB | 2302 MiB | 2047 MiB |
| Scale 1 → 20 pods Ready | 14 s | 5 s | 5 s |

"Before" includes page cache left by earlier runs, so it varies. A separate
trace every 30 s showed the VM back below its pre-cluster size 90 s after the
cluster was deleted (1686 MiB before, 1468 MiB after).

Under macOS "warn" memory pressure (pinned with `ORBIT_PRESSURE=warn`), with
the cluster idle, one minute each:

| memd | Page cache refaulted | Disk reads + writes | Page cache held |
|---|---|---|---|
| before #33 | 2.7 GB | 3.4 GB | 460–510 MiB |
| after | 0.09–0.2 GB | 0.14–0.27 GB | about 600 MiB |

Current result: 13/13 pass.

## Stability

`bench/stress/multi.sh N` runs N worktree stacks in one VM at the same time
and checks that edits in one worktree never reach another. `bench/stress/soak.sh <hours>`
repeats that in a loop together with container and file churn, and samples
the VMM's threads, fds and memory.

A 4 h soak passed 295 of 295 rounds, and thread and fd counts stayed flat.
