# ベンチマーク

[English](../../benchmarks.md) | [한국어](../ko/benchmarks.md) | **日本語** | [简体中文](../zh-CN/benchmarks.md) | [Español](../es/benchmarks.md) | [Português (Brasil)](../pt-BR/benchmarks.md) | [Français](../fr/benchmarks.md) | [Deutsch](../de/benchmarks.md) | [हिन्दी](../hi/benchmarks.md)

> この文書は翻訳です。正となるのは英語版の [benchmarks.md](../../benchmarks.md) で、内容が異なる場合は英語版に従ってください。

すべての数値は、2026 年 10 月に M シリーズの MacBook 1 台（macOS 26）で `bench/` の
スクリプトを使って計測したものです。示しているのはおおよその桁であり、厳密な結果ではありません。
ご自身のマシンで比較するには、スクリプトを実際に実行してください。

## macOS に返却されるメモリ

`bench/memtest.sh` は、VM プロセスの `phys_footprint` を 4 つの時点で計測します。
アイドル時、約 3.5 GB（ページキャッシュと匿名メモリ）を書き込むコンテナの実行中、そして
コンテナ終了から 30 s 後と 90 s 後です。表にはアイドル時と 90 s 後を示します。

| エンジン | アイドル時 | 終了から 90 s 後 |
|---|---|---|
| orbit-vmm | 0.66 GB | 0.69 GB |
| OrbStack | 1.25 GB | 1.11 GB |
| Colima (QEMU/vz) | 1.59 GB | 3.61 GB |

`bench/memfrag.sh` は断片化に負荷をかけます。それぞれ 150 MiB を保持するコンテナを 24 個
起動し、1 つおきに解放してから、残りを解放します。

| ステップ | Mac のフットプリント | ゲストの使用量 |
|---|---|---|
| 開始時 | 849 MiB | 304 MiB |
| 24 × 150 MiB を保持 | 4854 MiB | 4165 MiB |
| 1 つおきに解放（+60 s） | 2917 MiB | 2206 MiB |
| すべて解放（+60 s） | 1043 MiB | 334 MiB |

## 開発ワークフロー

`bench/devflow/devflow.sh` は 26 項目のチェックを実行します。

- healthcheck 付きの compose up と、Mac からの公開ポート
- サービスの DNS と `host.docker.internal`
- 名前付きボリューム、bind mount の可視性と inotify、bind mount 上での postgres
  `initdb`、`chown`
- TLS プロキシ配下での HTTPS と amd64 イメージ
- BuildKit ビルド（`# syntax`、ghcr、apt/uv のキャッシュマウント）と alembic
  マイグレーション
- logs、exec、stats、時刻のずれ

現在の結果: 26/26 合格。

## 安定性

`bench/stress/multi.sh N` は 1 つの VM で N 個のワークツリースタックを同時に実行し、
あるワークツリーでの編集が別のワークツリーに決して届かないことを確認します。`bench/stress/soak.sh <hours>`
は、コンテナとファイルの頻繁な作成・削除とあわせてこれをループで繰り返し、VMM のスレッド数、
fd 数、メモリをサンプリングします。

4 h のソークテストは 295 ラウンド中 295 ラウンドに合格し、スレッド数と fd 数は横ばいのままでした。
