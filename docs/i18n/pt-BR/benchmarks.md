# Benchmarks

[English](../../benchmarks.md) | [한국어](../ko/benchmarks.md) | [日本語](../ja/benchmarks.md) | [简体中文](../zh-CN/benchmarks.md) | [Español](../es/benchmarks.md) | **Português (Brasil)** | [Français](../fr/benchmarks.md) | [Deutsch](../de/benchmarks.md) | [हिन्दी](../hi/benchmarks.md)

> Esta é uma tradução. O documento em inglês ([benchmarks.md](../../benchmarks.md)) é a fonte da verdade; se os dois divergirem, siga o original em inglês.

Todos os números vêm de um único MacBook da série M (macOS 26), em outubro de 2026, usando
os scripts em `bench/`. Eles mostram ordens de grandeza, não resultados precisos.
Execute os scripts você mesmo para comparar na sua máquina.

## Memória devolvida ao macOS

`bench/memtest.sh` mede o `phys_footprint` do processo da VM em quatro momentos:
ociosa, durante um container que suja cerca de 3.5 GB (page cache mais memória
anônima) e 30 s e 90 s após o container encerrar. A tabela mostra o estado ocioso e o de
90 s depois.

| Engine | Ociosa | 90 s após o encerramento |
|---|---|---|
| orbit-vmm | 0.66 GB | 0.69 GB |
| OrbStack | 1.25 GB | 1.11 GB |
| Colima (QEMU/vz) | 1.59 GB | 3.61 GB |

`bench/memfrag.sh` estressa a fragmentação. Ele inicia 24 containers que retêm
150 MiB cada, libera um sim, um não, e depois libera o restante:

| Etapa | Footprint no Mac | Usado no guest |
|---|---|---|
| Início | 849 MiB | 304 MiB |
| 24 × 150 MiB retidos | 4854 MiB | 4165 MiB |
| Um sim, um não liberados (+60 s) | 2917 MiB | 2206 MiB |
| Todos liberados (+60 s) | 1043 MiB | 334 MiB |

## Fluxo de trabalho de desenvolvimento

`bench/devflow/devflow.sh` executa 26 verificações:

- compose up com healthchecks e portas publicadas acessíveis a partir do Mac
- DNS de serviços e `host.docker.internal`
- volumes nomeados, visibilidade e inotify de bind mounts, `initdb` do postgres em um
  bind mount, `chown`
- HTTPS atrás de um proxy TLS e uma imagem amd64
- um build com BuildKit (`# syntax`, ghcr, cache mounts de apt/uv) e uma migration
  do alembic
- logs, exec, stats e desvio de relógio

Resultado atual: 26/26 passam.

## Estabilidade

`bench/stress/multi.sh N` executa N stacks de worktree em uma única VM ao mesmo tempo
e verifica se edições em uma worktree nunca chegam a outra. `bench/stress/soak.sh <hours>`
repete isso em loop junto com churn de containers e de arquivos, e coleta amostras
das threads, fds e memória do VMM.

Um soak de 4 h passou em 295 de 295 rodadas, e as contagens de threads e fds se mantiveram estáveis.
