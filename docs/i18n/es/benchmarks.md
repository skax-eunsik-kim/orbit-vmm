# Benchmarks

[English](../../benchmarks.md) | [한국어](../ko/benchmarks.md) | [日本語](../ja/benchmarks.md) | [简体中文](../zh-CN/benchmarks.md) | **Español** | [Português (Brasil)](../pt-BR/benchmarks.md) | [Français](../fr/benchmarks.md) | [Deutsch](../de/benchmarks.md) | [हिन्दी](../hi/benchmarks.md)

> Esta es una traducción. El documento en inglés ([benchmarks.md](../../benchmarks.md)) es la fuente de verdad; si difieren, sigue el documento en inglés.

Todas las cifras proceden de un MacBook con chip de la serie M (macOS 26) en octubre de 2026, usando
los scripts de `bench/`. Muestran órdenes de magnitud, no resultados precisos.
Ejecuta los scripts tú mismo para comparar en tu máquina.

## Memoria devuelta a macOS

`bench/memtest.sh` mide el `phys_footprint` del proceso de la VM en cuatro momentos:
en reposo, durante un contenedor que ensucia unos 3.5 GB (page cache más memoria
anónima) y 30 s y 90 s después de que el contenedor termine. La tabla muestra el reposo y el estado tras
90 s.

| Motor | En reposo | 90 s después de terminar |
|---|---|---|
| orbit-vmm | 0.66 GB | 0.69 GB |
| OrbStack | 1.25 GB | 1.11 GB |
| Colima (QEMU/vz) | 1.59 GB | 3.61 GB |

`bench/memfrag.sh` somete a prueba la fragmentación. Inicia 24 contenedores que retienen
150 MiB cada uno, libera uno de cada dos y luego libera el resto:

| Paso | Huella en el Mac | Usado en el guest |
|---|---|---|
| Inicio | 849 MiB | 304 MiB |
| 24 × 150 MiB retenidos | 4854 MiB | 4165 MiB |
| Liberado uno de cada dos (+60 s) | 2917 MiB | 2206 MiB |
| Todos liberados (+60 s) | 1043 MiB | 334 MiB |

## Flujo de trabajo de desarrollo

`bench/devflow/devflow.sh` ejecuta 28 comprobaciones:

- compose up con healthchecks, y puertos publicados desde el Mac
- DNS de servicios y `host.docker.internal`
- volúmenes con nombre, visibilidad e inotify de bind mounts, `initdb` de postgres sobre un
  bind mount, `chown`
- HTTPS detrás de un proxy TLS y una imagen amd64
- una compilación con BuildKit (`# syntax`, ghcr, cache mounts de apt/uv) y una migración de
  alembic
- el wrapper de CA del invitado frente a pasos de build hostiles (directorios de CA con enlaces
  simbólicos o intercambiados, un disco lleno), y una compilación de RHEL (UBI) cuyo bundle es
  un enlace simbólico
- logs, exec, stats y desfase de reloj

Resultado actual: 28/28 superadas.

## Kubernetes

`bench/k8s/k8s.sh` comprueba `orbit k8s` (kind v0.33, Kubernetes v1.37, un
nodo) y registra el `phys_footprint` del VMM. Tres ejecuciones en una instancia
aislada (8 GiB, 6 vCPUs) en macOS 26.4, serie M, 2026-10-27:

| | Ejecución 1 | Ejecución 2 | Ejecución 3 |
|---|---|---|---|
| `orbit k8s up` (nodo Ready) | 43 s | 45 s | 40 s |
| VM antes del clúster | 2019 MiB | 2797 MiB | 2300 MiB |
| Clúster en reposo (60 s) | 3267 MiB | 3757 MiB | 4036 MiB |
| 20 pods de nginx | 4417 MiB | 5247 MiB | 4598 MiB |
| 60 s después de `orbit k8s down` | 2685 MiB | 2302 MiB | 2047 MiB |
| Escalar de 1 → 20 pods Ready | 14 s | 5 s | 5 s |

"Antes" incluye el page cache que dejaron ejecuciones anteriores, así que varía. Un
registro aparte cada 30 s mostró la VM de nuevo por debajo de su tamaño previo al clúster
90 s después de eliminarlo (1686 MiB antes, 1468 MiB después).

Bajo presión de memoria "warn" de macOS (fijada con `ORBIT_PRESSURE=warn`), con
el clúster en reposo, un minuto cada caso:

| memd | Page cache releído (refault) | Lecturas + escrituras de disco | Page cache retenido |
|---|---|---|---|
| antes de #33 | 2.7 GB | 3.4 GB | 460–510 MiB |
| después | 0.09–0.2 GB | 0.14–0.27 GB | unos 600 MiB |

Resultado actual: 13/13 superadas.

## Estabilidad

`bench/stress/multi.sh N` ejecuta N stacks de worktree en una VM al mismo tiempo
y comprueba que las ediciones en un worktree nunca llegan a otro. `bench/stress/soak.sh <hours>`
repite eso en bucle junto con creación y eliminación continua de contenedores y archivos, y muestrea
los hilos, los fds y la memoria del VMM.

Una prueba de resistencia (soak) de 4 h superó 295 de 295 rondas, y el número de hilos y de fds se mantuvo estable.
