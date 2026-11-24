---
id: VER-10
type: verification
name: arb-tray（メニューバー／システムトレイクライアント）検証結果
verifies:
  - SPEC-01
  - SPEC-03
  - REQ-04
  - REQ-09
---

# VER-10: `arb-tray`（メニューバー／システムトレイクライアント）検証結果

記録日: 2026-11-23（macOS arm64 / Rust workspace `core/`）

## 1. 実装対象と構成

`core/crates/arb-tray` に、`arbd` とのみローカル IPC（JSON-RPC 2.0 over `arb_os::ipc::LocalIpcStream`）で通信する軽量なクロス OS メニューバー／システムトレイクライアントを実装した。`arb-cli` と同じ依存方針（`arb-core` + `arb-os` のみに依存し、`arbd` 内部実装には依存しない: `SPEC-01`）を踏襲し、`arbd` の JSON-RPC レスポンスを `serde_json::Value` として疎結合に解釈する。

| コンポーネント | 役割・主な実装内容 |
| --- | --- |
| `core/crates/arb-tray` (lib + bin 分離: `src/lib.rs`, `src/main.rs`) | ライブラリクレートとバイナリを分離し、結合テスト（`tests/ipc_integration.rs`）から実 IPC 層を直接駆動できるようにした（`arb-cli` にはこの分離は不要だったため新規導入）。 |
| `src/rpc.rs` | `arb_os::ipc::LocalIpcStream` 経由の永続 JSON-RPC 2.0 クライアント `RpcClient`（`arb-cli/src/rpc.rs` と同一パターン）。`--auto-start` 時の sibling `arbd` 自動起動（`spawn_daemon_process`）と `system.hello` 到達待ち（`wait_for_daemon`）を実装。 |
| `src/controller.rs` | 純粋・単体テスト可能なドメインロジック。`TrayController`（`system.status` / `device.list` / `save.list_jobs` / `bundle.list` を設定可能な間隔でポーリングし、切断時は次回ポーリングで自動再接続）、`TrayViewState`（`TrayBadge`: `Idle` / `Capturing` / `Disconnected` / `StoragePressure` / `SaveAttentionNeeded` / `DaemonOffline` の優先順位付き導出）、`build_menu`（ステータス行・Save Last 30s/60s・Failed Save Jobs サブメニュー（Retry/Discard）・Recent Bundles（v2/Legacy(RO) バッジ + Trash/Restore + フォルダ表示）・Start/Stop Capture・Quit Tray（`arbd` は起動継続）/ Stop Daemon & Quit）、`diff_alerts`（遷移時のみ通知を発する純粋な状態差分、スパムなし）。 |
| `src/native_tray.rs`（`native-tray` フィーチャー、デフォルト ON） | `tray-icon` + `muda`（Tauri プロジェクト、MIT OR Apache-2.0）によるネイティブ OS 統合。macOS/Windows は `tao` で OS イベントループを駆動し、Linux/BSD は `ksni`（pure-Rust D-Bus StatusNotifierItem, Unlicense）フィーチャーを使い GTK に一切依存しない。 |
| `--headless-smoke` | 1 回だけ `system.status` 等を取得・メニューを構築し JSON サマリを標準出力して終了するモード。GUI/ディスプレイのない CI・Devin 環境での自動検証に使用。 |

## 2. 依存関係・ライセンス監査

ネイティブトレイ UI の追加により新規に取り込まれた直接・推移依存はすべて MIT / Apache-2.0 / Unlicense / BSD 系の許容ライセンスであり、GPL・AGPL は **0 件**。

- 直接依存として `tray-icon`（`default-features = false, features = ["ksni"]`）と、macOS/Windows のみをターゲットとする `[target.'cfg(any(target_os = "macos", target_os = "windows"))'.dependencies]` 配下の `tao` を追加した。Linux では `tao`（および GTK3）はビルドグラフに一切現れない。
- `tray-icon` の `ksni` フィーチャーは pure-Rust の D-Bus `StatusNotifierItem` 実装（`zbus` 経由）であり、独自ワーカースレッドを持つため OS イベントループが不要。
- `python3 core/scripts/check-licenses.py --manifest-path core/Cargo.toml` を実行し、ワークスペース全体（`arb-tray` の新規依存を含む）で **GPL/AGPL 違反 0 件** を確認した（実測値は本書末尾の「4. 品質ゲート」参照）。

## 3. テスト

### 3.1 単体テスト（`cargo test -p arb-tray`、`src/lib.rs` 側 24 件）

`TrayViewState` の全バッジ優先順位（`derive_badge_precedence_*`）、ステータス行の文言生成、`diff_alerts` による全状態遷移（接続断/復旧・ストレージ逼迫開始/解除・録画開始/停止・保存失敗からの再試行完了・初回観測時のスパム抑止）、`build_menu` の構造（オフライン時の操作無効化・トグルラベル・Failed Save Jobs サブメニューの有無・Recent Bundles の Trash/Restore/Reveal 分岐）、`TrayMenuEvent::to_rpc` の RPC メソッドマッピング、実際の `arbd` レスポンス形状（`arbd/src/protocol.rs` 準拠）を用いた `build_view_state` の結合的パース検証を網羅する。

### 3.2 CLI 引数テスト（`src/main.rs` 側 3 件）

`--data-dir` / `--auto-start` / `--headless-smoke` / `--poll-interval-ms` の複合解析、未知オプションの拒否、`--help` 認識を検証。

### 3.3 実 IPC 結合テスト（`tests/ipc_integration.rs`、1 件）

`arbd` クレートへの循環依存を避けるため、`arbd` 本体ではなく `arb_os::ipc::LocalIpcListener` を直接用いた手製モック JSON-RPC 2.0 サーバーを起動し、実ソケット経由で `TrayController` を駆動する。以下を単一テストで実証:

1. `poll_once()` が `system.status` / `device.list` / `save.list_jobs` / `bundle.list` を実 IPC 経由で取得し、`CaptureStatus::Capturing` および `FailedRetryable` な保存ジョブを正しく導出すること。
2. `dispatch(&TrayMenuEvent::SaveLast30)` が `save.trigger`（`{"duration_sec": 30}`）を実際に送信すること。
3. `dispatch(&TrayMenuEvent::RetryJob { job_id: "job-1" })` が `save.retry`（`{"job_id": "job-1"}`）を実際に送信すること。
4. サーバー側がコネクションを意図的に切断（socket drop）した直後の `poll_once()` が、パニックやハングなく `daemon_online: false` へ安全に遷移し `DaemonOffline` アラートを一度だけ報告すること。
5. 続く `poll_once()` が新しい接続へ自動的に再接続し、再試行済みジョブが `Completed` へ遷移したことを観測し、`DaemonOnline` / `CaptureStarted` / `SaveCompleted` の 3 アラートを正しく報告すること。
6. サーバーが観測した RPC メソッド列が、ポーリング + ディスパッチの契約どおりの順序（`system.status, device.list, save.list_jobs, bundle.list, save.trigger, save.retry, system.status, device.list, save.list_jobs, bundle.list`）であること。

## 4. 品質ゲート（`./core/scripts/verify.sh`）

全 6 ゲート PASS（終了コード 0）:

1. `cargo fmt --all -- --check`: PASS
2. `cargo clippy --workspace --all-targets -- -D warnings`: PASS（警告 0 件。`arb-tray` は `clippy.toml` の `max-fn-params-bools = 0`（`bool` 引数を一切許容しない）および `struct_excessive_bools` を満たすため、`capture_active`/`capture_connected` の 2 つの `bool` を単一の `CaptureStatus`（`Idle`/`Capturing`/`Disconnected`）列挙型に、`derive_badge` の `storage_pressure_stopped: bool` 引数を `StoragePressureState`（`Normal`/`Pressured`）列挙型に置き換えた）。
3. `cargo test --workspace`: PASS（全ワークスペース **192 テスト**合格。内訳: `arb-adb` 41 / `arb-cli` 2 / `arb-core` 35 / `arb-media` 22 / `arb-os` 25 / `arb-storage` 24 / **`arb-tray` 28**（lib 単体 24 + bin CLI 引数 3 + 実 IPC 結合テスト 1）/ `arbd` ユニット 2 + 結合 E2E 13）。
4. `python3 core/scripts/check-licenses.py`: PASS（**329 packages checked, 0 GPL/AGPL violations**。`arb-tray` が新規に追加した `tray-icon` / `muda` / `tao` / `ksni` / `zbus` 他の推移依存をすべて含む）。
5. `sara check --strict`: PASS（27 items / 93 relationships: Requirement 14, Specification 4, Verification 9 — 本書 VER-10 を含む）。
6. REQ → SPEC 被覆検査: OK（14/14 REQ items satisfied by downstream SPEC）。

## 5. 未検証事項

- macOS / Windows / Linux 実機でのネイティブメニューバー／トレイ GUI の目視確認（アイコンバッジの見た目、メニュー項目のクリック動作、OS 通知の表示）は本環境（macOS arm64・ヘッドレス CI 相当）では未実施。`--headless-smoke` による IPC・状態モデル・メニュー構築ロジックの自動検証と、`tests/ipc_integration.rs` による実 IPC 結合検証で代替しており、残りの視覚的確認は Devin Multi-OS テスト計画（macOS/Windows/Linux の実 GUI 起動・スクリーンショット照合）で実施する。
- Linux の `ksni`（D-Bus `StatusNotifierItem`）バックエンドは、対応するデスクトップ環境（GNOME Shell 拡張や KDE Plasma 等の StatusNotifierItem 実装）が存在する環境でのみ実際にアイコンが表示される。本環境には D-Bus セッションバスも対応シェル拡張もないため、Linux 上でのトレイアイコン表示そのものは未検証（`cargo build` / `cargo clippy` のクロスコンパイル可否とライセンス監査のみ本書で検証済み）。
- Android Studio / IntelliJ IDEA プラグイン UI との結合（`REQ-09` が要求する「同一 IPC 契約での統一」の UI 側の見た目的な一貫性）は Phase 5（W8 以降）の統合作業で別途検証する。
