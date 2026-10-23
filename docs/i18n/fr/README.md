# orbit-vmm

[English](../../../README.md) | [한국어](../ko/README.md) | [日本語](../ja/README.md) | [简体中文](../zh-CN/README.md) | [Español](../es/README.md) | [Português (Brasil)](../pt-BR/README.md) | **Français** | [Deutsch](../de/README.md) | [हिन्दी](../hi/README.md)

> Ceci est une traduction. Le [README anglais](../../../README.md) fait référence ; en cas de divergence, suivez la version anglaise.

[![CI](https://github.com/skax-eunsik-kim/orbit-vmm/actions/workflows/ci.yml/badge.svg)](https://github.com/skax-eunsik-kim/orbit-vmm/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](../../../LICENSE)

Un moteur Docker économe en mémoire pour les Mac Apple Silicon.

orbit-vmm est un petit moniteur de machine virtuelle écrit en Rust au-dessus de
Hypervisor.framework d'Apple. Il démarre un invité Alpine Linux qui exécute Docker,
et la CLI `docker` / `docker compose` de votre Mac communique avec lui via un socket Unix.

La plupart des VM Docker sur macOS grossissent jusqu'à leur pic d'utilisation
mémoire et y restent. orbit-vmm, lui, rend la mémoire. L'invité signale les pages
qu'il libère (*free page reporting* de virtio-balloon), et le VMM les restitue à
macOS en quelques secondes. Un petit démon dans l'invité vide aussi le cache de
pages inactif et compacte la mémoire, afin que la mémoire libérée puisse
réellement être rendue.

> **Statut : précoce.** Il fait tourner de vraies stacks compose tous les jours,
> mais il est jeune, n'a qu'un seul mainteneur et ne dispose ni d'installeur ni
> d'interface graphique. Lisez les [Limitations connues](#limitations-connues)
> avant d'en dépendre.

## Fonctionnalités

- **La mémoire retourne à macOS.** Free page reporting à une granularité de
  16 KiB, plus élagage du cache de pages inactif. Après la fin d'une charge de
  travail, l'empreinte de la VM sur le Mac redescend à environ 1 GB en moins
  d'une minute ([benchmarks](benchmarks.md)).
- **Bind mounts via virtio-fs.** `-v $PWD:/app` fonctionne au même chemin que
  sur le Mac. Plusieurs files de requêtes, mise en cache des attributs et
  readdirplus sont pris en charge. Les modifications faites sur le Mac, les
  nouveaux fichiers et répertoires, les renommages et les suppressions
  déclenchent des événements inotify dans les conteneurs, de sorte que le
  rechargement à chaud de `uvicorn --reload`, vite et nodemon continue de
  fonctionner à travers un `git checkout` et les générateurs de code. `chown` et `chmod` fonctionnent aussi dans un conteneur.
  Les deux sont émulés avec des xattrs. Le fichier réel reste le vôtre et ne
  devient jamais setuid ni accessible en écriture au groupe ou à tous sur le Mac.
- **Partage sur autorisation explicite.** Seuls les répertoires autorisés sont
  visibles par la VM, et le parcours des chemins est sûr vis-à-vis des liens
  symboliques. Un conteneur ayant accès au socket Docker ne peut pas lire
  `~/.ssh`.
- **Un réseau qui se comporte comme Docker Desktop :**
  - Les ports publiés apparaissent automatiquement sur `127.0.0.1`.
  - `host.docker.internal` et `host-gateway` se résolvent vers le Mac.
  - Les conteneurs font confiance aux CA racine auxquelles votre Mac fait
    confiance, donc `pip`, `npm`, `apk` et `curl` fonctionnent derrière des
    proxys d'inspection TLS.
- **Les images amd64** s'exécutent via l'émulation utilisateur qemu (binfmt).
- **Plusieurs VM côte à côte** avec `ORBIT_INSTANCE=<name>`.
- **Petit et auditable.** Un seul binaire Rust d'environ 5,5k lignes. Il dépend
  de `libc`, `vm-fdt` et libslirp, et de rien d'autre.

## Prérequis

- Mac Apple Silicon, macOS 15 ou ultérieur (l'API GIC intégrée au noyau
  nécessite la version 15+).
- Rust 1.87+ et libslirp : `brew install rust libslirp`.
- CLI Docker avec le plugin compose : `brew install docker docker-compose`.

### Sur une version de macOS sur laquelle orbit n'a pas été testé

Testé jusqu'à présent sur macOS 26.4. La manière dont la mémoire de
Hypervisor.framework peut être rendue à macOS, et la manière dont le Moniteur
d'activité la comptabilise, dépendent de macOS ; vérifiez donc ces trois points
sur une nouvelle version avant de vous y fier :

1. **Libération de la mémoire.** Sans aucune VM en cours d'exécution :

   ```bash
   vmm/target/release/orbit-vmm --reclaim-selftest
   ```

   Dans le bloc `guest RAM from hv_vm_allocate`, la ligne
   `unmap + mmap(MAP_FIXED anon) + map` doit se terminer à peu près par
   `released 1 MiB`. C'est la méthode qu'utilise le VMM (`vmm/src/reclaim.rs`).
   Si elle reste à ~257 MiB, la mémoire libérée par l'invité n'est pas rendue
   sur cette version : la VM ne fait que grossir sur le Mac.
2. **Workflow de développement.** Avec la VM démarrée : `make devflow` doit
   indiquer 0 fail (25 vérifications).
3. **Moniteur d'activité.** Sa colonne Mémoire correspond au `phys_footprint`
   du VMM. Lorsque le Mac manque de mémoire, macOS compresse une partie de la
   RAM de l'invité et ce nombre dépasse ce que l'invité utilise. Lors des tests,
   l'écart était proche de zéro sans compression, et d'environ un tiers à la
   moitié de la quantité compressée sous pression. Un écart différent sur une
   autre version de macOS traduit un changement dans la façon dont macOS
   comptabilise, et non une fuite, tant que `ORBIT_MEMDUMP` + `vmm/bench/memxref.py`
   montrent que le VMM détient à peu près ce que l'invité utilise (voir
   [docs/architecture.md](architecture.md)).

## Démarrage rapide

```bash
git clone https://github.com/skax-eunsik-kim/orbit-vmm ~/src/orbit-vmm
cd ~/src/orbit-vmm
make guest      # download pinned Alpine kernel + rootfs (sha256-checked) into ~/.orbit/guest
make install    # build, sign, copy the VMM and scripts to ~/.orbit/bin, install the watchdog
make up         # start the VM from ~/.orbit/bin; first boot installs Docker (~1 min), later boots 2-4 s
export DOCKER_HOST=unix://$HOME/.orbit/vmm/docker.sock
docker run --rm hello-world
```

Placez la ligne `DOCKER_HOST` dans le profil de votre shell, ou créez un contexte :

```bash
docker context create orbit --docker host=unix://$HOME/.orbit/vmm/docker.sock
docker context use orbit
```

`make up` (ou `~/.orbit/bin/vmm-up.sh`) redémarre une VM en cours d'exécution,
ce qui arrête tous ses conteneurs. La VM et le contrôle de santé s'exécutent
uniquement depuis la copie installée dans `~/.orbit/bin`. Les conteneurs peuvent
écrire dans vos répertoires partagés, et ce checkout en fait souvent partie ;
c'est pourquoi `make up` ne construit ni n'exécute rien depuis celui-ci. Seuls
`make install` et `make guest` le font. Avant de les lancer, vérifiez que le
checkout n'a pas changé de manière inattendue (`git status`). Mieux encore,
gardez le checkout en dehors de vos répertoires partagés.

## Configuration

`scripts/vmm-up.sh` (ce que lance `make up`) lit ces variables d'environnement :

| Variable | Valeur par défaut | Signification |
|---|---|---|
| `ORBIT_MEM_MIB` | `8192` | Plafond de RAM de l'invité. La mémoire libérée retourne à macOS, donc un plafond élevé coûte peu. |
| `ORBIT_CPUS` | Nombre de cœurs du Mac (`sysctl -n hw.ncpu`) | vCPU. Les vCPU inactifs sont mis en attente et ne consomment pas de CPU ; avec seulement quelques-uns, le démarrage parallèle de conteneurs (p. ex. des sandboxes d'agents) fait la queue pour le CPU. |
| `ORBIT_GUEST` | `~/.orbit/guest` | Répertoire contenant le `kernel` et l'`initramfs` de l'invité (p. ex. un invité construit avec `ORBIT_HOME=… scripts/fetch-guest.sh`). |
| `ORBIT_DISK_GB` | `64` | Taille d'un **nouveau** disque de données (fichier sparse). |
| `ORBIT_DISK_WARN_MIB` | `ORBIT_WARN_FREE_GB` en MiB | En dessous de cet espace libre sur le Mac, le VMM émet aussi un avertissement dans `console.log`. |
| `ORBIT_DISK_FLOOR_MIB` | `1024` | Dernier recours sous `ORBIT_STOP_FREE_GB` : en dessous, les écritures disque de l'invité sont mises en attente jusqu'à ce que de l'espace soit libéré (voir [Disque de données](#disque-de-données)). |
| `ORBIT_DISK_PAUSE_SECS` | `600` | Durée pendant laquelle une écriture en attente patiente avant d'échouer. |
| `ORBIT_SHARE_ALLOW` | voir ci-dessous | Répertoires séparés par des deux-points que les conteneurs peuvent monter en bind mount. |
| `ORBIT_HOST_LOOPBACK` | `1` | `0` empêche les conteneurs d'atteindre les services sur le `127.0.0.1` du Mac (et `host.docker.internal`). |
| `ORBIT_WARN_FREE_GB` / `ORBIT_STOP_FREE_GB` | `10` / `3` | Espace libre sur le disque du Mac à partir duquel la VM vous avertit, et à partir duquel elle s'arrête proprement avant que son disque ne se remplisse. |
| `ORBIT_INSTANCE` | (non défini) | Exécute une VM distincte avec son propre disque et ses propres sockets sous `~/.orbit/vmm-<name>`. |

### Partage de fichiers

Par défaut, la VM voit ceux des répertoires `~/work`, `~/src`, `~/code`, `~/dev`,
`~/projects` et `~/Developer` qui existent. Tout le reste de `/Users` est
invisible. Pour ajouter des répertoires :

```bash
ORBIT_SHARE_ALLOW="$HOME/src:$HOME/clients" make up
```

Les répertoires partagés sont **accessibles en écriture par tous les conteneurs**.
orbit-vmm refuse de partager votre répertoire personnel, tout ce qui se trouve
au-dessus, les répertoires cachés (`~/.ssh`, `~/.orbit`, …), `~/Library`, `~/bin` et `~/Applications`. Un répertoire situé à l'intérieur d'un autre
répertoire partagé est ignoré, puisque le répertoire englobant le couvre déjà.
Les conteneurs ne peuvent ni renommer ni supprimer les répertoires partagés
eux-mêmes.

- Les modifications de fichiers faites sur le Mac sont visibles dans les
  conteneurs en environ 0,5 s. Les nouveaux fichiers et les répertoires renommés
  apparaissent en environ 1 s.
- Les modifications et les nouveaux fichiers sur le Mac déclenchent
  `IN_MODIFY` / `IN_CLOSE_WRITE` dans les conteneurs environ 1 s plus tard.

### Réseau

- `-p 8080:80` et les `ports:` de compose sont redirigés vers `127.0.0.1:8080`
  sur le Mac en environ 1 s. Si le port du Mac est déjà pris, la redirection
  échoue et l'erreur est consignée dans `~/.orbit/vmm/console.log`.
- Les conteneurs atteignent les services sur le localhost du Mac via
  `host.docker.internal`. Cela inclut tout ce qui écoute uniquement sur
  `127.0.0.1` (bases de données, notebooks, serveurs de modèles locaux).
  Définissez `ORBIT_HOST_LOOPBACK=0` si vous ne le souhaitez pas.
- Tout processus de la VM qui écoute sur toutes les adresses obtient le même
  port sur le `127.0.0.1` du Mac, pas seulement les publications `-p` (comme
  avec `--network host`).

### Certificats racine (proxys d'inspection TLS)

- `make guest` lit les racines auxquelles votre Mac fait confiance en plus du
  jeu intégré d'Apple (celles marquées « Toujours approuver » pour SSL,
  généralement une CA d'inspection TLS d'entreprise) et les intègre à votre
  invité. Sur un Mac sans de telles racines, rien n'est ajouté et les conteneurs
  voient les bundles propres à leurs images. Le dépôt et les releases ne
  contiennent aucun certificat.
- Les conteneurs et les étapes de build voient ces racines ajoutées au bundle
  de CA de l'image, via un montage. Une étape de build reçoit une copie du
  répertoire du bundle, de sorte que `update-ca-certificates` et
  `apt-get upgrade` fonctionnent. Aucun des deux n'écrit les racines du Mac
  dans l'image.
- `ORBIT_HOST_CA=0 make guest && make up` construit un invité sans elles.
- Votre invité (`~/.orbit/guest`) et votre disque de données contiennent alors
  les CA de votre organisation : ne les partagez pas en dehors de celle-ci.

## Fichiers

| Chemin | Contenu |
|---|---|
| `~/.orbit/guest/` | Noyau et initramfs construits par `make guest`. |
| `~/.orbit/vmm/data.img` | Disque de données (images Docker, volumes). |
| `~/.orbit/vmm/docker.sock` | API Docker. |
| `~/.orbit/vmm/console.log` | Console de l'invité ; regardez ici en premier quand quelque chose échoue (`console.prev.log` correspond à l'exécution précédente). |
| `~/.orbit/vmm/watchdog.log`, `crashes/` | Journal du superviseur et enregistrements des arrêts inattendus. |

**Ne partagez pas `~/.orbit/guest/*` ni `data.img`.** Ils contiennent des
binaires GPL et les certificats racine approuvés de votre machine
([THIRD_PARTY_NOTICES.md](../../../THIRD_PARTY_NOTICES.md)).

Pour agrandir le disque de données, arrêtez la VM, agrandissez le fichier, puis
redémarrez la VM. L'invité redimensionne son système de fichiers au démarrage :

```bash
python3 -c "import os;p=os.path.expanduser('~/.orbit/vmm/data.img');os.truncate(p,max(os.path.getsize(p),128<<30))"
```

### Disque de données

`data.img` est un fichier sparse : il occupe sur le Mac autant d'espace que
Docker en a écrit, jusqu'à sa taille logique (`vmm-up.sh` affiche les deux). Il
grossit sans bruit, c'est pourquoi deux garde-fous surveillent l'espace libre du
Mac. Le superviseur (`vmm-run.sh`) vous avertit en dessous de
`ORBIT_WARN_FREE_GB`, et arrête proprement l'invité en dessous de
`ORBIT_STOP_FREE_GB` (il refuse aussi de démarrer à ce niveau). En dessous, en
dernier recours, le VMM vérifie toutes les 5 secondes :

- En dessous de `ORBIT_DISK_WARN_MIB`, il consigne `orbit-vmm: disk space low`
  dans `console.log` (puis à nouveau toutes les 10 minutes tant que l'espace
  reste bas), et `scripts/healthcheck.py` signale un avertissement.
- En dessous de `ORBIT_DISK_FLOOR_MIB`, ou lorsqu'une écriture échoue parce que
  le disque est plein, il met en attente les écritures disque de l'invité et
  consigne `orbit-vmm: DISK FULL` une fois. Docker se bloque (l'invité peut
  journaliser des avertissements de tâches bloquées), mais rien n'échoue.
  Libérez de l'espace sur le Mac et les écritures reprennent là où elles
  s'étaient arrêtées (`disk writes resumed`).
- Si aucun espace n'est libéré dans le délai `ORBIT_DISK_PAUSE_SECS`, les
  écritures échouent. ext4 signale alors des erreurs d'E/S et peut passer en
  lecture seule ; libérez donc de l'espace avant, et redémarrez la VM si c'est
  arrivé.

Mettre les écritures en attente plutôt que de les faire échouer est le
comportement par défaut de QEMU (`werror=enospc`) : une écriture échouée
représente des données qu'ext4 a déjà promis de stocker, et les perdre est
précisément la façon dont un disque de Mac plein corrompt l'invité.

## Mise à jour

```bash
make update            # the latest release; TO=v0.2.1 for a given one
make rollback          # back to what was installed before the last update
```

`make update` construit la release dans son propre clone (`~/.orbit/src`), pas
dans votre checkout, et construit le nouvel invité avant de remplacer quoi que
ce soit ; ainsi, un téléchargement ou un build qui échoue laisse la version
installée intacte. Il conserve les binaires et l'invité précédents pour
`make rollback`. Il ne redémarre la VM que si aucun conteneur n'est en cours
d'exécution ; sinon, la VM garde l'ancienne version jusqu'à `make up` (ou
lancez `NOW=1 make update`). Le premier démarrage après une mise à jour de
l'invité réinstalle les paquets de l'invité (environ 1 min).

Si le contrôle de santé est installé, il recherche la dernière release une fois
par jour et affiche une notification macOS par release plus récente. Rien n'est
installé sans `make update`.

Pour suivre `main` plutôt que les releases : `git pull && make guest install up`.

## Watchdog

`make install` installe aussi un agent launchd, `com.orbit.vmm`, qui exécute la
VM et la rétablit. Il existe parce que rien d'autre ne le fait : orbit n'a pas
d'application qui reste en cours d'exécution, donc sans lui, un VMM qui meurt ou
se bloque reste à l'arrêt.

```bash
ORBIT_WATCHDOG=0 make install          # install without it
./scripts/install-watchdog.sh --uninstall
./scripts/install-watchdog.sh --reload # apply a changed agent now (restarts the VM)
```

Réinstaller ne touche pas à une VM en cours d'exécution.

- **Crash :** si le VMM meurt, ou si l'invité redémarre ou panique, launchd le
  relance en quelques secondes.
- **Blocage :** l'invité reçoit un ping toutes les 30 s. Après trois pings
  manqués, le VMM est interrompu (abort), afin que macOS écrive un rapport de
  plantage, puis il est redémarré. Cela prend environ 90 s.
- **Boucle de crash :** après plus de trois démarrages en 10 minutes, il cesse
  de redémarrer et vous prévient.
- **Enregistrements de crash :** chaque arrêt inattendu est conservé dans
  `~/.orbit/vmm/crashes/<time>/` (journal de la console, rapport de plantage
  macOS, journal du watchdog). Les 10 plus récents sont conservés, et vous
  recevez une notification macOS. Les conteneurs s'arrêtent avec la VM, donc
  ceux qui n'ont pas de politique `restart:` doivent être relancés.
- **Volontaire :** `make down` (`vmm-up.sh --stop`) et un `poweroff` dans
  l'invité sont considérés comme des arrêts voulus et ne sont pas redémarrés.
  Une VM qui tournait lorsque vous vous êtes déconnecté redémarre à la
  connexion.

Sans le watchdog, `make up` exécute le même superviseur en arrière-plan : les
blocages sont toujours détectés et les crashs toujours enregistrés, mais rien
n'est redémarré.

## Contrôle de santé (facultatif)

Après `make install`, `scripts/install-healthcheck.sh` installe un agent
launchd qui exécute le `~/.orbit/bin/healthcheck.py` installé. Il vérifie
Docker, les ports, les bind mounts, inotify et l'horloge de l'invité, et
enregistre la mémoire, le nombre de threads et de fd du VMM dans
`~/.orbit/vmm/health.jsonl`.

La vérification s'exécute toutes les heures. Après trois exécutions saines
consécutives, elle s'espace à toutes les 2 h, puis toutes les 4 h. Tout problème
la ramène à une fréquence horaire. Pour la supprimer, lancez `--uninstall`.

## Limitations connues

- **inotify :** les changements côté Mac atteignent les conteneurs environ une
  seconde plus tard. Une nouvelle arborescence de répertoires de plus de 256
  entrées (un nouveau worktree, un `npm install` sur le Mac) déclenche un seul
  `IN_CREATE` pour son répertoire racine ; les watchers récursifs la parcourent
  à partir de là. Les renommages ne sont appariés que si l'invité avait déjà vu
  l'ancien nom ; sinon, ils apparaissent comme une suppression + une création.
- **Redirection de ports :** les publications UDP ne sont pas redirigées. Les
  publications sur une IP d'hôte spécifique (`-p 127.0.0.1:8080:80`) sont
  redirigées vers le `127.0.0.1` du Mac comme les autres ; le même port publié
  sur deux IP de ce type n'atteint que la plus récente. Si deux VM publient le
  même port, la redirection de la seconde échoue (avec une entrée de journal).
- **Bind mounts :** aucun verrou de fichier n'est partagé entre le Mac et les
  conteneurs. Les sockets Unix et les FIFO ne peuvent pas être créés sur des
  bind mounts.
- **Disque de données :** l'espace libéré par Docker ne retourne au Mac que
  lorsque l'invité effectue un trim (`fstrim`, toutes les 10 minutes quand la VM
  est au repos, et une fois après le démarrage) ; un `docker system prune` se
  répercute donc sur le Mac quelques minutes plus tard. L'image ne descend
  jamais en dessous de ce que l'invité a écrit sans le libérer.
- **Certificats racine :** une étape de build qui copie le bundle de CA ailleurs
  (`cp /etc/ssl/certs/ca-certificates.crt /app/`) copie avec lui les racines du
  Mac, dans l'image. Régénérer le bundle fonctionne dans les étapes de build
  mais pas dans les conteneurs en cours d'exécution, et les commandes suivantes
  du même `RUN` ne voient pas les racines du Mac. Si quelque chose est monté sur
  le répertoire des CA (`RUN --mount`, `-v`), les racines n'y sont pas ajoutées.
- **Éléments manquants :** pas d'installeur, d'interface graphique ni de
  Kubernetes.

## Fonctionnement

Voir [docs/architecture.md](architecture.md). En bref :

```
docker CLI ──unix socket── orbit-vmm ──vsock── guest: dockerd
                              │
          Hypervisor.framework (vCPUs, in-kernel GICv3)
          virtio-mmio: blk · net (libslirp) · vsock · fs · balloon (free page reporting)
```

## Contribuer

Les rapports de bugs et les pull requests sont les bienvenus. Voir
[CONTRIBUTING.md](../../../CONTRIBUTING.md) pour l'environnement de développement et les
vérifications. Signalez les problèmes de sécurité en privé
([SECURITY.md](../../../SECURITY.md)).

## Licence

Apache-2.0 ([LICENSE](../../../LICENSE)). L'image d'invité que vous construisez
télécharge des paquets Alpine Linux, qui ont leurs propres licences ; voir
[THIRD_PARTY_NOTICES.md](../../../THIRD_PARTY_NOTICES.md).
