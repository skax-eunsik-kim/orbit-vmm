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

`bench/devflow/devflow.sh` executa 28 verificações:

- compose up com healthchecks e portas publicadas acessíveis a partir do Mac
- DNS de serviços e `host.docker.internal`
- volumes nomeados, visibilidade e inotify de bind mounts, `initdb` do postgres em um
  bind mount, `chown`
- HTTPS atrás de um proxy TLS e uma imagem amd64
- um build com BuildKit (`# syntax`, ghcr, cache mounts de apt/uv) e uma migration
  do alembic
- o wrapper de CA do guest contra etapas de build hostis (diretórios de CA com links simbólicos
  ou trocados, um disco cheio) e um build RHEL (UBI) cujo bundle é um link simbólico
- logs, exec, stats e desvio de relógio

Resultado atual: 28/28 passam.

## Kubernetes

`bench/k8s/k8s.sh` verifica o `orbit k8s` (kind v0.33, Kubernetes v1.37, um
nó) e registra o `phys_footprint` do VMM. Três execuções em uma instância
isolada (8 GiB, 6 vCPUs) no macOS 26.4, série M, 2026-10-27:

| | Execução 1 | Execução 2 | Execução 3 |
|---|---|---|---|
| `orbit k8s up` (nó Ready) | 43 s | 45 s | 40 s |
| VM antes do cluster | 2019 MiB | 2797 MiB | 2300 MiB |
| Cluster ocioso (60 s) | 3267 MiB | 3757 MiB | 4036 MiB |
| 20 pods nginx | 4417 MiB | 5247 MiB | 4598 MiB |
| 60 s após `orbit k8s down` | 2685 MiB | 2302 MiB | 2047 MiB |
| Escalar de 1 → 20 pods Ready | 14 s | 5 s | 5 s |

"Antes" inclui page cache deixado por execuções anteriores, então varia. Um
registro separado a cada 30 s mostrou a VM de volta abaixo do tamanho anterior ao
cluster 90 s depois de ele ser removido (1686 MiB antes, 1468 MiB depois).

Sob pressão de memória "warn" do macOS (fixada com `ORBIT_PRESSURE=warn`), com
o cluster ocioso, um minuto cada:

| memd | Page cache relido (refault) | Leituras + gravações em disco | Page cache mantido |
|---|---|---|---|
| antes do #33 | 2.7 GB | 3.4 GB | 460–510 MiB |
| depois | 0.09–0.2 GB | 0.14–0.27 GB | cerca de 600 MiB |

Resultado atual: 13/13 passam.

## Estabilidade

`bench/stress/multi.sh N` executa N stacks de worktree em uma única VM ao mesmo tempo
e verifica se edições em uma worktree nunca chegam a outra. `bench/stress/soak.sh <hours>`
repete isso em loop junto com churn de containers e de arquivos, e coleta amostras
das threads, fds e memória do VMM.

Um soak de 4 h passou em 295 de 295 rodadas, e as contagens de threads e fds se mantiveram estáveis.
