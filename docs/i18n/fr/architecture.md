# Architecture

[English](../../architecture.md) | [한국어](../ko/architecture.md) | [日本語](../ja/architecture.md) | [简体中文](../zh-CN/architecture.md) | [Español](../es/architecture.md) | [Português (Brasil)](../pt-BR/architecture.md) | **Français** | [Deutsch](../de/architecture.md) | [हिन्दी](../hi/architecture.md)

> Ce document est une traduction. Le document anglais ([`architecture.md`](../../architecture.md)) fait foi ; en cas de divergence, suivez-le.

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

Le VMM est un unique processus Rust. `src/vm/` contient la machine elle-même :

- La FFI Hypervisor.framework (`hvf.rs`) et le GIC intégré au noyau (`gic.rs`).
- Le démarrage et le device tree (`boot.rs`, `fdt.rs`).
- La console PL011 (`pl011.rs`).
- Les threads vCPU, PSCI et le décodage MMIO (`vcpu.rs`).

`src/virtio/` contient le transport virtio-mmio v2 et les virtqueues scindées
(split virtqueues), ainsi qu'un fichier par périphérique.

`signals.rs` gère les SIGTERM et SIGINT envoyés au VMM lui-même : il consigne le
pid et le nom de l'émetteur dans `console.log`, demande à l'invité de s'éteindre (30 s
au maximum), puis se termine comme tué par ce signal, afin que le superviseur le redémarre quand même.
Un second signal le fait quitter immédiatement.

### Restitution de la mémoire

La RAM de l'invité est de la mémoire anonyme issue de `hv_vm_allocate`, mappée dans la VM. Quand l'invité
signale des pages libres (`VIRTIO_BALLOON_F_REPORTING`, file 2), le VMM :

1. retire le mappage de la plage dans la VM (`hv_vm_unmap`),
2. la remplace par de nouvelles pages à zéro (`mmap(MAP_FIXED)`), afin que macOS retire
   les anciennes pages de l'empreinte du processus,
3. remappe la plage (`hv_vm_map`).

`madvise` seul ne réduit pas l'empreinte d'une mémoire mappée dans une
VM.

Le noyau invité démarre avec `page_reporting.page_reporting_order=2`, si bien que
l'unité de signalement est de 16 KiB, la taille de page de macOS. La mémoire libre fragmentée
est donc tout de même restituée.

Linux conserve les données de fichiers en page cache, que le balloon ne peut pas signaler.
`guest/etc/orbit/memd.sh` s'en charge :

- Après 60 s de charge légère, il récupère la moitié du page cache des conteneurs
  (cgroup `memory.reclaim`).
- Après 30 s d'inactivité complète, il vide les caches propres.
- Dans les deux cas, il compacte la mémoire pour que les pages libérées forment des blocs signalables.
- En permanence, il maintient le page cache sous 1 GiB via le `memory.reclaim`
  du cgroup racine (les builds ne sont jamais inactifs ; le Mac met de toute façon `data.img` en cache).
- Tant que macOS signale une pression mémoire, il maintient plutôt le page cache sous 256 MiB
  (warn) ou 64 MiB (critical) (`orbit.memd_warn_mb`,
  `orbit.memd_crit_mb`).

`pressure.rs` surveille la pression mémoire de macOS (une source memory-pressure
libdispatch, plus `kern.memorystatus_vm_pressure_level` relu toutes les 30 s) et
envoie `pressure normal|warn|critical` sur le vsock de contrôle. À chaque changement,
`control.sh` enregistre le niveau dans `/run/orbit-pressure` et exécute
immédiatement `memd.sh reclaim <level>` : récupération jusqu'au nouveau plafond (en critical,
vidage préalable de tous les caches propres) puis compactage, afin que le free page reporting rende les
pages avant que macOS ait à les compresser. `ORBIT_PRESSURE=off` désactive ce mécanisme,
et `ORBIT_PRESSURE=warn|critical` fige le niveau, pour les comparaisons. Cela
aide pour le cache que l'invité n'a touché qu'une fois (les entrées d'un build, une image tirée) ;
un working set que l'invité relit sans cesse reste résident quel que soit le plafond.
L'invité est informé de chaque changement ; `console.log` reçoit au plus une ligne toutes les
10 minutes, avec le nombre de changements survenus entre-temps.

La colonne Mémoire du Moniteur d'activité correspond au `phys_footprint` du processus. Quand le
Mac manque de mémoire, macOS compresse ou place en swap une partie de la RAM de l'invité, et
l'empreinte dépasse alors les pages de l'invité que le VMM détient réellement (d'environ
un tiers à la moitié de la quantité compressée selon les mesures ; elle est quasi nulle
quand rien n'est compressé). Pour voir quelles pages de l'invité sont détenues, lancez le VMM
avec `ORBIT_MEMDUMP=<prefix>`, faites `touch <prefix>.req`, puis rapprochez `<prefix>.bin`
(un octet `mincore` par page de 16 KiB de RAM de l'invité) du
`/proc/kpageflags` de l'invité avec `vmm/bench/memxref.py`.

### Disque de données (virtio-blk)

`blk.rs` sert `data.img`, un fichier creux (sparse), et pose un `flock` dessus afin que deux
VM ne puissent pas partager un même disque.

- **Discard.** Les discards de l'invité deviennent des `F_PUNCHHOLE` sur l'image, et `memd`
  exécute `fstrim /data` après le démarrage puis toutes les 10 minutes quand la VM est calme, afin
  que l'espace libéré par Docker revienne au Mac.
- **Garde disque.** Toutes les 5 s, le VMM vérifie l'espace libre sur le Mac. Sous
  `ORBIT_DISK_WARN_MIB`, il consigne un avertissement. Sous `ORBIT_DISK_FLOOR_MIB`, ou quand
  une écriture échoue avec ENOSPC, il met en attente les écritures de l'invité au lieu de les faire échouer
  (le `werror=enospc` de QEMU) pendant au plus `ORBIT_DISK_PAUSE_SECS`, et reprend dès que
  de l'espace est libéré, si bien qu'ext4 ne voit jamais d'erreur d'écriture. Le superviseur arrête
  proprement la VM plus tôt, sous `ORBIT_STOP_FREE_GB`.

### Partage de fichiers (virtio-fs)

`fs.rs` est un serveur FUSE et `hostfs.rs` gère l'accès aux chemins.

- Chaque chemin est parcouru avec `openat(O_NOFOLLOW)` à partir d'un fd de répertoire, afin qu'un
  lien symbolique ne puisse pas sortir des répertoires autorisés (`--share-allow`). Les répertoires
  situés au-dessus d'un répertoire autorisé peuvent être traversés mais pas listés.
- Le VMM vérifie lui-même la liste d'autorisation : les entrées doivent être des chemins réels (pas
  de liens symboliques, casse exacte, pas d'imbrication), et le répertoire personnel, tout ce qui se trouve
  au-dessus, les répertoires cachés et `~/Library` sont refusés. L'invité ne peut ni renommer
  ni supprimer les répertoires autorisés eux-mêmes.
- Les fds de répertoire issus des parcours précédents sont mis en cache pendant 1 s, afin qu'un chemin ne
  rouvre pas chaque répertoire depuis `/Users`. Les renommages et
  rmdir effectués par l'invité lui-même retirent immédiatement les entrées, et les entrées expirées sont fermées à
  l'insertion suivante. Les chemins inexistants sont mis en cache par l'invité pendant 1 s
  (`ORBIT_FS_NEG_TTL_MS`).
- Il y a quatre files de requêtes.
- Le TTL des entrées est de 5 s et celui des attributs de 500 ms. `KEEP_CACHE` et
  `AUTO_INVAL_DATA` sont activés, de même que readdirplus.
- Le cache writeback est désactivé. Lorsqu'il était activé, les modifications faites côté Mac n'étaient pas visibles dans
  l'invité.
- Le `chown` des conteneurs est stocké dans l'xattr `user.orbit.owner` et renvoyé
  dans `stat`. Les bits de permission groupe/autres reflètent ceux du propriétaire, afin que
  les utilisateurs non root des conteneurs puissent écrire dans vos fichiers.
- Les modifications faites sur le Mac parviennent à l'invité sous forme d'événements inotify. `fsevents.rs` surveille les
  répertoires autorisés avec FSEvents ; `forward_events` transforme chaque lot en
  modifications, créations, renommages et suppressions (d'après l'état actuel du Mac et
  ce que l'invité a consulté) et les envoie à `fsnotify.sh` via vsock.
  L'invité rejoue chaque changement sous un gid réservé : un `truncate` à taille identique
  pour une modification (`IN_MODIFY`), create/mkdir, rename ou unlink/rmdir pour le
  reste. Le serveur reconnaît ce gid, n'en applique jamais rien au Mac,
  et ne le laisse réussir que pour les chemins qu'il a mis en file tant que le Mac correspond encore.
- Le répertoire du socket docker (`~/.orbit/vmm/`) est présenté à l'invité comme un
  point de montage vide en lecture seule, et l'invité y crée un lien vers `docker.sock`, afin que
  les outils qui montent en bind le chemin de `DOCKER_HOST` (Ryuk de testcontainers) fonctionnent.

### Réseau

`slirp.rs` exécute libslirp dans son propre thread.

- `ports.rs` redirige les ports publiés vers le `127.0.0.1` du Mac. Il suit
  les événements de conteneurs de Docker (`/events` sur le propre socket docker du VMM) et
  redirige une publication `-p` dès que le conteneur démarre, y compris les ports
  publiés sur une IP d'hôte précise. Toutes les 250 ms, il lit aussi les sockets
  en écoute de l'invité via le canal de contrôle, pour les services en `--network host`.
- Les conteneurs atteignent le localhost du Mac via `host.docker.internal`
  (`ORBIT_HOST_LOOPBACK=0` désactive cela).
- DNS : les conteneurs interrogent le dnsmasq de l'invité, qui répond pour
  `host.docker.internal` et transmet le reste au résolveur de libslirp, puis de
  là à celui du Mac. Il autorise 4096 requêtes simultanées ; la valeur par défaut de dnsmasq,
  150, faisait échouer les builds parallèles avec "Temporary failure in name resolution".
- Le canal de contrôle maintient aussi l'horloge de l'invité synchronisée. Il mesure le
  temps d'aller-retour et ne recale l'horloge que lorsqu'elle dérive de plus de 250 ms.
  L'horloge peut donc reculer : le code de l'invité ne doit pas se fier aux mtimes pour
  savoir si quelque chose a changé.
- `csrc/asyncclose.c` est une petite dylib qui intercepte `close()` pour les sockets
  internet. Les extensions réseau de filtrage de contenu peuvent faire bloquer `close()` après FIN
  pendant environ 1 s, ce qui bloquerait toutes les connexions du thread réseau.

## Invité (`guest/`)

`scripts/fetch-guest.sh` télécharge un noyau Alpine et un minirootfs à versions figées
(vérifiés par sha256) et construit à partir d'eux et de `guest/` un initramfs. Alpine
supprime les anciennes versions de paquets, si bien que le workflow hebdomadaire `guest-pins` vérifie que
les deux téléchargements figés existent toujours.

- **Premier démarrage :** installe Docker et ses compagnons sur le disque de données, puis prend un
  snapshot racine en lecture seule. Docker provient d'Alpine v3.23 (29.5) : la 29.8 reconstruisait
  toutes les couches au-dessus d'une image de base construite localement à chaque build à chaud.
- **Démarrages suivants :** utilisent ce snapshot sous un overlay tmpfs, et prennent donc
  2–4 s.

Composants de l'invité :

- `dockerd.sh` écrit `daemon.json` (cache de build plafonné à 20 GB avec
  `defaultMaxUsedSpace`) et démarre dockerd avec `orbit-runc`.
- `orbit-runc` est un wrapper de runtime OCI qui fait en sorte que les conteneurs fassent confiance aux racines
  auxquelles le Mac fait confiance en plus de celles d'Apple. `make guest` les exporte
  (`scripts/export-host-ca.py`, généralement une CA d'inspection TLS d'entreprise ; aucune sur
  la plupart des Mac, et aucune avec `ORBIT_HOST_CA=0`) dans `/etc/orbit/host-ca.pem`.
  - Un conteneur reçoit une copie du bundle de CA de l'image à laquelle elles sont ajoutées,
    montée en bind en lecture seule par-dessus le bundle.
  - Une étape `RUN` de build reçoit à la place une copie en lecture-écriture du répertoire du bundle,
    pour que les outils qui régénèrent le bundle par renommage fonctionnent. runc s'exécute en tant
    que processus enfant ; quand il rend la main (BuildKit passe `--keep`, donc le rootfs est encore
    monté), le répertoire est réécrit dans le rootfs de l'étape avec le
    bundle propre à l'image, uniquement si son contenu a changé. Les racines de l'hôte n'atteignent
    jamais une couche.
  - Un répertoire qui contient le répertoire d'un autre bundle ou un montage reçoit à la place le
    bind du fichier, et un bundle situé sur ou sous un montage existant
    (`RUN --mount`, `-v`) est laissé tel quel.
- `dns.sh` exécute dnsmasq (voir Réseau).
- `binfmt.sh` enregistre qemu-x86_64.
- `memd.sh` : voir plus haut.

## Sur le Mac (`scripts/`)

- `make install` copie le VMM, son intercepteur de `close()`, `vmm-up.sh`,
  `vmm-run.sh` et `healthcheck.py` dans `~/.orbit/bin`, écrit
  `~/.orbit/bin/VERSION` et installe le watchdog launchd. La VM ne s'exécute
  jamais qu'à partir de cette copie : les conteneurs peuvent écrire dans le checkout.
- `vmm-run.sh` supervise la VM, avec ou sans launchd :
  - Il la redémarre après un crash, un redémarrage ou une panique de l'invité, ou un blocage (trois
    pings du socket de contrôle sans réponse ; le VMM est d'abord avorté pour que macOS écrive un
    rapport de crash).
  - Il cesse de la redémarrer après plus de trois démarrages en 10 minutes.
  - Il conserve un enregistrement de chaque arrêt inattendu dans `crashes/<time>/`.
  - Il avertit sous `ORBIT_WARN_FREE_GB` et arrête proprement la VM sous
    `ORBIT_STOP_FREE_GB`.
- `healthcheck.py`, s'il est installé, vérifie Docker, les ports, les bind mounts, inotify,
  l'horloge et l'espace disque, enregistre la mémoire, les threads et les fds du VMM, et une
  fois par jour lit la dernière release sur la page `releases/latest` et affiche une
  notification par release plus récente.
- `update.sh` (`make update`, `make rollback`) construit un tag de release dans son propre
  clone (`~/.orbit/src`) et le nouvel invité dans un répertoire de staging, conserve les
  binaires et l'invité précédents pour le rollback, et ne redémarre la VM que lorsqu'aucun
  conteneur ne tourne.
