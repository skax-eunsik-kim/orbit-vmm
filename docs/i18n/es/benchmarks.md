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

`bench/devflow/devflow.sh` ejecuta 26 comprobaciones:

- compose up con healthchecks, y puertos publicados desde el Mac
- DNS de servicios y `host.docker.internal`
- volúmenes con nombre, visibilidad e inotify de bind mounts, `initdb` de postgres sobre un
  bind mount, `chown`
- HTTPS detrás de un proxy TLS y una imagen amd64
- una compilación con BuildKit (`# syntax`, ghcr, cache mounts de apt/uv) y una migración de
  alembic
- logs, exec, stats y desfase de reloj

Resultado actual: 26/26 superadas.

## Estabilidad

`bench/stress/multi.sh N` ejecuta N stacks de worktree en una VM al mismo tiempo
y comprueba que las ediciones en un worktree nunca llegan a otro. `bench/stress/soak.sh <hours>`
repite eso en bucle junto con creación y eliminación continua de contenedores y archivos, y muestrea
los hilos, los fds y la memoria del VMM.

Una prueba de resistencia (soak) de 4 h superó 295 de 295 rondas, y el número de hilos y de fds se mantuvo estable.
