# orbit-vmm

[English](../../../README.md) | [한국어](../ko/README.md) | [日本語](../ja/README.md) | **简体中文** | [Español](../es/README.md) | [Português (Brasil)](../pt-BR/README.md) | [Français](../fr/README.md) | [Deutsch](../de/README.md) | [हिन्दी](../hi/README.md)

> 本文档为译文。英文 [README](../../../README.md) 为准；如两者有出入，请以英文版为准。

[![CI](https://github.com/skax-eunsik-kim/orbit-vmm/actions/workflows/ci.yml/badge.svg)](https://github.com/skax-eunsik-kim/orbit-vmm/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](../../../LICENSE)

一个面向 Apple Silicon Mac、节省内存的 Docker 引擎。

orbit-vmm 是一个基于 Apple Hypervisor.framework、用 Rust 编写的小型虚拟机监视器（VMM）。
它会启动一个运行 Docker 的 Alpine Linux 客户机，Mac 上的
`docker` / `docker compose` CLI 通过 Unix socket 与它通信。

macOS 上的大多数 Docker 虚拟机会把内存占用涨到峰值后一直停留在那里。
orbit-vmm 则会把内存还回去。客户机会报告它释放的内存页
（virtio-balloon *free page reporting*），VMM 在几秒内把这些页释放给 macOS。
一个小型客户机守护进程还会丢弃空闲的页缓存并整理（compact）内存，
使释放出来的内存能够真正归还。

> **状态：早期阶段。** 它每天都在运行真实的 compose 栈，但项目还很年轻，
> 只有一位维护者，也没有安装程序或 GUI。依赖它之前，请先阅读
> [已知限制](#已知限制)。

## 功能

- **内存归还给 macOS。** 以 16 KiB 粒度进行 free page reporting，
  并裁剪空闲页缓存。工作负载退出后，虚拟机在 Mac 上的内存占用会在
  一分钟内回落到约 1 GB
  （[基准测试](benchmarks.md)）。
- **基于 virtio-fs 的 bind mount。** `-v $PWD:/app` 使用与 Mac 上相同的路径。
  它具有多个请求队列、属性缓存和 readdirplus。
  在 Mac 上编辑文件、新建文件和目录、重命名和删除都会在容器中触发 inotify
  事件，因此 `uvicorn --reload`、vite 和 nodemon 的热重载
  在 `git checkout` 和代码生成器运行期间也能持续工作。容器内的 `chown` 和 `chmod` 同样可用。
  两者都通过 xattr 模拟。真实文件仍归你所有，在 Mac 上永远不会变成
  setuid，也不会变成组可写或所有人可写。
- **按需共享（opt-in）。** 只有被允许的目录对虚拟机可见，
  路径遍历对符号链接是安全的。持有 Docker socket 的容器也无法读取
  `~/.ssh`。
- **行为与 Docker Desktop 一致的网络：**
  - 发布的端口会自动出现在 `127.0.0.1` 上。
  - `host.docker.internal` 和 `host-gateway` 解析到 Mac。
  - 容器信任 Mac 所信任的根 CA，因此 `pip`、`npm`、`apk` 和
    `curl` 在进行 TLS 检查的代理后面也能正常工作。
- **amd64 镜像** 通过 qemu 用户态模拟（binfmt）运行。
- **多个虚拟机并行运行**，使用 `ORBIT_INSTANCE=<name>`。
- **小巧且易于审计。** 单个约 5.5k 行的 Rust 二进制文件。它只依赖
  `libc`、`vm-fdt` 和 libslirp，没有其他依赖。

## 系统要求

- Apple Silicon Mac，macOS 15 或更高版本（内核内 GIC API 需要 15+）。
- Rust 1.87+ 和 libslirp：`brew install rust libslirp`。
- 带 compose 插件的 Docker CLI：`brew install docker docker-compose`。

### 在 orbit 尚未测试过的 macOS 版本上

目前仅在 macOS 26.4 上测试过。Hypervisor.framework 的内存如何归还给
macOS，以及活动监视器（Activity Monitor）如何统计它，都由 macOS 决定，
因此在新版本上依赖它之前，请先检查以下三点：

1. **内存释放。** 在没有虚拟机运行时：

   ```bash
   vmm/target/release/orbit-vmm --reclaim-selftest
   ```

   在 `guest RAM from hv_vm_allocate` 块中，
   `unmap + mmap(MAP_FIXED anon) + map` 这一行的结尾必须接近 `released 1 MiB`。
   这正是 VMM 使用的方法（`vmm/src/reclaim.rs`）。如果它停留在
   ~257 MiB，说明在该版本上释放的客户机内存不会被归还：虚拟机在 Mac 上只会
   增长。
2. **开发者工作流。** 在虚拟机运行时：`make devflow` 必须报告
   0 fail（26 项检查）。
3. **活动监视器。** 其"内存"列显示的是 VMM 的 `phys_footprint`。
   当 Mac 内存不足时，macOS 会压缩部分客户机 RAM，
   该数值会超过客户机实际使用的量。测试中，在没有任何压缩时差距接近
   零，在内存压力下约为压缩量的三分之一到一半。如果在其他 macOS
   版本上差距不同，那是 macOS 统计方式的变化，而不是内存泄漏，前提是
   `ORBIT_MEMDUMP` + `vmm/bench/memxref.py` 显示 VMM 持有的内存与
   客户机使用的量大致相当（参见 [docs/architecture.md](architecture.md)）。

## 快速开始

```bash
git clone https://github.com/skax-eunsik-kim/orbit-vmm ~/src/orbit-vmm
cd ~/src/orbit-vmm
make guest      # download pinned Alpine kernel + rootfs (sha256-checked) into ~/.orbit/guest
make install    # build, sign, copy the VMM and scripts to ~/.orbit/bin, install the watchdog
make up         # start the VM from ~/.orbit/bin; first boot installs Docker (~1 min), later boots 2-4 s
export DOCKER_HOST=unix://$HOME/.orbit/vmm/docker.sock
docker run --rm hello-world
```

把 `DOCKER_HOST` 这一行加入 shell 配置文件，或者创建一个 context：

```bash
docker context create orbit --docker host=unix://$HOME/.orbit/vmm/docker.sock
docker context use orbit
```

`make up`（或 `~/.orbit/bin/vmm-up.sh`）会重启正在运行的虚拟机，这会停止
其中的所有容器。虚拟机和健康检查只从安装在 `~/.orbit/bin` 中的副本运行。
容器可以写入你的共享目录，而这个检出目录往往就是其中之一，因此 `make up`
不会从这里构建或运行任何东西。只有 `make install` 和 `make guest` 会这样做。
运行它们之前，请确认检出目录没有发生意外变化（`git status`）。更好的做法是把
检出目录放在共享目录之外。

## 配置

`scripts/vmm-up.sh`（即 `make up` 所运行的脚本）读取以下环境变量：

| 变量 | 默认值 | 含义 |
|---|---|---|
| `ORBIT_MEM_MIB` | `8192` | 客户机 RAM 上限。释放的内存会归还给 macOS，所以设置较高的上限代价很小。 |
| `ORBIT_CPUS` | Mac 核心数（`sysctl -n hw.ncpu`） | vCPU 数量。空闲的 vCPU 会休眠，不消耗 CPU；若数量很少，并行启动容器（例如 agent 沙箱）时会在 CPU 上排队。 |
| `ORBIT_GUEST` | `~/.orbit/guest` | 存放客户机 `kernel` 和 `initramfs` 的目录（例如用 `ORBIT_HOME=… scripts/fetch-guest.sh` 构建的目录）。 |
| `ORBIT_DISK_GB` | `64` | **新建**数据磁盘（稀疏文件）的大小。 |
| `ORBIT_DISK_WARN_MIB` | 以 MiB 计的 `ORBIT_WARN_FREE_GB` | 当 Mac 可用空间低于该值时，VMM 也会在 `console.log` 中发出警告。 |
| `ORBIT_DISK_FLOOR_MIB` | `1024` | 低于 `ORBIT_STOP_FREE_GB` 时的最后手段：低于该值时，客户机的磁盘写入会被挂起，直到空间被释放（参见[数据磁盘](#数据磁盘)）。 |
| `ORBIT_DISK_PAUSE_SECS` | `600` | 被挂起的写入在失败前等待的时长。 |
| `ORBIT_SHARE_ALLOW` | 见下文 | 以冒号分隔的、允许容器 bind mount 的目录。 |
| `ORBIT_HOST_LOOPBACK` | `1` | 设为 `0` 可阻止容器访问 Mac 上 `127.0.0.1`（以及 `host.docker.internal`）的服务。 |
| `ORBIT_WARN_FREE_GB` / `ORBIT_STOP_FREE_GB` | `10` / `3` | Mac 磁盘可用空间阈值：分别是虚拟机向你发出警告的阈值，以及在磁盘写满前干净关机的阈值。 |
| `ORBIT_INSTANCE` | （未设置） | 在 `~/.orbit/vmm-<name>` 下运行一个拥有独立磁盘和 socket 的单独虚拟机。 |

### 文件共享

默认情况下，虚拟机可以看到 `~/work`、`~/src`、`~/code`、`~/dev`、
`~/projects` 和 `~/Developer` 中实际存在的那些目录。`/Users` 中的其他内容都
不可见。要添加目录：

```bash
ORBIT_SHARE_ALLOW="$HOME/src:$HOME/clients" make up
```

共享目录**对所有容器都是可写的**。orbit-vmm 拒绝共享
你的主目录、主目录之上的任何目录、隐藏目录（`~/.ssh`、
`~/.orbit`、…）、`~/Library`、`~/bin` 和 `~/Applications`。位于另一个共享目录内部的目录
会被忽略，因为外层目录已经覆盖了它。容器无法重命名或
删除共享目录本身。

- 在 Mac 上对文件的编辑约 0.5 秒内在容器中可见。新文件
  和重命名的目录约 1 秒内出现。
- 在 Mac 上编辑文件和新建文件，约 1 秒后会在容器中触发
  `IN_MODIFY` / `IN_CLOSE_WRITE`。

### 网络

- `-p 8080:80` 和 compose 的 `ports:` 会在约 1 秒内被转发到 Mac 上的
  `127.0.0.1:8080`。如果 Mac 端口已被占用，转发会失败，
  错误会写入 `~/.orbit/vmm/console.log`。
- 容器通过 `host.docker.internal` 访问 Mac 本机（localhost）上的服务。
  这包括仅监听 `127.0.0.1` 的任何服务（数据库、notebook、
  本地模型服务器）。如果不希望这样，请设置 `ORBIT_HOST_LOOPBACK=0`。
- 虚拟机中任何监听所有地址的进程，都会在 Mac 的 `127.0.0.1` 上获得相同端口，
  而不仅限于 `-p` 发布的端口（类似 `--network host`）。

### 根证书（进行 TLS 检查的代理）

- `make guest` 会读取 Mac 在 Apple 内置证书集之外所信任的根证书
  （即针对 SSL 标记为"始终信任"的证书，通常是企业的
  TLS 检查 CA），并把它们构建进你的客户机。在没有这类根证书的 Mac 上
  不会添加任何内容，容器看到的是其镜像自带的证书包。
  仓库和发布版本中不包含任何证书。
- 容器和构建步骤会通过挂载看到这些根证书被追加到镜像的 CA
  证书包中。构建步骤会得到证书包所在目录的一份副本，因此
  `update-ca-certificates` 和 `apt-get upgrade` 可以正常工作。两者都不会
  把 Mac 的根证书写入镜像。
- `ORBIT_HOST_CA=0 make guest && make up` 会构建一个不含这些证书的客户机。
- 此后你的客户机（`~/.orbit/guest`）和数据磁盘中会包含你所在组织的
  CA：不要把它们分享到组织之外。

## 文件

| 路径 | 内容 |
|---|---|
| `~/.orbit/guest/` | 由 `make guest` 构建的内核和 initramfs。 |
| `~/.orbit/vmm/data.img` | 数据磁盘（Docker 镜像、卷）。 |
| `~/.orbit/vmm/docker.sock` | Docker API。 |
| `~/.orbit/vmm/console.log` | 客户机控制台；出现故障时首先查看这里（`console.prev.log` 是上一次运行的日志）。 |
| `~/.orbit/vmm/watchdog.log`、`crashes/` | 监督进程日志以及意外停止的记录。 |

**不要分享 `~/.orbit/guest/*` 或 `data.img`。** 它们包含 GPL 二进制文件
以及你这台机器所信任的根证书
（[THIRD_PARTY_NOTICES.md](../../../THIRD_PARTY_NOTICES.md)）。

要扩大数据磁盘，请停止虚拟机、扩大该文件，然后重新启动虚拟机。
客户机会在启动时调整其文件系统大小：

```bash
python3 -c "import os;p=os.path.expanduser('~/.orbit/vmm/data.img');os.truncate(p,max(os.path.getsize(p),128<<30))"
```

### 数据磁盘

`data.img` 是稀疏文件：它在 Mac 上占用的空间与 Docker 写入的量相同，
最多不超过其逻辑大小（`vmm-up.sh` 会同时打印两者）。它会悄无声息地增长，
因此有两道防护监视 Mac 的可用空间。监督进程（`vmm-run.sh`）
在低于 `ORBIT_WARN_FREE_GB` 时通知你，在低于 `ORBIT_STOP_FREE_GB` 时
干净地关闭客户机（在该阈值以下也不会启动）。在此之下，作为最后
手段，VMM 每 5 秒检查一次：

- 低于 `ORBIT_DISK_WARN_MIB` 时，它会向 `console.log` 记录 `orbit-vmm: disk space low`
  （空间持续不足期间每 10 分钟记录一次），并且
  `scripts/healthcheck.py` 会报告一个警告。
- 低于 `ORBIT_DISK_FLOOR_MIB`，或者因磁盘已满导致写入失败时，
  它会挂起客户机的磁盘写入，并记录一次 `orbit-vmm: DISK FULL`。
  Docker 会停顿（客户机可能会记录 hung-task 警告），但不会有任何操作失败。
  在 Mac 上释放空间后，写入会从中断处继续
  （`disk writes resumed`）。
- 如果在 `ORBIT_DISK_PAUSE_SECS` 内没有释放空间，写入就会失败。此时 ext4
  会报告 I/O 错误，并可能变为只读，因此请在此之前释放空间；
  如果已经变为只读，请重启虚拟机。

挂起写入而不是让其失败，是 QEMU 的默认行为
（`werror=enospc`）：失败的写入是 ext4 已经承诺要保存的数据，
丢失这些数据正是 Mac 磁盘写满时损坏客户机的原因。

## 升级

```bash
make update            # the latest release; TO=v0.2.1 for a given one
make rollback          # back to what was installed before the last update
```

`make update` 在它自己的克隆（`~/.orbit/src`）中构建发布版本，而不是在
你的检出目录中，并且会在替换任何东西之前先构建新的客户机，因此下载或构建失败
不会影响已安装的版本。它会保留之前的二进制文件和客户机，供 `make rollback` 使用。
只有在没有容器运行时，它才会重启虚拟机；否则虚拟机会继续使用旧版本，直到
执行 `make up`（或者运行 `NOW=1 make update`）。客户机升级后的首次启动会
重新安装客户机软件包（约 1 分钟）。

如果安装了健康检查，它每天会查询一次最新发布版本，
并针对每个更新的版本显示一条 macOS 通知。不执行 `make update`
就不会安装任何东西。

如果想跟随 `main` 而不是发布版本：`git pull && make guest install up`。

## Watchdog

`make install` 还会安装一个 launchd agent `com.orbit.vmm`，用于运行
虚拟机并在故障时恢复它。之所以需要它，是因为没有别的东西承担这项工作：orbit 没有常驻运行的
应用，所以没有它的话，崩溃或卡死的 VMM 会一直处于停止状态。

```bash
ORBIT_WATCHDOG=0 make install          # install without it
./scripts/install-watchdog.sh --uninstall
./scripts/install-watchdog.sh --reload # apply a changed agent now (restarts the VM)
```

再次安装不会影响正在运行的虚拟机。

- **崩溃：** 如果 VMM 退出，或客户机重启或 panic，launchd 会在几秒内
  重新启动它。
- **卡死：** 每 30 秒 ping 一次客户机。连续三次 ping 未响应后，VMM
  会被 abort，从而让 macOS 生成崩溃报告，然后再重新启动。整个过程
  约需 90 秒。
- **崩溃循环：** 如果 10 分钟内启动超过三次，它会停止
  重启并通知你。
- **崩溃记录：** 每次意外停止都会保存在
  `~/.orbit/vmm/crashes/<time>/` 中（控制台日志、macOS 崩溃报告、watchdog
  日志）。保留最新的 10 条，并会向你发送 macOS 通知。容器
  会随虚拟机一起停止，因此没有 `restart:` 策略的容器需要重新
  启动。
- **主动停止：** `make down`（`vmm-up.sh --stop`）和客户机内的 `poweroff`
  被视为有意的停止，不会被重启。在你注销时正在运行的虚拟机
  会在登录时再次启动。

没有 watchdog 时，`make up` 会在后台运行同一个监督进程，
因此卡死仍会被检测到、崩溃仍会被记录，但不会有任何东西被
重启。

## 健康检查（可选）

执行 `make install` 后，`scripts/install-healthcheck.sh` 会安装一个 launchd
agent，用来运行已安装的 `~/.orbit/bin/healthcheck.py`。它会检查 Docker、
端口、bind mount、inotify 和客户机时钟，并把 VMM 的内存、线程
和 fd 数量记录到 `~/.orbit/vmm/health.jsonl`。

检查每小时运行一次。连续三次运行正常后，间隔会放宽到
每 2 小时，然后每 4 小时。任何问题都会把它重置为每小时一次。要移除它，请运行
`--uninstall`。

## 已知限制

- **inotify：** Mac 端的变更大约一秒后才会到达容器。一个包含超过 256 个条目的
  新目录树（新建的 worktree、在 Mac 上执行的
  `npm install`）只会为其顶层目录触发一次 `IN_CREATE`；
  递归监视器会从那里开始扫描。只有当客户机
  之前已经见过旧名称时，重命名事件才能成对出现；否则会表现为删除 + 创建。
- **端口转发：** UDP 发布不会被转发。绑定在特定
  主机 IP 上的发布（`-p 127.0.0.1:8080:80`）与其他发布一样被转发到 Mac 的 `127.0.0.1`；
  同一端口若发布在两个这样的 IP 上，只有最新的那个能被访问到。
  如果两个虚拟机发布了相同端口，第二个的转发会失败（会记录日志）。
- **Bind mount：** Mac 与容器之间不共享文件锁。
  无法在 bind mount 上创建 Unix socket 和 FIFO。
- **数据磁盘：** Docker 释放的空间只有在客户机执行
  trim（`fstrim`，虚拟机空闲时每 10 分钟一次，以及启动后执行一次）时才会还给 Mac，
  因此 `docker system prune` 的效果要几分钟后才会在 Mac 上体现。磁盘镜像
  永远不会缩小到客户机已写入但尚未释放的量以下。
- **根证书：** 把 CA 证书包复制到其他位置的构建步骤
  （`cp /etc/ssl/certs/ca-certificates.crt /app/`）会把 Mac 的根证书一并
  复制进镜像。重新生成证书包在构建步骤中可行，但在
  运行中的容器里不可行，而且同一个 `RUN` 中后续的命令看不到 Mac 的
  根证书。如果 CA 目录上挂载了其他内容（`RUN --mount`、`-v`），
  根证书就不会被添加到那里。
- **缺失的部分：** 没有安装程序、GUI 或 Kubernetes。

## 工作原理

参见 [docs/architecture.md](architecture.md)。简而言之：

```
docker CLI ──unix socket── orbit-vmm ──vsock── guest: dockerd
                              │
          Hypervisor.framework (vCPUs, in-kernel GICv3)
          virtio-mmio: blk · net (libslirp) · vsock · fs · balloon (free page reporting)
```

## 参与贡献

欢迎提交 bug 报告和 pull request。开发环境搭建和检查项请参见
[CONTRIBUTING.md](../../../CONTRIBUTING.md)。
安全问题请私下报告（[SECURITY.md](../../../SECURITY.md)）。

## 许可证

Apache-2.0（[LICENSE](../../../LICENSE)）。你构建的客户机镜像会下载 Alpine
Linux 软件包，这些软件包有各自的许可证；参见
[THIRD_PARTY_NOTICES.md](../../../THIRD_PARTY_NOTICES.md)。
