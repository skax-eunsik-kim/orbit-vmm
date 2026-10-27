# orbit-vmm

[English](../../../README.md) | [한국어](../ko/README.md) | **日本語** | [简体中文](../zh-CN/README.md) | [Español](../es/README.md) | [Português (Brasil)](../pt-BR/README.md) | [Français](../fr/README.md) | [Deutsch](../de/README.md) | [हिन्दी](../hi/README.md)

> これは翻訳です。英語版の [README](../../../README.md) が正となる文書です。内容に食い違いがある場合は英語版に従ってください。

[![CI](https://github.com/skax-eunsik-kim/orbit-vmm/actions/workflows/ci.yml/badge.svg)](https://github.com/skax-eunsik-kim/orbit-vmm/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](../../../LICENSE)

Apple Silicon Mac 向けの、メモリを節約する Docker エンジンです。

orbit-vmm は、Apple の Hypervisor.framework 上に Rust で書かれた小さな仮想マシンモニター（VMM）です。
Docker を実行する Alpine Linux ゲストを起動し、Mac 上の
`docker` / `docker compose` CLI は Unix ソケット経由でそのゲストと通信します。

macOS 上の多くの Docker VM は、メモリ使用量がピークまで増えるとそのまま減りません。
orbit-vmm はメモリを返却します。ゲストは解放したページを報告し
（virtio-balloon の *free page reporting*）、VMM は数秒以内にそれらを macOS へ解放します。
さらに小さなゲストデーモンがアイドル状態のページキャッシュを破棄してメモリをコンパクション
（デフラグして、空きブロック全体を報告できるように）するため、解放されたメモリを実際に返却できます。

> **ステータス: 初期段階。** 実際の compose スタックを毎日動かしていますが、まだ若いプロジェクトで、
> メンテナーは 1 人だけ、インストーラーや GUI もありません。依存する前に
> [既知の制限](#既知の制限) をお読みください。

## 機能

- **メモリが macOS に戻ります。** 16 KiB 単位の free page reporting に加え、
  アイドル状態のページキャッシュを削減します。ワークロードが終了すると、Mac 上の VM のフットプリントは
  1 分以内に約 1 GB まで戻ります
  （[ベンチマーク](benchmarks.md)）。
- **virtio-fs によるバインドマウント。** `-v $PWD:/app` は Mac 上と同じパスで動作します。
  複数のリクエストキュー、属性キャッシュ、readdirplus を備えています。
  Mac での編集、新規ファイルやディレクトリの作成、リネーム、削除はコンテナ内で inotify
  イベントを発生させるため、`uvicorn --reload`、vite、nodemon のホットリロードは
  `git checkout` やコードジェネレーターを経ても動作し続けます。コンテナ内での `chown` や `chmod` も動作します。
  どちらも xattr でエミュレートされます。実ファイルはあなたの所有のままで、Mac 上で
  setuid になったり、グループやその他のユーザーから書き込み可能になったりすることはありません。
- **オプトイン方式の共有。** 許可したディレクトリだけが VM から見え、
  パスの走査はシンボリックリンクに対して安全です。Docker ソケットを持つコンテナでも
  `~/.ssh` は読めません。
- **Docker Desktop と同じように振る舞うネットワーク:**
  - 公開したポートは自動的に `127.0.0.1` に現れます。
  - `host.docker.internal` と `host-gateway` は Mac に解決されます。
  - コンテナは Mac が信頼するルート CA を信頼するため、`pip`、`npm`、`apk`、
    `curl` は TLS 検査プロキシの背後でも動作します。
- **amd64 イメージ** は qemu のユーザーモードエミュレーション（binfmt）で実行されます。
- `ORBIT_INSTANCE=<name>` で **複数の VM を並べて** 実行できます。
- **小さく、監査しやすい。** コード約 6.3k 行の Rust バイナリ 1 つです。依存するのは
  `libc`、`vm-fdt`、libslirp だけで、それ以外はありません。

## 動作要件

- Apple Silicon Mac、macOS 15 以降（カーネル内 GIC API に 15 以上が必要です）。
  これまでに検証したのは macOS 26.4 だけです。別のバージョンでは、先に下記のチェックを
  実行してください。
- Xcode Command Line Tools（`xcode-select --install`）と
  [Homebrew](https://brew.sh)。
- Rust 1.87+ と libslirp: `brew install rust libslirp`。
- compose と buildx プラグイン付きの Docker CLI:
  `brew install docker docker-compose docker-buildx`。Homebrew はプラグインを CLI が
  デフォルトでは探さない場所に置くため、`~/.docker/config.json` に追加してください:

  ```json
  { "cliPluginsExtraDirs": ["/opt/homebrew/lib/docker/cli-plugins"] }
  ```

  buildx がないと、`docker build` はレガシービルダーにフォールバックし、
  `make devflow` の BuildKit チェックが失敗します。
- 任意のヘルスチェックには Python 3.11+（`brew install python`）。
- ディスクの空き容量: Mac の空き容量が 3 GB 未満だと VM は起動せず、10 GB を下回ると
  警告します（`ORBIT_STOP_FREE_GB` / `ORBIT_WARN_FREE_GB`）。Docker のデータは
  スパースファイル `~/.orbit/vmm/data.img` に置かれ、pull やビルドに応じて大きくなります。

### orbit が未検証の macOS バージョンで使う場合

これまでに検証したのは macOS 26.4 です。Hypervisor.framework のメモリをどのように
macOS へ返却できるか、またアクティビティモニタがそれをどう数えるかは macOS 次第です。そのため、
新しいバージョンで利用する前に次の 3 点を確認してください。

1. **メモリの解放。** VM を実行していない状態で:

   ```bash
   vmm/target/release/orbit-vmm --reclaim-selftest
   ```

   `guest RAM from hv_vm_allocate` ブロック内の
   `unmap + mmap(MAP_FIXED anon) + map` 行が `released 1 MiB` 付近で終わっている必要があります。
   これは VMM が使う方式です（`vmm/src/reclaim.rs`）。値が
   約 257 MiB のままであれば、このバージョンでは解放されたゲストメモリが返却されません。つまり、VM は
   Mac 上で増える一方になります。
2. **開発者ワークフロー。** VM を起動した状態で、`make devflow` が
   0 fail（28 チェック）と報告する必要があります。
3. **アクティビティモニタ。** そのメモリ列は VMM の `phys_footprint` です。
   Mac のメモリが不足すると、macOS はゲスト RAM の一部を圧縮し、
   その数値はゲストの使用量を超えて増えます。テストでは、何も圧縮されていないときの差はほぼ
   ゼロで、メモリ逼迫時には圧縮量の約 3 分の 1 から 2 分の 1 でした。別の macOS
   バージョンで差が異なっていても、
   `ORBIT_MEMDUMP` + `vmm/bench/memxref.py` で VMM がゲストの使用量とほぼ同じ量を保持していると
   確認できる限り、それはリークではなく macOS の数え方の変化です（[docs/architecture.md](architecture.md) を参照）。

## クイックスタート

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

`DOCKER_HOST` の行をシェルのプロファイルに追加するか、コンテキストを作成してください:

```bash
docker context create orbit --docker host=unix://$HOME/.orbit/vmm/docker.sock
docker context use orbit
```

`orbit status` は、インストール済みのバージョンと VM が実行中かどうかを表示します。
`orbit up` は実行中の VM を再起動し、その際に VM 上のすべてのコンテナが停止します。

上記のように、orbit は **共有ディレクトリの外に** クローンしてください
（[ファイル共有](#ファイル共有) を参照）。コンテナは共有ディレクトリに書き込めるため、
その中にあるチェックアウトは知らないうちに変更される可能性があり、次の `make install` や
`make guest` がそのコードを Mac 上でビルド・実行してしまいます。チェックアウトが共有ディレクトリ内に
ある場合、`make install` は警告を出します。インストール後、`orbit` は `~/.orbit/bin` に
インストールされたコピーだけを実行し、チェックアウトは実行しません。`make up`、`make down`、
`make update`、`make rollback` はそれを呼び出すだけです。

## 設定

`scripts/vmm-up.sh`（`orbit up` が実行するもの）は次の環境変数を読み取ります:

| 変数 | デフォルト | 意味 |
|---|---|---|
| `ORBIT_MEM_MIB` | `8192` | ゲスト RAM の上限。解放されたメモリは macOS に戻るため、上限を高くしてもコストはわずかです。 |
| `ORBIT_CPUS` | Mac のコア数（`sysctl -n hw.ncpu`） | vCPU 数。アイドル状態の vCPU は待機し CPU を消費しません。数が少ないと、コンテナの並列起動（例: エージェントのサンドボックス）が CPU 待ちになります。 |
| `ORBIT_GUEST` | `~/.orbit/guest` | ゲストの `kernel` と `initramfs` を置くディレクトリ（例: `ORBIT_HOME=… scripts/fetch-guest.sh` でビルドしたもの）。 |
| `ORBIT_DISK_GB` | `64` | **新規** データディスク（スパースファイル）のサイズ。 |
| `ORBIT_DISK_WARN_MIB` | MiB 換算の `ORBIT_WARN_FREE_GB` | Mac の空き容量がこれを下回ると、VMM も `console.log` に警告を出します。 |
| `ORBIT_DISK_FLOOR_MIB` | `1024` | `ORBIT_STOP_FREE_GB` を下回った場合の最後の手段: これを下回ると、空き容量が確保されるまでゲストのディスク書き込みを保留します（[データディスク](#データディスク) を参照）。 |
| `ORBIT_DISK_PAUSE_SECS` | `600` | 保留された書き込みが失敗するまで待つ時間。 |
| `ORBIT_SHARE_ALLOW` | 下記参照 | コンテナがバインドマウントできるディレクトリ（コロン区切り）。 |
| `ORBIT_HOST_LOOPBACK` | `1` | `0` にすると、コンテナから Mac の `127.0.0.1`（および `host.docker.internal`）上のサービスに到達できなくなります。 |
| `ORBIT_WARN_FREE_GB` / `ORBIT_STOP_FREE_GB` | `10` / `3` | VM が警告を出す Mac ディスクの空き容量と、ディスクが満杯になる前に VM をクリーンにシャットダウンする空き容量。 |
| `ORBIT_INSTANCE` | （未設定） | `~/.orbit/vmm-<name>` 配下に専用のディスクとソケットを持つ別の VM を実行します。 |
| `ORBIT_PRESSURE` | （Mac に従う） | `off` にすると macOS のメモリ逼迫をゲストに伝えなくなります。`warn` または `critical` はレベルを固定します（計測用）。 |
| `ORBIT_FS_OWNERSHIP` | （エミュレート） | `host` にすると、バインドマウントしたファイルをエミュレートした値ではなく、Mac 上の実際の所有者とモードで表示します。 |
| `ORBIT_FS_TTL` / `ORBIT_FS_ATTR_TTL_MS` | `5` / `500` | コンテナがバインドマウントしたファイルの名前（秒）と属性（ms）をキャッシュする時間。小さくすると Mac での編集が早く見えますが、多少遅くなります。 |

`make guest`（`scripts/fetch-guest.sh`）は、`ALPINE_MIRROR`（デフォルト
`https://dl-cdn.alpinelinux.org/alpine`。sha256 による固定はそのまま適用されます）、
`ORBIT_HOST_CA=0`（[ルート証明書](#ルート証明書tls-検査プロキシ) を参照）、
`ORBIT_HOME`（`~/.orbit/guest` ではなく `<dir>/guest` にビルド）を読み取ります。

### ファイル共有

デフォルトでは、`~/work`、`~/src`、`~/code`、`~/dev`、
`~/projects`、`~/Developer` のうち存在するものが VM から見えます。`/Users` 内のそれ以外のものは
すべて見えません。ディレクトリを追加するには:

```bash
ORBIT_SHARE_ALLOW="$HOME/src:$HOME/clients" orbit up
```

共有ディレクトリは **すべてのコンテナから書き込み可能** です。orbit-vmm は、
ホームディレクトリ、それより上位のディレクトリ、隠しディレクトリ（`~/.ssh`、
`~/.orbit`、…）、`~/Library`、`~/bin`、`~/Applications` の共有を拒否します。別の共有ディレクトリの内側にあるディレクトリは、
外側のディレクトリですでにカバーされているため除外されます。コンテナは共有ディレクトリそのものを
リネームしたり削除したりできません。

- Mac でのファイルの編集は約 0.5 秒以内にコンテナから見えます。新規ファイル
  やリネームされたディレクトリは約 1 秒以内に現れます。
- Mac での編集や新規ファイルは、約 1 秒後にコンテナ内で `IN_MODIFY` / `IN_CLOSE_WRITE` を
  発生させます。

### ネットワーク

- `-p 8080:80` や compose の `ports:` は、約 1 秒以内に Mac の `127.0.0.1:8080` へ
  転送されます。Mac 側のポートが使用中の場合は転送が失敗し、
  エラーは `~/.orbit/vmm/console.log` に記録されます。
- コンテナは `host.docker.internal` 経由で Mac の localhost 上のサービスに到達できます。
  これには `127.0.0.1` でのみ待ち受けているもの（データベース、ノートブック、
  ローカルのモデルサーバー）も含まれます。それを望まない場合は `ORBIT_HOST_LOOPBACK=0` を設定してください。
- VM 内ですべてのアドレスで待ち受けているプロセスは、`-p` で公開したものに限らず、Mac の
  `127.0.0.1` 上で同じポートを得ます（`--network host` の場合と同様です）。

### ルート証明書（TLS 検査プロキシ）

- `make guest` は、Apple の組み込みセット以外に Mac が信頼しているルート証明書
  （SSL に対して「常に信頼」とマークされたもの。一般的には企業の
  TLS 検査用 CA）を読み取り、ゲストに組み込みます。そのようなルート証明書がない Mac では
  何も追加されず、コンテナにはイメージ自身のバンドルが見えます。
  リポジトリとリリースには証明書は含まれていません。
- コンテナ、ビルドステップ、Kubernetes の Pod（`orbit k8s`）には、マウントを通じて、
  それらのルート証明書がイメージの CA バンドルに追記された状態で見えます。バンドルのパスが
  シンボリックリンクになっている RHEL、Fedora、Amazon Linux のイメージも同様です。ビルドステップにはバンドルのディレクトリのコピーが渡されるため、
  `update-ca-certificates` や `apt-get upgrade` も動作します。どちらも
  Mac のルート証明書をイメージに書き込むことはありません。
- `ORBIT_HOST_CA=0 make guest && orbit up` で、それらを含まないゲストをビルドできます。
- 明示的なプロキシには対応していません。ゲストは Mac の HTTP(S) プロキシ設定や PAC
  ファイルを使わず、Docker の pull は Mac のネットワークから直接出ていきます。明示的なプロキシ
  経由の通信しか許可しないネットワークではまだ動作しません。透過的な TLS 検査プロキシなら動作します。
- その場合、ゲスト（`~/.orbit/guest`）とデータディスクには組織の
  CA が含まれます。組織の外部と共有しないでください。

## ファイル

| パス | 内容 |
|---|---|
| `~/.orbit/guest/` | `make guest` でビルドされたカーネルと initramfs。 |
| `~/.orbit/vmm/data.img` | データディスク（Docker イメージ、ボリューム）。 |
| `~/.orbit/vmm/docker.sock` | Docker API。 |
| `~/.orbit/vmm/console.log` | ゲストのコンソール。何かが失敗したときはまずここを確認してください（`console.prev.log` は前回の実行分です）。 |
| `~/.orbit/vmm/watchdog.log`, `crashes/` | スーパーバイザーのログと、予期しない停止の記録。 |

**`~/.orbit/guest/*` や `data.img` を共有しないでください。** これらには GPL バイナリと
マシンが信頼するルート証明書が含まれています
（[THIRD_PARTY_NOTICES.md](../../../THIRD_PARTY_NOTICES.md)）。

データディスクを拡張するには、VM を停止し、ファイルを拡大してから、VM を再び起動します。
ゲストは起動時にファイルシステムのサイズを変更します:

```bash
python3 -c "import os;p=os.path.expanduser('~/.orbit/vmm/data.img');os.truncate(p,max(os.path.getsize(p),128<<30))"
```

### データディスク

`data.img` はスパースファイルです。論理サイズを上限として、Docker が書き込んだ分だけ Mac 上の容量を
占有します（`vmm-up.sh` は両方を表示します）。気付かないうちに増えていくため、
2 つのガードが Mac の空き容量を監視しています。スーパーバイザー（`vmm-run.sh`）は
`ORBIT_WARN_FREE_GB` を下回ると通知し、`ORBIT_STOP_FREE_GB` を下回るとゲストをクリーンにシャットダウンします
（その状態では起動もしません）。それより下では、最後の手段として
VMM が 2 秒ごとにチェックします:

- `ORBIT_DISK_WARN_MIB` を下回ると、`console.log` に `orbit-vmm: disk space low` を記録し
  （低い状態が続く間は 10 分ごとに再度記録）、
  `scripts/healthcheck.py` が警告を報告します。
- `ORBIT_DISK_FLOOR_MIB` を下回るか、ディスクが満杯で書き込みが失敗すると、
  ゲストのディスク書き込みを保留し、`orbit-vmm: DISK FULL` を一度だけ記録します。
  Docker は停滞します（ゲストが hung-task 警告を記録することがあります）が、何も失敗しません。
  Mac の空き容量を確保すると、書き込みは中断した箇所から再開します
  （`disk writes resumed`）。
- `ORBIT_DISK_PAUSE_SECS` 以内に空き容量が確保されないと、書き込みは失敗します。すると ext4 は
  I/O エラーを報告し、読み取り専用になることがあります。その前に空き容量を確保し、
  読み取り専用になった場合は VM を再起動してください。

書き込みを失敗させずに保留するのは、QEMU のデフォルトの動作と同じです
（`werror=enospc`）。失敗した書き込みは ext4 がすでに保存を約束したデータであり、
それが失われることこそ、Mac のディスクが満杯になったときにゲストが破損する原因です。

## アップグレード

```bash
orbit update           # the latest release; --to v0.2.1 for a given one
orbit rollback         # back to what was installed before the last update
```

`orbit update` は、あなたのチェックアウトではなく、インストール時に記録したリポジトリの
専用クローン（`~/.orbit/src`）でリリースをビルドし、何かを置き換える前に新しいゲストをビルドします。
そのため、ダウンロードやビルドが失敗してもインストール済みのバージョンはそのまま残ります。
`orbit rollback` 用に以前のバイナリとゲストを保持します。VM を再起動するのは
実行中のコンテナがない場合だけです。それ以外の場合、VM は `orbit up` まで
古いバージョンのまま動作します（または `orbit update --now` を実行してください）。ゲストのアップグレード後の最初の起動では、
ゲストのパッケージが再インストールされます（約 1 分）。

ヘルスチェックがインストールされている場合、1 日に 1 回最新リリースを確認し、
新しいリリースごとに macOS の通知を 1 回表示します。`orbit update` を実行しない限り、
何もインストールされません。

リリースではなく `main` を追う場合は、チェックアウトで
`git pull && make guest install && orbit up` を実行します。

v0.2.1 以前をインストールしている場合は、チェックアウトから `make update` を一度実行してください。
これで `orbit` コマンドがインストールされます。

## アンインストール

```bash
orbit uninstall
```

すべての orbit VM を停止し、launchd エージェント（ウォッチドッグとヘルスチェック）、
共有ディレクトリ内のヘルスチェック用の作業ディレクトリ、`~/.orbit` を削除します。
**すべてのイメージ、コンテナ、ボリュームを含むデータディスクも削除されます。**
実行前に確認を求めます（`--yes` で省略）。チェックアウトと、作成していれば `orbit` という名前の
Docker コンテキストは残ります（`docker context rm orbit`）。

## ウォッチドッグ

`make install` は、VM を実行して復旧させる launchd エージェント `com.orbit.vmm` もインストールします。
これが存在するのは、他に何もないからです。orbit には常駐するアプリがないため、
これがなければ、終了したりハングしたりした VMM は停止したままになります。

```bash
ORBIT_WATCHDOG=0 make install          # install without it
./scripts/install-watchdog.sh --uninstall
./scripts/install-watchdog.sh --reload # apply a changed agent now (restarts the VM)
```

再インストールしても、実行中の VM には影響しません。

- **クラッシュ:** VMM が終了するか、ゲストが再起動またはパニックした場合、launchd が数秒以内に
  再び起動します。
- **ハング:** ゲストには 30 秒ごとに ping が送られます。ping に 3 回応答がないと VMM は
  abort され、macOS がクラッシュレポートを書き出したうえで再起動されます。これには
  約 90 秒かかります。
- **クラッシュループ:** 10 分以内に 3 回を超えて起動すると、再起動をやめて
  通知します。
- **クラッシュ記録:** 予期しない停止はそれぞれ
  `~/.orbit/vmm/crashes/<time>/` に保存されます（コンソールログ、macOS のクラッシュレポート、ウォッチドッグの
  ログ）。新しい順に 10 件が保持され、macOS の通知が届きます。コンテナは
  VM とともに停止するため、`restart:` ポリシーのないコンテナは再度
  起動する必要があります。
- **意図的な停止:** `orbit down`（`vmm-up.sh --stop`）とゲストの `poweroff` は
  意図した停止とみなされ、再起動されません。ログアウト時に実行中だった VM は
  ログイン時に再び起動します。

ウォッチドッグがない場合、`orbit up` は同じスーパーバイザーをバックグラウンドで実行します。
そのため、ハングの検出とクラッシュの記録は引き続き行われますが、何も
再起動されません。

## Kubernetes

```bash
brew install kind kubectl
orbit k8s up                  # a single-node cluster named orbit (about 45 s the first time)
kubectl --context kind-orbit get nodes
orbit k8s down                # delete it; its memory goes back to the Mac within ~90 s
```

`orbit k8s` は [kind](https://kind.sigs.k8s.io/) を実行します。クラスターは VM 内の
コンテナなので、起動するまでコストはかからず、ゲストにも何も追加されません。素の kind と
違うのは、ノードの containerd が orbit の CA ラッパーを通じて Pod を起動する点です。
そのため Pod もコンテナと同じように Mac の追加のルート証明書を信頼します（レジストリからの
pull、`curl`、Node.js）。`orbit k8s up <name> [kind flags]` で別のクラスターを作ったり、
`--image kindest/node:v1.33.0` のようなフラグを渡したりできます。`orbit k8s status` は
クラスターを一覧表示します。kind は kubeconfig に `kind-<name>` コンテキストを追加し、
それを現在のコンテキストにします。

- アイドル状態のクラスターは、クラスターなしの VM より Mac 上で 1.0–1.7 GB 多く
  使用しました（[ベンチマーク](benchmarks.md#kubernetes)）。
- サービスには `kubectl port-forward` でアクセスします。API サーバーは Mac の
  `127.0.0.1` にあります。ローカルでビルドしたイメージには
  `kind load docker-image <image> --name orbit` を使ってください。
- クラスターは `orbit up`（VM の再起動）後も残り、`orbit uninstall` で削除されます。

## ヘルスチェック（任意）

`make install` の後、`scripts/install-healthcheck.sh` を実行すると、インストール済みの
`~/.orbit/bin/healthcheck.py` を実行する launchd エージェントがインストールされます。これは Docker、
ポート、バインドマウント、inotify、ゲストの時計をチェックし、VMM のメモリ、スレッド数、
fd 数を `~/.orbit/vmm/health.jsonl` に記録します。

チェックは 1 時間ごとに実行されます。3 回連続で正常だった場合は
2 時間ごと、さらに 4 時間ごとへと間隔を延ばします。問題が見つかると 1 時間ごとに戻ります。削除するには
`--uninstall` を実行してください。

## 既知の制限

- **inotify:** Mac 側の変更がコンテナに届くのは約 1 秒後です。256 件を超えるエントリを持つ
  新しいディレクトリツリー（新しいワークツリー、Mac 上での
  `npm install`）では、最上位ディレクトリに対して `IN_CREATE` が 1 回だけ発生します。
  再帰的なウォッチャーはそこからスキャンします。リネームが対になるのは、ゲストが
  古い名前をすでに認識していた場合だけです。それ以外の場合は削除 + 作成として現れます。
- **ポート転送:** UDP の公開は転送されません。特定の
  ホスト IP での公開（`-p 127.0.0.1:8080:80`）は、他と同様に Mac の `127.0.0.1` に転送されます。
  そのような 2 つの IP で同じポートを公開した場合は、最も新しいものだけに到達します。
  2 つの VM が同じポートを公開した場合、2 つ目の転送は失敗します（ログに記録されます）。
- **バインドマウント:** Mac とコンテナの間でファイルロックは共有されません。
  バインドマウント上には Unix ソケットや FIFO を作成できません。
- **データディスク:** Docker が解放した容量が Mac に戻るのは、ゲストが
  trim を行ったとき（`fstrim`。VM が静かなときに 10 分ごと、および起動後に 1 回）だけです。
  そのため `docker system prune` の効果が Mac に現れるのは数分後です。イメージは、
  ゲストが書き込んだまま解放していない分よりも小さくなることはありません。
- **ルート証明書:** CA バンドルを別の場所にコピーするビルドステップ
  （`cp /etc/ssl/certs/ca-certificates.crt /app/`）は、Mac のルート証明書も一緒に
  イメージ内へコピーします。バンドルの再生成はビルドステップでは動作しますが、実行中のコンテナでは動作せず、
  同じ `RUN` 内の後続コマンドには Mac の
  ルート証明書が見えません。CA ディレクトリに何かがマウントされている場合（`RUN --mount`、`-v`）、
  そこにはルート証明書は追加されません。
- **amd64 イメージ** は Rosetta ではなく qemu のユーザーモードエミュレーションで実行されるため、
  arm64 イメージより明らかに遅くなります。
- **ゲストのパッケージ:** カーネルと Alpine のルートファイルシステムは sha256 で固定されていますが、
  Docker、qemu などのゲストパッケージは、ゲストをビルドするたびにその後の最初の起動で Alpine の
  リポジトリからインストールされるため、Alpine の更新に合わせて変わります。
- **Docker Desktop や OrbStack からの移行:** イメージ、コンテナ、ボリュームは移行されません。
  イメージは pull し直すか `docker save` / `docker load` で移し、ボリュームの内容は
  両方をマウントしたコンテナでコピーしてください。
- **Kubernetes:** kind（`orbit k8s`）経由のみで、デフォルトはシングルノードです。
  `LoadBalancer` サービスや、独自の kind 設定の `extraPortMappings` は自動では
  設定されません。
- **未対応のもの:** GUI はなく、署名済みのバイナリリリースもありません。
  自分でビルドしてください。

## 仕組み

[docs/architecture.md](architecture.md) を参照してください。概要は次のとおりです:

```
docker CLI ──unix socket── orbit-vmm ──vsock── guest: dockerd
                              │
          Hypervisor.framework (vCPUs, in-kernel GICv3)
          virtio-mmio: blk · net (libslirp) · vsock · fs · balloon (free page reporting)
```

## バグの報告

[issue](https://github.com/skax-eunsik-kim/orbit-vmm/issues) を作成し、実行したこと、
期待した結果、実際に起きたことを書いて、次を添付してください:

- `~/.orbit/vmm/console.log` と `~/.orbit/vmm/watchdog.log` の末尾
- `sw_vers -productVersion` と `orbit status`
- クラッシュの場合は、最新の `~/.orbit/vmm/crashes/<time>/` ディレクトリ

ログにはパスやイメージ名が含まれることがあるため、投稿前に確認してください。
`~/.orbit/guest`、`data.img`、`host-ca.pem` は決して添付しないでください。Mac が追加で
信頼しているルート証明書が含まれています。

## コントリビュート

バグ報告やプルリクエストを歓迎します。開発環境のセットアップとチェックについては
[CONTRIBUTING.md](../../../CONTRIBUTING.md) を参照してください。
セキュリティ上の問題は非公開で報告してください（[SECURITY.md](../../../SECURITY.md)）。

## ライセンス

Apache-2.0（[LICENSE](../../../LICENSE)）。ビルドするゲストイメージは Alpine
Linux のパッケージをダウンロードします。それらには独自のライセンスがあります。
[THIRD_PARTY_NOTICES.md](../../../THIRD_PARTY_NOTICES.md) を参照してください。

orbit-vmm は独立したプロジェクトです。Docker, Inc.、
OrbStack、Apple とは提携していません。
