# 벤치마크

[English](../../benchmarks.md) | **한국어** | [日本語](../ja/benchmarks.md) | [简体中文](../zh-CN/benchmarks.md) | [Español](../es/benchmarks.md) | [Português (Brasil)](../pt-BR/benchmarks.md) | [Français](../fr/benchmarks.md) | [Deutsch](../de/benchmarks.md) | [हिन्दी](../hi/benchmarks.md)

> 이 문서는 번역본입니다. 원본은 영어 문서([benchmarks.md](../../benchmarks.md))이며, 내용이 다르면 영어 문서를 따르세요.

모든 수치는 2026년 10월에 M-series MacBook 한 대(macOS 26)에서 `bench/`의 스크립트로
측정했습니다. 정확한 결과가 아니라 대략적인 규모를 보여 줍니다.
직접 스크립트를 실행해 자신의 머신에서 비교해 보세요.

## macOS로 반환되는 메모리

`bench/memtest.sh`는 VM 프로세스의 `phys_footprint`를 네 시점에서 측정합니다.
유휴 상태, 약 3.5 GB(page cache와 익명 메모리)를 dirty하게 만드는 컨테이너가 실행되는
동안, 그리고 컨테이너가 종료되고 30 s 뒤와 90 s 뒤입니다. 표에는 유휴 상태와 90 s 뒤 값을
보여 줍니다.

| 엔진 | 유휴 | 종료 90 s 후 |
|---|---|---|
| orbit-vmm | 0.66 GB | 0.69 GB |
| OrbStack | 1.25 GB | 1.11 GB |
| Colima (QEMU/vz) | 1.59 GB | 3.61 GB |

`bench/memfrag.sh`는 단편화 부하를 줍니다. 각각 150 MiB를 점유하는 컨테이너 24개를
시작하고, 하나 걸러 하나씩 해제한 다음 나머지를 해제합니다.

| 단계 | Mac footprint | 게스트 사용량 |
|---|---|---|
| 시작 | 849 MiB | 304 MiB |
| 24 × 150 MiB 점유 | 4854 MiB | 4165 MiB |
| 하나 걸러 해제 (+60 s) | 2917 MiB | 2206 MiB |
| 모두 해제 (+60 s) | 1043 MiB | 334 MiB |

## 개발자 워크플로

`bench/devflow/devflow.sh`는 28개 항목을 검사합니다.

- healthcheck를 포함한 compose up, Mac에서 접근하는 published port
- 서비스 DNS와 `host.docker.internal`
- named volume, bind mount 가시성과 inotify, bind mount 위의 postgres `initdb`,
  `chown`
- TLS 프록시 뒤의 HTTPS와 amd64 이미지
- BuildKit 빌드(`# syntax`, ghcr, apt/uv cache mount)와 alembic
  마이그레이션
- 적대적인 빌드 단계(symlink이거나 바꿔치기한 CA 디렉터리, 가득 찬 디스크)에 대한 게스트
  CA wrapper, 그리고 bundle이 symlink인 RHEL(UBI) 빌드
- logs, exec, stats, 시계 오차

현재 결과: 28/28 통과.

## Kubernetes

`bench/k8s/k8s.sh`는 `orbit k8s`(kind v0.33, Kubernetes v1.37, 노드 1개)를 검사하고
VMM의 `phys_footprint`를 기록합니다. 격리된 인스턴스(8 GiB, 6 vCPU), macOS 26.4,
M-series에서 2026-10-27에 세 번 실행했습니다.

| | 실행 1 | 실행 2 | 실행 3 |
|---|---|---|---|
| `orbit k8s up` (노드 Ready) | 43 s | 45 s | 40 s |
| 클러스터 전 VM | 2019 MiB | 2797 MiB | 2300 MiB |
| 클러스터 유휴 (60 s) | 3267 MiB | 3757 MiB | 4036 MiB |
| nginx 파드 20개 | 4417 MiB | 5247 MiB | 4598 MiB |
| `orbit k8s down` 60 s 후 | 2685 MiB | 2302 MiB | 2047 MiB |
| 1 → 20 파드 Ready 스케일 | 14 s | 5 s | 5 s |

"전" 값에는 앞선 실행이 남긴 page cache가 포함되어 있어 들쭉날쭉합니다. 별도로 30 s마다
기록한 추적에서는 클러스터를 지우고 90 s 뒤 VM이 클러스터 전 크기보다 작아졌습니다
(전 1686 MiB, 후 1468 MiB).

macOS "warn" memory pressure(`ORBIT_PRESSURE=warn`으로 고정)에서 클러스터가 유휴 상태일
때, 각각 1분 동안:

| memd | 다시 읽은 page cache | 디스크 읽기 + 쓰기 | 유지된 page cache |
|---|---|---|---|
| #33 이전 | 2.7 GB | 3.4 GB | 460–510 MiB |
| 이후 | 0.09–0.2 GB | 0.14–0.27 GB | 약 600 MiB |

현재 결과: 13/13 통과.

## 안정성

`bench/stress/multi.sh N`은 한 VM에서 워크트리 스택 N개를 동시에 실행하고, 한
워크트리의 수정이 다른 워크트리에 절대 전달되지 않는지 확인합니다. `bench/stress/soak.sh <hours>`는
이를 컨테이너·파일 churn과 함께 반복 실행하면서 VMM의 스레드, fd, 메모리를 샘플링합니다.

4 h soak에서 295회 중 295회를 통과했고, 스레드 수와 fd 수는 일정하게 유지되었습니다.
