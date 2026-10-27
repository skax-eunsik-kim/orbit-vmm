# orbit-vmm

[English](../../../README.md) | [한국어](../ko/README.md) | [日本語](../ja/README.md) | [简体中文](../zh-CN/README.md) | [Español](../es/README.md) | [Português (Brasil)](../pt-BR/README.md) | [Français](../fr/README.md) | [Deutsch](../de/README.md) | **हिन्दी**

> यह एक अनुवाद है। English [README](../../../README.md) ही source of truth है; अगर दोनों में अंतर हो, तो English version को मानें।

[![CI](https://github.com/skax-eunsik-kim/orbit-vmm/actions/workflows/ci.yml/badge.svg)](https://github.com/skax-eunsik-kim/orbit-vmm/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](../../../LICENSE)

Apple Silicon Mac के लिए memory की बचत करने वाला Docker engine।

orbit-vmm, Apple के Hypervisor.framework पर Rust में लिखा गया एक छोटा virtual
machine monitor है। यह एक Alpine Linux guest boot करता है जो Docker चलाता है, और
आपके Mac पर `docker` / `docker compose` CLI एक Unix socket के ज़रिए उससे बात करता है।

macOS पर ज़्यादातर Docker VM अपने peak memory use तक बढ़ जाते हैं और वहीं बने रहते हैं।
orbit-vmm memory वापस लौटाता है। Guest उन pages की report करता है जिन्हें वह free करता है
(virtio-balloon *free page reporting*), और VMM उन्हें कुछ ही seconds में macOS को
release कर देता है। एक छोटा guest daemon idle page cache को भी drop करता है और memory को
compact करता है (यानी उसे defragment करता है, ताकि पूरे free blocks report किए जा सकें),
ताकि free हुई memory सच में वापस लौटाई जा सके।

> **Status: शुरुआती दौर।** यह हर दिन असली compose stacks चलाता है, लेकिन यह अभी नया है,
> इसका एक ही maintainer है, और इसमें कोई installer या GUI नहीं है। इस पर निर्भर होने से पहले
> [ज्ञात सीमाएँ](#ज्ञात-सीमाएँ) पढ़ें।

## विशेषताएँ

- **Memory वापस macOS को जाती है।** 16 KiB granularity पर free page reporting,
  साथ में idle page cache trimming। किसी workload के खत्म होने के बाद, Mac पर VM का
  footprint एक minute के अंदर लगभग 1 GB पर वापस आ जाता है
  ([benchmarks](benchmarks.md))।
- **virtio-fs पर bind mounts।** `-v $PWD:/app` उसी path पर काम करता है जैसे Mac पर।
  इसमें multiple request queues, attribute caching और readdirplus हैं।
  Mac पर किए गए edits, नई files और directories, renames और deletions containers में
  inotify events trigger करते हैं, इसलिए `uvicorn --reload`, vite और nodemon का hot reload
  `git checkout` और code generators के दौरान भी काम करता रहता है। Container के अंदर `chown` और `chmod` भी काम करते हैं।
  दोनों xattrs से emulate किए जाते हैं। असली file आपकी ही रहती है और Mac पर कभी भी
  setuid या group- या world-writable नहीं बनती।
- **Opt-in sharing।** VM को सिर्फ़ अनुमति दी गई directories दिखती हैं, और
  path walks symlink-safe हैं। Docker socket वाला container भी
  `~/.ssh` नहीं पढ़ सकता।
- **Networking जो Docker Desktop की तरह व्यवहार करती है:**
  - Published ports अपने-आप `127.0.0.1` पर दिखते हैं।
  - `host.docker.internal` और `host-gateway` Mac पर resolve होते हैं।
  - Containers उन root CAs पर भरोसा करते हैं जिन पर आपका Mac भरोसा करता है, इसलिए `pip`, `npm`, `apk` और
    `curl` TLS-inspecting proxies के पीछे भी काम करते हैं।
- **amd64 images** qemu user emulation (binfmt) के ज़रिए चलती हैं।
- `ORBIT_INSTANCE=<name>` के साथ **कई VMs एक साथ**।
- **छोटा और auditable।** लगभग 6.3k lines of code की एक Rust binary। यह
  `libc`, `vm-fdt` और libslirp पर निर्भर है, और किसी और चीज़ पर नहीं।

## आवश्यकताएँ

- Apple Silicon Mac, macOS 15 या उसके बाद का (in-kernel GIC API के लिए 15+ चाहिए)।
  अब तक सिर्फ़ macOS 26.4 पर test किया गया है; किसी दूसरे version पर, पहले नीचे दिए
  checks चलाएँ।
- Xcode Command Line Tools (`xcode-select --install`) और
  [Homebrew](https://brew.sh)।
- Rust 1.87+ और libslirp: `brew install rust libslirp`।
- Compose और buildx plugins के साथ Docker CLI:
  `brew install docker docker-compose docker-buildx`। Homebrew plugins को ऐसी जगह रखता है
  जहाँ CLI default रूप से नहीं देखता, इसलिए उन्हें `~/.docker/config.json` में जोड़ें:

  ```json
  { "cliPluginsExtraDirs": ["/opt/homebrew/lib/docker/cli-plugins"] }
  ```

  buildx के बिना, `docker build` legacy builder पर लौट जाता है और `make devflow` के
  BuildKit checks fail हो जाते हैं।
- वैकल्पिक health check के लिए Python 3.11+ (`brew install python`)।
- Free disk space: Mac पर 3 GB से कम free होने पर VM start नहीं होगा और
  10 GB से कम होने पर warning देता है (`ORBIT_STOP_FREE_GB` / `ORBIT_WARN_FREE_GB`)। Docker का
  data एक sparse `~/.orbit/vmm/data.img` में रहता है, जो आपके pull और
  build करने के साथ बढ़ता है।

### ऐसे macOS version पर जिस पर orbit test नहीं हुआ है

अब तक macOS 26.4 पर test किया गया है। Hypervisor.framework की memory macOS को कैसे
वापस दी जा सकती है, और Activity Monitor उसे कैसे गिनता है, यह macOS पर निर्भर है, इसलिए
किसी नए version पर भरोसा करने से पहले ये तीन चीज़ें जाँच लें:

1. **Memory release।** जब कोई VM न चल रहा हो:

   ```bash
   vmm/target/release/orbit-vmm --reclaim-selftest
   ```

   `guest RAM from hv_vm_allocate` block में,
   `unmap + mmap(MAP_FIXED anon) + map` line का अंत `released 1 MiB` के आसपास होना चाहिए।
   VMM यही तरीका इस्तेमाल करता है (`vmm/src/reclaim.rs`)। अगर यह
   ~257 MiB पर रुका रहता है, तो इस version पर free हुई guest memory वापस नहीं लौटती: VM Mac पर
   सिर्फ़ बढ़ता ही है।
2. **Developer workflow।** VM चालू होने पर: `make devflow` को
   0 fail (28 checks) report करना चाहिए।
3. **Activity Monitor।** इसका Memory column VMM का `phys_footprint` है।
   जब Mac पर memory कम होती है, तो macOS guest RAM के एक हिस्से को compress करता है और
   वह संख्या guest के इस्तेमाल से ज़्यादा हो जाती है। Testing में, कुछ भी compressed न होने पर
   यह अंतर लगभग शून्य था, और pressure में compressed मात्रा का लगभग एक-तिहाई से आधा था।
   किसी दूसरे macOS version पर अलग अंतर macOS के गिनने के तरीके में बदलाव है, leak नहीं,
   बशर्ते `ORBIT_MEMDUMP` + `vmm/bench/memxref.py` यह दिखाएँ कि VMM लगभग उतनी ही memory
   रखे हुए है जितनी guest इस्तेमाल करता है ([docs/architecture.md](architecture.md) देखें)।

## Quick start

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

`DOCKER_HOST` वाली line को अपने shell profile में डालें, या एक context बनाएँ:

```bash
docker context create orbit --docker host=unix://$HOME/.orbit/vmm/docker.sock
docker context use orbit
```

`orbit status` installed version दिखाता है और बताता है कि VM चल रहा है या नहीं।
`orbit up` चल रहे VM को restart करता है, जिससे उसके सभी containers रुक जाते हैं।

ऊपर की तरह, orbit को **अपनी shared directories से बाहर** clone करें
([File sharing](#file-sharing) देखें)। Containers shared directories में लिख सकते हैं,
इसलिए उनमें रखा checkout आपकी जानकारी के बिना बदला जा सकता है, और अगला
`make install` या `make guest` उस code को Mac पर build करके चला देगा।
Checkout किसी shared directory में होने पर `make install` warning देता है। Install करने के बाद,
`orbit` सिर्फ़ `~/.orbit/bin` की installed copies चलाता है, checkout को कभी नहीं;
`make up`, `make down`, `make update` और `make rollback` बस उसे
call करते हैं।

## Configuration

`scripts/vmm-up.sh` (जिसे `orbit up` चलाता है) ये environment variables पढ़ता है:

| Variable | Default | अर्थ |
|---|---|---|
| `ORBIT_MEM_MIB` | `8192` | Guest RAM की ऊपरी सीमा। Free हुई memory macOS को लौट जाती है, इसलिए ऊँची सीमा की कीमत कम है। |
| `ORBIT_CPUS` | Mac का core count (`sysctl -n hw.ncpu`) | vCPUs। Idle vCPUs park हो जाते हैं और कोई CPU खर्च नहीं करते; अगर कुछ ही हों, तो parallel container start-up (जैसे agent sandboxes) CPU पर queue में लगता है। |
| `ORBIT_GUEST` | `~/.orbit/guest` | Guest `kernel` और `initramfs` वाली directory (जैसे `ORBIT_HOME=… scripts/fetch-guest.sh` से build की गई)। |
| `ORBIT_DISK_GB` | `64` | **नए** data disk (sparse file) का size। |
| `ORBIT_DISK_WARN_MIB` | MiB में `ORBIT_WARN_FREE_GB` | Mac पर free space इससे कम होने पर, VMM `console.log` में भी warning देता है। |
| `ORBIT_DISK_FLOOR_MIB` | `1024` | `ORBIT_STOP_FREE_GB` के नीचे आख़िरी उपाय: इससे कम होने पर, space free होने तक guest disk writes रोक दिए जाते हैं ([डेटा डिस्क](#डेटा-डिस्क) देखें)। |
| `ORBIT_DISK_PAUSE_SECS` | `600` | रोका गया write fail होने से पहले कितनी देर इंतज़ार करता है। |
| `ORBIT_SHARE_ALLOW` | नीचे देखें | Colon से अलग की गई directories जिन्हें containers bind-mount कर सकते हैं। |
| `ORBIT_HOST_LOOPBACK` | `1` | `0` containers को Mac के `127.0.0.1` (और `host.docker.internal`) पर services तक पहुँचने से रोकता है। |
| `ORBIT_WARN_FREE_GB` / `ORBIT_STOP_FREE_GB` | `10` / `3` | Mac disk का free space जिस पर VM आपको warning देता है, और जिस पर disk भरने से पहले वह साफ़ तरीके से shut down हो जाता है। |
| `ORBIT_INSTANCE` | (unset) | `~/.orbit/vmm-<name>` के तहत अपने disk और sockets के साथ एक अलग VM चलाएँ। |
| `ORBIT_PRESSURE` | (Mac को follow करता है) | `off` macOS memory pressure को guest तक भेजना बंद करता है; `warn` या `critical` level को fix कर देता है (measurements के लिए)। |
| `ORBIT_FS_OWNERSHIP` | (emulated) | `host` bind-mounted files को emulated के बजाय उनके असली Mac owner और mode के साथ दिखाता है। |
| `ORBIT_FS_TTL` / `ORBIT_FS_ATTR_TTL_MS` | `5` / `500` | Containers bind-mounted files के names (s) और attributes (ms) को कितनी देर cache करते हैं। कम value का मतलब है कि Mac के edits जल्दी दिखते हैं, थोड़ी speed की कीमत पर। |

`make guest` (`scripts/fetch-guest.sh`) `ALPINE_MIRROR` (default
`https://dl-cdn.alpinelinux.org/alpine`; sha256 pins फिर भी लागू होते हैं),
`ORBIT_HOST_CA=0` ([Root certificates](#root-certificates-tls-inspecting-proxies) देखें)
और `ORBIT_HOME` (`~/.orbit/guest` के बजाय `<dir>/guest` में build करता है) पढ़ता है।

### File sharing

Default रूप से VM को `~/work`, `~/src`, `~/code`, `~/dev`,
`~/projects` और `~/Developer` में से जो भी मौजूद हों, वे दिखती हैं। `/Users` में बाकी सब कुछ
अदृश्य रहता है। Directories जोड़ने के लिए:

```bash
ORBIT_SHARE_ALLOW="$HOME/src:$HOME/clients" orbit up
```

Shared directories **हर container के लिए writable** होती हैं। orbit-vmm आपकी
home directory, उसके ऊपर की कोई भी चीज़, hidden directories (`~/.ssh`,
`~/.orbit`, …), `~/Library`, `~/bin` और `~/Applications` को share करने से मना करता है। किसी दूसरी shared directory के अंदर की directory
हटा दी जाती है, क्योंकि बाहरी directory उसे पहले से cover करती है। Containers shared directories को
खुद rename या remove नहीं कर सकते।

- Mac पर files में किए गए edits containers में लगभग 0.5 s के अंदर दिखते हैं। नई files
  और rename की गई directories लगभग 1 s के अंदर दिखती हैं।
- Mac पर किए गए edits और नई files containers में लगभग 1 s बाद
  `IN_MODIFY` / `IN_CLOSE_WRITE` raise करते हैं।

### Networking

- `-p 8080:80` और compose `ports:` लगभग 1 s के अंदर Mac पर `127.0.0.1:8080` पर
  forward हो जाते हैं। अगर Mac का port पहले से इस्तेमाल में है, तो forward fail हो जाता है, और
  error `~/.orbit/vmm/console.log` में जाती है।
- Containers `host.docker.internal` के ज़रिए Mac के localhost पर services तक पहुँचते हैं।
  इसमें वह सब शामिल है जो सिर्फ़ `127.0.0.1` पर listen कर रहा है (databases, notebooks,
  local model servers)। अगर आप यह नहीं चाहते, तो `ORBIT_HOST_LOOPBACK=0` set करें।
- VM में सभी addresses पर listen करने वाली कोई भी process, Mac के `127.0.0.1` पर वही port पाती है,
  सिर्फ़ `-p` publishes ही नहीं (जैसे `--network host` के साथ)।

### Root certificates (TLS-inspecting proxies)

- `make guest` उन roots को पढ़ता है जिन पर आपका Mac Apple के built-in set के अलावा भरोसा करता है
  (जिन्हें SSL के लिए "Always Trust" mark किया गया है, आमतौर पर कोई corporate
  TLS-inspection CA) और उन्हें आपके guest में build करता है। ऐसे roots के बिना वाले Mac पर
  कुछ नहीं जोड़ा जाता, और containers अपनी images के अपने bundles देखते हैं।
  Repository और releases में कोई certificate नहीं होता।
- Containers, build steps और Kubernetes pods (`orbit k8s`) उन roots को image के
  CA bundle में जुड़ा हुआ देखते हैं, एक mount के ज़रिए। इसमें RHEL, Fedora और
  Amazon Linux images भी शामिल हैं, जिनके bundle paths symlinks होते हैं। किसी build step को bundle की directory की एक copy मिलती है,
  इसलिए `update-ca-certificates` और `apt-get upgrade` काम करते हैं। इनमें से कोई भी
  Mac के roots को image में नहीं लिखता।
- `ORBIT_HOST_CA=0 make guest && orbit up` उनके बिना एक guest build करता है।
- Explicit proxy का support नहीं है: guest Mac की HTTP(S) proxy settings या PAC file
  इस्तेमाल नहीं करता, और Docker pulls सीधे Mac के network से बाहर जाते हैं। ऐसे networks
  जो traffic सिर्फ़ explicit proxy के ज़रिए allow करते हैं, अभी काम नहीं करते; transparent
  TLS-inspecting proxy काम करता है।
- इसके बाद आपका guest (`~/.orbit/guest`) और data disk आपके organisation के
  CAs रखते हैं: इन्हें organisation के बाहर share न करें।

## Files

| Path | सामग्री |
|---|---|
| `~/.orbit/guest/` | `make guest` से build किए गए Kernel और initramfs। |
| `~/.orbit/vmm/data.img` | Data disk (Docker images, volumes)। |
| `~/.orbit/vmm/docker.sock` | Docker API। |
| `~/.orbit/vmm/console.log` | Guest console; कुछ fail होने पर सबसे पहले यहीं देखें (`console.prev.log` पिछला run है)। |
| `~/.orbit/vmm/watchdog.log`, `crashes/` | Supervisor log और अनपेक्षित रुकावटों के records। |

**`~/.orbit/guest/*` या `data.img` को share न करें।** इनमें GPL binaries
और आपकी machine के trusted root certificates होते हैं
([THIRD_PARTY_NOTICES.md](../../../THIRD_PARTY_NOTICES.md))।

Data disk बढ़ाने के लिए, VM को रोकें, file को बड़ा करें, और VM को फिर से start करें।
Guest boot होने पर अपने filesystem का size बदल लेता है:

```bash
python3 -c "import os;p=os.path.expanduser('~/.orbit/vmm/data.img');os.truncate(p,max(os.path.getsize(p),128<<30))"
```

### डेटा डिस्क

`data.img` sparse है: यह Mac पर उतना ही space लेता है जितना Docker ने
लिखा है, अपने logical size तक (`vmm-up.sh` दोनों print करता है)। यह चुपचाप बढ़ता है,
इसलिए दो guards Mac के free space पर नज़र रखते हैं। Supervisor (`vmm-run.sh`)
`ORBIT_WARN_FREE_GB` से नीचे आपको notify करता है, और `ORBIT_STOP_FREE_GB` से नीचे
guest को साफ़ तरीके से shut down कर देता है (वहाँ वह start भी नहीं होगा)। उसके नीचे, आख़िरी
उपाय के रूप में, VMM हर 2 seconds में जाँच करता है:

- `ORBIT_DISK_WARN_MIB` से नीचे, यह `console.log` में `orbit-vmm: disk space low`
  log करता है (कम बने रहने तक हर 10 minutes में दोबारा), और
  `scripts/healthcheck.py` एक warning report करता है।
- `ORBIT_DISK_FLOOR_MIB` से नीचे, या जब disk भरा होने के कारण कोई write fail हो जाए,
  तो यह guest disk writes को रोक देता है और एक बार `orbit-vmm: DISK FULL` log करता है।
  Docker रुक जाता है (guest hung-task warnings log कर सकता है), लेकिन कुछ भी fail नहीं होता।
  Mac पर space free करें और writes वहीं से फिर शुरू हो जाते हैं जहाँ रुके थे
  (`disk writes resumed`)।
- अगर `ORBIT_DISK_PAUSE_SECS` के अंदर कोई space free नहीं होता, तो writes fail हो जाते हैं। तब ext4
  I/O errors report करता है और read-only हो सकता है, इसलिए उससे पहले space free करें और
  अगर ऐसा हो गया हो तो VM restart करें।

Writes को fail करने के बजाय रोके रखना QEMU का default व्यवहार है
(`werror=enospc`): fail हुआ write वह data है जिसे store करने का वादा ext4 पहले ही कर चुका है,
और उसे खोना ही वह तरीका है जिससे भरा हुआ Mac disk guest को corrupt कर देता है।

## Upgrade करना

```bash
orbit update           # the latest release; --to v0.2.1 for a given one
orbit rollback         # back to what was installed before the last update
```

`orbit update` release को आपके checkout में नहीं, बल्कि install के समय record की गई
repository के अपने clone (`~/.orbit/src`) में build करता है,
और कुछ भी replace करने से पहले नया guest build करता है, इसलिए fail हुआ
download या build installed version को वैसा ही छोड़ देता है। यह `orbit rollback` के लिए
पिछली binaries और guest रखता है। यह VM को सिर्फ़ तभी restart करता है जब
कोई container न चल रहा हो; अन्यथा VM `orbit up` तक पुराना version रखता है
(या `orbit update --now` चलाएँ)। Guest upgrade के बाद पहला boot
guest packages को फिर से install करता है (लगभग 1 min)।

अगर health check installed है, तो यह दिन में एक बार latest release देखता है
और हर नए release के लिए एक macOS notification दिखाता है। `orbit update` के बिना
कुछ भी install नहीं होता।

Releases के बजाय `main` को follow करने के लिए: अपने checkout में
`git pull && make guest install && orbit up` चलाएँ।

v0.2.1 या उससे पुराना installed है? अपने checkout से एक बार `make update` चलाएँ; वह
`orbit` command install कर देता है।

## Uninstall

```bash
orbit uninstall
```

यह हर orbit VM को रोकता है, और launchd agents (watchdog और health
check), आपकी shared directories में health check की scratch directories, और
`~/.orbit` को हटा देता है, **जिसमें हर image, container और volume वाले data disks
भी शामिल हैं**। यह पहले पूछता है (`--yes` सवाल को skip करता है)। यह आपका checkout,
और अगर आपने `orbit` नाम का Docker context बनाया है तो उसे भी छोड़ देता है
(`docker context rm orbit`)।

## Watchdog

`make install` एक launchd agent, `com.orbit.vmm`, भी install करता है, जो
VM चलाता है और उसे recover करता है। यह इसलिए है क्योंकि और कुछ नहीं है: orbit में कोई ऐसा app नहीं है जो
चलता रहे, इसलिए इसके बिना जो VMM मर जाए या hang हो जाए, वह बंद ही रहता है।

```bash
ORBIT_WATCHDOG=0 make install          # install without it
./scripts/install-watchdog.sh --uninstall
./scripts/install-watchdog.sh --reload # apply a changed agent now (restarts the VM)
```

दोबारा install करने पर चल रहे VM को नहीं छेड़ा जाता।

- **Crash:** अगर VMM मर जाए, या guest reboot या panic हो जाए, तो launchd उसे
  कुछ seconds के अंदर फिर से start कर देता है।
- **Hang:** Guest को हर 30 s पर ping किया जाता है। लगातार तीन pings miss होने पर VMM
  abort कर दिया जाता है, ताकि macOS एक crash report लिखे, और फिर उसे restart किया जाता है। इसमें
  लगभग 90 s लगते हैं।
- **Crash loop:** 10 minutes में तीन से ज़्यादा starts के बाद यह
  restart करना बंद कर देता है और आपको बताता है।
- **Crash records:** हर अनपेक्षित रुकावट
  `~/.orbit/vmm/crashes/<time>/` में रखी जाती है (console log, macOS crash report, watchdog
  log)। सबसे नए 10 रखे जाते हैं, और आपको एक macOS notification मिलता है। Containers
  VM के साथ रुक जाते हैं, इसलिए जिनमें `restart:` policy नहीं है, उन्हें फिर से
  start करना होगा।
- **जानबूझकर:** `orbit down` (`vmm-up.sh --stop`) और guest का `poweroff`
  इच्छित रुकावटें माने जाते हैं और restart नहीं किए जाते। जो VM आपके log out करते समय
  चल रहा था, वह login पर फिर से start हो जाता है।

Watchdog के बिना, `orbit up` वही supervisor background में चलाता है,
इसलिए hangs अब भी detect होते हैं और crashes अब भी record होते हैं, लेकिन कुछ भी
restart नहीं होता।

## Kubernetes

```bash
brew install kind kubectl
orbit k8s up                  # a single-node cluster named orbit (about 45 s the first time)
kubectl --context kind-orbit get nodes
orbit k8s down                # delete it; its memory goes back to the Mac within ~90 s
```

`orbit k8s` [kind](https://kind.sigs.k8s.io/) चलाता है: cluster VM में एक
container है, इसलिए जब तक आप उसे start नहीं करते तब तक उसकी कोई कीमत नहीं, और guest में
कुछ नहीं जोड़ा जाता। Plain kind के ऊपर यह इतना जोड़ता है कि node का
containerd pods को orbit के CA wrapper के ज़रिए start करता है, इसलिए pods भी
containers की तरह Mac के अतिरिक्त roots पर भरोसा करते हैं (registry pulls, `curl`, Node.js)।
`orbit k8s up <name> [kind flags]` एक और cluster बनाता है या
`--image kindest/node:v1.33.0` जैसे flags pass करता है; `orbit k8s status` clusters की list दिखाता है।
kind आपकी kubeconfig में `kind-<name>` context जोड़ता है और उसे current बना देता है।

- एक idle cluster ने Mac पर उसके बिना वाले VM से 1.0–1.7 GB ज़्यादा लिया
  ([benchmarks](benchmarks.md#kubernetes))।
- Services तक `kubectl port-forward` से पहुँचें; API server Mac के
  `127.0.0.1` पर है। Locally build की गई images के लिए
  `kind load docker-image <image> --name orbit` इस्तेमाल करें।
- Cluster `orbit up` (VM restart) के बाद भी बना रहता है और `orbit uninstall` से
  delete होता है।

## Health check (वैकल्पिक)

`make install` के बाद, `scripts/install-healthcheck.sh` एक launchd
agent install करता है जो installed `~/.orbit/bin/healthcheck.py` चलाता है। यह Docker,
ports, bind mounts, inotify और guest clock की जाँच करता है, और VMM memory, thread
और fd counts को `~/.orbit/vmm/health.jsonl` में record करता है।

Check हर घंटे चलता है। लगातार तीन healthy runs के बाद यह घटकर
हर 2 h, फिर हर 4 h पर चलता है। कोई भी समस्या इसे फिर से hourly कर देती है। इसे हटाने के लिए,
`--uninstall` चलाएँ।

## ज्ञात सीमाएँ

- **inotify:** Mac-side बदलाव containers तक लगभग एक second बाद पहुँचते हैं। 256 से ज़्यादा
  entries वाला नया directory tree (एक fresh worktree, Mac पर एक
  `npm install`) अपनी top directory के लिए एक `IN_CREATE` raise करता है;
  recursive watchers वहाँ से उसे scan करते हैं। Renames की जोड़ी तभी बनती है जब guest
  पुराना नाम पहले ही देख चुका हो; अन्यथा वे delete + create के रूप में दिखते हैं।
- **Port forwarding:** UDP publishes forward नहीं होते। किसी specific
  host IP पर publishes (`-p 127.0.0.1:8080:80`) बाकी की तरह Mac के `127.0.0.1` पर
  forward होते हैं; ऐसे दो IPs पर publish किया गया एक ही port सिर्फ़ सबसे नए तक पहुँचता है।
  अगर दो VMs एक ही port publish करें, तो दूसरे का forward fail हो जाता है (logged)।
- **Bind mounts:** Mac और containers के बीच कोई file locks share नहीं होते।
  Bind mounts पर Unix sockets और FIFOs नहीं बनाए जा सकते।
- **Data disk:** Docker जो space free करता है, वह Mac को तभी वापस मिलता है जब guest
  trim करता है (`fstrim`, VM शांत होने पर हर 10 minutes में, और boot के बाद एक बार),
  इसलिए `docker system prune` का असर Mac पर कुछ minutes बाद दिखता है। Image
  कभी भी उससे छोटी नहीं होती जितना guest ने लिखा है लेकिन free नहीं किया।
- **Root certificates:** जो build step CA bundle को कहीं और copy करता है
  (`cp /etc/ssl/certs/ca-certificates.crt /app/`), वह उसके साथ Mac के roots को भी
  image में copy कर देता है। Bundle को regenerate करना build steps में काम करता है लेकिन
  चल रहे containers में नहीं, और उसी `RUN` के बाद वाले commands Mac के
  roots नहीं देखते। अगर CA directory पर कुछ mount किया गया हो (`RUN --mount`, `-v`),
  तो वहाँ roots नहीं जोड़े जाते।
- **amd64 images** Rosetta के बजाय qemu user emulation के तहत चलती हैं, इसलिए वे
  arm64 images से काफ़ी धीमी होती हैं।
- **Guest packages:** Kernel और Alpine root filesystem sha256 से pinned हैं,
  लेकिन Docker, qemu और बाकी guest packages हर guest build के बाद पहले boot पर
  Alpine की repositories से install होते हैं, इसलिए वे Alpine के updates के साथ बदलते हैं।
- **Docker Desktop या OrbStack से आ रहे हैं:** images, containers और volumes
  migrate नहीं होते। Images फिर से pull करें, या उन्हें `docker save` /
  `docker load` से move करें; volume contents को ऐसे container से copy करें जो दोनों को mount करे।
- **Kubernetes:** सिर्फ़ kind (`orbit k8s`) के ज़रिए, default रूप से single node।
  `LoadBalancer` services और आपकी अपनी kind config के `extraPortMappings`
  आपके लिए set up नहीं किए जाते।
- **कमी वाले हिस्से:** कोई GUI नहीं, और कोई signed binary release नहीं: आपको
  इसे खुद build करना होगा।

## यह कैसे काम करता है

[docs/architecture.md](architecture.md) देखें। संक्षेप में:

```
docker CLI ──unix socket── orbit-vmm ──vsock── guest: dockerd
                              │
          Hypervisor.framework (vCPUs, in-kernel GICv3)
          virtio-mmio: blk · net (libslirp) · vsock · fs · balloon (free page reporting)
```

## Bug report करना

एक [issue](https://github.com/skax-eunsik-kim/orbit-vmm/issues) खोलें, जिसमें बताएँ कि
आपने क्या चलाया, आपको क्या उम्मीद थी और क्या हुआ, और ये attach करें:

- `~/.orbit/vmm/console.log` और `~/.orbit/vmm/watchdog.log` का आख़िरी हिस्सा;
- `sw_vers -productVersion` और `orbit status`;
- Crash होने पर, सबसे नई `~/.orbit/vmm/crashes/<time>/` directory।

Logs में paths और image names हो सकते हैं; post करने से पहले जाँच लें। `~/.orbit/guest`,
`data.img` या `host-ca.pem` कभी attach न करें: इनमें आपके Mac के अतिरिक्त root
certificates होते हैं।

## योगदान

Bug reports और pull requests का स्वागत है। Development setup और checks के लिए
[CONTRIBUTING.md](../../../CONTRIBUTING.md) देखें।
Security issues की report निजी तौर पर करें ([SECURITY.md](../../../SECURITY.md))।

## License

Apache-2.0 ([LICENSE](../../../LICENSE))। आप जो guest image build करते हैं, वह Alpine
Linux packages download करती है, जिनके अपने licences हैं;
[THIRD_PARTY_NOTICES.md](../../../THIRD_PARTY_NOTICES.md) देखें।

orbit-vmm एक स्वतंत्र project है। यह Docker, Inc.,
OrbStack, या Apple से संबद्ध नहीं है।
