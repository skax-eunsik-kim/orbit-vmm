# Benchmarks

[English](../../benchmarks.md) | [한국어](../ko/benchmarks.md) | [日本語](../ja/benchmarks.md) | [简体中文](../zh-CN/benchmarks.md) | [Español](../es/benchmarks.md) | [Português (Brasil)](../pt-BR/benchmarks.md) | [Français](../fr/benchmarks.md) | **Deutsch** | [हिन्दी](../hi/benchmarks.md)

> Dies ist eine Übersetzung. Maßgeblich ist das englische Dokument ([benchmarks.md](../../benchmarks.md)); falls beide voneinander abweichen, gilt die englische Fassung.

Alle Zahlen stammen von einem MacBook mit M-Chip (macOS 26) aus dem Oktober 2026 und wurden mit
den Skripten in `bench/` ermittelt. Sie zeigen Größenordnungen, keine exakten Ergebnisse.
Führen Sie die Skripte selbst aus, um auf Ihrem Rechner zu vergleichen.

## An macOS zurückgegebener Speicher

`bench/memtest.sh` misst den `phys_footprint` des VM-Prozesses an vier Punkten:
im Leerlauf, während eines Containers, der etwa 3.5 GB beschreibt (Page Cache plus anonymer
Speicher), sowie 30 s und 90 s nachdem der Container beendet wurde. Die Tabelle zeigt den Leerlauf und den Wert nach
90 s.

| Engine | Leerlauf | 90 s nach Beenden |
|---|---|---|
| orbit-vmm | 0.66 GB | 0.69 GB |
| OrbStack | 1.25 GB | 1.11 GB |
| Colima (QEMU/vz) | 1.59 GB | 3.61 GB |

`bench/memfrag.sh` setzt die Fragmentierung unter Last. Es startet 24 Container, die
jeweils 150 MiB halten, gibt jeden zweiten frei und danach den Rest:

| Schritt | Footprint auf dem Mac | Im Guest belegt |
|---|---|---|
| Start | 849 MiB | 304 MiB |
| 24 × 150 MiB gehalten | 4854 MiB | 4165 MiB |
| Jeder zweite freigegeben (+60 s) | 2917 MiB | 2206 MiB |
| Alle freigegeben (+60 s) | 1043 MiB | 334 MiB |

## Entwickler-Workflow

`bench/devflow/devflow.sh` führt 25 Prüfungen aus:

- compose up mit Healthchecks und vom Mac aus erreichbare veröffentlichte Ports
- Service-DNS und `host.docker.internal`
- Named Volumes, Sichtbarkeit und inotify bei Bind-Mounts, postgres-`initdb` auf einem
  Bind-Mount, `chown`
- HTTPS hinter einem TLS-Proxy und ein amd64-Image
- ein BuildKit-Build (`# syntax`, ghcr, apt/uv-Cache-Mounts) und eine
  alembic-Migration
- Logs, exec, stats und Clock Skew

Aktuelles Ergebnis: 25/25 bestanden.

## Stabilität

`bench/stress/multi.sh N` führt N Worktree-Stacks gleichzeitig in einer VM aus
und prüft, dass Änderungen in einem Worktree nie einen anderen erreichen. `bench/stress/soak.sh <hours>`
wiederholt das in einer Schleife zusammen mit Container- und Datei-Churn und erfasst
Threads, fds und Speicher des VMM.

Ein Soak-Test über 4 h bestand 295 von 295 Runden, und die Thread- und fd-Zahlen blieben konstant.
