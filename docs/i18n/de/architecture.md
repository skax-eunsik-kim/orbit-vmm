# Architektur

[English](../../architecture.md) | [한국어](../ko/architecture.md) | [日本語](../ja/architecture.md) | [简体中文](../zh-CN/architecture.md) | [Español](../es/architecture.md) | [Português (Brasil)](../pt-BR/architecture.md) | [Français](../fr/architecture.md) | **Deutsch** | [हिन्दी](../hi/architecture.md)

> Dies ist eine Übersetzung. Maßgeblich ist das englische Dokument ([architecture.md](../../architecture.md)); falls beide voneinander abweichen, gilt die englische Fassung.

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

Der VMM ist ein einzelner Rust-Prozess. `src/vm/` enthält die eigentliche Maschine:

- Das FFI für Hypervisor.framework (`hvf.rs`) und den In-Kernel-GIC (`gic.rs`).
- Boot und Device Tree (`boot.rs`, `fdt.rs`).
- Die PL011-Konsole (`pl011.rs`).
- vCPU-Threads, PSCI und MMIO-Dekodierung (`vcpu.rs`).

`src/virtio/` enthält den virtio-mmio-v2-Transport und Split-Virtqueues sowie
eine Datei pro Gerät.

`signals.rs` behandelt SIGTERM und SIGINT, die an den VMM selbst gesendet werden: Es protokolliert
pid und Namen des Absenders in `console.log`, fordert den Guest zum Ausschalten auf (höchstens 30 s)
und beendet sich so, als wäre es durch dieses Signal beendet worden, damit der Supervisor es trotzdem neu startet.
Ein zweites Signal beendet es sofort.

### Speicher zurückgeben

Der Guest-RAM ist anonymer Speicher aus `hv_vm_allocate`, der in die VM gemappt wird. Wenn der Guest
freie Pages meldet (`VIRTIO_BALLOON_F_REPORTING`, Queue 2), geht der VMM so vor:

1. Er entfernt das Mapping des Bereichs aus der VM (`hv_vm_unmap`),
2. ersetzt ihn durch frische Null-Pages (`mmap(MAP_FIXED)`), sodass macOS die
   alten Pages aus dem Footprint des Prozesses entfernt,
3. und mappt den Bereich wieder ein (`hv_vm_map`).

`madvise` allein verkleinert den Footprint von Speicher, der in eine VM gemappt
ist, nicht.

Der Guest-Kernel bootet mit `page_reporting.page_reporting_order=2`, daher ist die
Meldeeinheit 16 KiB, die Page-Größe von macOS. Auch fragmentierter freier Speicher
wird so zurückgegeben.

Linux behält Dateidaten als Page Cache, den der Balloon nicht melden kann.
`guest/etc/orbit/memd.sh` kümmert sich darum:

- Nach 60 s geringer Last gibt es die Hälfte des Page Cache der Container frei
  (cgroup `memory.reclaim`).
- Nach 30 s völligen Leerlaufs verwirft es saubere Caches.
- In beiden Fällen kompaktiert es den Speicher, damit die freigegebenen Pages meldbare Blöcke bilden.
- Jederzeit hält es den Page Cache über `memory.reclaim` der Root-cgroup unter 1 GiB
  (Builds sind nie im Leerlauf; der Mac cacht `data.img` ohnehin).
- Solange macOS Speicherdruck meldet, hält es den Page Cache stattdessen unter 256 MiB
  (warn) bzw. 64 MiB (critical) (`orbit.memd_warn_mb`,
  `orbit.memd_crit_mb`).
- Es drückt nie unter das Working Set. Werden Pages, die die Obergrenze freigegeben hat,
  sofort wieder gelesen (`workingset_refault_file`), bleibt die Obergrenze auf der Größe
  des Cache stehen und lockert sich um ein Viertel pro Minute ohne Refaults.
  Zuvor verlor und las eine Kubernetes-Control-Plane unter macOS-Druck „warn“ etwa
  2.7 GB pro Minute neu; jetzt etwa 0.1–0.2 GB.

`pressure.rs` überwacht den Speicherdruck von macOS (eine libdispatch-Memory-Pressure-Source
sowie `kern.memorystatus_vm_pressure_level`, alle 30 s neu gelesen) und
sendet `pressure normal|warn|critical` über den Control-vsock. Bei einer Änderung
schreibt `control.sh` die Stufe nach `/run/orbit-pressure` und führt
sofort `memd.sh reclaim <level>` aus: Es gibt Speicher bis zur neuen Obergrenze frei (bei critical
werden zuerst alle sauberen Caches verworfen) und kompaktiert, sodass Free Page Reporting die
Pages zurückgibt, bevor macOS sie komprimieren muss. `ORBIT_PRESSURE=off` schaltet das
ab, und `ORBIT_PRESSURE=warn|critical` legt die Stufe für Vergleiche fest. Das
hilft bei Cache, den der Guest nur einmal angefasst hat (die Eingaben eines Builds, ein gepulltes Image);
ein Working Set, das der Guest immer wieder liest, bleibt unabhängig von der Obergrenze resident.
Der Guest wird über jede Änderung informiert; `console.log` erhält höchstens eine Zeile pro
10 Minuten, mit der Anzahl der Änderungen dazwischen.

Die Spalte „Speicher“ in der Aktivitätsanzeige ist der `phys_footprint` des Prozesses. Wenn der
Mac knapp an Speicher ist, komprimiert oder swappt macOS einen Teil des Guest-RAM, und der
Footprint wächst dann über die Guest-Pages hinaus, die der VMM tatsächlich hält (in Messungen um etwa
ein Drittel bis die Hälfte der komprimierten Menge; ohne Komprimierung liegt der Wert nahe null).
Um zu sehen, welche Guest-Pages gehalten werden, startet man den VMM
mit `ORBIT_MEMDUMP=<prefix>`, führt `touch <prefix>.req` aus und gleicht `<prefix>.bin`
(ein `mincore`-Byte pro 16-KiB-Page des Guest-RAM) mit `/proc/kpageflags` des Guests
über `vmm/bench/memxref.py` ab.

### Datenplatte (virtio-blk)

`blk.rs` stellt `data.img` bereit, eine Sparse-Datei, und setzt einen `flock` darauf, damit sich nicht zwei
VMs eine Platte teilen können.

- **Discard.** Discards des Guests werden zu `F_PUNCHHOLE` auf dem Image, und `memd`
  führt nach dem Boot und alle 10 Minuten, wenn die VM ruhig ist, `fstrim /data` aus, sodass
  Speicherplatz, den Docker freigibt, an den Mac zurückgeht.
- **Disk Guard.** Alle 2 s prüft der VMM den freien Speicherplatz auf dem Mac. Unterhalb von
  `ORBIT_DISK_WARN_MIB` protokolliert er eine Warnung. Unterhalb von `ORBIT_DISK_FLOOR_MIB` oder wenn
  ein Schreibvorgang mit ENOSPC fehlschlägt, hält er Schreibvorgänge des Guests an, statt sie fehlschlagen zu lassen
  (wie `werror=enospc` bei QEMU), und zwar für bis zu `ORBIT_DISK_PAUSE_SECS`; sobald wieder
  Platz frei ist, setzt er sie fort, sodass ext4 nie einen Schreibfehler sieht. Der Supervisor stoppt die
  VM bereits früher sauber, unterhalb von `ORBIT_STOP_FREE_GB`.

### Dateifreigabe (virtio-fs)

`fs.rs` ist ein FUSE-Server, und `hostfs.rs` übernimmt den Pfadzugriff.

- Jeder Pfad wird mit `openat(O_NOFOLLOW)` ausgehend von einem Verzeichnis-fd durchlaufen, sodass ein
  Symlink nicht aus den erlaubten Verzeichnissen (`--share-allow`) ausbrechen kann. Verzeichnisse
  oberhalb eines erlaubten Verzeichnisses können durchlaufen, aber nicht aufgelistet werden.
- Der VMM prüft die Allowlist selbst: Einträge müssen echte Pfade sein (keine
  Symlinks, exakte Groß-/Kleinschreibung, nicht verschachtelt), und das Home-Verzeichnis, alles oberhalb
  davon, versteckte Verzeichnisse, `~/Library`, `~/bin` und `~/Applications` werden abgelehnt. Der Guest kann die erlaubten
  Verzeichnisse selbst weder umbenennen noch entfernen.
- Verzeichnis-fds aus früheren Durchläufen werden 1 s lang gecacht, sodass ein Pfad nicht
  jedes Verzeichnis ab `/Users` abwärts erneut öffnet. Eigene Umbenennungen und
  rmdirs des Guests entfernen Einträge sofort, und abgelaufene Einträge werden beim nächsten
  Einfügen geschlossen. Nicht vorhandene Pfade cacht der Guest 1 s lang
  (`ORBIT_FS_NEG_TTL_MS`).
- Es gibt vier Request-Queues.
- Die Entry-TTL beträgt 5 s und die Attribut-TTL 500 ms. `KEEP_CACHE` und
  `AUTO_INVAL_DATA` sind aktiviert, ebenso readdirplus.
- Writeback-Caching ist ausgeschaltet. Mit eingeschaltetem Writeback-Caching waren Änderungen auf dem Mac im
  Guest nicht sichtbar.
- `chown` in Containern wird im xattr `user.orbit.owner` gespeichert und in
  `stat` zurückgemeldet. Die Berechtigungsbits für Gruppe/Andere spiegeln die Owner-Bits, sodass
  Container-Benutzer ohne Root-Rechte in Ihre Dateien schreiben können.
- Änderungen auf dem Mac erreichen den Guest als inotify-Events. `fsevents.rs` überwacht die
  erlaubten Verzeichnisse mit FSEvents; `forward_events` macht aus jedem Batch
  Änderungen, Erstellungen, Umbenennungen und Löschungen (anhand des aktuellen Zustands auf dem Mac und
  dessen, was der Guest bereits nachgeschlagen hat) und sendet sie über vsock an `fsnotify.sh`.
  Der Guest wiederholt jede Änderung unter einer reservierten gid: ein `truncate` auf dieselbe Größe
  für eine Bearbeitung (`IN_MODIFY`), create/mkdir, rename oder unlink/rmdir für den
  Rest. Der Server erkennt diese gid, wendet nichts davon auf den Mac an
  und lässt es nur für Pfade gelingen, die er selbst eingereiht hat, solange der Mac noch übereinstimmt.
- Das Verzeichnis des Docker-Sockets (`~/.orbit/vmm/`) erscheint im Guest als
  leerer, schreibgeschützter Mount-Point, und der Guest legt dort einen Link auf `docker.sock` an, sodass
  Tools funktionieren, die den `DOCKER_HOST`-Pfad per Bind-Mount einbinden (Ryuk von testcontainers).

### Netzwerk

`slirp.rs` führt libslirp in einem eigenen Thread aus.

- `ports.rs` leitet veröffentlichte Ports an `127.0.0.1` des Macs weiter. Es verfolgt
  die Container-Events von Docker (`/events` auf dem eigenen Docker-Socket des VMM) und
  leitet ein `-p`-Publish weiter, sobald der Container startet, einschließlich Ports,
  die auf einer bestimmten Host-IP veröffentlicht sind. Zusätzlich liest es alle 250 ms über den Control-Kanal die
  lauschenden Sockets des Guests, für Services mit `--network host`.
- Container erreichen localhost des Macs über `host.docker.internal`
  (`ORBIT_HOST_LOOPBACK=0` schaltet das ab).
- DNS: Container fragen den dnsmasq des Guests, der
  `host.docker.internal` beantwortet und alles andere an den Resolver von libslirp und von
  dort an den des Macs weiterleitet. Er erlaubt 4096 gleichzeitig laufende Anfragen; der Standardwert von dnsmasq,
  150, ließ parallele Builds mit "Temporary failure in name resolution" fehlschlagen.
- Der Control-Kanal hält außerdem die Uhr des Guests synchron. Er misst die
  Round-Trip-Time und stellt die Uhr nur dann, wenn sie um mehr als 250 ms abweicht.
  Die Uhr kann daher zurückspringen: Code im Guest darf sich nicht auf mtimes verlassen, um
  festzustellen, ob sich etwas geändert hat.
- `csrc/asyncclose.c` ist eine kleine dylib, die `close()` für Internet-Sockets
  abfängt (Interposing). Network Extensions mit Content-Filter können `close()` nach FIN
  für etwa 1 s blockieren lassen, was jede Verbindung im Netzwerk-Thread aufhalten würde.

## Guest (`guest/`)

`scripts/fetch-guest.sh` lädt einen gepinnten Alpine-Kernel und ein Minirootfs herunter
(per sha256 verifiziert) und baut daraus sowie aus `guest/` ein initramfs. Die
Pakete, die der erste Boot installiert (Docker, qemu, …), sind nicht gepinnt: Sie stammen
aus den Alpine-Repositorys zum jeweiligen Zeitpunkt. Alpine
entfernt alte Paketversionen, daher prüft der wöchentliche `guest-pins`-Workflow, dass
beide gepinnten Downloads noch existieren.

- **Erster Boot:** installiert Docker und zugehörige Tools auf die Datenplatte und erstellt dann einen
  schreibgeschützten Snapshot des Root-Dateisystems. Docker stammt aus Alpine v3.23 (29.5): 29.8 baute
  bei jedem warmen Build jeden Layer über einem lokal gebauten Base-Image neu.
- **Spätere Boots:** verwenden diesen Snapshot unter einem tmpfs-Overlay und brauchen daher
  2–4 s.

Bestandteile des Guests:

- `dockerd.sh` schreibt `daemon.json` (Build-Cache mit
  `defaultMaxUsedSpace` auf 20 GB begrenzt) und startet dockerd mit `orbit-runc`.
- `orbit-runc` ist ein Wrapper um die OCI-Runtime, der Container den Root-Zertifikaten vertrauen lässt,
  denen der Mac über Apples Satz hinaus vertraut. `make guest` exportiert diese
  (`scripts/export-host-ca.py`, meist eine CA zur TLS-Inspektion des Unternehmens; auf
  den meisten Macs keine, und mit `ORBIT_HOST_CA=0` keine) nach `/etc/orbit/host-ca.pem`.
  - Ein Container erhält eine Kopie des CA-Bundles des Images, an die diese angehängt sind,
    schreibgeschützt per Bind-Mount über das Bundle gelegt. Ein Bundle-Pfad, der ein
    Symlink ist (RHEL, Fedora und Amazon Linux verlinken ihren nach `ca-trust/`), wird so
    aufgelöst, wie der Container ihn sehen würde, und zwar nur innerhalb seines rootfs; die
    Kopie wird über die Datei gelegt, auf die er zeigt.
  - In einem von `orbit k8s` erzeugten kind-Node startet das containerd des Nodes Pods über
    denselben Wrapper (aus dem Gast zusammen mit `host-ca.pem` eingebunden und als runc-
    `BinaryName` gesetzt), sodass Pods dieselbe Behandlung erhalten.
  - Ein `RUN`-Schritt eines Builds erhält stattdessen eine beschreibbare Kopie des Bundle-Verzeichnisses,
    sodass Tools funktionieren, die das Bundle per Umbenennung neu erzeugen. runc läuft als
    Kindprozess; wenn er zurückkehrt (BuildKit übergibt `--keep`, sodass das rootfs noch
    gemountet ist), wird das Verzeichnis mit dem eigenen Bundle des Images in das rootfs des Schritts
    zurückgeschrieben, und zwar nur, wenn sich sein Inhalt geändert hat. Die Root-Zertifikate des Hosts
    gelangen nie in einen Layer.
  - Ein Verzeichnis, das das Verzeichnis eines anderen Bundles oder einen Mount enthält, erhält stattdessen den
    Datei-Bind-Mount, und ein Bundle an oder unterhalb eines bestehenden Mounts
    (`RUN --mount`, `-v`) bleibt unangetastet.
- `dns.sh` führt dnsmasq aus (siehe Netzwerk).
- `binfmt.sh` registriert qemu-x86_64.
- `memd.sh`: siehe oben.

## Auf dem Mac (`scripts/`)

- `make install` kopiert den VMM, seinen `close()`-Interposer, `vmm-up.sh`,
  `vmm-run.sh` und `healthcheck.py` nach `~/.orbit/bin`, schreibt
  `~/.orbit/bin/VERSION` und installiert den launchd-Watchdog. Die VM läuft immer nur
  aus dieser Kopie: Container können in den Checkout schreiben.
- `vmm-run.sh` überwacht die VM, mit oder ohne launchd. Es lädt die Einstellungen, die
  `vmm-up.sh` in `config.env` gespeichert hat, und exportiert sie, da launchd es ohne die
  Umgebung des Benutzers startet.
  - Es erkennt einen Absturz, einen Reboot oder Panic des Guests oder einen Hänger (drei
    verpasste Pings auf dem Control-Socket; der VMM wird zuvor abgebrochen, damit macOS einen
    Crash Report schreibt). Nur unter launchd (dem Watchdog) wird die VM danach neu gestartet;
    ohne ihn hält der Supervisor den Stopp fest und beendet sich.
  - Es hört nach mehr als drei Starts innerhalb von 10 Minuten mit dem Neustarten auf.
  - Es hält jeden unerwarteten Stopp in `crashes/<time>/` fest.
  - Es warnt unterhalb von `ORBIT_WARN_FREE_GB` und stoppt die VM unterhalb von
    `ORBIT_STOP_FREE_GB` sauber.
- `healthcheck.py` prüft, sofern installiert, Docker, Ports, Bind-Mounts, inotify,
  die Uhr und den Speicherplatz, erfasst Speicher, Threads und fds des VMM, liest einmal
  täglich das neueste Release von der Seite `releases/latest` und zeigt pro neuerem Release
  eine Benachrichtigung an.
- `orbit` ist der installierte Befehl: `up` und `down` führen `vmm-up.sh` aus, `update`
  und `rollback` führen `update.sh` aus, `status` gibt den Zustand aus, und `uninstall` entfernt die
  Agenten, die Arbeitsverzeichnisse des Health Checks und `~/.orbit`. Die Run-Targets des
  Makefile rufen ihn auf, sodass nichts aus dem Checkout ausgeführt wird.
  `orbit k8s up|down|status` legt kind-Cluster mit dem oben beschriebenen Node an und
  löscht sie; `uninstall` löscht sie ebenfalls, wodurch auch ihre kubectl-Kontexte
  entfernt werden.
- `update.sh` (`orbit update`, `orbit rollback`) baut ein Release-Tag in einem eigenen
  Clone (`~/.orbit/src`) des in `~/.orbit/bin/VERSION` festgehaltenen Repositorys und den
  neuen Guest in einem Staging-Verzeichnis, behält die
  vorherigen Binaries und den vorherigen Guest für ein Rollback und startet die VM nur neu, wenn keine
  Container laufen.
