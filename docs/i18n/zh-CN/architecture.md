# 架构

[English](../../architecture.md) | [한국어](../ko/architecture.md) | [日本語](../ja/architecture.md) | **简体中文** | [Español](../es/architecture.md) | [Português (Brasil)](../pt-BR/architecture.md) | [Français](../fr/architecture.md) | [Deutsch](../de/architecture.md) | [हिन्दी](../hi/architecture.md)

> 本文为译文，以英文文档 [architecture.md](../../architecture.md) 为准；如两者内容不一致，请以英文版为准。

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

## VMM（`vmm/`）

VMM 是一个单独的 Rust 进程。`src/vm/` 包含虚拟机本身：

- Hypervisor.framework FFI（`hvf.rs`）和内核态 GIC（`gic.rs`）。
- 启动与设备树（`boot.rs`、`fdt.rs`）。
- PL011 控制台（`pl011.rs`）。
- vCPU 线程、PSCI 和 MMIO 解码（`vcpu.rs`）。

`src/virtio/` 包含 virtio-mmio v2 传输层和 split virtqueue，另外每个设备各占一个
文件。

`signals.rs` 处理发给 VMM 自身的 SIGTERM 和 SIGINT：它把发送者的 pid 和名称记录到
`console.log`，请求 guest 关机（最多等待 30 s），然后以“被该信号终止”的状态退出，
这样 supervisor 仍会重启它。收到第二个信号时会立即退出。

### 归还内存

Guest RAM 是来自 `hv_vm_allocate` 的匿名内存，映射到虚拟机中。当 guest
报告空闲页（`VIRTIO_BALLOON_F_REPORTING`，队列 2）时，VMM 会：

1. 从虚拟机中解除该范围的映射（`hv_vm_unmap`），
2. 用全新的零页替换它（`mmap(MAP_FIXED)`），这样 macOS 会把旧页从进程
   footprint 中移除，
3. 重新映射该范围（`hv_vm_map`）。

仅靠 `madvise` 无法缩小已映射到虚拟机中的内存的 footprint。

Guest 内核以 `page_reporting.page_reporting_order=2` 启动，因此报告单位是
16 KiB，即 macOS 的页大小。碎片化的空闲内存也照样能被归还。

Linux 会把文件数据保留为页缓存（page cache），而 balloon 无法报告这部分内存。
`guest/etc/orbit/memd.sh` 负责处理这一问题：

- 轻负载持续 60 s 后，回收容器页缓存的一半
  （cgroup `memory.reclaim`）。
- 完全空闲 30 s 后，丢弃干净的缓存。
- 两种情况下都会整理内存（compact），使释放出的页组成可报告的块。
- 任何时候都通过根 cgroup 的 `memory.reclaim` 把页缓存控制在 1 GiB 以下
  （构建从不空闲；而且 Mac 本身也会缓存 `data.img`）。
- 当 macOS 报告内存压力时，改为把页缓存控制在 256 MiB（warn）或 64 MiB
  （critical）以下（`orbit.memd_warn_mb`、`orbit.memd_crit_mb`）。

`pressure.rs` 监视 macOS 内存压力（一个 libdispatch 内存压力源，外加每 30 s
重新读取一次的 `kern.memorystatus_vm_pressure_level`），并通过控制 vsock 发送
`pressure normal|warn|critical`。级别变化时，`control.sh` 把级别记录到
`/run/orbit-pressure`，并立即运行 `memd.sh reclaim <level>`：回收到新的上限
（critical 时先丢弃所有干净缓存）并整理内存，从而让空闲页报告在 macOS 不得不
压缩这些页之前就把它们交还。`ORBIT_PRESSURE=off` 会关闭此功能，
`ORBIT_PRESSURE=warn|critical` 则固定级别，便于对比测试。它对 guest 只访问过
一次的缓存（构建的输入、拉取的镜像）有帮助；guest 反复读取的工作集无论上限多少
都会常驻内存。
每次级别变化都会通知 guest；`console.log` 每 10 分钟最多记录一行，并附上期间的
变化次数。

活动监视器（Activity Monitor）的“内存”列显示的是进程的 `phys_footprint`。当
Mac 内存不足时，macOS 会压缩或交换部分 guest RAM，此时 footprint 会超出 VMM
实际持有的 guest 页（实测约多出被压缩量的三分之一到一半；没有压缩时接近于零）。
要查看哪些 guest 页被持有，可以用 `ORBIT_MEMDUMP=<prefix>` 启动 VMM，执行
`touch <prefix>.req`，然后用 `vmm/bench/memxref.py` 将 `<prefix>.bin`
（guest RAM 每个 16 KiB 页对应一个 `mincore` 字节）与 guest 的
`/proc/kpageflags` 进行比对。

### 数据盘（virtio-blk）

`blk.rs` 提供 `data.img`（一个稀疏文件），并对其加 `flock`，使两个虚拟机无法共用
同一块磁盘。

- **Discard。** Guest 的 discard 会转换为对镜像文件的 `F_PUNCHHOLE`，并且 `memd`
  会在启动后以及虚拟机空闲时每 10 分钟运行一次 `fstrim /data`，从而把 Docker
  释放的空间归还给 Mac。
- **磁盘保护。** VMM 每 2 s 检查一次 Mac 上的可用空间。低于
  `ORBIT_DISK_WARN_MIB` 时记录警告。低于 `ORBIT_DISK_FLOOR_MIB`，或写入因 ENOSPC
  失败时，它会暂缓 guest 的写入而不是让其失败（相当于 QEMU 的 `werror=enospc`），
  最长 `ORBIT_DISK_PAUSE_SECS`，并在空间释放后恢复，因此 ext4 永远不会遇到写入
  错误。Supervisor 会更早地在低于 `ORBIT_STOP_FREE_GB` 时干净地停止虚拟机。

### 文件共享（virtio-fs）

`fs.rs` 是 FUSE 服务端，`hostfs.rs` 负责路径访问。

- 每条路径都从一个目录 fd 出发，用 `openat(O_NOFOLLOW)` 逐级遍历，因此符号链接
  无法逃出允许的目录（`--share-allow`）。允许目录的上级目录可以穿越，但不能列出
  内容。
- VMM 会自行检查允许列表：条目必须是真实路径（不能是符号链接、大小写须完全一致、
  不能相互嵌套），并且会拒绝主目录、其上级的任何目录、隐藏目录、`~/Library`、`~/bin` 和
  `~/Applications`。
  Guest 无法重命名或删除允许的目录本身。
- 先前遍历得到的目录 fd 会缓存 1 s，因此访问一条路径时不必从 `/Users` 开始逐级
  重新打开每个目录。Guest 自己执行的重命名和 rmdir 会立即清除相应条目，过期条目
  会在下次插入时关闭。不存在的路径由 guest 缓存 1 s（`ORBIT_FS_NEG_TTL_MS`）。
- 共有四个请求队列。
- 条目 TTL 为 5 s，属性 TTL 为 500 ms。启用了 `KEEP_CACHE` 和
  `AUTO_INVAL_DATA`，readdirplus 也已启用。
- Writeback 缓存是关闭的。开启时，Mac 端的修改在 guest 中不可见。
- 容器中的 `chown` 存储在 `user.orbit.owner` xattr 中，并在 `stat` 中回报。
  组/其他用户的权限位与所有者权限位保持一致，因此容器中的非 root 用户也能写入
  你的文件。
- Mac 端的修改以 inotify 事件的形式到达 guest。`fsevents.rs` 用 FSEvents 监视
  允许的目录；`forward_events` 把每一批事件转换为修改、创建、重命名和删除（依据
  Mac 的当前状态以及 guest 已查找过的内容），并通过 vsock 发送给 `fsnotify.sh`。
  Guest 以一个保留的 gid 重放每项变更：修改（`IN_MODIFY`）对应一次不改变大小的
  `truncate`，其余分别对应 create/mkdir、rename 或 unlink/rmdir。服务端能识别该
  gid，绝不会把其中任何操作应用到 Mac 上，并且只对它已排队、且 Mac 端状态仍然
  一致的路径让操作成功。
- Docker 套接字所在的目录（`~/.orbit/vmm/`）在 guest 中显示为一个空的只读挂载点，
  guest 会在其中链接 `docker.sock`，因此会 bind-mount `DOCKER_HOST` 路径的工具
  （testcontainers 的 Ryuk）也能正常工作。

### 网络

`slirp.rs` 在独立线程上运行 libslirp。

- `ports.rs` 把已发布的端口转发到 Mac 的 `127.0.0.1`。它跟踪 Docker 的容器事件
  （VMM 自身 docker 套接字上的 `/events`），在容器启动后立即转发 `-p` 发布的
  端口，包括发布在特定主机 IP 上的端口。此外，它每 250 ms 还会通过控制通道读取
  guest 中处于监听状态的套接字，以支持 `--network host` 服务。
- 容器通过 `host.docker.internal` 访问 Mac 的 localhost
  （`ORBIT_HOST_LOOPBACK=0` 可关闭此功能）。
- DNS：容器向 guest 的 dnsmasq 发起查询，dnsmasq 应答
  `host.docker.internal`，其余查询转发给 libslirp 的解析器，再由它转发给 Mac 的
  解析器。它允许同时有 4096 个查询在途；dnsmasq 的默认值 150 曾导致并行构建因
  “Temporary failure in name resolution” 而失败。
- 控制通道还负责保持 guest 时钟同步。它会测量往返时间，只有在偏差超过 250 ms
  时才调整时钟。因此时钟可能会向回跳：guest 中的代码不能依赖 mtime 来判断内容
  是否发生了变化。
- `csrc/asyncclose.c` 是一个小型 dylib，用于拦截（interpose）互联网套接字的
  `close()`。内容过滤类网络扩展可能导致 FIN 之后的 `close()` 阻塞约 1 s，这会
  卡住网络线程上的所有连接。

## Guest（`guest/`）

`scripts/fetch-guest.sh` 下载固定版本的 Alpine 内核和 minirootfs（经 sha256
校验），并用它们加上 `guest/` 构建 initramfs。首次启动时安装的软件包（Docker、
qemu 等）不是固定版本的：它们来自当时 Alpine 的软件仓库。Alpine 会移除旧版本的软件包，
因此每周运行的 `guest-pins` workflow 会检查这两个固定版本的下载是否仍然存在。

- **首次启动：** 将 Docker 及相关组件安装到数据盘上，然后创建一个只读的根
  快照。Docker 来自 Alpine v3.23（29.5）：29.8 在每次热构建时都会在本地构建的
  基础镜像之上重建每一层。
- **后续启动：** 在 tmpfs overlay 下使用该快照，因此只需 2–4 s。

Guest 组件：

- `dockerd.sh` 写入 `daemon.json`（通过 `defaultMaxUsedSpace` 将构建缓存上限设为
  20 GB），并以 `orbit-runc` 启动 dockerd。
- `orbit-runc` 是一个 OCI 运行时包装器，让容器信任 Mac 在 Apple 自带根证书之外
  额外信任的根证书。`make guest` 会把这些根证书导出到 `/etc/orbit/host-ca.pem`
  （`scripts/export-host-ca.py`，通常是公司的 TLS 检查 CA；大多数 Mac 上没有，
  设置 `ORBIT_HOST_CA=0` 时也没有）。
  - 容器会得到一份镜像 CA bundle 的副本，其中追加了这些根证书，并以只读方式
    bind-mount 覆盖在原 bundle 上。
  - 构建中的 `RUN` 步骤则会得到该 bundle 所在目录的一份可读写副本，这样通过
    重命名来重新生成 bundle 的工具也能正常工作。runc 作为子进程运行；它返回时
    （BuildKit 会传入 `--keep`，因此 rootfs 仍处于挂载状态），仅当该目录的内容
    发生变化时，才会把它连同镜像自带的 bundle 一起写回该步骤的 rootfs。主机的
    根证书永远不会进入镜像层。
  - 如果某个目录包含另一个 bundle 的目录或挂载点，则改为对文件进行 bind；位于
    已有挂载（`RUN --mount`、`-v`）处或其下的 bundle 则保持不动。
- `dns.sh` 运行 dnsmasq（见“网络”一节）。
- `binfmt.sh` 注册 qemu-x86_64。
- `memd.sh`：见上文。

## Mac 端（`scripts/`）

- `make install` 把 VMM、它的 `close()` 拦截库、`vmm-up.sh`、`vmm-run.sh` 和
  `healthcheck.py` 复制到 `~/.orbit/bin`，写入 `~/.orbit/bin/VERSION`，并安装
  launchd 看门狗。虚拟机只会从这份副本运行：容器可以写入源码 checkout。
- `vmm-run.sh` 负责监管虚拟机，无论是否使用 launchd。它会加载 `vmm-up.sh` 保存在
  `config.env` 中的设置并导出它们，因为 launchd 启动它时不带用户的环境变量。
  - 它会检测崩溃、guest 重启或 panic、或者卡死（连续三次控制套接字 ping 无响应；
    会先中止 VMM，以便 macOS 写入崩溃报告）。只有在 launchd（看门狗）之下，虚拟机
    才会随后再次启动；没有它时，监管进程会记录这次停止然后退出。
  - 如果 10 分钟内启动超过三次，则停止重启。
  - 每次意外停止都会在 `crashes/<time>/` 中保留一份记录。
  - 可用空间低于 `ORBIT_WARN_FREE_GB` 时发出警告，低于 `ORBIT_STOP_FREE_GB` 时
    干净地停止虚拟机。
- `healthcheck.py`（如已安装）会检查 Docker、端口、bind mount、inotify、时钟和
  磁盘空间，记录 VMM 的内存、线程和 fd，并且每天从 `releases/latest` 页面读取一次
  最新版本，对每个更新的版本显示一次通知。
- `orbit` 是安装后的命令：`up` 和 `down` 运行 `vmm-up.sh`，`update` 和 `rollback`
  运行 `update.sh`，`status` 报告状态，`uninstall` 移除 agent、健康检查的临时目录和
  `~/.orbit`。Makefile 的运行类 target 都调用它，因此不会运行 checkout 中的任何内容。
- `update.sh`（`orbit update`、`orbit rollback`）在自己的 clone（`~/.orbit/src`，
  克隆的是 `~/.orbit/bin/VERSION` 中记录的仓库）中构建发布标签，在暂存目录中构建新的 guest，保留之前的二进制文件和 guest 以便
  回滚，并且只在没有容器运行时才重启虚拟机。
