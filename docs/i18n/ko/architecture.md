# 아키텍처

[English](../../architecture.md) | **한국어** | [日本語](../ja/architecture.md) | [简体中文](../zh-CN/architecture.md) | [Español](../es/architecture.md) | [Português (Brasil)](../pt-BR/architecture.md) | [Français](../fr/architecture.md) | [Deutsch](../de/architecture.md) | [हिन्दी](../hi/architecture.md)

> 이 문서는 번역본입니다. 원본은 영어 문서([architecture.md](../../architecture.md))이며, 내용이 다르면 영어 문서를 따르세요.

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

VMM은 하나의 Rust 프로세스입니다. `src/vm/`에는 머신 자체가 있습니다.

- Hypervisor.framework FFI(`hvf.rs`)와 in-kernel GIC(`gic.rs`).
- 부팅과 device tree(`boot.rs`, `fdt.rs`).
- PL011 콘솔(`pl011.rs`).
- vCPU 스레드, PSCI, MMIO 디코딩(`vcpu.rs`).

`src/virtio/`에는 virtio-mmio v2 transport와 split virtqueue가 있고, 디바이스마다
파일이 하나씩 있습니다.

`signals.rs`는 VMM 자신에게 전달된 SIGTERM과 SIGINT를 처리합니다. 보낸 프로세스의 pid와
이름을 `console.log`에 기록하고, 게스트에 전원을 끄도록 요청한 뒤(최대 30 s), 해당
시그널로 종료된 것처럼 종료하므로 supervisor가 여전히 VMM을 재시작합니다.
두 번째 시그널을 받으면 즉시 종료합니다.

### 메모리 반환

게스트 RAM은 `hv_vm_allocate`로 할당한 익명 메모리이며 VM에 매핑됩니다. 게스트가
free page를 보고하면(`VIRTIO_BALLOON_F_REPORTING`, queue 2) VMM은 다음을 수행합니다.

1. 해당 범위를 VM에서 매핑 해제합니다(`hv_vm_unmap`).
2. 그 범위를 새 zero page로 교체합니다(`mmap(MAP_FIXED)`). 그러면 macOS가
   기존 페이지를 프로세스 footprint에서 제외합니다.
3. 범위를 다시 매핑합니다(`hv_vm_map`).

VM에 매핑된 메모리는 `madvise`만으로는 footprint가 줄어들지 않습니다.

게스트 커널은 `page_reporting.page_reporting_order=2`로 부팅하므로 보고 단위는
macOS 페이지 크기인 16 KiB입니다. 단편화된 free 메모리도 반환됩니다.

Linux는 파일 데이터를 page cache로 붙잡아 두는데, balloon은 이를 보고할 수 없습니다.
`guest/etc/orbit/memd.sh`가 이 문제를 처리합니다.

- 부하가 가벼운 상태가 60 s 이어지면 컨테이너 page cache의 절반을 회수합니다
  (cgroup `memory.reclaim`).
- 완전히 유휴 상태가 30 s 이어지면 clean cache를 비웁니다.
- 두 경우 모두 메모리를 compaction해서 해제된 페이지가 보고 가능한 블록을 이루게 합니다.
- 항상 root cgroup의 `memory.reclaim`으로 page cache를 1 GiB 미만으로 유지합니다
  (빌드 중에는 유휴 상태가 되지 않고, 어차피 Mac이 `data.img`를 캐시합니다).
- macOS가 memory pressure를 보고하는 동안에는 대신 page cache를 256 MiB(warn) 또는
  64 MiB(critical) 미만으로 유지합니다(`orbit.memd_warn_mb`,
  `orbit.memd_crit_mb`).

`pressure.rs`는 macOS memory pressure를 감시하고(libdispatch memory-pressure
source와 30 s마다 다시 읽는 `kern.memorystatus_vm_pressure_level`) control vsock으로
`pressure normal|warn|critical`을 보냅니다. 레벨이 바뀌면 `control.sh`가 그 레벨을
`/run/orbit-pressure`에 기록하고 즉시 `memd.sh reclaim <level>`을 실행합니다. 새 상한까지
회수하고(critical이면 먼저 clean cache를 모두 비움) compaction해서, macOS가 압축해야 하기
전에 free page reporting이 페이지를 돌려주도록 합니다. `ORBIT_PRESSURE=off`는 이 기능을
끄고, `ORBIT_PRESSURE=warn|critical`은 비교용으로 레벨을 고정합니다. 이 기능은 게스트가
한 번만 건드린 캐시(빌드 입력, pull한 이미지)에 효과가 있으며, 게스트가 계속 다시 읽는
working set은 상한과 관계없이 메모리에 남아 있습니다.
게스트에는 모든 변경을 알리며, `console.log`에는 10분에 최대 한 줄을 그 사이의 변경 횟수와
함께 기록합니다.

활성 상태 보기(Activity Monitor)의 메모리 열은 프로세스의 `phys_footprint`입니다. Mac의
메모리가 부족하면 macOS가 게스트 RAM 일부를 압축하거나 스왑하고, 그러면 footprint가 VMM이
실제로 보유한 게스트 페이지보다 커집니다(측정상 압축된 양의 약 3분의 1에서 2분의 1만큼이며,
압축된 것이 없으면 0에 가깝습니다). 어떤 게스트 페이지가 보유되어 있는지 보려면
`ORBIT_MEMDUMP=<prefix>`로 VMM을 시작하고 `touch <prefix>.req`를 실행한 다음,
`vmm/bench/memxref.py`로 `<prefix>.bin`(게스트 RAM 16 KiB 페이지마다 `mincore` 1바이트)을
게스트의 `/proc/kpageflags`와 대조합니다.

### 데이터 디스크 (virtio-blk)

`blk.rs`는 sparse file인 `data.img`를 제공하고, 두 VM이 한 디스크를 공유하지 못하도록
그 파일에 `flock`을 겁니다.

- **Discard.** 게스트의 discard는 이미지에 대한 `F_PUNCHHOLE`이 되고, `memd`가
  부팅 후 그리고 VM이 조용할 때 10분마다 `fstrim /data`를 실행하므로, Docker가 해제한
  공간이 Mac으로 돌아갑니다.
- **디스크 가드.** VMM은 5 s마다 Mac의 여유 공간을 확인합니다.
  `ORBIT_DISK_WARN_MIB` 미만이면 경고를 기록합니다. `ORBIT_DISK_FLOOR_MIB` 미만이거나
  쓰기가 ENOSPC로 실패하면, 게스트 쓰기를 실패시키는 대신 최대 `ORBIT_DISK_PAUSE_SECS`
  동안 보류하고(QEMU의 `werror=enospc`) 공간이 확보되면 재개하므로, ext4는 쓰기 오류를
  전혀 보지 않습니다. supervisor는 그보다 먼저 `ORBIT_STOP_FREE_GB` 미만에서 VM을
  정상적으로 중지합니다.

### 파일 공유 (virtio-fs)

`fs.rs`는 FUSE 서버이고 `hostfs.rs`는 경로 접근을 처리합니다.

- 모든 경로는 디렉터리 fd에서 `openat(O_NOFOLLOW)`로 따라가므로 symlink로 허용된
  디렉터리(`--share-allow`)를 벗어날 수 없습니다. 허용된 디렉터리의 상위 디렉터리는
  통과할 수는 있지만 목록을 볼 수는 없습니다.
- VMM은 허용 목록을 직접 검사합니다. 항목은 실제 경로여야 하며(symlink 불가, 대소문자
  정확히 일치, 중첩 불가), 홈 디렉터리와 그 상위 경로, 숨김 디렉터리, `~/Library`는
  거부됩니다. 게스트는 허용된 디렉터리 자체의 이름을 바꾸거나 삭제할 수 없습니다.
- 이전 탐색에서 연 디렉터리 fd는 1 s 동안 캐시되므로, 경로마다 `/Users`부터 모든
  디렉터리를 다시 열지 않습니다. 게스트 자신의 rename과 rmdir는 항목을 즉시 제거하고,
  만료된 항목은 다음 삽입 때 닫힙니다. 존재하지 않는 경로는 게스트가 1 s 동안
  캐시합니다(`ORBIT_FS_NEG_TTL_MS`).
- 요청 queue는 4개입니다.
- entry TTL은 5 s, attribute TTL은 500 ms입니다. `KEEP_CACHE`와
  `AUTO_INVAL_DATA`가 켜져 있고 readdirplus도 켜져 있습니다.
- writeback caching은 꺼져 있습니다. 켜면 Mac 쪽 수정이 게스트에서 보이지 않았습니다.
- 컨테이너의 `chown`은 `user.orbit.owner` xattr에 저장되고 `stat`에서 다시
  보고됩니다. group/other 권한 비트는 owner 비트를 그대로 따르므로 root가 아닌
  컨테이너 사용자도 사용자의 파일에 쓸 수 있습니다.
- Mac에서의 수정은 inotify 이벤트로 게스트에 전달됩니다. `fsevents.rs`가 허용된
  디렉터리를 FSEvents로 감시하고, `forward_events`가 각 배치를 (Mac의 현재 상태와 게스트가
  조회한 내용을 바탕으로) 수정, 생성, 이름 변경, 삭제로 바꿔 vsock으로 `fsnotify.sh`에
  보냅니다. 게스트는 각 변경을 예약된 gid로 다시 수행합니다. 수정에는 크기가 같은
  `truncate`(`IN_MODIFY`)를, 나머지에는 create/mkdir, rename 또는 unlink/rmdir를
  사용합니다. 서버는 그 gid를 알아보고 어떤 작업도 Mac에 적용하지 않으며, 자신이
  queue에 넣었고 Mac이 아직 일치하는 경로에 대해서만 성공시킵니다.
- docker socket의 디렉터리(`~/.orbit/vmm/`)는 게스트에 비어 있는 읽기 전용 mount
  point로 보이고 게스트가 그곳에 `docker.sock`을 링크하므로, `DOCKER_HOST` 경로를
  bind-mount하는 도구(testcontainers의 Ryuk)도 동작합니다.

### 네트워킹

`slirp.rs`는 libslirp를 별도 스레드에서 실행합니다.

- `ports.rs`는 published port를 Mac의 `127.0.0.1`로 포워딩합니다. Docker의 컨테이너
  이벤트(VMM 자체 docker socket의 `/events`)를 따라가며, 특정 호스트 IP에 publish한
  포트를 포함해 `-p` publish를 컨테이너가 시작되자마자 포워딩합니다. 또한
  `--network host` 서비스를 위해 250 ms마다 control 채널로 게스트의 listening
  socket을 읽습니다.
- 컨테이너는 `host.docker.internal`을 통해 Mac의 localhost에 접근합니다
  (`ORBIT_HOST_LOOPBACK=0`으로 끌 수 있습니다).
- DNS: 컨테이너는 게스트의 dnsmasq에 질의하며, dnsmasq는
  `host.docker.internal`에 응답하고 나머지는 libslirp의 resolver로, 그리고 거기서
  Mac의 resolver로 전달합니다. 동시에 처리 중인 질의를 4096개까지 허용합니다. dnsmasq
  기본값인 150에서는 병렬 빌드가 "Temporary failure in name resolution"으로 실패했습니다.
- control 채널은 게스트 시계도 동기화합니다. 왕복 시간을 측정하고, 시계가
  250 ms보다 많이 어긋났을 때만 시계를 조정합니다. 따라서 시계가 뒤로 갈 수
  있으므로, 게스트 코드는 무언가가 바뀌었는지 판단할 때 mtime에 의존하면 안 됩니다.
- `csrc/asyncclose.c`는 인터넷 소켓의 `close()`를 interpose하는 작은 dylib입니다.
  content-filter network extension이 있으면 FIN 이후 `close()`가 약 1 s 동안 block될
  수 있고, 그러면 네트워크 스레드의 모든 연결이 멈추게 됩니다.

## 게스트 (`guest/`)

`scripts/fetch-guest.sh`는 버전이 고정된 Alpine 커널과 minirootfs를 내려받고
(sha256 검증), 이것들과 `guest/`로 initramfs를 빌드합니다. Alpine은
오래된 패키지 버전을 삭제하므로, 매주 실행되는 `guest-pins` workflow가 고정된 두
다운로드가 아직 존재하는지 확인합니다.

- **첫 부팅:** Docker와 관련 도구를 데이터 디스크에 설치한 뒤 읽기 전용 root
  snapshot을 만듭니다. Docker는 Alpine v3.23(29.5)에서 가져옵니다. 29.8은 warm
  build마다 로컬에서 빌드한 base image 위의 모든 레이어를 다시 빌드했습니다.
- **이후 부팅:** 그 snapshot을 tmpfs overlay 아래에서 사용하므로 2–4 s가
  걸립니다.

게스트 구성 요소:

- `dockerd.sh`는 `daemon.json`(`defaultMaxUsedSpace`로 build cache 상한 20 GB)을
  작성하고 `orbit-runc`로 dockerd를 시작합니다.
- `orbit-runc`는 Apple 기본 세트 외에 Mac이 신뢰하는 root 인증서를 컨테이너도
  신뢰하게 하는 OCI runtime wrapper입니다. `make guest`가 그 인증서를
  (`scripts/export-host-ca.py`, 보통 회사의 TLS 검사 CA이며, 대부분의 Mac에는 없고
  `ORBIT_HOST_CA=0`이면 내보내지 않음) `/etc/orbit/host-ca.pem`으로 내보냅니다.
  - 컨테이너는 이미지의 CA bundle에 그 인증서를 덧붙인 사본을 받으며, 이 사본은 기존
    bundle 위에 읽기 전용으로 bind-mount됩니다.
  - 빌드의 `RUN` 단계는 대신 bundle 디렉터리의 읽기/쓰기 사본을 받으므로, rename으로
    bundle을 다시 생성하는 도구도 동작합니다. runc는 자식 프로세스로 실행되며, runc가
    반환되면(BuildKit이 `--keep`을 넘기므로 rootfs는 아직 마운트되어 있음) 내용이
    바뀐 경우에만 그 디렉터리를 이미지 자체의 bundle과 함께 해당 단계의 rootfs에 다시
    기록합니다. 호스트의 root 인증서는 레이어에 절대 들어가지 않습니다.
  - 다른 bundle의 디렉터리나 마운트를 포함하는 디렉터리는 대신 파일 bind를 받고,
    기존 마운트(`RUN --mount`, `-v`) 위치나 그 아래에 있는 bundle은 그대로 둡니다.
- `dns.sh`는 dnsmasq를 실행합니다(네트워킹 참고).
- `binfmt.sh`는 qemu-x86_64를 등록합니다.
- `memd.sh`: 위를 참고하세요.

## Mac 쪽 (`scripts/`)

- `make install`은 VMM, VMM의 `close()` interposer, `vmm-up.sh`,
  `vmm-run.sh`, `healthcheck.py`를 `~/.orbit/bin`에 복사하고,
  `~/.orbit/bin/VERSION`을 작성하고, launchd watchdog을 설치합니다. VM은 항상 그
  사본에서만 실행됩니다. 컨테이너가 checkout에 쓸 수 있기 때문입니다.
- `vmm-run.sh`는 launchd 유무와 관계없이 VM을 감독합니다.
  - crash, 게스트 재부팅이나 panic, hang(control socket ping 3회 무응답. 이때는
    macOS가 crash report를 작성하도록 VMM을 먼저 abort함) 후에 VM을 재시작합니다.
  - 10분 안에 세 번을 넘게 시작되면 재시작을 멈춥니다.
  - 예기치 않은 중지마다 `crashes/<time>/`에 기록을 남깁니다.
  - `ORBIT_WARN_FREE_GB` 미만이면 경고하고, `ORBIT_STOP_FREE_GB` 미만이면 VM을
    정상적으로 중지합니다.
- `healthcheck.py`는 설치되어 있으면 Docker, 포트, bind mount, inotify, 시계, 디스크
  공간을 검사하고, VMM의 메모리, 스레드, fd를 기록하며, 하루에 한 번
  `releases/latest` 페이지에서 최신 릴리스를 읽어 새 릴리스마다 알림을 한 번씩
  표시합니다.
- `update.sh`(`make update`, `make rollback`)는 자체 clone(`~/.orbit/src`)에서 릴리스
  태그를, staging 디렉터리에서 새 게스트를 빌드하고, rollback을 위해 이전 바이너리와
  게스트를 보관하며, 실행 중인 컨테이너가 없을 때만 VM을 재시작합니다.
