# Benchmarks

[English](../../benchmarks.md) | [한국어](../ko/benchmarks.md) | [日本語](../ja/benchmarks.md) | [简体中文](../zh-CN/benchmarks.md) | [Español](../es/benchmarks.md) | [Português (Brasil)](../pt-BR/benchmarks.md) | **Français** | [Deutsch](../de/benchmarks.md) | [हिन्दी](../hi/benchmarks.md)

> Ce document est une traduction. Le document anglais ([`benchmarks.md`](../../benchmarks.md)) fait foi ; en cas de divergence, suivez-le.

Tous les chiffres proviennent d'un même MacBook M-series (macOS 26) en octobre 2026, avec
les scripts de `bench/`. Ils donnent des ordres de grandeur, pas des résultats précis.
Exécutez les scripts vous-même pour comparer sur votre machine.

## Mémoire restituée à macOS

`bench/memtest.sh` mesure le `phys_footprint` du processus de la VM à quatre moments :
au repos, pendant un conteneur qui salit environ 3.5 GB (page cache plus mémoire
anonyme), puis 30 s et 90 s après la fin du conteneur. Le tableau montre le repos et l'état après
90 s.

| Moteur | Au repos | 90 s après la fin |
|---|---|---|
| orbit-vmm | 0.66 GB | 0.69 GB |
| OrbStack | 1.25 GB | 1.11 GB |
| Colima (QEMU/vz) | 1.59 GB | 3.61 GB |

`bench/memfrag.sh` met la fragmentation à l'épreuve. Il démarre 24 conteneurs occupant
150 MiB chacun, en libère un sur deux, puis libère le reste :

| Étape | Empreinte Mac | Utilisé par l'invité |
|---|---|---|
| Départ | 849 MiB | 304 MiB |
| 24 × 150 MiB occupés | 4854 MiB | 4165 MiB |
| Un sur deux libéré (+60 s) | 2917 MiB | 2206 MiB |
| Tous libérés (+60 s) | 1043 MiB | 334 MiB |

## Workflow de développement

`bench/devflow/devflow.sh` exécute 28 vérifications :

- compose up avec healthchecks, et ports publiés depuis le Mac
- DNS des services et `host.docker.internal`
- volumes nommés, visibilité des bind mounts et inotify, `initdb` postgres sur un
  bind mount, `chown`
- HTTPS derrière un proxy TLS et une image amd64
- un build BuildKit (`# syntax`, ghcr, cache mounts apt/uv) et une migration
  alembic
- le wrapper de CA de l'invité face à des étapes de build hostiles (répertoires
  de CA liés par lien symbolique ou échangés, disque plein), et un build RHEL
  (UBI) dont le bundle est un lien symbolique
- logs, exec, stats et décalage d'horloge

Résultat actuel : 28/28 réussies.

## Kubernetes

`bench/k8s/k8s.sh` vérifie `orbit k8s` (kind v0.33, Kubernetes v1.37, un seul
nœud) et enregistre le `phys_footprint` du VMM. Trois exécutions sur une
instance isolée (8 GiB, 6 vCPU) sous macOS 26.4, M-series, le 2026-10-27 :

| | Exécution 1 | Exécution 2 | Exécution 3 |
|---|---|---|---|
| `orbit k8s up` (nœud Ready) | 43 s | 45 s | 40 s |
| VM avant le cluster | 2019 MiB | 2797 MiB | 2300 MiB |
| Cluster au repos (60 s) | 3267 MiB | 3757 MiB | 4036 MiB |
| 20 pods nginx | 4417 MiB | 5247 MiB | 4598 MiB |
| 60 s après `orbit k8s down` | 2685 MiB | 2302 MiB | 2047 MiB |
| Passage de 1 → 20 pods Ready | 14 s | 5 s | 5 s |

« Avant » inclut le page cache laissé par les exécutions précédentes, d'où sa
variation. Une trace séparée, relevée toutes les 30 s, a montré la VM revenue
sous sa taille d'avant le cluster 90 s après la suppression du cluster
(1686 MiB avant, 1468 MiB après).

Sous pression mémoire macOS « warn » (forcée avec `ORBIT_PRESSURE=warn`), avec
le cluster au repos, une minute chacun :

| memd | Page cache relu (refault) | Lectures + écritures disque | Page cache conservé |
|---|---|---|---|
| avant #33 | 2.7 GB | 3.4 GB | 460–510 MiB |
| après | 0.09–0.2 GB | 0.14–0.27 GB | environ 600 MiB |

Résultat actuel : 13/13 réussies.

## Stabilité

`bench/stress/multi.sh N` exécute N stacks de worktrees dans une même VM en même temps
et vérifie que les modifications faites dans un worktree n'atteignent jamais un autre. `bench/stress/soak.sh <hours>`
répète cela en boucle en y ajoutant un brassage de conteneurs et de fichiers, et échantillonne
les threads, les fds et la mémoire du VMM.

Un soak de 4 h a réussi 295 rounds sur 295, et le nombre de threads et de fds est resté stable.
