# orbit-vmm

[English](../../../README.md) | [한국어](../ko/README.md) | [日本語](../ja/README.md) | [简体中文](../zh-CN/README.md) | [Español](../es/README.md) | [Português (Brasil)](../pt-BR/README.md) | [Français](../fr/README.md) | **Deutsch** | [हिन्दी](../hi/README.md)

> Dies ist eine Übersetzung. Die englische [README](../../../README.md) ist maßgeblich; bei Abweichungen gilt die englische Fassung.

[![CI](https://github.com/skax-eunsik-kim/orbit-vmm/actions/workflows/ci.yml/badge.svg)](https://github.com/skax-eunsik-kim/orbit-vmm/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](../../../LICENSE)

Eine speichersparsame Docker-Engine für Macs mit Apple Silicon.

orbit-vmm ist ein kleiner Virtual Machine Monitor, in Rust geschrieben und auf
Apples Hypervisor.framework aufgebaut. Er bootet einen Alpine-Linux-Gast, auf dem
Docker läuft, und die `docker`- / `docker compose`-CLI auf dem Mac spricht über
einen Unix-Socket mit ihm.

Die meisten Docker-VMs unter macOS wachsen bis zu ihrem Spitzenverbrauch an
Speicher und bleiben dort. orbit-vmm gibt Speicher zurück. Der Gast meldet die
Seiten, die er freigibt (virtio-balloon *Free Page Reporting*), und der VMM gibt
sie innerhalb von Sekunden an macOS zurück. Ein kleiner Daemon im Gast verwirft
außerdem ungenutzten Page Cache und kompaktiert den Speicher (defragmentiert
ihn, damit ganze freie Blöcke gemeldet werden können), damit freigegebener
Speicher tatsächlich zurückgegeben werden kann.

> **Status: früh.** Es betreibt täglich echte Compose-Stacks, ist aber jung,
> hat einen einzigen Maintainer und weder Installer noch GUI. Vor dem Einsatz
> [Bekannte Einschränkungen](#bekannte-einschränkungen) lesen.

## Funktionen

- **Speicher geht an macOS zurück.** Free Page Reporting mit einer Granularität
  von 16 KiB, dazu das Kürzen von ungenutztem Page Cache. Nach dem Ende einer
  Last sinkt der Speicherbedarf der VM auf dem Mac innerhalb einer Minute wieder
  auf etwa 1 GB ([Benchmarks](benchmarks.md)).
- **Bind-Mounts über virtio-fs.** `-v $PWD:/app` funktioniert unter demselben
  Pfad wie auf dem Mac. Es gibt mehrere Request-Queues, Attribut-Caching und
  readdirplus. Änderungen auf dem Mac, neue Dateien und Verzeichnisse,
  Umbenennungen und Löschungen lösen in Containern inotify-Ereignisse aus, sodass
  Hot Reload mit `uvicorn --reload`, vite und nodemon auch bei `git checkout` und
  Codegeneratoren weiter funktioniert. `chown` und `chmod` funktionieren
  innerhalb eines Containers ebenfalls. Beides wird über xattrs emuliert. Die
  echte Datei gehört weiterhin dem eigenen Benutzer und wird auf dem Mac nie
  setuid oder für Gruppe oder alle beschreibbar.
- **Freigabe nur auf Anforderung.** Nur erlaubte Verzeichnisse sind für die VM
  sichtbar, und das Durchlaufen von Pfaden ist sicher gegenüber Symlinks. Ein
  Container mit dem Docker-Socket kann `~/.ssh` nicht lesen.
- **Netzwerk, das sich wie Docker Desktop verhält:**
  - Veröffentlichte Ports erscheinen automatisch auf `127.0.0.1`.
  - `host.docker.internal` und `host-gateway` lösen zum Mac auf.
  - Container vertrauen denselben Root-CAs wie der Mac, sodass `pip`, `npm`,
    `apk` und `curl` hinter TLS-inspizierenden Proxys funktionieren.
- **amd64-Images** laufen über qemu-User-Emulation (binfmt).
- **Mehrere VMs nebeneinander** mit `ORBIT_INSTANCE=<name>`.
- **Klein und überprüfbar.** Ein einzelnes Rust-Binary mit etwa 6.3k Zeilen Code. Es
  hängt von `libc`, `vm-fdt` und libslirp ab, sonst von nichts.

## Voraussetzungen

- Mac mit Apple Silicon, macOS 15 oder neuer (die In-Kernel-GIC-API erfordert 15+).
  Bisher wurde nur macOS 26.4 getestet; auf einer anderen Version zuerst die
  Prüfungen unten ausführen.
- Xcode Command Line Tools (`xcode-select --install`) und
  [Homebrew](https://brew.sh).
- Rust 1.87+ und libslirp: `brew install rust libslirp`.
- Docker-CLI mit den Plugins Compose und Buildx:
  `brew install docker docker-compose docker-buildx`. Homebrew legt die Plugins
  dort ab, wo die CLI standardmäßig nicht sucht, daher sie in
  `~/.docker/config.json` eintragen:

  ```json
  { "cliPluginsExtraDirs": ["/opt/homebrew/lib/docker/cli-plugins"] }
  ```

  Ohne Buildx fällt `docker build` auf den alten Builder zurück, und die
  BuildKit-Prüfungen in `make devflow` schlagen fehl.
- Python 3.11+ für den optionalen Health Check (`brew install python`).
- Freier Speicherplatz: Die VM startet nicht mit weniger als 3 GB frei auf dem
  Mac und warnt unterhalb von 10 GB (`ORBIT_STOP_FREE_GB` / `ORBIT_WARN_FREE_GB`).
  Die Daten von Docker liegen in einer Sparse-Datei `~/.orbit/vmm/data.img`, die
  mit jedem Pull und Build wächst.

### Auf einer macOS-Version, auf der orbit noch nicht getestet wurde

Bisher getestet unter macOS 26.4. Wie Speicher von Hypervisor.framework an macOS
zurückgegeben werden kann und wie die Aktivitätsanzeige ihn zählt, bestimmt
macOS. Daher vor dem Einsatz auf einer neuen Version diese drei Punkte prüfen:

1. **Speicherfreigabe.** Ohne laufende VM:

   ```bash
   vmm/target/release/orbit-vmm --reclaim-selftest
   ```

   Im Block `guest RAM from hv_vm_allocate` muss die Zeile
   `unmap + mmap(MAP_FIXED anon) + map` ungefähr mit `released 1 MiB` enden.
   Das ist die Methode, die der VMM verwendet (`vmm/src/reclaim.rs`). Bleibt der
   Wert bei ~257 MiB, wird freigegebener Gastspeicher auf dieser Version nicht
   zurückgegeben: Die VM wächst auf dem Mac dann nur.
2. **Entwickler-Workflow.** Bei laufender VM muss `make devflow`
   0 fail (25 Prüfungen) melden.
3. **Aktivitätsanzeige.** Ihre Spalte „Speicher“ entspricht dem
   `phys_footprint` des VMM. Wenn der Mac knapp an Speicher ist, komprimiert
   macOS einen Teil des Gast-RAM, und dieser Wert steigt über das hinaus, was der
   Gast nutzt. In Tests lag die Differenz ohne Komprimierung nahe null und unter
   Druck bei etwa einem Drittel bis zur Hälfte der komprimierten Menge. Eine
   andere Differenz auf einer anderen macOS-Version ist eine Änderung in der
   Zählweise von macOS, kein Leck, solange `ORBIT_MEMDUMP` +
   `vmm/bench/memxref.py` zeigen, dass der VMM ungefähr so viel hält, wie der
   Gast nutzt (siehe [docs/architecture.md](architecture.md)).

## Schnellstart

```bash
git clone https://github.com/skax-eunsik-kim/orbit-vmm ~/.local/src/orbit-vmm
cd ~/.local/src/orbit-vmm
make guest      # download Alpine kernel + rootfs (pinned, sha256-checked) into ~/.orbit/guest
make install    # build, sign, copy the VMM, the orbit command and scripts to ~/.orbit/bin, install the watchdog
export PATH="$HOME/.orbit/bin:$PATH"   # put this in your shell profile
orbit up        # start the VM; first boot installs Docker (~1 min, waits up to 5), later boots 2-4 s
export DOCKER_HOST=unix://$HOME/.orbit/vmm/docker.sock
docker run --rm hello-world
```

Die Zeile mit `DOCKER_HOST` ins Shell-Profil eintragen oder einen Context anlegen:

```bash
docker context create orbit --docker host=unix://$HOME/.orbit/vmm/docker.sock
docker context use orbit
```

`orbit status` zeigt die installierte Version und ob die VM läuft.
`orbit up` startet eine laufende VM neu, wodurch alle ihre Container gestoppt
werden.

orbit **außerhalb der freigegebenen Verzeichnisse** klonen (siehe
[Dateifreigabe](#dateifreigabe)), wie oben. Container können in freigegebene
Verzeichnisse schreiben, daher kann ein Checkout in einem davon unbemerkt
verändert werden, und das nächste `make install` oder `make guest` würde diesen
Code auf dem Mac bauen und ausführen. `make install` warnt, wenn der Checkout
in einem freigegebenen Verzeichnis liegt. Nach der Installation führt `orbit`
nur die installierten Kopien in `~/.orbit/bin` aus und nie den Checkout;
`make up`, `make down`, `make update` und `make rollback` rufen es lediglich auf.

## Konfiguration

`scripts/vmm-up.sh` (das, was `orbit up` ausführt) liest diese Umgebungsvariablen:

| Variable | Standard | Bedeutung |
|---|---|---|
| `ORBIT_MEM_MIB` | `8192` | Obergrenze für den Gast-RAM. Freigegebener Speicher geht an macOS zurück, daher kostet eine hohe Obergrenze wenig. |
| `ORBIT_CPUS` | Anzahl der Mac-Kerne (`sysctl -n hw.ncpu`) | vCPUs. Untätige vCPUs ruhen und kosten keine CPU; bei nur wenigen stauen sich parallele Container-Starts (z. B. Agent-Sandboxes) an der CPU. |
| `ORBIT_GUEST` | `~/.orbit/guest` | Verzeichnis mit `kernel` und `initramfs` des Gasts (z. B. eines, das mit `ORBIT_HOME=… scripts/fetch-guest.sh` gebaut wurde). |
| `ORBIT_DISK_GB` | `64` | Größe eines **neuen** Datenträgers (Sparse-Datei). |
| `ORBIT_DISK_WARN_MIB` | `ORBIT_WARN_FREE_GB` in MiB | Unterhalb dieses freien Speicherplatzes auf dem Mac warnt zusätzlich der VMM in `console.log`. |
| `ORBIT_DISK_FLOOR_MIB` | `1024` | Letzte Absicherung unterhalb von `ORBIT_STOP_FREE_GB`: Darunter werden Schreibzugriffe des Gasts auf den Datenträger zurückgehalten, bis Platz frei wird (siehe [Datenträger](#datenträger)). |
| `ORBIT_DISK_PAUSE_SECS` | `600` | Wie lange ein zurückgehaltener Schreibzugriff wartet, bevor er fehlschlägt. |
| `ORBIT_SHARE_ALLOW` | siehe unten | Durch Doppelpunkte getrennte Verzeichnisse, die Container per Bind-Mount einbinden dürfen. |
| `ORBIT_HOST_LOOPBACK` | `1` | `0` verhindert, dass Container Dienste auf `127.0.0.1` des Mac (und `host.docker.internal`) erreichen. |
| `ORBIT_WARN_FREE_GB` / `ORBIT_STOP_FREE_GB` | `10` / `3` | Freier Speicherplatz auf dem Mac, bei dem die VM warnt bzw. bei dem sie sich sauber herunterfährt, bevor ihr Datenträger vollläuft. |
| `ORBIT_INSTANCE` | (nicht gesetzt) | Eine separate VM mit eigenem Datenträger und eigenen Sockets unter `~/.orbit/vmm-<name>` betreiben. |
| `ORBIT_PRESSURE` | (folgt dem Mac) | `off` gibt den Speicherdruck von macOS nicht mehr an den Gast weiter; `warn` oder `critical` legt die Stufe fest (für Messungen). |
| `ORBIT_FS_OWNERSHIP` | (emuliert) | `host` zeigt per Bind-Mount eingebundene Dateien mit ihrem echten Besitzer und Modus auf dem Mac statt mit den emulierten. |
| `ORBIT_FS_TTL` / `ORBIT_FS_ATTR_TTL_MS` | `5` / `500` | Wie lange Container Namen (s) und Attribute (ms) per Bind-Mount eingebundener Dateien cachen. Niedrigere Werte machen Änderungen auf dem Mac früher sichtbar, kosten aber etwas Geschwindigkeit. |

`make guest` (`scripts/fetch-guest.sh`) liest `ALPINE_MIRROR` (Standard
`https://dl-cdn.alpinelinux.org/alpine`; die sha256-Pins gelten weiterhin),
`ORBIT_HOST_CA=0` (siehe [Root-Zertifikate](#root-zertifikate-tls-inspizierende-proxys))
und `ORBIT_HOME` (baut nach `<dir>/guest` statt nach `~/.orbit/guest`).

### Dateifreigabe

Standardmäßig sieht die VM diejenigen von `~/work`, `~/src`, `~/code`, `~/dev`,
`~/projects` und `~/Developer`, die existieren. Alles andere in `/Users` ist
unsichtbar. Um Verzeichnisse hinzuzufügen:

```bash
ORBIT_SHARE_ALLOW="$HOME/src:$HOME/clients" orbit up
```

Freigegebene Verzeichnisse sind **für jeden Container beschreibbar**. orbit-vmm
verweigert die Freigabe des Home-Verzeichnisses, von allem darüber, von
versteckten Verzeichnissen (`~/.ssh`, `~/.orbit`, …), `~/Library`, `~/bin` und
`~/Applications`. Ein Verzeichnis innerhalb eines anderen freigegebenen
Verzeichnisses wird verworfen, da das äußere es bereits abdeckt. Container
können die freigegebenen Verzeichnisse selbst weder umbenennen noch entfernen.

- Änderungen an Dateien auf dem Mac sind in Containern innerhalb von etwa
  0.5 s sichtbar. Neue Dateien und umbenannte Verzeichnisse erscheinen
  innerhalb von etwa 1 s.
- Änderungen und neue Dateien auf dem Mac lösen in Containern etwa 1 s später
  `IN_MODIFY` / `IN_CLOSE_WRITE` aus.

### Netzwerk

- `-p 8080:80` und Compose-`ports:` werden innerhalb von etwa 1 s an
  `127.0.0.1:8080` auf dem Mac weitergeleitet. Ist der Port auf dem Mac belegt,
  schlägt die Weiterleitung fehl, und der Fehler landet in
  `~/.orbit/vmm/console.log`.
- Container erreichen Dienste auf localhost des Mac über `host.docker.internal`.
  Das schließt alles ein, was nur auf `127.0.0.1` lauscht (Datenbanken,
  Notebooks, lokale Modellserver). `ORBIT_HOST_LOOPBACK=0` setzen, wenn das
  nicht gewünscht ist.
- Jeder Prozess, der in der VM auf allen Adressen lauscht, erhält denselben Port
  auf `127.0.0.1` des Mac, nicht nur per `-p` veröffentlichte (wie bei
  `--network host`).

### Root-Zertifikate (TLS-inspizierende Proxys)

- `make guest` liest die Roots, denen der Mac über Apples eingebauten Satz
  hinaus vertraut (die für SSL als „Immer vertrauen“ markierten, typischerweise
  eine TLS-Inspection-CA des Unternehmens), und baut sie in den Gast ein. Auf
  einem Mac ohne solche Roots wird nichts hinzugefügt, und Container sehen die
  eigenen Bundles ihrer Images. Das Repository und die Releases enthalten keine
  Zertifikate.
- Container und Build-Schritte sehen diese Roots über einen Mount an das
  CA-Bundle des Images angehängt. Ein Build-Schritt erhält eine Kopie des
  Bundle-Verzeichnisses, sodass `update-ca-certificates` und
  `apt-get upgrade` funktionieren. Keiner von beiden schreibt die Roots des Mac
  in das Image.
- `ORBIT_HOST_CA=0 make guest && orbit up` baut einen Gast ohne sie.
- Explizite Proxys werden nicht unterstützt: Der Gast verwendet weder die
  HTTP(S)-Proxy-Einstellungen des Mac noch dessen PAC-Datei, und Docker-Pulls
  gehen direkt über das Netzwerk des Mac hinaus. Netzwerke, die Verkehr nur über
  einen expliziten Proxy zulassen, funktionieren noch nicht; ein transparenter
  TLS-inspizierender Proxy dagegen schon.
- Der Gast (`~/.orbit/guest`) und der Datenträger enthalten dann die CAs der
  eigenen Organisation: diese nicht außerhalb davon weitergeben.

## Dateien

| Pfad | Inhalt |
|---|---|
| `~/.orbit/guest/` | Kernel und initramfs, gebaut von `make guest`. |
| `~/.orbit/vmm/data.img` | Datenträger (Docker-Images, Volumes). |
| `~/.orbit/vmm/docker.sock` | Docker-API. |
| `~/.orbit/vmm/console.log` | Gastkonsole; bei Fehlern zuerst hier nachsehen (`console.prev.log` ist der vorherige Lauf). |
| `~/.orbit/vmm/watchdog.log`, `crashes/` | Supervisor-Log und Aufzeichnungen unerwarteter Stopps. |

**`~/.orbit/guest/*` und `data.img` nicht weitergeben.** Sie enthalten
GPL-Binaries und die vertrauenswürdigen Root-Zertifikate des eigenen Rechners
([THIRD_PARTY_NOTICES.md](../../../THIRD_PARTY_NOTICES.md)).

Um den Datenträger zu vergrößern, die VM stoppen, die Datei vergrößern und die
VM wieder starten. Der Gast passt sein Dateisystem beim Booten an:

```bash
python3 -c "import os;p=os.path.expanduser('~/.orbit/vmm/data.img');os.truncate(p,max(os.path.getsize(p),128<<30))"
```

### Datenträger

`data.img` ist eine Sparse-Datei: Sie belegt auf dem Mac so viel Platz, wie
Docker geschrieben hat, bis zu ihrer logischen Größe (`vmm-up.sh` gibt beides
aus). Sie wächst unbemerkt, daher überwachen zwei Schutzmechanismen den freien
Speicherplatz des Mac. Der Supervisor (`vmm-run.sh`) benachrichtigt unterhalb
von `ORBIT_WARN_FREE_GB` und fährt den Gast unterhalb von `ORBIT_STOP_FREE_GB`
sauber herunter (dort startet er auch nicht). Darunter prüft der VMM als letzte
Absicherung alle 2 Sekunden:

- Unterhalb von `ORBIT_DISK_WARN_MIB` schreibt er `orbit-vmm: disk space low`
  in `console.log` (erneut alle 10 Minuten, solange der Platz knapp bleibt), und
  `scripts/healthcheck.py` meldet eine Warnung.
- Unterhalb von `ORBIT_DISK_FLOOR_MIB` oder wenn ein Schreibzugriff fehlschlägt,
  weil der Datenträger voll ist, hält er Schreibzugriffe des Gasts zurück und
  protokolliert einmalig `orbit-vmm: DISK FULL`. Docker stockt (der Gast kann
  Hung-Task-Warnungen protokollieren), aber nichts schlägt fehl. Sobald auf dem
  Mac Platz freigegeben wird, werden die Schreibzugriffe dort fortgesetzt, wo
  sie angehalten wurden (`disk writes resumed`).
- Wird innerhalb von `ORBIT_DISK_PAUSE_SECS` kein Platz frei, schlagen die
  Schreibzugriffe fehl. ext4 meldet dann I/O-Fehler und kann in den
  Nur-Lese-Modus wechseln. Daher vorher Platz freigeben und die VM neu starten,
  falls das passiert ist.

Schreibzugriffe zurückzuhalten statt sie fehlschlagen zu lassen ist das
Standardverhalten von QEMU (`werror=enospc`): Ein fehlgeschlagener Schreibzugriff
sind Daten, deren Speicherung ext4 bereits zugesagt hat, und ihr Verlust ist
genau der Weg, auf dem eine volle Mac-Festplatte den Gast beschädigt.

## Aktualisieren

```bash
orbit update           # the latest release; --to v0.2.1 for a given one
orbit rollback         # back to what was installed before the last update
```

`orbit update` baut das Release in einem eigenen Klon (`~/.orbit/src`) des bei
der Installation festgehaltenen Repositorys, nicht im eigenen Checkout, und baut
den neuen Gast, bevor irgendetwas ersetzt wird. Ein fehlgeschlagener Download
oder Build lässt die installierte Version also unverändert. Die vorherigen
Binaries und der vorherige Gast werden für `orbit rollback` aufbewahrt. Die VM
wird nur neu gestartet, wenn keine Container laufen; andernfalls behält die VM
die alte Version bis `orbit up` (oder `orbit update --now` ausführen). Der erste Boot nach einem Gast-Upgrade
installiert die Gastpakete neu (etwa 1 min).

Ist der Health Check installiert, sucht er einmal täglich nach dem neuesten
Release und zeigt pro neuerem Release eine macOS-Benachrichtigung. Ohne
`orbit update` wird nichts installiert.

Um `main` statt Releases zu folgen: `git pull && make guest install && orbit up`
im eigenen Checkout.

v0.2.1 oder älter installiert? Einmal `make update` aus dem Checkout ausführen;
das installiert den Befehl `orbit`.

## Deinstallation

```bash
orbit uninstall
```

Der Befehl stoppt alle orbit-VMs und entfernt die launchd-Agenten (Watchdog und
Health Check), die Arbeitsverzeichnisse des Health Checks in den freigegebenen
Verzeichnissen sowie `~/.orbit`, **einschließlich der Datenträger mit allen
Images, Containern und Volumes**. Vorher fragt er nach (`--yes` überspringt die
Frage). Der Checkout bleibt erhalten, ebenso ein Docker-Context namens `orbit`,
falls einer angelegt wurde (`docker context rm orbit`).

## Watchdog

`make install` installiert außerdem einen launchd-Agenten, `com.orbit.vmm`, der
die VM betreibt und wiederherstellt. Er existiert, weil sonst nichts diese
Aufgabe übernimmt: orbit hat keine dauerhaft laufende App, sodass ein VMM, der
abstürzt oder hängt, ohne ihn ausgefallen bleibt.

```bash
ORBIT_WATCHDOG=0 make install          # install without it
./scripts/install-watchdog.sh --uninstall
./scripts/install-watchdog.sh --reload # apply a changed agent now (restarts the VM)
```

Eine erneute Installation lässt eine laufende VM unberührt.

- **Absturz:** Stirbt der VMM oder bootet der Gast neu bzw. gerät in eine
  Panic, startet launchd ihn innerhalb von Sekunden erneut.
- **Hänger:** Der Gast wird alle 30 s angepingt. Nach drei verpassten Pings
  wird der VMM abgebrochen, sodass macOS einen Absturzbericht schreibt, und
  anschließend neu gestartet. Das dauert etwa 90 s.
- **Absturzschleife:** Nach mehr als drei Starts innerhalb von 10 Minuten hört
  er mit den Neustarts auf und gibt Bescheid.
- **Absturzaufzeichnungen:** Jeder unerwartete Stopp wird in
  `~/.orbit/vmm/crashes/<time>/` aufbewahrt (Konsolen-Log, macOS-Absturzbericht,
  Watchdog-Log). Die 10 neuesten werden behalten, und es erscheint eine
  macOS-Benachrichtigung. Container stoppen mit der VM, daher müssen solche ohne
  `restart:`-Richtlinie erneut gestartet werden.
- **Absichtlich:** `orbit down` (`vmm-up.sh --stop`) und ein `poweroff` im Gast
  gelten als beabsichtigte Stopps und werden nicht neu gestartet. Eine VM, die
  beim Abmelden lief, startet bei der Anmeldung wieder.

Ohne den Watchdog führt `orbit up` denselben Supervisor im Hintergrund aus, sodass
Hänger weiterhin erkannt und Abstürze weiterhin aufgezeichnet werden, aber
nichts neu gestartet wird.

## Health Check (optional)

Nach `make install` installiert `scripts/install-healthcheck.sh` einen
launchd-Agenten, der das installierte `~/.orbit/bin/healthcheck.py` ausführt. Er
prüft Docker, Ports, Bind-Mounts, inotify und die Uhr des Gasts und
protokolliert Speicher, Thread- und fd-Anzahl des VMM in
`~/.orbit/vmm/health.jsonl`.

Die Prüfung läuft stündlich. Nach drei fehlerfreien Läufen in Folge verlängert
sich das Intervall auf alle 2 h, dann auf alle 4 h. Jedes Problem setzt es auf
stündlich zurück. Zum Entfernen `--uninstall` ausführen.

## Bekannte Einschränkungen

- **inotify:** Änderungen auf dem Mac erreichen Container etwa eine Sekunde
  später. Ein neuer Verzeichnisbaum mit mehr als 256 Einträgen (ein frischer
  Worktree, ein `npm install` auf dem Mac) löst ein einziges `IN_CREATE` für sein
  oberstes Verzeichnis aus; rekursive Watcher durchsuchen ihn von dort aus.
  Umbenennungen werden nur dann paarweise zugeordnet, wenn der Gast den alten
  Namen bereits gesehen hatte; andernfalls erscheinen sie als Löschen +
  Erstellen.
- **Portweiterleitung:** UDP-Veröffentlichungen werden nicht weitergeleitet.
  Veröffentlichungen auf einer bestimmten Host-IP (`-p 127.0.0.1:8080:80`)
  werden wie alle anderen an `127.0.0.1` des Mac weitergeleitet; derselbe Port,
  auf zwei solchen IPs veröffentlicht, erreicht nur die neueste. Veröffentlichen
  zwei VMs denselben Port, schlägt die Weiterleitung der zweiten fehl
  (protokolliert).
- **Bind-Mounts:** Zwischen Mac und Containern werden keine Dateisperren
  geteilt. Unix-Sockets und FIFOs können auf Bind-Mounts nicht angelegt werden.
- **Datenträger:** Platz, den Docker freigibt, geht erst an den Mac zurück, wenn
  der Gast trimmt (`fstrim`, alle 10 Minuten bei ruhiger VM und einmal nach dem
  Booten), daher zeigt sich `docker system prune` auf dem Mac einige Minuten
  später. Das Image schrumpft nie unter das, was der Gast geschrieben, aber nicht
  freigegeben hat.
- **Root-Zertifikate:** Ein Build-Schritt, der das CA-Bundle an einen anderen
  Ort kopiert (`cp /etc/ssl/certs/ca-certificates.crt /app/`), kopiert die Roots
  des Mac mit in das Image. Das Neuerzeugen des Bundles funktioniert in
  Build-Schritten, aber nicht in laufenden Containern, und spätere Befehle im
  selben `RUN` sehen die Roots des Mac nicht. Ist etwas auf das CA-Verzeichnis
  gemountet (`RUN --mount`, `-v`), werden die Roots dort nicht hinzugefügt.
- **amd64-Images** laufen unter qemu-User-Emulation, nicht unter Rosetta, und
  sind daher spürbar langsamer als arm64-Images.
- **Gastpakete:** Kernel und Alpine-Root-Dateisystem sind per sha256 gepinnt,
  aber Docker, qemu und die anderen Gastpakete werden beim ersten Boot nach
  jedem Gast-Build aus den Alpine-Repositorys installiert und ändern sich daher
  mit den Updates von Alpine.
- **Umstieg von Docker Desktop oder OrbStack:** Images, Container und Volumes
  werden nicht migriert. Images erneut pullen oder mit `docker save` /
  `docker load` übertragen; Volume-Inhalte mit einem Container kopieren, der
  beide einbindet.
- **Fehlende Teile:** keine GUI, kein Kubernetes und kein signiertes
  Binär-Release: orbit wird selbst gebaut.

## Funktionsweise

Siehe [docs/architecture.md](architecture.md). Kurz gesagt:

```
docker CLI ──unix socket── orbit-vmm ──vsock── guest: dockerd
                              │
          Hypervisor.framework (vCPUs, in-kernel GICv3)
          virtio-mmio: blk · net (libslirp) · vsock · fs · balloon (free page reporting)
```

## Fehler melden

Ein [Issue](https://github.com/skax-eunsik-kim/orbit-vmm/issues) eröffnen, mit
dem, was ausgeführt wurde, was erwartet wurde und was passiert ist, und
Folgendes anhängen:

- das Ende von `~/.orbit/vmm/console.log` und `~/.orbit/vmm/watchdog.log`;
- `sw_vers -productVersion` und `orbit status`;
- bei einem Absturz das neueste Verzeichnis `~/.orbit/vmm/crashes/<time>/`.

Logs können Pfade und Image-Namen enthalten; vor dem Posten prüfen. Niemals
`~/.orbit/guest`, `data.img` oder `host-ca.pem` anhängen: Sie enthalten die
zusätzlichen Root-Zertifikate des Mac.

## Mitwirken

Fehlerberichte und Pull Requests sind willkommen. Siehe
[CONTRIBUTING.md](../../../CONTRIBUTING.md) für die Entwicklungsumgebung und die Prüfungen.
Sicherheitsprobleme vertraulich melden ([SECURITY.md](../../../SECURITY.md)).

## Lizenz

Apache-2.0 ([LICENSE](../../../LICENSE)). Das selbst gebaute Gast-Image lädt
Alpine-Linux-Pakete herunter, die eigene Lizenzen haben; siehe
[THIRD_PARTY_NOTICES.md](../../../THIRD_PARTY_NOTICES.md).

orbit-vmm ist ein unabhängiges Projekt. Es steht in keiner Verbindung zu
Docker, Inc., OrbStack oder Apple.
