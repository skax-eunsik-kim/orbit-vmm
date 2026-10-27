# 基准测试

[English](../../benchmarks.md) | [한국어](../ko/benchmarks.md) | [日本語](../ja/benchmarks.md) | **简体中文** | [Español](../es/benchmarks.md) | [Português (Brasil)](../pt-BR/benchmarks.md) | [Français](../fr/benchmarks.md) | [Deutsch](../de/benchmarks.md) | [हिन्दी](../hi/benchmarks.md)

> 本文为译文，以英文文档 [benchmarks.md](../../benchmarks.md) 为准；如两者内容不一致，请以英文版为准。

所有数据均于 2026 年 10 月在一台 M 系列 MacBook（macOS 26）上使用 `bench/`
中的脚本测得。它们反映的是数量级，而非精确结果。
你可以自己运行这些脚本，在自己的机器上进行对比。

## 归还给 macOS 的内存

`bench/memtest.sh` 在四个时间点测量虚拟机进程的 `phys_footprint`：
空闲时、运行一个弄脏约 3.5 GB 内存（页缓存加匿名内存）的容器期间，以及容器
退出后 30 s 和 90 s。下表列出空闲时和 90 s 后的数据。

| 引擎 | 空闲 | 退出后 90 s |
|---|---|---|
| orbit-vmm | 0.66 GB | 0.69 GB |
| OrbStack | 1.25 GB | 1.11 GB |
| Colima (QEMU/vz) | 1.59 GB | 3.61 GB |

`bench/memfrag.sh` 对碎片化进行压力测试。它启动 24 个各占用 150 MiB 的容器，
先每隔一个释放一个，然后释放其余的：

| 步骤 | Mac footprint | Guest 已用 |
|---|---|---|
| 开始 | 849 MiB | 304 MiB |
| 持有 24 × 150 MiB | 4854 MiB | 4165 MiB |
| 隔一个释放一个（+60 s） | 2917 MiB | 2206 MiB |
| 全部释放（+60 s） | 1043 MiB | 334 MiB |

## 开发者工作流

`bench/devflow/devflow.sh` 运行 28 项检查：

- 带健康检查的 compose up，以及从 Mac 访问已发布的端口
- 服务 DNS 和 `host.docker.internal`
- 命名卷、bind mount 的可见性和 inotify、在 bind mount 上执行 postgres
  `initdb`、`chown`
- TLS 代理后的 HTTPS 和 amd64 镜像
- 一次 BuildKit 构建（`# syntax`、ghcr、apt/uv 缓存挂载）和一次 alembic
  迁移
- guest 的 CA 包装器面对恶意构建步骤（符号链接或被替换的 CA 目录、磁盘写满）
  的表现，以及一次证书包为符号链接的 RHEL（UBI）构建
- 日志、exec、stats 和时钟偏差

当前结果：28/28 通过。

## Kubernetes

`bench/k8s/k8s.sh` 检查 `orbit k8s`（kind v0.33、Kubernetes v1.37、单节点），
并记录 VMM 的 `phys_footprint`。在 macOS 26.4、M 系列上的一个隔离实例
（8 GiB、6 个 vCPU）上运行三次，2026-10-27：

| | 第 1 次 | 第 2 次 | 第 3 次 |
|---|---|---|---|
| `orbit k8s up`（节点 Ready） | 43 s | 45 s | 40 s |
| 创建集群前的虚拟机 | 2019 MiB | 2797 MiB | 2300 MiB |
| 集群空闲（60 s） | 3267 MiB | 3757 MiB | 4036 MiB |
| 20 个 nginx pod | 4417 MiB | 5247 MiB | 4598 MiB |
| `orbit k8s down` 后 60 s | 2685 MiB | 2302 MiB | 2047 MiB |
| 从 1 扩到 20 个 pod Ready | 14 s | 5 s | 5 s |

“创建集群前”包含之前运行留下的页缓存，因此会有波动。另一次每 30 s 记录一次的
跟踪显示，集群删除 90 s 后虚拟机已回落到创建集群前的大小以下（之前 1686 MiB，
之后 1468 MiB）。

在 macOS "warn" 内存压力下（用 `ORBIT_PRESSURE=warn` 固定），集群空闲，
每项一分钟：

| memd | 重新读回的页缓存 | 磁盘读 + 写 | 保留的页缓存 |
|---|---|---|---|
| #33 之前 | 2.7 GB | 3.4 GB | 460–510 MiB |
| 之后 | 0.09–0.2 GB | 0.14–0.27 GB | 约 600 MiB |

当前结果：13/13 通过。

## 稳定性

`bench/stress/multi.sh N` 在一个虚拟机中同时运行 N 个 worktree 栈，并检查某个
worktree 中的修改绝不会到达另一个。`bench/stress/soak.sh <hours>`
在循环中重复上述过程，同时伴随容器和文件的频繁创建与删除，并对 VMM 的线程、
fd 和内存进行采样。

一次 4 h 的 soak 测试通过了 295 轮中的全部 295 轮，线程数和 fd 数保持平稳。
