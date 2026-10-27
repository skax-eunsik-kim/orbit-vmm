# आर्किटेक्चर

[English](../../architecture.md) | [한국어](../ko/architecture.md) | [日本語](../ja/architecture.md) | [简体中文](../zh-CN/architecture.md) | [Español](../es/architecture.md) | [Português (Brasil)](../pt-BR/architecture.md) | [Français](../fr/architecture.md) | [Deutsch](../de/architecture.md) | **हिन्दी**

> यह एक अनुवाद है। अंग्रेज़ी दस्तावेज़ ([architecture.md](../../architecture.md)) ही source of truth है; अगर दोनों में अंतर हो तो अंग्रेज़ी दस्तावेज़ को ही मानें।

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

VMM एक ही Rust process है। `src/vm/` में खुद machine है:

- Hypervisor.framework FFI (`hvf.rs`) और in-kernel GIC (`gic.rs`)।
- Boot और device tree (`boot.rs`, `fdt.rs`)।
- PL011 console (`pl011.rs`)।
- vCPU threads, PSCI और MMIO decoding (`vcpu.rs`)।

`src/virtio/` में virtio-mmio v2 transport और split virtqueues हैं, साथ ही हर
device के लिए एक file।

`signals.rs` खुद VMM को भेजे गए SIGTERM और SIGINT को संभालता है: यह भेजने वाले का
pid और नाम `console.log` में log करता है, guest से power off करने को कहता है (ज़्यादा से ज़्यादा
30 s), और उसी signal से killed process की तरह exit करता है, ताकि supervisor उसे फिर भी restart करे।
दूसरा signal मिलने पर यह तुरंत exit कर जाता है।

### मेमोरी लौटाना

Guest RAM, `hv_vm_allocate` से मिली anonymous memory है, जो VM में map की जाती है। जब guest
free pages report करता है (`VIRTIO_BALLOON_F_REPORTING`, queue 2), तो VMM:

1. उस range को VM से unmap करता है (`hv_vm_unmap`),
2. उसकी जगह fresh zero pages रखता है (`mmap(MAP_FIXED)`), ताकि macOS पुराने
   pages को process footprint से हटा दे,
3. range को वापस map करता है (`hv_vm_map`)।

सिर्फ़ `madvise` से उस memory का footprint कम नहीं होता जो किसी VM में map
की गई हो।

Guest kernel `page_reporting.page_reporting_order=2` के साथ boot होता है, इसलिए
reporting unit 16 KiB है, जो macOS का page size है। Fragmented free memory भी
वापस लौट जाती है।

Linux file data को page cache के रूप में पकड़े रखता है, जिसे balloon report नहीं कर सकता।
`guest/etc/orbit/memd.sh` इसे संभालता है:

- 60 s तक हल्के load के बाद, यह containers के page cache का आधा हिस्सा reclaim करता है
  (cgroup `memory.reclaim`)।
- 30 s तक पूरी तरह idle रहने के बाद, यह clean caches drop कर देता है।
- दोनों बार, यह memory को compact करता है ताकि free हुए pages reportable blocks बना सकें।
- हर समय, यह root cgroup के `memory.reclaim` के ज़रिए page cache को 1 GiB से नीचे रखता है
  (builds कभी idle नहीं होते; Mac वैसे भी `data.img` को cache करता है)।
- जब तक macOS memory pressure report करता है, यह इसके बजाय page cache को 256 MiB
  (warn) या 64 MiB (critical) से नीचे रखता है (`orbit.memd_warn_mb`,
  `orbit.memd_crit_mb`)।
- यह कभी working set से नीचे नहीं दबाता। जब cap के reclaim किए pages तुरंत वापस
  पढ़े जाते हैं (`workingset_refault_file`), तो cap cache के size पर रुक जाता है और
  बिना refaults वाले हर minute में एक चौथाई ढीला होता है। इससे पहले, macOS "warn"
  pressure के तहत एक Kubernetes control plane हर minute लगभग 2.7 GB खोकर दोबारा पढ़ता था;
  अब लगभग 0.1–0.2 GB।

`pressure.rs` macOS memory pressure पर नज़र रखता है (एक libdispatch memory-pressure
source, साथ ही हर 30 s पर दोबारा पढ़ा जाने वाला `kern.memorystatus_vm_pressure_level`) और
control vsock पर `pressure normal|warn|critical` भेजता है। बदलाव होने पर,
`control.sh` level को `/run/orbit-pressure` में दर्ज करता है और तुरंत
`memd.sh reclaim <level>` चलाता है: नए cap तक reclaim करता है (critical पर,
पहले सारे clean caches drop करता है) और compact करता है, ताकि free page reporting
pages को macOS के compress करने से पहले ही वापस दे दे। `ORBIT_PRESSURE=off` इसे
बंद करता है, और `ORBIT_PRESSURE=warn|critical` तुलना के लिए level को fix कर देता है। यह
उस cache में मदद करता है जिसे guest ने एक बार छुआ हो (किसी build के inputs, pull की गई image);
जिस working set को guest बार-बार पढ़ता रहता है, वह cap कुछ भी हो, resident ही रहता है।
Guest को हर बदलाव बताया जाता है; `console.log` में हर 10 minutes में ज़्यादा से ज़्यादा एक line
आती है, जिसमें बीच में हुए बदलावों की संख्या होती है।

Activity Monitor का Memory column process का `phys_footprint` है। जब
Mac में memory कम होती है, तो macOS guest RAM के कुछ हिस्से को compress या swap करता है, और
तब footprint उन guest pages से आगे बढ़ जाता है जो VMM के पास वास्तव में होते हैं (measurements में
compressed मात्रा के लगभग एक-तिहाई से आधे तक; जब कुछ भी compressed नहीं होता
तब यह लगभग शून्य होता है)। कौन-से guest pages held हैं, यह देखने के लिए VMM को
`ORBIT_MEMDUMP=<prefix>` के साथ start करें, `touch <prefix>.req` करें, और `<prefix>.bin`
(guest RAM के हर 16 KiB page के लिए एक `mincore` byte) को guest के
`/proc/kpageflags` से `vmm/bench/memxref.py` की मदद से मिलाएँ।

### डेटा डिस्क (virtio-blk)

`blk.rs` `data.img` को serve करता है, जो एक sparse file है, और उस पर `flock` लेता है ताकि दो
VMs एक ही disk share न कर सकें।

- **Discard.** Guest के discards image पर `F_PUNCHHOLE` बन जाते हैं, और `memd`
  boot के बाद और VM के शांत रहने पर हर 10 minutes में `fstrim /data` चलाता है, ताकि
  Docker जो space free करता है वह Mac को वापस मिल जाए।
- **Disk guard.** हर 2 s पर VMM, Mac पर free space जाँचता है।
  `ORBIT_DISK_WARN_MIB` से नीचे होने पर यह एक warning log करता है। `ORBIT_DISK_FLOOR_MIB` से नीचे होने पर, या जब
  कोई write ENOSPC के साथ fail हो, तो यह guest writes को fail करने के बजाय `ORBIT_DISK_PAUSE_SECS` तक
  रोक कर रखता है (QEMU के `werror=enospc` की तरह), और space free होते ही फिर से शुरू कर देता है,
  ताकि ext4 को कभी write error न दिखे। Supervisor इससे पहले ही, `ORBIT_STOP_FREE_GB` से नीचे,
  VM को cleanly बंद कर देता है।

### फ़ाइल शेयरिंग (virtio-fs)

`fs.rs` एक FUSE server है और `hostfs.rs` path access संभालता है।

- हर path को किसी directory fd से `openat(O_NOFOLLOW)` के साथ walk किया जाता है, ताकि
  कोई symlink allowed directories (`--share-allow`) से बाहर न जा सके। किसी allowed directory
  के ऊपर की directories traverse की जा सकती हैं, पर list नहीं की जा सकतीं।
- VMM allowlist को खुद जाँचता है: entries असली paths होनी चाहिए (कोई
  symlinks नहीं, exact case, nested नहीं), और home directory, उसके ऊपर की कोई भी चीज़,
  hidden directories, `~/Library`, `~/bin` और `~/Applications` अस्वीकार कर दिए जाते हैं। Guest खुद allowed directories को
  rename या remove नहीं कर सकता।
- पिछले walks के directory fds 1 s तक cache किए जाते हैं, ताकि कोई path `/Users` से नीचे तक
  हर directory को दोबारा open न करे। Guest के अपने renames और
  rmdirs entries को तुरंत हटा देते हैं, और expired entries अगले
  insert पर close कर दी जाती हैं। Missing paths को guest 1 s तक cache करता है
  (`ORBIT_FS_NEG_TTL_MS`)।
- चार request queues हैं।
- Entry TTL 5 s है और attribute TTL 500 ms है। `KEEP_CACHE` और
  `AUTO_INVAL_DATA` enabled हैं, और readdirplus भी।
- Writeback caching बंद है। इसे चालू रखने पर, Mac की तरफ़ किए गए edits
  guest में दिखाई नहीं दे रहे थे।
- Container का `chown`, `user.orbit.owner` xattr में store होता है और `stat` में वापस
  report किया जाता है। Group/other permission bits, owner bits को mirror करते हैं, ताकि
  non-root container users आपकी files में लिख सकें।
- Mac के edits guest तक inotify events के रूप में पहुँचते हैं। `fsevents.rs`
  allowed directories को FSEvents से watch करता है; `forward_events` हर batch को
  edits, creations, renames और deletions में बदलता है (Mac की मौजूदा स्थिति और
  guest ने जो look up किया है, उसके आधार पर) और उन्हें vsock पर `fsnotify.sh` को भेजता है।
  Guest हर बदलाव को एक reserved gid के तहत दोहराता है: edit (`IN_MODIFY`) के लिए एक same-size `truncate`,
  बाक़ी के लिए create/mkdir, rename या unlink/rmdir। Server उस gid को पहचानता है, इनमें से कुछ भी
  Mac पर कभी apply नहीं करता, और इसे सिर्फ़ उन्हीं paths के लिए सफल होने देता है जिन्हें उसने queue किया हो, जब तक Mac अब भी match करता हो।
- Docker socket की directory (`~/.orbit/vmm/`) guest को एक
  खाली, read-only mount point के रूप में दिखाई जाती है, और guest वहाँ `docker.sock` को link करता है, ताकि
  जो tools `DOCKER_HOST` path को bind-mount करते हैं (testcontainers का Ryuk) वे काम करें।

### नेटवर्किंग

`slirp.rs` libslirp को उसके अपने thread पर चलाता है।

- `ports.rs` published ports को Mac के `127.0.0.1` पर forward करता है। यह
  Docker के container events (VMM के अपने docker socket पर `/events`) को follow करता है और
  container start होते ही `-p` publish को forward कर देता है, जिसमें किसी specific host IP पर
  published ports भी शामिल हैं। हर 250 ms पर यह `--network host` services के लिए control channel पर
  guest के listening sockets भी पढ़ता है।
- Containers, `host.docker.internal` के ज़रिए Mac के localhost तक पहुँचते हैं
  (`ORBIT_HOST_LOOPBACK=0` इसे बंद करता है)।
- DNS: containers guest के dnsmasq से पूछते हैं, जो
  `host.docker.internal` का जवाब देता है और बाक़ी queries को libslirp के resolver को और
  वहाँ से Mac के resolver को forward करता है। यह 4096 queries को एक साथ in flight रहने देता है; dnsmasq के default
  150 की वजह से parallel builds "Temporary failure in name resolution" के साथ fail हो जाते थे।
- Control channel guest clock को भी sync में रखता है। यह
  round-trip time मापता है और clock को तभी step करता है जब वह 250 ms से ज़्यादा अलग हो।
  इसलिए clock पीछे भी step कर सकता है: guest code को यह जानने के लिए कि कुछ बदला है या नहीं,
  mtimes पर निर्भर नहीं रहना चाहिए।
- `csrc/asyncclose.c` एक छोटी dylib है जो internet sockets के लिए `close()` को interpose करती है।
  Content-filter network extensions, FIN के बाद `close()` को लगभग 1 s तक block
  करवा सकते हैं, जिससे network thread पर हर connection अटक जाता।

## Guest (`guest/`)

`scripts/fetch-guest.sh` एक pinned Alpine kernel और minirootfs
(sha256-verified) download करता है और उनसे तथा `guest/` से एक initramfs build करता है। पहला
boot जो packages install करता है (Docker, qemu, …) वे pinned नहीं हैं: वे उस समय की Alpine
repositories से आते हैं। Alpine पुराने package versions हटा देता है, इसलिए साप्ताहिक `guest-pins` workflow जाँचता है कि
दोनों pinned downloads अब भी मौजूद हैं।

- **पहला boot:** Docker और उससे जुड़े tools को data disk पर install करता है, फिर एक
  read-only root snapshot लेता है। Docker, Alpine v3.23 (29.5) से आता है: 29.8 हर warm build पर
  locally built base image के ऊपर की हर layer को फिर से build कर देता था।
- **बाद के boots:** उस snapshot को tmpfs overlay के नीचे इस्तेमाल करते हैं, इसलिए उनमें
  2–4 s लगते हैं।

Guest के हिस्से:

- `dockerd.sh` `daemon.json` लिखता है (`defaultMaxUsedSpace` के साथ build cache की सीमा
  20 GB) और dockerd को `orbit-runc` के साथ start करता है।
- `orbit-runc` एक OCI runtime wrapper है जो containers से उन roots पर भरोसा करवाता है
  जिन पर Mac, Apple के set के अलावा भरोसा करता है। `make guest` उन्हें
  (`scripts/export-host-ca.py`, आमतौर पर किसी company का TLS-inspection CA; ज़्यादातर Macs पर कोई नहीं,
  और `ORBIT_HOST_CA=0` के साथ कोई नहीं) `/etc/orbit/host-ca.pem` में export करता है।
  - किसी container को image के CA bundle की एक copy मिलती है जिसमें ये roots जोड़े गए होते हैं,
    और वह bundle के ऊपर read-only bind-mount की जाती है। जो bundle path symlink हो
    (RHEL, Fedora, Amazon Linux अपने bundle को `ca-trust/` में link करते हैं), उसे वैसे ही
    resolve किया जाता है जैसे container उसे देखेगा, सिर्फ़ उसके rootfs के अंदर, और copy उस
    file के ऊपर जाती है जिसकी ओर वह इशारा करता है।
  - `orbit k8s` से बने kind node में, node का containerd pods को उसी wrapper के ज़रिए चलाता है
    (guest से `host-ca.pem` के साथ mount किया गया, और runc `BinaryName` के रूप में set),
    इसलिए pods को भी वही treatment मिलता है।
  - किसी build के `RUN` step को इसके बजाय bundle की directory की एक read-write copy
    मिलती है, ताकि जो tools bundle को rename के ज़रिए regenerate करते हैं वे काम करें। runc एक
    child के रूप में चलता है; जब वह लौटता है (BuildKit `--keep` पास करता है, इसलिए rootfs अब भी
    mounted रहता है) तो directory को image के अपने bundle के साथ step के rootfs में वापस लिख दिया जाता है,
    सिर्फ़ तभी जब उसका content बदला हो। Host roots कभी किसी
    layer तक नहीं पहुँचते।
  - जिस directory में किसी दूसरे bundle की directory या कोई mount हो, उसे इसके बजाय
    file bind मिलता है, और किसी मौजूदा mount (`RUN --mount`, `-v`) पर या उसके नीचे
    मौजूद bundle को छोड़ दिया जाता है।
- `dns.sh` dnsmasq चलाता है (नेटवर्किंग देखें)।
- `binfmt.sh` qemu-x86_64 को register करता है।
- `memd.sh`: ऊपर देखें।

## Mac पर (`scripts/`)

- `make install` VMM, उसके `close()` interposer, `vmm-up.sh`,
  `vmm-run.sh` और `healthcheck.py` को `~/.orbit/bin` में copy करता है,
  `~/.orbit/bin/VERSION` लिखता है, और launchd watchdog install करता है। VM हमेशा
  सिर्फ़ उसी copy से चलता है: containers checkout में लिख सकते हैं।
- `vmm-run.sh` VM को supervise करता है, launchd के साथ या उसके बिना। यह `vmm-up.sh` द्वारा
  `config.env` में save की गई settings load करके export करता है, क्योंकि launchd इसे
  user के environment के बिना start करता है।
  - यह crash, guest reboot या panic, या hang (तीन
    missed control-socket pings; macOS crash report लिखे इसके लिए VMM को पहले abort किया जाता है)
    detect करता है। सिर्फ़ launchd (watchdog) के तहत ही VM फिर से start होता है;
    उसके बिना, supervisor stop को record करके exit हो जाता है।
  - 10 minutes में तीन से ज़्यादा starts होने के बाद यह restart करना बंद कर देता है।
  - यह हर unexpected stop का record `crashes/<time>/` में रखता है।
  - यह `ORBIT_WARN_FREE_GB` से नीचे warning देता है और `ORBIT_STOP_FREE_GB` से नीचे
    VM को cleanly बंद कर देता है।
- `healthcheck.py`, अगर install हो, तो Docker, ports, bind mounts, inotify,
  clock और disk space जाँचता है, VMM की memory, threads और fds record करता है, और दिन में
  एक बार `releases/latest` page से latest release पढ़ता है और हर नए release के लिए
  एक notification दिखाता है।
- `orbit` installed command है: `up` और `down` `vmm-up.sh` चलाते हैं, `update`
  और `rollback` `update.sh` चलाते हैं, `status` report करता है, और `uninstall` agents,
  health check की scratch directories और `~/.orbit` को हटाता है। Makefile के run targets
  इसे call करते हैं, इसलिए checkout में से कुछ नहीं चलता।
  `orbit k8s up|down|status` ऊपर बताए अनुसार configured node वाले kind clusters बनाता और
  delete करता है; `uninstall` भी उन्हें delete करता है, जिससे उनके kubectl contexts भी हट जाते हैं।
- `update.sh` (`orbit update`, `orbit rollback`) `~/.orbit/bin/VERSION` में record की गई
  repository के अपने खुद के clone (`~/.orbit/src`) में एक release tag
  और एक staging directory में नया guest build करता है, rollback के लिए पिछली binaries और guest
  रखता है, और VM को तभी restart करता है जब कोई container न चल रहा हो।
