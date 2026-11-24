---
id: SPEC-01
type: spec
name: 全体アーキテクチャ・クレート DAG・ライセンスと配布境界
satisfies:
  - REQ-05
  - REQ-06
  - REQ-08
  - REQ-09
---

# SPEC-01: 全体アーキテクチャ・クレート DAG・ライセンスと配布境界

## 1. 全体トポロジと単一書き込み所有者

- PC 側 Core Engine は `core/` 配下の Rust ワークスペース（`edition = "2024"`, `resolver = "2"`, `license = "MIT OR Apache-2.0"`）として構築する。
- **`arbd`（ローカル常駐デーモン）** がライブリング（Zone 1）、WORM 保存バンドル（Zone 2）、SQLite WAL カタログ・Lease・物理 GC（Zone 3）の **単一書き込み所有者** となる。
- Android Studio / IntelliJ IDEA プラグイン（薄い Kotlin UI シェル）、`arb-cli`、`arb-tray` はすべて `arbd` のローカル IPC（JSON-RPC 2.0 + 共有メモリフレームリング）クライアントとして同一基盤を共有し、業務ロジックを重複実装しない。
- `arbd` はユーザー単位のロック＋ソケット／Named Pipe で単一インスタンスを保証し、クライアント接続時に必要なら Lazy Spawn し `system.hello` で `protocol_version` / `binary_hash` を照合する。

## 2. 厳格なクレート依存 DAG

```text
arb-core   (純粋ドメイン：OS/ADB/SQLite/IPC/Media 依存ゼロ)
  ▲
  ├── arb-os      (Linux/macOS/Windows 二重時計・fs・IPC・SHM・パス)
  ├── arb-media   (ISO-BMFF・Codec バックエンド・トリム・GOP-LRU。OS/DB/ADB 非依存)
  ├── arb-adb     (ADB 通信・Device Agent ライフサイクル・フレーミング)
  └── arb-storage (Zone 1〜3 永続化・SQLite WAL・V2/Legacy BundleReader)
        ▲
       arbd       (上記 5 クレートを統合する単一書き込みデーモン)
        ▲
  [Local IPC 境界：UDS / Named Pipe + 共有メモリ]
        ▲
  arb-cli / arb-tray / IDE プラグイン (arbd 内部実装に依存しない)
```

## 3. ライセンスおよび配布ポリシー

- プロジェクト本体は `MIT OR Apache-2.0`。
- 許可ライセンスは `MIT` / `Apache-2.0` / `BSD-2-Clause` / `BSD-3-Clause` / `ISC` / `Zlib` / `CC0-1.0` / `Unlicense` / `0BSD` / `Unicode-3.0` 等および OS 標準フレームワークに限定する。
- **GPL / AGPL は静的・動的を問わず禁止** とし、`core/scripts/check-licenses.py`（`cargo metadata --format-version 1`）で自動検査する。許容的 OR 条項（例: `MIT OR Apache-2.0 OR LGPL-2.1-or-later`）を選択する場合のみ明示ログを出して許可する。
- 有料の Apple 公証や Windows コード署名証明書は導入しない。GitHub Actions で `macos-arm64` / `macos-x64` / `linux-x64` / `windows-x64` 成果物とプラグイン ZIP を生成し、OS 初回許可手順をドキュメント化する。

## 4. Entire チェックポイント運用境界

- 本リポジトリは `.entire/settings.json` によりチェックポイントを専用の非公開リモート（`shinma06/entire-checkpoints`）へ同期する構成となっている。
- チェックポイントや録画・生ログ・診断データを `origin` へ決してプッシュしない。
