# Arquitectura

[English](../../architecture.md) | [한국어](../ko/architecture.md) | [日本語](../ja/architecture.md) | [简体中文](../zh-CN/architecture.md) | **Español** | [Português (Brasil)](../pt-BR/architecture.md) | [Français](../fr/architecture.md) | [Deutsch](../de/architecture.md) | [हिन्दी](../hi/architecture.md)

> Esta es una traducción. El documento en inglés ([architecture.md](../../architecture.md)) es la fuente de verdad; si difieren, sigue el documento en inglés.

```
 macOS                                          Linux guest (Alpine)
 ─────                                          ────────────────────
 docker CLI ── docker.sock ─┐                ┌─ socat ── dockerd / containerd
 vmm-up.sh  ── control.sock ┤   vsock        ├─ control.sh  (ports, clock, pressure, shutdown)
 FSEvents   ── fsnotify     ┘ ─────────────  └─ fsnotify.sh (Mac edits → inotify)
                         orbit-vmm
   vCPU threads ─ Hypervisor.framework ─ in-kernel GICv3, PSCI, PL011
   virtio-mmio:  blk  ─ data.img (ext4, flock'd: one VM per disk; discard)
                 net  ─ libslirp (NAT, DNS, runtime port forwards)
                 vsock ─ Unix sockets on the Mac
                 fs   ─ FUSE passthrough of allowed dirs under /Users
                 balloon ─ free page reporting → memory back to macOS
 launchd ── vmm-run.sh (supervisor: restart, disk guard, crash records)
         └─ healthcheck.py (hourly: checks, resource use, release notice)
```

## VMM (`vmm/`)

El VMM es un único proceso de Rust. `src/vm/` contiene la máquina en sí:

- La FFI de Hypervisor.framework (`hvf.rs`) y el GIC en el kernel (`gic.rs`).
- El arranque y el device tree (`boot.rs`, `fdt.rs`).
- La consola PL011 (`pl011.rs`).
- Los hilos de vCPU, PSCI y la decodificación de MMIO (`vcpu.rs`).

`src/virtio/` contiene el transporte virtio-mmio v2 y las virtqueues divididas
(split virtqueues), además de un archivo por dispositivo.

`signals.rs` gestiona las señales SIGTERM y SIGINT enviadas al propio VMM: registra el
pid y el nombre del emisor en `console.log`, pide al guest que se apague (como
máximo 30 s) y termina como si lo hubiera matado esa señal, de modo que el supervisor lo reinicia igualmente.
Una segunda señal hace que termine de inmediato.

### Devolución de memoria

La RAM del guest es memoria anónima obtenida con `hv_vm_allocate` y mapeada en la VM. Cuando el guest
informa de páginas libres (`VIRTIO_BALLOON_F_REPORTING`, cola 2), el VMM:

1. desmapea el rango de la VM (`hv_vm_unmap`),
2. lo sustituye por páginas nuevas a cero (`mmap(MAP_FIXED)`), de modo que macOS
   elimina las páginas antiguas de la huella del proceso,
3. vuelve a mapear el rango (`hv_vm_map`).

`madvise` por sí solo no reduce la huella de la memoria que está mapeada en una
VM.

El kernel del guest arranca con `page_reporting.page_reporting_order=2`, así que la
unidad de reporte es de 16 KiB, el tamaño de página de macOS. La memoria libre
fragmentada también se devuelve.

Linux retiene los datos de archivos como page cache, que el balloon no puede reportar.
`guest/etc/orbit/memd.sh` se encarga de esto:

- Tras 60 s de carga ligera, recupera la mitad del page cache de los contenedores
  (cgroup `memory.reclaim`).
- Tras 30 s completamente inactivo, descarta las cachés limpias.
- En ambos casos, compacta la memoria para que las páginas liberadas formen bloques reportables.
- En todo momento, mantiene el page cache por debajo de 1 GiB mediante el
  `memory.reclaim` del cgroup raíz (las compilaciones nunca están inactivas; el Mac almacena `data.img` en caché de todos modos).
- Mientras macOS informa de presión de memoria, mantiene en su lugar el page cache por debajo de 256 MiB
  (warn) o 64 MiB (critical) (`orbit.memd_warn_mb`,
  `orbit.memd_crit_mb`).

`pressure.rs` vigila la presión de memoria de macOS (una fuente de presión de memoria de
libdispatch, más `kern.memorystatus_vm_pressure_level` releído cada 30 s) y
envía `pressure normal|warn|critical` por el vsock de control. Ante un cambio,
`control.sh` registra el nivel en `/run/orbit-pressure` y ejecuta
`memd.sh reclaim <level>` de inmediato: recupera memoria hasta el nuevo límite (en critical,
descarta primero todas las cachés limpias) y compacta, de modo que el free page reporting devuelve las
páginas antes de que macOS tenga que comprimirlas. `ORBIT_PRESSURE=off` lo
desactiva, y `ORBIT_PRESSURE=warn|critical` fija el nivel, para hacer comparaciones. Ayuda
con la caché que el guest tocó una sola vez (las entradas de una compilación, una imagen descargada);
un conjunto de trabajo que el guest vuelve a leer una y otra vez permanece residente sea cual sea el límite.
Al guest se le notifica cada cambio; `console.log` recibe como máximo una línea cada
10 minutos, con el número de cambios producidos entre medias.

La columna Memoria del Monitor de Actividad es el `phys_footprint` del proceso. Cuando al
Mac le falta memoria, macOS comprime o envía a swap parte de la RAM del guest, y entonces la
huella crece por encima de las páginas del guest que el VMM realmente retiene (entre aproximadamente
un tercio y la mitad de la cantidad comprimida según las mediciones; es casi cero
cuando no hay nada comprimido). Para ver qué páginas del guest están retenidas, inicia el VMM
con `ORBIT_MEMDUMP=<prefix>`, ejecuta `touch <prefix>.req` y cruza `<prefix>.bin`
(un byte de `mincore` por cada página de 16 KiB de RAM del guest) con el
`/proc/kpageflags` del guest usando `vmm/bench/memxref.py`.

### Disco de datos (virtio-blk)

`blk.rs` sirve `data.img`, un archivo disperso (sparse), y toma un `flock` sobre él para que dos
VM no puedan compartir un mismo disco.

- **Discard.** Los discards del guest se convierten en `F_PUNCHHOLE` sobre la imagen, y `memd`
  ejecuta `fstrim /data` tras el arranque y cada 10 minutos cuando la VM está tranquila, de modo
  que el espacio que libera Docker vuelve al Mac.
- **Protección de disco.** Cada 5 s el VMM comprueba el espacio libre en el Mac. Por debajo de
  `ORBIT_DISK_WARN_MIB` registra una advertencia. Por debajo de `ORBIT_DISK_FLOOR_MIB`, o cuando
  una escritura falla con ENOSPC, retiene las escrituras del guest en lugar de hacerlas fallar
  (como el `werror=enospc` de QEMU) durante un máximo de `ORBIT_DISK_PAUSE_SECS`, y las reanuda en cuanto
  se libera espacio, de modo que ext4 nunca ve un error de escritura. El supervisor detiene la
  VM de forma limpia antes, por debajo de `ORBIT_STOP_FREE_GB`.

### Compartición de archivos (virtio-fs)

`fs.rs` es un servidor FUSE y `hostfs.rs` gestiona el acceso a rutas.

- Cada ruta se recorre con `openat(O_NOFOLLOW)` desde un fd de directorio, de modo que un
  enlace simbólico no puede escapar de los directorios permitidos (`--share-allow`). Los directorios
  por encima de uno permitido se pueden atravesar, pero no listar.
- El propio VMM comprueba la lista de permitidos: las entradas deben ser rutas reales (sin
  enlaces simbólicos, con las mayúsculas y minúsculas exactas, no anidadas), y se rechazan el directorio personal, cualquier directorio por encima
  de él, los directorios ocultos y `~/Library`. El guest no puede renombrar
  ni eliminar los propios directorios permitidos.
- Los fds de directorio de recorridos anteriores se guardan en caché durante 1 s, para que una ruta no
  vuelva a abrir cada directorio desde `/Users` hacia abajo. Los renombrados y
  rmdir del propio guest descartan las entradas de inmediato, y las entradas caducadas se cierran en la siguiente
  inserción. El guest guarda en caché las rutas inexistentes durante 1 s
  (`ORBIT_FS_NEG_TTL_MS`).
- Hay cuatro colas de solicitudes.
- El TTL de las entradas es de 5 s y el de los atributos, de 500 ms. `KEEP_CACHE` y
  `AUTO_INVAL_DATA` están habilitados, al igual que readdirplus.
- La caché de escritura diferida (writeback) está desactivada. Con ella activada, las ediciones hechas en el Mac no eran visibles en el
  guest.
- El `chown` de los contenedores se guarda en el xattr `user.orbit.owner` y se devuelve
  en `stat`. Los bits de permisos de grupo y de otros replican los del propietario, de modo que
  los usuarios no root de los contenedores pueden escribir en tus archivos.
- Las ediciones del Mac llegan al guest como eventos de inotify. `fsevents.rs` vigila los
  directorios permitidos con FSEvents; `forward_events` convierte cada lote en
  ediciones, creaciones, renombrados y eliminaciones (a partir del estado actual del Mac y de
  lo que el guest ha consultado) y los envía a `fsnotify.sh` por vsock.
  El guest repite cada cambio bajo un gid reservado: un `truncate` al mismo tamaño
  para una edición (`IN_MODIFY`), y create/mkdir, rename o unlink/rmdir para el
  resto. El servidor reconoce ese gid, nunca aplica nada de ello al Mac
  y solo deja que tenga éxito para las rutas que encoló mientras el Mac aún coincide.
- El directorio del socket de docker (`~/.orbit/vmm/`) se muestra al guest como un
  punto de montaje vacío y de solo lectura, y el guest enlaza `docker.sock` ahí, de modo que
  funcionan las herramientas que montan con bind mount la ruta de `DOCKER_HOST` (Ryuk de testcontainers).

### Red

`slirp.rs` ejecuta libslirp en su propio hilo.

- `ports.rs` reenvía los puertos publicados al `127.0.0.1` del Mac. Sigue los
  eventos de contenedores de Docker (`/events` en el propio socket de docker del VMM) y
  reenvía una publicación `-p` en cuanto el contenedor se inicia, incluidos los puertos
  publicados en una IP concreta del host. Además, cada 250 ms lee los sockets en escucha del guest
  a través del canal de control, para los servicios con `--network host`.
- Los contenedores llegan al localhost del Mac a través de `host.docker.internal`
  (`ORBIT_HOST_LOOPBACK=0` lo desactiva).
- DNS: los contenedores consultan al dnsmasq del guest, que responde a
  `host.docker.internal` y reenvía el resto al resolver de libslirp y, desde
  ahí, al del Mac. Admite 4096 consultas en curso; el valor predeterminado de dnsmasq,
  150, hacía que las compilaciones en paralelo fallaran con "Temporary failure in name resolution".
- El canal de control también mantiene sincronizado el reloj del guest. Mide el
  tiempo de ida y vuelta y solo ajusta el reloj cuando la desviación supera los 250 ms.
  Por lo tanto, el reloj puede retroceder: el código del guest no debe basarse en los mtimes para
  saber si algo ha cambiado.
- `csrc/asyncclose.c` es una pequeña dylib que intercepta `close()` para los sockets
  de internet. Las extensiones de red de filtrado de contenido pueden hacer que `close()` tras un FIN se bloquee
  durante aproximadamente 1 s, lo que detendría todas las conexiones del hilo de red.

## Guest (`guest/`)

`scripts/fetch-guest.sh` descarga un kernel y un minirootfs de Alpine con versión fijada
(verificados con sha256) y construye con ellos, junto con `guest/`, un initramfs. Alpine
elimina las versiones antiguas de los paquetes, así que el workflow semanal `guest-pins` comprueba que
ambas descargas fijadas siguen existiendo.

- **Primer arranque:** instala Docker y sus herramientas asociadas en el disco de datos y luego toma una
  instantánea de solo lectura de la raíz. Docker procede de Alpine v3.23 (29.5): 29.8 reconstruía
  todas las capas sobre una imagen base compilada localmente en cada compilación en caliente.
- **Arranques posteriores:** usan esa instantánea bajo una superposición tmpfs, así que tardan
  2–4 s.

Componentes del guest:

- `dockerd.sh` escribe `daemon.json` (caché de compilación limitada a 20 GB con
  `defaultMaxUsedSpace`) e inicia dockerd con `orbit-runc`.
- `orbit-runc` es un wrapper de runtime OCI que hace que los contenedores confíen en las raíces
  en las que confía el Mac más allá del conjunto de Apple. `make guest` las exporta
  (`scripts/export-host-ca.py`; normalmente una CA de inspección TLS de la empresa; ninguna en
  la mayoría de los Mac, y ninguna con `ORBIT_HOST_CA=0`) a `/etc/orbit/host-ca.pem`.
  - Un contenedor recibe una copia del bundle de CA de la imagen con ellas añadidas al final,
    montada con bind mount de solo lectura sobre el bundle.
  - En cambio, un paso `RUN` de una compilación recibe una copia de lectura y escritura del directorio del bundle,
    de modo que funcionan las herramientas que regeneran el bundle mediante renombrado. runc se ejecuta como
    proceso hijo; cuando termina (BuildKit pasa `--keep`, así que el rootfs sigue
    montado), el directorio se vuelve a escribir en el rootfs del paso con el
    bundle propio de la imagen, solo si su contenido cambió. Las raíces del host nunca
    llegan a una capa.
  - Un directorio que contiene el directorio de otro bundle o un montaje recibe en su lugar el
    bind del archivo, y un bundle situado en un montaje existente o por debajo de él
    (`RUN --mount`, `-v`) no se toca.
- `dns.sh` ejecuta dnsmasq (ver Red).
- `binfmt.sh` registra qemu-x86_64.
- `memd.sh`: ver más arriba.

## En el Mac (`scripts/`)

- `make install` copia el VMM, su interceptor de `close()`, `vmm-up.sh`,
  `vmm-run.sh` y `healthcheck.py` a `~/.orbit/bin`, escribe
  `~/.orbit/bin/VERSION` e instala el watchdog de launchd. La VM solo se
  ejecuta desde esa copia: los contenedores pueden escribir en el checkout.
- `vmm-run.sh` supervisa la VM, con o sin launchd:
  - La reinicia tras un fallo, un reinicio o panic del guest, o un bloqueo (tres
    pings fallidos al socket de control; antes se aborta el VMM para que macOS escriba un
    informe de fallo).
  - Deja de reiniciarla tras más de tres arranques en 10 minutos.
  - Guarda un registro de cada parada inesperada en `crashes/<time>/`.
  - Avisa por debajo de `ORBIT_WARN_FREE_GB` y detiene la VM de forma limpia por debajo de
    `ORBIT_STOP_FREE_GB`.
- `healthcheck.py`, si está instalado, comprueba Docker, los puertos, los bind mounts, inotify,
  el reloj y el espacio en disco, registra la memoria, los hilos y los fds del VMM, y una vez
  al día lee la última versión publicada en la página `releases/latest` y muestra una
  notificación por cada versión más reciente.
- `update.sh` (`make update`, `make rollback`) compila una etiqueta de versión en su propio
  clon (`~/.orbit/src`) y el nuevo guest en un directorio de staging, conserva los
  binarios y el guest anteriores para poder hacer rollback, y reinicia la VM solo cuando no hay
  contenedores en ejecución.
