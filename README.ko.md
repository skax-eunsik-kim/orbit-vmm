# orbit-vmm

[English](README.md) | **한국어**

[![CI](https://github.com/skax-eunsik-kim/orbit-vmm/actions/workflows/ci.yml/badge.svg)](https://github.com/skax-eunsik-kim/orbit-vmm/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

Apple Silicon Mac 용으로 메모리를 아껴 쓰는 Docker 엔진입니다.

orbit-vmm 은 Apple Hypervisor.framework 위에 Rust 로 만든 작은 가상 머신 모니터입니다.
Docker 가 도는 Alpine Linux 게스트를 띄우고, Mac 의 `docker` / `docker compose` CLI 가
Unix 소켓으로 그 게스트와 통신합니다.

macOS 의 Docker VM 은 대개 가장 많이 쓴 만큼 메모리가 늘어난 채 줄지 않습니다.
orbit-vmm 은 메모리를 돌려줍니다. 게스트가 해제한 페이지를 알리면(virtio-balloon
*free page reporting*) VMM 이 몇 초 안에 macOS 에 돌려줍니다. 게스트의 작은 데몬이
쓰지 않는 페이지 캐시를 비우고 메모리를 압축 정리해, 해제한 메모리를 실제로 돌려줄 수
있게 합니다.

> **상태: 초기.** 매일 실제 compose 스택을 돌리고 있지만, 아직 젊은 프로젝트이고
> 관리자가 한 명이며 설치 프로그램이나 GUI 가 없습니다. 의존하기 전에
> [알려진 한계](#알려진-한계)를 읽어 주세요.

## 특징

- **메모리를 macOS 에 돌려줍니다.** 16 KiB 단위 free page reporting 과 유휴 페이지
  캐시 정리를 합니다. 작업이 끝나면 Mac 에서 VM 이 차지하는 메모리가 1분 안에 약
  1 GB 로 돌아옵니다([벤치마크](docs/benchmarks.md)).
- **virtio-fs 로 bind mount.** `-v $PWD:/app` 이 Mac 과 같은 경로로 동작합니다. 요청
  큐 여러 개, 속성 캐시, readdirplus 를 씁니다. Mac 에서 파일을 고치거나, 새 파일·
  디렉터리를 만들거나, 이름을 바꾸거나 지우면 컨테이너에 inotify 이벤트가 갑니다.
  그래서 `git checkout` 이나 코드 생성기를 거쳐도 `uvicorn --reload`, vite, nodemon 의
  핫 리로드가 계속 됩니다. 컨테이너 안의 `chown`·`chmod` 도 됩니다. 둘 다 xattr 로
  흉내 내므로 실제 파일은 여러분 소유 그대로이고, Mac 에서 setuid 나 그룹·전체 쓰기
  가능 파일이 되지 않습니다.
- **허용한 곳만 공유.** 허용한 디렉터리만 VM 에 보이고, 경로 탐색은 심볼릭 링크에
  안전합니다. Docker 소켓을 가진 컨테이너도 `~/.ssh` 를 읽을 수 없습니다.
- **Docker Desktop 처럼 동작하는 네트워크:**
  - 공개한 포트가 자동으로 `127.0.0.1` 에 나타납니다.
  - `host.docker.internal` 과 `host-gateway` 가 Mac 을 가리킵니다.
  - 컨테이너가 Mac 이 신뢰하는 루트 CA 를 신뢰하므로, TLS 검사 프록시 뒤에서도
    `pip`, `npm`, `apk`, `curl` 이 됩니다.
- **amd64 이미지**는 qemu 사용자 모드 에뮬레이션(binfmt)으로 돕니다.
- **VM 여러 개를 나란히** `ORBIT_INSTANCE=<이름>` 으로 띄울 수 있습니다.
- **작고 검토하기 쉽습니다.** Rust 바이너리 하나, 약 5,500줄입니다. 의존성은 `libc`,
  `vm-fdt`, libslirp 뿐입니다.

## 요구 사항

- Apple Silicon Mac, macOS 15 이상(커널 내장 GIC API 가 15 이상을 요구합니다).
- Rust 1.87 이상과 libslirp: `brew install rust libslirp`.
- compose 플러그인이 있는 Docker CLI: `brew install docker docker-compose`.

### 아직 시험하지 않은 macOS 버전에서

지금까지 macOS 26.4 에서 시험했습니다. Hypervisor.framework 메모리를 macOS 에 돌려주는
방식과 활성 상태 보기가 그것을 세는 방식은 macOS 가 정하므로, 새 버전에서는 의존하기
전에 다음 세 가지를 확인하세요.

1. **메모리 반환.** VM 이 꺼진 상태에서:

   ```bash
   vmm/target/release/orbit-vmm --reclaim-selftest
   ```

   `guest RAM from hv_vm_allocate` 블록에서
   `unmap + mmap(MAP_FIXED anon) + map` 줄이 `released 1 MiB` 근처로 끝나야 합니다.
   VMM 이 쓰는 방식입니다(`vmm/src/reclaim.rs`). 약 257 MiB 에 머물면 그 버전에서는
   해제한 게스트 메모리가 돌아오지 않아, Mac 에서 VM 이 늘기만 합니다.
2. **개발 흐름.** VM 이 떠 있는 상태에서 `make devflow` 가 실패 0(검사 25개)이어야
   합니다.
3. **활성 상태 보기.** 메모리 열은 VMM 의 `phys_footprint` 입니다. Mac 메모리가
   모자라면 macOS 가 게스트 RAM 일부를 압축하고, 그 숫자가 게스트가 실제로 쓰는
   양보다 커집니다. 시험에서는 압축이 없을 때 차이가 거의 0이었고, 압박이 있을 때는
   압축량의 3분의 1에서 절반 정도였습니다. 다른 macOS 버전에서 차이가 달라도,
   `ORBIT_MEMDUMP` + `vmm/bench/memxref.py` 로 VMM 이 게스트가 쓰는 만큼만 들고 있는
   것이 확인되면 누수가 아니라 macOS 가 세는 방식이 바뀐 것입니다
   ([docs/architecture.md](docs/architecture.md) 참고).

## 빠른 시작

```bash
git clone https://github.com/skax-eunsik-kim/orbit-vmm ~/src/orbit-vmm
cd ~/src/orbit-vmm
make guest      # 고정 버전 Alpine 커널 + rootfs 를 받아(sha256 확인) ~/.orbit/guest 에 빌드
make install    # 빌드·서명하고 VMM 과 스크립트를 ~/.orbit/bin 에 복사, 워치독 설치
make up         # ~/.orbit/bin 에서 VM 시작. 첫 부팅은 Docker 설치로 약 1분, 이후 2~4초
export DOCKER_HOST=unix://$HOME/.orbit/vmm/docker.sock
docker run --rm hello-world
```

`DOCKER_HOST` 줄을 셸 프로필에 넣거나 context 를 만드세요.

```bash
docker context create orbit --docker host=unix://$HOME/.orbit/vmm/docker.sock
docker context use orbit
```

`make up`(또는 `~/.orbit/bin/vmm-up.sh`)은 돌고 있는 VM 을 재기동하므로 그 안의
컨테이너가 모두 멈춥니다. VM 과 건강 점검은 `~/.orbit/bin` 에 설치된 사본에서만
돕니다. 컨테이너는 공유 디렉터리에 쓸 수 있고 이 체크아웃도 그중 하나인 경우가 많아서,
`make up` 은 체크아웃에서 아무것도 빌드하거나 실행하지 않습니다. 그렇게 하는 것은
`make install` 과 `make guest` 뿐입니다. 그 둘을 돌리기 전에 체크아웃이 뜻밖에
바뀌지 않았는지(`git status`) 확인하세요. 체크아웃을 공유 디렉터리 밖에 두면 더
좋습니다.

## 설정

`scripts/vmm-up.sh`(`make up` 이 실행하는 것)는 다음 환경 변수를 읽습니다.

| 변수 | 기본값 | 뜻 |
|---|---|---|
| `ORBIT_MEM_MIB` | `8192` | 게스트 RAM 상한. 해제한 메모리는 macOS 로 돌아가므로 높게 잡아도 손해가 적습니다. |
| `ORBIT_CPUS` | Mac 코어 수(`sysctl -n hw.ncpu`) | vCPU 수. 쉬는 vCPU 는 CPU 를 쓰지 않습니다. 적으면 컨테이너 여러 개를 동시에 띄울 때(에이전트 샌드박스 등) CPU 에서 줄을 섭니다. |
| `ORBIT_GUEST` | `~/.orbit/guest` | 게스트 `kernel` 과 `initramfs` 가 있는 디렉터리(예: `ORBIT_HOME=… scripts/fetch-guest.sh` 로 만든 것). |
| `ORBIT_DISK_GB` | `64` | **새** 데이터 디스크 크기(희소 파일). |
| `ORBIT_DISK_WARN_MIB` | `ORBIT_WARN_FREE_GB` 를 MiB 로 | Mac 여유 공간이 이보다 적으면 VMM 도 `console.log` 에 경고합니다. |
| `ORBIT_DISK_FLOOR_MIB` | `1024` | `ORBIT_STOP_FREE_GB` 아래의 마지막 안전장치. 이보다 적으면 공간이 생길 때까지 게스트 디스크 쓰기를 붙잡아 둡니다([데이터 디스크](#데이터-디스크) 참고). |
| `ORBIT_DISK_PAUSE_SECS` | `600` | 붙잡힌 쓰기가 실패하기 전까지 기다리는 시간. |
| `ORBIT_SHARE_ALLOW` | 아래 참고 | 컨테이너가 bind mount 할 수 있는 디렉터리(콜론으로 구분). |
| `ORBIT_HOST_LOOPBACK` | `1` | `0` 이면 컨테이너가 Mac 의 `127.0.0.1`(과 `host.docker.internal`)의 서비스에 닿지 못합니다. |
| `ORBIT_WARN_FREE_GB` / `ORBIT_STOP_FREE_GB` | `10` / `3` | Mac 디스크 여유 공간이 이만큼일 때 VM 이 경고하고, 이만큼일 때 디스크가 차기 전에 깨끗이 종료합니다. |
| `ORBIT_INSTANCE` | (없음) | `~/.orbit/vmm-<이름>` 아래에 자체 디스크와 소켓을 가진 VM 을 따로 띄웁니다. |

### 파일 공유

기본으로 VM 은 `~/work`, `~/src`, `~/code`, `~/dev`, `~/projects`, `~/Developer` 중
있는 것을 봅니다. `/Users` 의 나머지는 보이지 않습니다. 디렉터리를 더하려면:

```bash
ORBIT_SHARE_ALLOW="$HOME/src:$HOME/clients" make up
```

공유 디렉터리는 **모든 컨테이너가 쓸 수 있습니다.** orbit-vmm 은 홈 디렉터리와 그
위, 숨은 디렉터리(`~/.ssh`, `~/.orbit`, …), `~/Library`, `~/bin`, `~/Applications`
는 공유하지 않습니다. 다른 공유 디렉터리 안에 있는 디렉터리는 바깥 것이 이미 덮으므로
빠집니다. 컨테이너는 공유 디렉터리 자체의 이름을 바꾸거나 지울 수 없습니다.

- Mac 에서 고친 파일은 약 0.5초 안에 컨테이너에 보입니다. 새 파일과 이름이 바뀐
  디렉터리는 약 1초 안에 나타납니다.
- Mac 에서 고치거나 새로 만든 파일은 약 1초 뒤 컨테이너에서
  `IN_MODIFY` / `IN_CLOSE_WRITE` 를 일으킵니다.

### 네트워크

- `-p 8080:80` 과 compose `ports:` 는 약 1초 안에 Mac 의 `127.0.0.1:8080` 으로
  전달됩니다. Mac 포트가 이미 쓰이고 있으면 전달이 실패하고, 오류는
  `~/.orbit/vmm/console.log` 에 남습니다.
- 컨테이너는 `host.docker.internal` 로 Mac localhost 의 서비스에 닿습니다.
  `127.0.0.1` 에서만 듣는 것(데이터베이스, 노트북, 로컬 모델 서버)도 포함됩니다.
  원하지 않으면 `ORBIT_HOST_LOOPBACK=0` 을 주세요.
- VM 안에서 모든 주소로 듣는 프로세스는 `-p` 공개가 아니어도 Mac 의 `127.0.0.1` 에
  같은 포트를 얻습니다(`--network host` 와 같습니다).

### 루트 인증서(TLS 검사 프록시)

- `make guest` 는 Mac 이 Apple 기본 목록 외에 신뢰하는 루트(SSL 에 "항상 신뢰"로 표시한
  것, 보통 회사의 TLS 검사 CA)를 읽어 게스트에 넣습니다. 그런 루트가 없는 Mac 에서는
  아무것도 넣지 않고, 컨테이너는 이미지 자체의 번들을 봅니다. 저장소와 릴리스에는
  인증서가 들어 있지 않습니다.
- 컨테이너와 빌드 단계는 마운트를 통해 이미지의 CA 번들 뒤에 그 루트가 붙은 것을
  봅니다. 빌드 단계는 번들 디렉터리의 사본을 받으므로 `update-ca-certificates` 와
  `apt-get upgrade` 가 됩니다. 어느 쪽도 Mac 의 루트를 이미지에 쓰지 않습니다.
- `ORBIT_HOST_CA=0 make guest && make up` 은 루트를 넣지 않은 게스트를 만듭니다.
- 그렇게 만든 게스트(`~/.orbit/guest`)와 데이터 디스크에는 여러분 조직의 CA 가
  들어 있으니, 조직 밖에 공유하지 마세요.

## 파일

| 경로 | 내용 |
|---|---|
| `~/.orbit/guest/` | `make guest` 가 만든 커널과 initramfs. |
| `~/.orbit/vmm/data.img` | 데이터 디스크(Docker 이미지, 볼륨). |
| `~/.orbit/vmm/docker.sock` | Docker API. |
| `~/.orbit/vmm/console.log` | 게스트 콘솔. 뭔가 실패하면 먼저 여기를 보세요(`console.prev.log` 는 직전 실행). |
| `~/.orbit/vmm/watchdog.log`, `crashes/` | 감시 프로세스 로그와 예기치 않은 종료 기록. |

**`~/.orbit/guest/*` 와 `data.img` 는 공유하지 마세요.** GPL 바이너리와 이 Mac 이
신뢰하는 루트 인증서가 들어 있습니다([THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)).

데이터 디스크를 키우려면 VM 을 멈추고 파일을 늘린 뒤 다시 시작하세요. 게스트가 부팅할
때 파일 시스템 크기를 맞춥니다.

```bash
python3 -c "import os;p=os.path.expanduser('~/.orbit/vmm/data.img');os.truncate(p,max(os.path.getsize(p),128<<30))"
```

### 데이터 디스크

`data.img` 는 희소 파일입니다. Docker 가 쓴 만큼만, 논리 크기까지 Mac 공간을 차지합니다
(`vmm-up.sh` 가 둘 다 보여 줍니다). 조용히 커지므로 두 가지 장치가 Mac 의 여유 공간을
지켜봅니다. 감시 프로세스(`vmm-run.sh`)는 `ORBIT_WARN_FREE_GB` 아래에서 알리고,
`ORBIT_STOP_FREE_GB` 아래에서 게스트를 깨끗이 종료합니다(그 아래에서는 시작도 하지
않습니다). 그보다 더 아래를 위한 마지막 장치로 VMM 이 5초마다 확인합니다.

- `ORBIT_DISK_WARN_MIB` 아래이면 `console.log` 에 `orbit-vmm: disk space low` 를
  남기고(낮은 동안 10분마다 다시), `scripts/healthcheck.py` 가 경고합니다.
- `ORBIT_DISK_FLOOR_MIB` 아래이거나 디스크가 가득 차서 쓰기가 실패하면, 게스트 디스크
  쓰기를 붙잡아 두고 `orbit-vmm: DISK FULL` 을 한 번 남깁니다. Docker 가 멈추지만
  (게스트가 hung-task 경고를 남길 수 있습니다) 실패하는 것은 없습니다. Mac 공간을
  비우면 쓰기가 멈춘 자리에서 이어집니다(`disk writes resumed`).
- `ORBIT_DISK_PAUSE_SECS` 안에 공간이 생기지 않으면 쓰기가 실패합니다. 그러면 ext4
  가 I/O 오류를 내고 읽기 전용이 될 수 있으니, 그 전에 공간을 비우고 그렇게 됐다면 VM
  을 재기동하세요.

쓰기를 실패시키는 대신 붙잡아 두는 것은 QEMU 의 기본 동작(`werror=enospc`)과 같습니다.
실패한 쓰기는 ext4 가 이미 저장하겠다고 약속한 데이터이고, 그것을 잃는 것이 Mac
디스크가 가득 찼을 때 게스트가 망가지는 경로입니다.

## 업그레이드

```bash
make update            # 최신 릴리스. 특정 버전은 TO=v0.2.1
make rollback          # 마지막 업데이트 전에 설치돼 있던 버전으로
```

`make update` 는 여러분의 체크아웃이 아니라 자체 클론(`~/.orbit/src`)에서 릴리스를
빌드하고, 무엇이든 바꾸기 전에 새 게스트부터 만듭니다. 그래서 다운로드나 빌드가
실패하면 설치된 버전이 그대로 남습니다. 직전 바이너리와 게스트를 `make rollback` 용으로
남겨 둡니다. 컨테이너가 하나도 돌지 않을 때만 VM 을 재기동하고, 아니면 `make up` 할
때까지 VM 이 옛 버전으로 돕니다(`NOW=1 make update` 로 바로 재기동). 게스트를
업그레이드한 뒤 첫 부팅은 게스트 패키지를 다시 설치합니다(약 1분).

건강 점검을 설치했다면 하루 한 번 최신 릴리스를 확인하고, 새 릴리스마다 macOS 알림을
한 번 보냅니다. `make update` 없이는 아무것도 설치하지 않습니다.

릴리스 대신 `main` 을 따라가려면: `git pull && make guest install up`.

## 워치독

`make install` 은 VM 을 돌리고 복구하는 launchd 에이전트 `com.orbit.vmm` 도 설치합니다.
이 일을 할 다른 주체가 없어서입니다. orbit 에는 상주하는 앱이 없으므로, 워치독이 없으면
죽거나 멈춘 VMM 이 그대로 내려가 있습니다.

```bash
ORBIT_WATCHDOG=0 make install          # 워치독 없이 설치
./scripts/install-watchdog.sh --uninstall
./scripts/install-watchdog.sh --reload # 바뀐 에이전트를 지금 적용(VM 재기동)
```

다시 설치해도 돌고 있는 VM 은 건드리지 않습니다.

- **크래시:** VMM 이 죽거나 게스트가 재부팅·패닉하면 launchd 가 몇 초 안에 다시
  띄웁니다.
- **멈춤:** 30초마다 게스트에 ping 을 보냅니다. 세 번 응답이 없으면 VMM 을 중단시켜
  macOS 가 크래시 보고서를 남기게 한 뒤 다시 띄웁니다. 약 90초 걸립니다.
- **크래시 반복:** 10분 안에 세 번 넘게 시작하면 재시작을 멈추고 알려 줍니다.
- **크래시 기록:** 예기치 않은 종료마다 `~/.orbit/vmm/crashes/<시각>/` 에 콘솔 로그,
  macOS 크래시 보고서, 워치독 로그를 남깁니다. 최근 10개를 남기고 macOS 알림을 보냅니다.
  컨테이너는 VM 과 함께 멈추므로 `restart:` 정책이 없는 것은 다시 시작해야 합니다.
- **의도한 종료:** `make down`(`vmm-up.sh --stop`)과 게스트 `poweroff` 는 의도한 종료로
  보고 다시 띄우지 않습니다. 로그아웃할 때 돌고 있던 VM 은 로그인할 때 다시 시작합니다.

워치독이 없어도 `make up` 이 같은 감시 프로세스를 백그라운드로 돌리므로, 멈춤을 감지하고
크래시를 기록하지만 다시 띄우지는 않습니다.

## 건강 점검(선택)

`make install` 뒤에 `scripts/install-healthcheck.sh` 로 설치된
`~/.orbit/bin/healthcheck.py` 를 도는 launchd 에이전트를 설치합니다. Docker, 포트,
bind mount, inotify, 게스트 시계를 확인하고, VMM 의 메모리·스레드·fd 수를
`~/.orbit/vmm/health.jsonl` 에 기록합니다.

매시간 돌고, 세 번 연속 정상이면 2시간, 그다음 4시간 간격으로 늘립니다. 문제가 하나라도
있으면 다시 매시간으로 돌아갑니다. 지우려면 `--uninstall` 을 실행하세요.

## 알려진 한계

- **inotify:** Mac 쪽 변경은 약 1초 늦게 컨테이너에 닿습니다. 항목이 256개를 넘는 새
  디렉터리 트리(새 워크트리, Mac 에서 한 `npm install`)는 맨 위 디렉터리에 대한
  `IN_CREATE` 하나만 일으키고, 재귀 감시자가 거기서부터 훑습니다. 이름 바꾸기는 게스트가
  옛 이름을 이미 본 경우에만 짝이 맞고, 아니면 삭제 + 생성으로 보입니다.
- **포트 전달:** UDP 공개는 전달하지 않습니다. 특정 호스트 IP 공개
  (`-p 127.0.0.1:8080:80`)도 다른 것처럼 Mac 의 `127.0.0.1` 로 전달하며, 같은 포트를
  그런 IP 두 개에 공개하면 가장 최근 것만 닿습니다. VM 두 개가 같은 포트를 공개하면 두
  번째 것의 전달이 실패합니다(로그에 남음).
- **bind mount:** Mac 과 컨테이너 사이에 파일 잠금이 공유되지 않습니다. bind mount
  위에는 Unix 소켓과 FIFO 를 만들 수 없습니다.
- **데이터 디스크:** Docker 가 비운 공간은 게스트가 trim 할 때만 Mac 으로 돌아갑니다
  (`fstrim`, VM 이 조용할 때 10분마다, 그리고 부팅 뒤 한 번). 그래서
  `docker system prune` 결과는 몇 분 뒤 Mac 에 나타납니다. 이미지 파일은 게스트가 쓰고
  비우지 않은 크기 아래로는 줄지 않습니다.
- **루트 인증서:** 빌드 단계에서 CA 번들을 다른 곳으로 복사하면
  (`cp /etc/ssl/certs/ca-certificates.crt /app/`) Mac 의 루트도 함께 이미지에
  복사됩니다. 번들 재생성은 빌드 단계에서는 되지만 실행 중인 컨테이너에서는 안 되고,
  같은 `RUN` 의 뒤쪽 명령은 Mac 의 루트를 보지 못합니다. CA 디렉터리에 무언가가
  마운트돼 있으면(`RUN --mount`, `-v`) 그곳에는 루트를 넣지 않습니다.
- **빠진 것:** 설치 프로그램, GUI, Kubernetes 가 없습니다.

## 동작 방식

[docs/architecture.md](docs/architecture.md)를 보세요. 요약하면:

```
docker CLI ──unix socket── orbit-vmm ──vsock── guest: dockerd
                              │
          Hypervisor.framework (vCPUs, in-kernel GICv3)
          virtio-mmio: blk · net (libslirp) · vsock · fs · balloon (free page reporting)
```

## 기여

버그 보고와 풀 리퀘스트를 환영합니다. 개발 환경과 검사는
[CONTRIBUTING.md](CONTRIBUTING.md)를 보세요. 보안 문제는 비공개로 알려 주세요
([SECURITY.md](SECURITY.md)).

## 라이선스

Apache-2.0([LICENSE](LICENSE)). 빌드하는 게스트 이미지는 Alpine Linux 패키지를 받으며,
그 패키지에는 각자의 라이선스가 있습니다([THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)).

orbit-vmm 은 독립 프로젝트이며 Docker, Inc., OrbStack, Apple 과 관계가 없습니다.
