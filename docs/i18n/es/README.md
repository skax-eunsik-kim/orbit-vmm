# orbit-vmm

[English](../../../README.md) | [한국어](../ko/README.md) | [日本語](../ja/README.md) | [简体中文](../zh-CN/README.md) | **Español** | [Português (Brasil)](../pt-BR/README.md) | [Français](../fr/README.md) | [Deutsch](../de/README.md) | [हिन्दी](../hi/README.md)

> Esta es una traducción. El [README en inglés](../../../README.md) es la fuente de verdad; si difieren, sigue la versión en inglés.

[![CI](https://github.com/skax-eunsik-kim/orbit-vmm/actions/workflows/ci.yml/badge.svg)](https://github.com/skax-eunsik-kim/orbit-vmm/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](../../../LICENSE)

Un motor de Docker que ahorra memoria para Macs con Apple Silicon.

orbit-vmm es un pequeño monitor de máquinas virtuales escrito en Rust sobre
Hypervisor.framework de Apple. Arranca un invitado Alpine Linux que ejecuta
Docker, y la CLI `docker` / `docker compose` de tu Mac se comunica con él a
través de un socket Unix.

La mayoría de las VM de Docker en macOS crecen hasta su pico de uso de memoria
y se quedan ahí. orbit-vmm devuelve la memoria. El invitado informa de las
páginas que libera (*free page reporting* de virtio-balloon), y el VMM las
devuelve a macOS en cuestión de segundos. Un pequeño demonio en el invitado
también descarta la caché de páginas inactiva y compacta la memoria, para que
la memoria liberada pueda devolverse de verdad.

> **Estado: temprano.** Ejecuta stacks de compose reales todos los días, pero
> es joven, tiene un solo mantenedor y no tiene instalador ni GUI. Lee
> [Limitaciones conocidas](#limitaciones-conocidas) antes de depender de él.

> **¿Usas Windows?** [orbit-win](https://github.com/skax-eunsik-kim/orbit-win) es el proyecto hermano para Windows:
> el mismo invitado y la misma devolución de memoria, sobre Windows Hypervisor
> Platform. Por ahora su repositorio es privado.

## Características

- **La memoria vuelve a macOS.** Free page reporting con granularidad de
  16 KiB, más recorte de la caché de páginas inactiva. Cuando termina una
  carga de trabajo, la huella de la VM en el Mac vuelve a bajar a alrededor de
  1 GB en menos de un minuto ([benchmarks](benchmarks.md)).
- **Bind mounts sobre virtio-fs.** `-v $PWD:/app` funciona en la misma ruta que
  en el Mac. Tiene varias colas de peticiones, caché de atributos y
  readdirplus. Las ediciones en el Mac, los archivos y directorios nuevos, los
  renombrados y los borrados disparan eventos inotify en los contenedores, de
  modo que la recarga en caliente de `uvicorn --reload`, vite y nodemon sigue
  funcionando a través de `git checkout` y de generadores de código. `chown` y
  `chmod` dentro de un contenedor también funcionan. Ambos se emulan con
  xattrs. El archivo real sigue siendo tuyo y nunca pasa a ser setuid ni
  escribible por el grupo o por todos en el Mac.
- **Compartición opcional (opt-in).** Solo los directorios permitidos son
  visibles para la VM, y el recorrido de rutas es seguro frente a enlaces
  simbólicos. Un contenedor con el socket de Docker no puede leer `~/.ssh`.
- **Red que se comporta como Docker Desktop:**
  - Los puertos publicados aparecen en `127.0.0.1` automáticamente.
  - `host.docker.internal` y `host-gateway` resuelven al Mac.
  - Los contenedores confían en las CA raíz en las que confía tu Mac, así que
    `pip`, `npm`, `apk` y `curl` funcionan detrás de proxies que inspeccionan
    TLS.
- **Imágenes amd64** se ejecutan mediante emulación de usuario de qemu
  (binfmt).
- **Varias VM en paralelo** con `ORBIT_INSTANCE=<name>`.
- **Pequeño y auditable.** Un único binario de Rust de unas 5.5k líneas.
  Depende de `libc`, `vm-fdt` y libslirp, y de nada más.

## Requisitos

- Mac con Apple Silicon, macOS 15 o posterior (la API de GIC en el kernel
  necesita 15+).
- Rust 1.87+ y libslirp: `brew install rust libslirp`.
- Docker CLI con el plugin de compose: `brew install docker docker-compose`.

### En una versión de macOS en la que orbit no se ha probado

Probado hasta ahora en macOS 26.4. Cómo se puede devolver a macOS la memoria
de Hypervisor.framework, y cómo la contabiliza Activity Monitor, depende de
macOS, así que comprueba estas tres cosas en una versión nueva antes de
confiar en él:

1. **Liberación de memoria.** Sin ninguna VM en ejecución:

   ```bash
   vmm/target/release/orbit-vmm --reclaim-selftest
   ```

   En el bloque `guest RAM from hv_vm_allocate`, la línea
   `unmap + mmap(MAP_FIXED anon) + map` debe terminar cerca de `released 1 MiB`.
   Ese es el método que usa el VMM (`vmm/src/reclaim.rs`). Si se queda en
   ~257 MiB, la memoria liberada del invitado no se devuelve en esta versión:
   la VM solo crece en el Mac.
2. **Flujo de trabajo de desarrollo.** Con la VM levantada: `make devflow`
   debe informar 0 fail (25 comprobaciones).
3. **Activity Monitor.** Su columna Memory es el `phys_footprint` del VMM.
   Cuando al Mac le falta memoria, macOS comprime parte de la RAM del invitado
   y ese número crece por encima de lo que usa el invitado. En las pruebas la
   diferencia fue casi cero sin nada comprimido, y de entre un tercio y la
   mitad de la cantidad comprimida bajo presión. Una diferencia distinta en
   otra versión de macOS es un cambio en cómo contabiliza macOS, no una fuga,
   siempre que `ORBIT_MEMDUMP` + `vmm/bench/memxref.py` muestren que el VMM
   retiene aproximadamente lo que usa el invitado (consulta
   [docs/architecture.md](architecture.md)).

## Inicio rápido

```bash
git clone https://github.com/skax-eunsik-kim/orbit-vmm ~/src/orbit-vmm
cd ~/src/orbit-vmm
make guest      # download pinned Alpine kernel + rootfs (sha256-checked) into ~/.orbit/guest
make install    # build, sign, copy the VMM and scripts to ~/.orbit/bin, install the watchdog
make up         # start the VM from ~/.orbit/bin; first boot installs Docker (~1 min), later boots 2-4 s
export DOCKER_HOST=unix://$HOME/.orbit/vmm/docker.sock
docker run --rm hello-world
```

Pon la línea de `DOCKER_HOST` en el perfil de tu shell, o crea un contexto:

```bash
docker context create orbit --docker host=unix://$HOME/.orbit/vmm/docker.sock
docker context use orbit
```

`make up` (o `~/.orbit/bin/vmm-up.sh`) reinicia una VM en ejecución, lo que
detiene todos sus contenedores. La VM y la comprobación de estado se ejecutan
solo desde la copia instalada en `~/.orbit/bin`. Los contenedores pueden
escribir en tus directorios compartidos, y este checkout suele ser uno de
ellos, así que `make up` no compila ni ejecuta nada desde él. Solo lo hacen
`make install` y `make guest`. Antes de ejecutarlos, comprueba que el checkout
no ha cambiado inesperadamente (`git status`). Mejor aún, mantén el checkout
fuera de tus directorios compartidos.

## Configuración

`scripts/vmm-up.sh` (lo que ejecuta `make up`) lee estas variables de entorno:

| Variable | Valor predeterminado | Significado |
|---|---|---|
| `ORBIT_MEM_MIB` | `8192` | Límite de RAM del invitado. La memoria liberada vuelve a macOS, así que un límite alto cuesta poco. |
| `ORBIT_CPUS` | Número de núcleos del Mac (`sysctl -n hw.ncpu`) | vCPU. Las vCPU inactivas quedan aparcadas y no consumen CPU; con solo unas pocas, el arranque de contenedores en paralelo (p. ej., sandboxes de agentes) hace cola por CPU. |
| `ORBIT_GUEST` | `~/.orbit/guest` | Directorio con el `kernel` y el `initramfs` del invitado (p. ej., uno generado con `ORBIT_HOME=… scripts/fetch-guest.sh`). |
| `ORBIT_DISK_GB` | `64` | Tamaño de un disco de datos **nuevo** (archivo disperso). |
| `ORBIT_DISK_WARN_MIB` | `ORBIT_WARN_FREE_GB` en MiB | Por debajo de este espacio libre en el Mac, el VMM también avisa en `console.log`. |
| `ORBIT_DISK_FLOOR_MIB` | `1024` | Último recurso por debajo de `ORBIT_STOP_FREE_GB`: por debajo de este valor, las escrituras en disco del invitado se retienen hasta que se libere espacio (consulta [Disco de datos](#disco-de-datos)). |
| `ORBIT_DISK_PAUSE_SECS` | `600` | Cuánto espera una escritura retenida antes de fallar. |
| `ORBIT_SHARE_ALLOW` | ver más abajo | Directorios separados por dos puntos que los contenedores pueden montar con bind mount. |
| `ORBIT_HOST_LOOPBACK` | `1` | `0` impide que los contenedores alcancen servicios en el `127.0.0.1` del Mac (y `host.docker.internal`). |
| `ORBIT_WARN_FREE_GB` / `ORBIT_STOP_FREE_GB` | `10` / `3` | Espacio libre en el disco del Mac con el que la VM te avisa, y con el que se apaga limpiamente antes de que su disco se llene. |
| `ORBIT_INSTANCE` | (sin definir) | Ejecuta una VM independiente con su propio disco y sockets en `~/.orbit/vmm-<name>`. |

### Compartición de archivos

De forma predeterminada, la VM puede ver los directorios que existan entre
`~/work`, `~/src`, `~/code`, `~/dev`, `~/projects` y `~/Developer`. Todo lo
demás en `/Users` es invisible. Para añadir directorios:

```bash
ORBIT_SHARE_ALLOW="$HOME/src:$HOME/clients" make up
```

Los directorios compartidos son **escribibles por todos los contenedores**.
orbit-vmm se niega a compartir tu directorio personal, cualquier cosa por
encima de él, los directorios ocultos (`~/.ssh`, `~/.orbit`, …), `~/Library`,
`~/bin` y `~/Applications`. Un directorio dentro de otro compartido se
descarta, ya que el exterior ya lo cubre. Los contenedores no pueden renombrar
ni eliminar los propios directorios compartidos.

- Las ediciones de archivos en el Mac son visibles en los contenedores en
  aproximadamente 0.5 s. Los archivos nuevos y los directorios renombrados
  aparecen en aproximadamente 1 s.
- Las ediciones y los archivos nuevos en el Mac generan `IN_MODIFY` /
  `IN_CLOSE_WRITE` en los contenedores aproximadamente 1 s después.

### Red

- `-p 8080:80` y `ports:` de compose se redirigen a `127.0.0.1:8080` en el Mac
  en aproximadamente 1 s. Si el puerto del Mac está ocupado, la redirección
  falla y el error va a `~/.orbit/vmm/console.log`.
- Los contenedores alcanzan los servicios en el localhost del Mac a través de
  `host.docker.internal`. Eso incluye cualquier cosa que escuche solo en
  `127.0.0.1` (bases de datos, notebooks, servidores de modelos locales).
  Define `ORBIT_HOST_LOOPBACK=0` si no quieres eso.
- Cualquier proceso que escuche en todas las direcciones dentro de la VM
  obtiene el mismo puerto en el `127.0.0.1` del Mac, no solo las publicaciones
  con `-p` (como con `--network host`).

### Certificados raíz (proxies que inspeccionan TLS)

- `make guest` lee las raíces en las que confía tu Mac más allá del conjunto
  integrado de Apple (las marcadas como "Always Trust" para SSL, normalmente
  una CA corporativa de inspección TLS) y las incorpora a tu invitado. En un
  Mac sin raíces de ese tipo no se añade nada, y los contenedores ven los
  bundles propios de sus imágenes. El repositorio y las releases no contienen
  certificados.
- Los contenedores y los pasos de build ven esas raíces añadidas al bundle de
  CA de la imagen, mediante un montaje. Un paso de build recibe una copia del
  directorio del bundle, así que `update-ca-certificates` y `apt-get upgrade`
  funcionan. Ninguno de los dos escribe las raíces del Mac en la imagen.
- `ORBIT_HOST_CA=0 make guest && make up` genera un invitado sin ellas.
- Tu invitado (`~/.orbit/guest`) y tu disco de datos contienen entonces las CA
  de tu organización: no los compartas fuera de ella.

## Archivos

| Ruta | Contenido |
|---|---|
| `~/.orbit/guest/` | Kernel e initramfs generados por `make guest`. |
| `~/.orbit/vmm/data.img` | Disco de datos (imágenes de Docker, volúmenes). |
| `~/.orbit/vmm/docker.sock` | API de Docker. |
| `~/.orbit/vmm/console.log` | Consola del invitado; mira aquí primero cuando algo falle (`console.prev.log` es la ejecución anterior). |
| `~/.orbit/vmm/watchdog.log`, `crashes/` | Log del supervisor y registros de paradas inesperadas. |

**No compartas `~/.orbit/guest/*` ni `data.img`.** Contienen binarios GPL y
los certificados raíz de confianza de tu máquina
([THIRD_PARTY_NOTICES.md](../../../THIRD_PARTY_NOTICES.md)).

Para ampliar el disco de datos, detén la VM, agranda el archivo y vuelve a
iniciar la VM. El invitado redimensiona su sistema de archivos al arrancar:

```bash
python3 -c "import os;p=os.path.expanduser('~/.orbit/vmm/data.img');os.truncate(p,max(os.path.getsize(p),128<<30))"
```

### Disco de datos

`data.img` es disperso: ocupa en el Mac tanto espacio como Docker haya
escrito, hasta su tamaño lógico (`vmm-up.sh` muestra ambos). Crece sin avisar,
así que dos salvaguardas vigilan el espacio libre del Mac. El supervisor
(`vmm-run.sh`) te notifica por debajo de `ORBIT_WARN_FREE_GB`, y apaga el
invitado limpiamente por debajo de `ORBIT_STOP_FREE_GB` (tampoco arrancará en
ese caso). Por debajo de eso, como último recurso, el VMM comprueba cada 5
segundos:

- Por debajo de `ORBIT_DISK_WARN_MIB`, escribe `orbit-vmm: disk space low` en
  `console.log` (de nuevo cada 10 minutos mientras siga bajo), y
  `scripts/healthcheck.py` informa de un aviso.
- Por debajo de `ORBIT_DISK_FLOOR_MIB`, o cuando una escritura falla porque el
  disco está lleno, retiene las escrituras en disco del invitado y escribe
  `orbit-vmm: DISK FULL` una vez en el log. Docker se queda parado (el
  invitado puede registrar avisos de tareas colgadas), pero nada falla. Libera
  espacio en el Mac y las escrituras se reanudan donde se detuvieron
  (`disk writes resumed`).
- Si no se libera espacio dentro de `ORBIT_DISK_PAUSE_SECS`, las escrituras
  fallan. ext4 entonces informa errores de E/S y puede pasar a solo lectura,
  así que libera espacio antes de eso y reinicia la VM si ocurrió.

Retener las escrituras en lugar de hacerlas fallar es lo que hace QEMU de
forma predeterminada (`werror=enospc`): una escritura fallida son datos que
ext4 ya ha prometido almacenar, y perderlos es la forma en que un disco del
Mac lleno corrompe al invitado.

## Actualización

```bash
make update            # the latest release; TO=v0.2.1 for a given one
make rollback          # back to what was installed before the last update
```

`make update` compila la release en su propio clon (`~/.orbit/src`), no en tu
checkout, y genera el nuevo invitado antes de reemplazar nada, de modo que una
descarga o compilación fallida deja la versión instalada como estaba. Conserva
los binarios y el invitado anteriores para `make rollback`. Reinicia la VM
solo cuando no hay contenedores en ejecución; de lo contrario, la VM mantiene
la versión antigua hasta `make up` (o ejecuta `NOW=1 make update`). El primer
arranque tras una actualización del invitado reinstala los paquetes del
invitado (aproximadamente 1 min).

Si la comprobación de estado está instalada, consulta la última release una
vez al día y muestra una notificación de macOS por cada release más nueva.
No se instala nada sin `make update`.

Para seguir `main` en lugar de las releases: `git pull && make guest install up`.

## Watchdog

`make install` también instala un agente de launchd, `com.orbit.vmm`, que
ejecuta la VM y la recupera. Existe porque no hay nada más que lo haga: orbit
no tiene ninguna app que permanezca en ejecución, así que sin él un VMM que
muere o se cuelga se queda caído.

```bash
ORBIT_WATCHDOG=0 make install          # install without it
./scripts/install-watchdog.sh --uninstall
./scripts/install-watchdog.sh --reload # apply a changed agent now (restarts the VM)
```

Volver a instalarlo no afecta a una VM en ejecución.

- **Fallo:** si el VMM muere, o el invitado se reinicia o entra en pánico,
  launchd lo vuelve a iniciar en cuestión de segundos.
- **Cuelgue:** se hace ping al invitado cada 30 s. Tras tres pings sin
  respuesta, el VMM se aborta, para que macOS escriba un informe de fallo, y
  luego se reinicia. Eso tarda unos 90 s.
- **Bucle de fallos:** tras más de tres arranques en 10 minutos, deja de
  reiniciar y te avisa.
- **Registros de fallos:** cada parada inesperada se guarda en
  `~/.orbit/vmm/crashes/<time>/` (log de consola, informe de fallo de macOS,
  log del watchdog). Se conservan los 10 más recientes, y recibes una
  notificación de macOS. Los contenedores se detienen con la VM, así que los
  que no tengan una política `restart:` deben volver a iniciarse.
- **A propósito:** `make down` (`vmm-up.sh --stop`) y un `poweroff` en el
  invitado cuentan como paradas intencionadas y no se reinician. Una VM que
  estaba en ejecución cuando cerraste sesión vuelve a arrancar al iniciar
  sesión.

Sin el watchdog, `make up` ejecuta el mismo supervisor en segundo plano, así
que los cuelgues se siguen detectando y los fallos se siguen registrando, pero
no se reinicia nada.

## Comprobación de estado (opcional)

Tras `make install`, `scripts/install-healthcheck.sh` instala un agente de
launchd que ejecuta el `~/.orbit/bin/healthcheck.py` instalado. Comprueba
Docker, los puertos, los bind mounts, inotify y el reloj del invitado, y
registra la memoria del VMM, el número de hilos y de descriptores de archivo
(fd) en `~/.orbit/vmm/health.jsonl`.

La comprobación se ejecuta cada hora. Tras tres ejecuciones correctas
seguidas, se espacia a cada 2 h y luego a cada 4 h. Cualquier problema la
vuelve a poner cada hora. Para eliminarla, ejecuta `--uninstall`.

## Limitaciones conocidas

- **inotify:** los cambios en el lado del Mac llegan a los contenedores
  aproximadamente un segundo después. Un árbol de directorios nuevo con más de
  256 entradas (un worktree recién creado, un `npm install` en el Mac) genera
  un único `IN_CREATE` para su directorio superior; los watchers recursivos lo
  escanean a partir de ahí. Los renombrados se emparejan solo cuando el
  invitado ya había visto el nombre antiguo; de lo contrario aparecen como
  borrado + creación.
- **Redirección de puertos:** las publicaciones UDP no se redirigen. Las
  publicaciones en una IP concreta del host (`-p 127.0.0.1:8080:80`) se
  redirigen al `127.0.0.1` del Mac como las demás; el mismo puerto publicado
  en dos de esas IP llega solo a la más reciente. Si dos VM publican el mismo
  puerto, la redirección de la segunda falla (se registra en el log).
- **Bind mounts:** no se comparten bloqueos de archivos entre el Mac y los
  contenedores. No se pueden crear sockets Unix ni FIFO en bind mounts.
- **Disco de datos:** el espacio que libera Docker vuelve al Mac solo cuando
  el invitado hace trim (`fstrim`, cada 10 minutos cuando la VM está en
  reposo, y una vez tras el arranque), así que `docker system prune` se nota
  en el Mac unos minutos después. La imagen nunca se reduce por debajo de lo
  que el invitado ha escrito pero no ha liberado.
- **Certificados raíz:** un paso de build que copia el bundle de CA a otro
  lugar (`cp /etc/ssl/certs/ca-certificates.crt /app/`) copia con él las
  raíces del Mac dentro de la imagen. Regenerar el bundle funciona en los
  pasos de build pero no en contenedores en ejecución, y los comandos
  posteriores del mismo `RUN` no ven las raíces del Mac. Si hay algo montado
  sobre el directorio de CA (`RUN --mount`, `-v`), las raíces no se añaden
  ahí.
- **Piezas que faltan:** no hay instalador, GUI ni Kubernetes.

## Cómo funciona

Consulta [docs/architecture.md](architecture.md). En resumen:

```
docker CLI ──unix socket── orbit-vmm ──vsock── guest: dockerd
                              │
          Hypervisor.framework (vCPUs, in-kernel GICv3)
          virtio-mmio: blk · net (libslirp) · vsock · fs · balloon (free page reporting)
```

## Contribuir

Los informes de errores y las pull requests son bienvenidos. Consulta
[CONTRIBUTING.md](../../../CONTRIBUTING.md) para la configuración de desarrollo y las
comprobaciones. Informa de los problemas de seguridad de forma privada
([SECURITY.md](../../../SECURITY.md)).

## Licencia

Apache-2.0 ([LICENSE](../../../LICENSE)). La imagen de invitado que generas descarga
paquetes de Alpine Linux, que tienen sus propias licencias; consulta
[THIRD_PARTY_NOTICES.md](../../../THIRD_PARTY_NOTICES.md).
