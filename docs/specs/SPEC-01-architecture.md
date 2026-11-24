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
- **Android Studio を親とするライフサイクル**: IDE プラグインが起動する `arbd` と `arb-tray` は「Android Studio が生きていれば稼働し、終了していれば稼働しない」。
  - IDE は最初のプロジェクトを開いた時点で（Tool Window を開かなくても）`arbd --exit-on-stdin-eof` を起動し、子プロセスの標準入力パイプの書き込み端を保持し続ける。`trayEnabled` が ON のときは `arb-tray --exit-on-stdin-eof` も同様に起動する（`arbd` は IDE が所有するため `--auto-start` は付けない）。
  - IDE が終了・クラッシュ・強制終了（`kill -9`）すると OS がパイプを閉じ、子は標準入力の EOF を受け取る。`arbd` は `system.shutdown` と同じ経路（受付停止・キャプチャ停止・ソケットとロックの解放・終了コード 0）で、`arb-tray` は終了要求（`QuitNow`）で終了する。メニュー表示中などで 1.5 秒以内に終了できない `arb-tray` は自ら終了する。PID の監視はしないため PID 再利用の競合はない。挙動は Linux / macOS / Windows で同じ。
  - 通常の IDE 終了・プラグインのアンロードでは `ArbdDaemonService.dispose()` が `arb-tray`、`arbd` の順に停止する（標準入力を閉じ、`arbd` には `system.shutdown` も送る。残った場合は `destroy` / `destroyForcibly`）。複数プロジェクトのうち 1 つを閉じても止めない。トレイ表示を OFF にした場合は `arb-tray` だけを止める。
  - 稼働中の `arbd` が親を持たない（旧プラグイン・`arb-cli --auto-start`・手動起動）場合、IDE は同じ IPC 接続で保存と競合しない引継ぎを要求し、終了を待って親付きの `arbd` に置き換える。処理中は次の監視周期で再試行する。引継ぎ RPC のない旧版では、G-M9 の同梱バイナリ更新が検出された場合に限り、保存状況を確認して同一接続で通常終了する。それ以外は明示的に再起動するまで利用を続ける。別の生きている親（別の IDE）に紐付いた `arbd`（`system.hello` の `parent_bound == true`）はそのまま使う。IDE は約 10 秒ごとに `arbd` の稼働を確認し、止まっていれば親付きで起動し直す。
  - `--exit-on-stdin-eof` は既定で OFF。単体の `arb-cli` / MCP / `start-arb.sh` / `start-arb.ps1` / `arb-tray` の挙動は変わらない。このフラグを受け付けない古い `arbd`（設定画面の独自バイナリなど）は、引数の拒否を確認した起動だけフラグなしで管理し、通常終了時に回収する。

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
