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

`bench/devflow/devflow.sh` führt 28 Prüfungen aus:

- compose up mit Healthchecks und vom Mac aus erreichbare veröffentlichte Ports
- Service-DNS und `host.docker.internal`
- Named Volumes, Sichtbarkeit und inotify bei Bind-Mounts, postgres-`initdb` auf einem
  Bind-Mount, `chown`
- HTTPS hinter einem TLS-Proxy und ein amd64-Image
- ein BuildKit-Build (`# syntax`, ghcr, apt/uv-Cache-Mounts) und eine
  alembic-Migration
- den CA-Wrapper des Gasts gegen feindselige Build-Schritte (per Symlink
  verlinkte oder ausgetauschte CA-Verzeichnisse, eine volle Festplatte) sowie
  ein RHEL-(UBI-)Build, dessen Bundle ein Symlink ist
- Logs, exec, stats und Clock Skew

Aktuelles Ergebnis: 28/28 bestanden.

## Kubernetes

`bench/k8s/k8s.sh` prüft `orbit k8s` (kind v0.33, Kubernetes v1.37, ein Node)
und zeichnet den `phys_footprint` des VMM auf. Drei Läufe auf einer isolierten
Instanz (8 GiB, 6 vCPUs) unter macOS 26.4, M-series, am 2026-10-27:

| | Lauf 1 | Lauf 2 | Lauf 3 |
|---|---|---|---|
| `orbit k8s up` (Node Ready) | 43 s | 45 s | 40 s |
| VM vor dem Cluster | 2019 MiB | 2797 MiB | 2300 MiB |
| Cluster im Leerlauf (60 s) | 3267 MiB | 3757 MiB | 4036 MiB |
| 20 nginx-Pods | 4417 MiB | 5247 MiB | 4598 MiB |
| 60 s nach `orbit k8s down` | 2685 MiB | 2302 MiB | 2047 MiB |
| Skalierung 1 → 20 Pods Ready | 14 s | 5 s | 5 s |

„Vorher“ enthält Page Cache aus früheren Läufen und schwankt daher. Ein
separater Trace alle 30 s zeigte die VM 90 s nach dem Löschen des Clusters
wieder unter ihrer Größe vor dem Cluster (1686 MiB vorher, 1468 MiB nachher).

Unter macOS-Speicherdruck „warn“ (fixiert mit `ORBIT_PRESSURE=warn`), mit dem
Cluster im Leerlauf, jeweils eine Minute:

| memd | Erneut gelesener Page Cache (Refaults) | Lese- + Schreibzugriffe auf die Festplatte | Gehaltener Page Cache |
|---|---|---|---|
| vor #33 | 2.7 GB | 3.4 GB | 460–510 MiB |
| nachher | 0.09–0.2 GB | 0.14–0.27 GB | etwa 600 MiB |

Aktuelles Ergebnis: 13/13 bestanden.

## Stabilität

`bench/stress/multi.sh N` führt N Worktree-Stacks gleichzeitig in einer VM aus
und prüft, dass Änderungen in einem Worktree nie einen anderen erreichen. `bench/stress/soak.sh <hours>`
wiederholt das in einer Schleife zusammen mit Container- und Datei-Churn und erfasst
Threads, fds und Speicher des VMM.

Ein Soak-Test über 4 h bestand 295 von 295 Runden, und die Thread- und fd-Zahlen blieben konstant.
