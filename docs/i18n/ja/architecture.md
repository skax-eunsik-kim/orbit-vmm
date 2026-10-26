# アーキテクチャ

[English](../../architecture.md) | [한국어](../ko/architecture.md) | **日本語** | [简体中文](../zh-CN/architecture.md) | [Español](../es/architecture.md) | [Português (Brasil)](../pt-BR/architecture.md) | [Français](../fr/architecture.md) | [Deutsch](../de/architecture.md) | [हिन्दी](../hi/architecture.md)

> この文書は翻訳です。正となるのは英語版の [architecture.md](../../architecture.md) で、内容が異なる場合は英語版に従ってください。

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

VMM は 1 つの Rust プロセスです。`src/vm/` にはマシン本体があります。

- Hypervisor.framework の FFI（`hvf.rs`）とカーネル内 GIC（`gic.rs`）。
- ブートとデバイスツリー（`boot.rs`、`fdt.rs`）。
- PL011 コンソール（`pl011.rs`）。
- vCPU スレッド、PSCI、MMIO デコード（`vcpu.rs`）。

`src/virtio/` には virtio-mmio v2 トランスポートと split virtqueue に加え、デバイスごとに
1 つのファイルがあります。

`signals.rs` は、VMM 自身に送られた SIGTERM と SIGINT を処理します。送信元の pid と名前を
`console.log` に記録し、ゲストに電源オフを要求して（最大 30 s）、そのシグナルで強制終了された
ものとして終了します。そのため、スーパーバイザーは引き続き VMM を再起動します。
2 回目のシグナルを受け取ると、すぐに終了します。

### メモリの返却

ゲスト RAM は `hv_vm_allocate` で確保した匿名メモリで、VM にマップされています。ゲストが
空きページを報告すると（`VIRTIO_BALLOON_F_REPORTING`、キュー 2）、VMM は次の処理を行います。

1. その範囲を VM からアンマップします（`hv_vm_unmap`）。
2. 新しいゼロページで置き換えます（`mmap(MAP_FIXED)`）。これにより macOS は古いページを
   プロセスのフットプリントから外します。
3. その範囲を再びマップします（`hv_vm_map`）。

`madvise` だけでは、VM にマップされたメモリのフットプリントは縮みません。

ゲストカーネルは `page_reporting.page_reporting_order=2` で起動するため、報告単位は
macOS のページサイズと同じ 16 KiB です。断片化した空きメモリも返却されます。

Linux はファイルデータをページキャッシュとして保持し続けますが、balloon はこれを報告できません。
`guest/etc/orbit/memd.sh` がこれに対処します。

- 低負荷が 60 s 続くと、コンテナのページキャッシュの半分を回収します
  （cgroup `memory.reclaim`）。
- 完全なアイドルが 30 s 続くと、クリーンなキャッシュを破棄します。
- どちらの場合も、解放したページが報告可能なブロックになるようにメモリをコンパクションします。
- 常に、ルート cgroup の `memory.reclaim` を使ってページキャッシュを 1 GiB 未満に保ちます
  （ビルドはアイドルになりませんし、Mac 側でもどのみち `data.img` がキャッシュされます）。
- macOS がメモリプレッシャーを報告している間は、代わりにページキャッシュを 256 MiB（warn）
  または 64 MiB（critical）未満に保ちます（`orbit.memd_warn_mb`、
  `orbit.memd_crit_mb`）。

`pressure.rs` は macOS のメモリプレッシャーを監視し（libdispatch のメモリプレッシャー
ソースに加え、30 s ごとに `kern.memorystatus_vm_pressure_level` を読み直します）、
control vsock で `pressure normal|warn|critical` を送ります。変化があると、
`control.sh` はレベルを `/run/orbit-pressure` に記録し、すぐに
`memd.sh reclaim <level>` を実行します。新しい上限まで回収し（critical の場合は先に
すべてのクリーンなキャッシュを破棄します）、コンパクションを行うことで、macOS が圧縮せざるを
得なくなる前に、空きページ報告でページを返却します。`ORBIT_PRESSURE=off` で無効にでき、
`ORBIT_PRESSURE=warn|critical` でレベルを固定できます。これは比較用です。この仕組みは、
ゲストが一度だけ触れたキャッシュ（ビルドの入力、pull したイメージ）には効果がありますが、
ゲストが繰り返し読み直すワーキングセットは、上限にかかわらず常駐し続けます。
ゲストにはすべての変化が通知されますが、`console.log` に書かれるのは 10 分あたり最大 1 行で、
その間の変化の回数が併記されます。

アクティビティモニタの「メモリ」列は、プロセスの `phys_footprint` です。Mac のメモリが
不足すると、macOS はゲスト RAM の一部を圧縮またはスワップし、フットプリントは VMM が
実際に保持しているゲストページを上回って増えます（計測では圧縮量の約 3 分の 1 から半分。
何も圧縮されていなければほぼゼロです）。どのゲストページが保持されているかを確認するには、
`ORBIT_MEMDUMP=<prefix>` を付けて VMM を起動し、`touch <prefix>.req` を実行してから、
`vmm/bench/memxref.py` を使って `<prefix>.bin`（ゲスト RAM の 16 KiB ページごとに
`mincore` 1 バイト）をゲストの `/proc/kpageflags` と突き合わせます。

### データディスク（virtio-blk）

`blk.rs` はスパースファイルである `data.img` を提供し、2 つの VM が 1 つのディスクを共有
できないように、そのファイルに `flock` をかけます。

- **Discard。** ゲストの discard はイメージ上の `F_PUNCHHOLE` になります。また `memd` は
  起動後と、VM が静かなときに 10 分ごとに `fstrim /data` を実行するため、Docker が解放した
  領域は Mac に戻ります。
- **ディスクガード。** VMM は 2 s ごとに Mac の空き容量を確認します。
  `ORBIT_DISK_WARN_MIB` を下回ると警告をログに出します。`ORBIT_DISK_FLOOR_MIB` を下回るか、
  書き込みが ENOSPC で失敗すると、ゲストの書き込みを失敗させる代わりに最大
  `ORBIT_DISK_PAUSE_SECS` の間保留し（QEMU の `werror=enospc`）、空き容量ができたら再開します。
  そのため ext4 が書き込みエラーを見ることはありません。スーパーバイザーはそれより早く、
  `ORBIT_STOP_FREE_GB` を下回った時点で VM を正常に停止します。

### ファイル共有（virtio-fs）

`fs.rs` は FUSE サーバーで、`hostfs.rs` がパスへのアクセスを扱います。

- すべてのパスはディレクトリ fd から `openat(O_NOFOLLOW)` でたどるため、シンボリックリンクで
  許可されたディレクトリ（`--share-allow`）の外に出ることはできません。許可された
  ディレクトリより上位のディレクトリは通過できますが、一覧表示はできません。
- VMM は許可リストを自身で検査します。エントリは実在のパスである必要があり（シンボリック
  リンク不可、大文字小文字の完全一致、入れ子不可）、ホームディレクトリ、その上位のすべて、
  隠しディレクトリ、`~/Library`、`~/bin`、`~/Applications` は拒否されます。ゲストは許可されたディレクトリそのものを
  名前変更したり削除したりできません。
- 以前のパス探索で得たディレクトリ fd は 1 s キャッシュされるため、パスごとに `/Users` から
  下のすべてのディレクトリを開き直すことはありません。ゲスト自身による rename と rmdir は
  エントリを即座に破棄し、期限切れのエントリは次の挿入時に閉じられます。存在しないパスは
  ゲスト側で 1 s キャッシュされます（`ORBIT_FS_NEG_TTL_MS`）。
- リクエストキューは 4 つあります。
- エントリの TTL は 5 s、属性の TTL は 500 ms です。`KEEP_CACHE` と
  `AUTO_INVAL_DATA` を有効にしており、readdirplus も有効です。
- ライトバックキャッシュは無効です。有効にすると、Mac 側での編集がゲストから
  見えませんでした。
- コンテナの `chown` は `user.orbit.owner` xattr に保存され、`stat` で
  返されます。group/other の権限ビットは owner のビットと同じになるため、
  root 以外のコンテナユーザーもあなたのファイルに書き込めます。
- Mac での編集は inotify イベントとしてゲストに届きます。`fsevents.rs` は
  許可されたディレクトリを FSEvents で監視し、`forward_events` は各バッチを
  （Mac の現在の状態と、ゲストがルックアップしたものから）編集、作成、名前変更、削除に変換し、
  vsock 経由で `fsnotify.sh` に送ります。
  ゲストは各変更を予約済みの gid の下で再現します。編集（`IN_MODIFY`）には同じサイズへの
  `truncate`、それ以外には create/mkdir、rename、unlink/rmdir を使います。サーバーはその gid を
  認識し、これらを Mac には一切適用せず、自身がキューに入れたパスで、かつ Mac 側がまだ一致して
  いる場合にのみ成功させます。
- docker ソケットのディレクトリ（`~/.orbit/vmm/`）は、ゲストには空の読み取り専用
  マウントポイントとして見え、ゲストはそこに `docker.sock` をリンクします。そのため、
  `DOCKER_HOST` のパスを bind mount するツール（testcontainers の Ryuk）も動作します。

### ネットワーク

`slirp.rs` は libslirp を専用スレッドで実行します。

- `ports.rs` は公開ポートを Mac の `127.0.0.1` にフォワードします。Docker のコンテナ
  イベント（VMM 自身の docker ソケットの `/events`）を追跡し、コンテナが起動するとすぐに
  `-p` による公開をフォワードします。特定のホスト IP で公開されたポートも対象です。
  `--network host` のサービス向けに、250 ms ごとに control チャネル経由でゲストの待ち受け
  ソケットも読み取ります。
- コンテナは `host.docker.internal` を通じて Mac の localhost に到達します
  （`ORBIT_HOST_LOOPBACK=0` で無効にできます）。
- DNS: コンテナはゲストの dnsmasq に問い合わせます。dnsmasq は
  `host.docker.internal` に応答し、それ以外は libslirp のリゾルバーへ、さらにそこから
  Mac のリゾルバーへ転送します。同時に処理中のクエリは 4096 まで許可しています。dnsmasq の
  デフォルトの 150 では、並列ビルドが "Temporary failure in name resolution" で失敗していました。
- control チャネルはゲストの時刻の同期も行います。ラウンドトリップ時間を計測し、
  ずれが 250 ms を超えたときだけ時刻をステップ調整します。
  そのため時刻が巻き戻ることがあります。ゲスト側のコードは、何かが変更されたかどうかの
  判定に mtime を頼ってはいけません。
- `csrc/asyncclose.c` は、インターネットソケットの `close()` をインターポーズする小さな
  dylib です。コンテンツフィルター型のネットワーク拡張があると、FIN の後の `close()` が
  約 1 s ブロックすることがあり、ネットワークスレッド上のすべての接続が止まってしまいます。

## ゲスト（`guest/`）

`scripts/fetch-guest.sh` は、バージョンを固定した Alpine のカーネルと minirootfs を
（sha256 で検証して）ダウンロードし、それらと `guest/` から initramfs をビルドします。初回起動で
インストールされるパッケージ（Docker、qemu など）は固定されておらず、その時点の Alpine の
リポジトリから取得されます。Alpine は古いパッケージバージョンを削除するため、毎週実行される `guest-pins` ワークフローで、固定した
2 つのダウンロードがまだ存在するかを確認しています。

- **初回起動:** Docker と関連ツールをデータディスクにインストールし、その後
  読み取り専用のルートスナップショットを作成します。Docker は Alpine v3.23 のもの（29.5）を
  使います。29.8 では、ウォームビルドのたびに、ローカルでビルドしたベースイメージの上の
  すべてのレイヤーが再ビルドされていました。
- **2 回目以降の起動:** そのスナップショットを tmpfs オーバーレイの下で使うため、
  2–4 s で起動します。

ゲストの構成要素:

- `dockerd.sh` は `daemon.json`（`defaultMaxUsedSpace` でビルドキャッシュの上限を 20 GB に
  設定）を書き出し、`orbit-runc` で dockerd を起動します。
- `orbit-runc` は、Apple の標準セット以外に Mac が信頼しているルート証明書をコンテナにも
  信頼させる OCI ランタイムラッパーです。`make guest` がそれらを書き出し
  （`scripts/export-host-ca.py`。通常は社内の TLS インスペクション用 CA で、ほとんどの Mac では
  存在せず、`ORBIT_HOST_CA=0` の場合もありません）、`/etc/orbit/host-ca.pem` に格納します。
  - コンテナには、イメージの CA バンドルにそれらを追記したコピーが、バンドルの上に読み取り
    専用で bind mount されます。
  - ビルドの `RUN` ステップには、代わりにバンドルのディレクトリの読み書き可能なコピーが
    渡されるため、rename によってバンドルを再生成するツールも動作します。runc は子プロセス
    として実行されます。runc が戻ったとき（BuildKit は `--keep` を渡すため、rootfs はまだ
    マウントされています）、内容が変わっていた場合に限り、そのディレクトリはイメージ自身の
    バンドルとともにステップの rootfs に書き戻されます。ホストのルート証明書がレイヤーに
    入ることはありません。
  - 別のバンドルのディレクトリやマウントを含むディレクトリには、代わりにファイル単位の bind が
    使われます。また、既存のマウント（`RUN --mount`、`-v`）の位置またはその配下にある
    バンドルには手を加えません。
- `dns.sh` は dnsmasq を実行します（「ネットワーク」を参照）。
- `binfmt.sh` は qemu-x86_64 を登録します。
- `memd.sh`: 上記を参照してください。

## Mac 側（`scripts/`）

- `make install` は、VMM、その `close()` インターポーザー、`vmm-up.sh`、
  `vmm-run.sh`、`healthcheck.py` を `~/.orbit/bin` にコピーし、
  `~/.orbit/bin/VERSION` を書き出して、launchd のウォッチドッグをインストールします。VM は
  常にそのコピーから実行されます。コンテナはチェックアウトに書き込めるためです。
- `vmm-run.sh` は、launchd の有無にかかわらず VM を監視します。launchd はユーザーの環境なしで
  これを起動するため、`vmm-up.sh` が `config.env` に保存した設定を読み込んで export します。
  - クラッシュ、ゲストの再起動やパニック、ハング（control ソケットへの ping に 3 回応答が
    ない場合。macOS がクラッシュレポートを書くよう、先に VMM を abort します）を検出します。
    その後 VM を再び起動するのは launchd（ウォッチドッグ）の下にいる場合だけで、そうでなければ
    スーパーバイザーは停止を記録して終了します。
  - 10 分間に 3 回を超えて起動すると、再起動をやめます。
  - 予期しない停止のたびに、その記録を `crashes/<time>/` に残します。
  - `ORBIT_WARN_FREE_GB` を下回ると警告し、`ORBIT_STOP_FREE_GB` を下回ると VM を
    正常に停止します。
- `healthcheck.py` は、インストールされている場合、Docker、ポート、bind mount、inotify、
  時刻、ディスク容量を確認し、VMM のメモリ、スレッド数、fd 数を記録します。また 1 日に
  1 回 `releases/latest` ページから最新リリースを読み取り、新しいリリースごとに 1 回通知を
  表示します。
- `orbit` はインストールされるコマンドです。`up` と `down` は `vmm-up.sh` を、`update` と
  `rollback` は `update.sh` を実行し、`status` は状態を報告し、`uninstall` はエージェント、
  ヘルスチェックの作業ディレクトリ、`~/.orbit` を削除します。Makefile の実行ターゲットは
  これを呼び出すため、チェックアウト内のものは何も実行されません。
- `update.sh`（`orbit update`、`orbit rollback`）は、`~/.orbit/bin/VERSION` に記録された
  リポジトリの専用クローン（`~/.orbit/src`）でリリースタグをビルドし、新しいゲストをステージングディレクトリでビルドします。ロールバック
  用に以前のバイナリとゲストを保持し、コンテナが 1 つも動いていないときにだけ VM を再起動します。
