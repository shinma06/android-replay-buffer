---
id: VER-02
type: verification
name: arb-core 純粋ドメインモデル（時刻・3値選別・v2 スキーマ・状態機械・旧形式読取）検証記録
verifies:
  - SPEC-02
  - REQ-01
  - REQ-02
  - REQ-03
  - REQ-04
  - REQ-07
  - REQ-08
  - REQ-10
  - REQ-12
---

# VER-02: `arb-core` 純粋ドメインモデル検証記録

記録日: 2026-11-22

## 1. 実装・検証範囲（`core/crates/arb-core`）

OS / DB / ADB / メディアコーデック依存ゼロの純粋 Rust クレート `arb-core` に以下 5 モジュールを実装し、ユニットテスト・プロパティテスト（`proptest`）・実リポジトリフィクスチャ統合テストにより検証した。

1. **`arb_core::time`（二層時刻モデル・不連続検出エンジン）**:
   - 生観測（`DeviceClockTriple`, `PcSyncProbe`, `RawTimestampObservation`）を上書きせず保持し、同一 `boot_id` 内の `DeviceBoottime` 基準軸へ `TimeAlignment`（`Anchored` / `Bounded` / `Unanchored`）として写像。
   - `boot_id` 変化による `DeviceReboot` ハード境界（クロスブート演算・補間を型とエラーで禁止）、`DeviceSleep`、`DeviceWallJump`、`PcSleep`、`ClockSyncStale` を検出。
   - 壁時計巻き戻り／跳躍を跨ぐ logcat 行やディープスリープを跨ぐ単調時計 PTS では点時刻を捏造せず、`device_read_boottime` 上限付きの `Bounded` または `Unanchored` へ縮退。
2. **`arb_core::selection`（保存・トリム共通 3 値選別エンジン）**:
   - `classify_alignment` / `select_records` / `select_for_trim` / `WindowSpec::from_frame_indices` / `evaluate_stream_drain`。
   - 49 秒遅延到着バグ回帰テスト（古い `Anchored` レコードの確実な `OutOfRange` 除外）。
   - 137 秒 unanchored 暴走バグ回帰テスト（PC 受信近傍 `[ingest_start - margin, ingest_end + margin]` 制限、件数・バイト上限、`unanchored_truncated_count` 厳密計上）。
   - 保存↔全範囲トリムの同一性不変条件、および全件保存則を `proptest` で検証。
3. **`arb_core::bundle`（不変 WORM バンドル v2 スキーマとバリデータ）**:
   - `ManifestV2`・`Provenance`・`StreamsSummary`・`FrameRecord`・`LogRecord`・`TapRecord` の Serde 往復および `ManifestV2::validate`（`schema_version == 2`、必須項目、64 桁小文字 SHA-256、パス正規化、ストリーム矛盾検査）。
4. **`arb_core::state`（保存ジョブ・バンドル寿命・リング容量予算状態機械）**:
   - `SaveJobState` / `SaveJob`（`FailedRetryable` で Pin 済みチャンク ID を保持し `Retry` / `Retarget` / `Discard` 可能）。
   - `BundleLifecycleState::can_purge`（`Trashed && active_leases == 0 && active_jobs == 0` でのみ許可）。
   - `RingEvictionPlanner::plan`（空き容量下限割れで即座に `StopStoragePressure`、Pin 済み・書込中チャンクは絶対に Evict しないことを `proptest` で検証）。
5. **`arb_core::legacy`（Schema 3 / Schema 4 読取専用パーサ）**:
   - 実リポジトリフィクスチャ `plugin/src/test/resources/replay-trim-parity/replay-20240101-000000-manual-feedface/`（`replay.json`, `video-frames.jsonl`, `logcat-device.jsonl`, `logcat-app.jsonl`, `touches.jsonl`）の読取専用統合テスト。
   - 合成 Schema 3（`session.json`, `video-001.mp4`, `frames.jsonl`, `logcat-*.jsonl`）、Schema 4 ログ単体保存（`-logs` / `LOGS_ONLY`）、未知オプション JSON フィールド許容、文字列化 64bit 時刻および `Long.MAX_VALUE`（`9223372036854775807`）番兵値のデコードを検証。

## 2. ゲート実行結果（`./core/scripts/verify.sh`）

実行環境: macOS / `rustc 1.100.0` / `sara 0.12.3`

| ゲート | 結果 | 詳細 |
| --- | --- | --- |
| `[1/6] cargo fmt --check` | PASS | ワークスペース全ファイル差分なし |
| `[2/6] cargo clippy --workspace --all-targets -- -D warnings` | PASS | BeautifulRust 厳格 lint 含め警告ゼロ |
| `[3/6] cargo test --workspace` | PASS | 全 34 テスト合格（`arb-core`: ユニット 18 件 + `legacy_fixtures` 統合 3 件 + `selection_properties` プロパティ/回帰 6 件 = 計 27 件、他ワークスペーススモーク 7 件） |
| `[4/6] license check` | PASS | 63 パッケージ検証、GPL/AGPL 違反 0 件（`r-efi 6.0.0` は許容条項 `MIT` を選択） |
| `[5/6] sara check --strict` | PASS | 20 アイテム（REQ 14 / SPEC 4 / VER 2）、39 リレーション検証合格 |
| `[6/6] REQ -> downstream SPEC coverage` | PASS | 14/14 REQ が下流 SPEC に充足済み |
| `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p arb-core` | PASS | 公開 API ドキュメント警告・リンク切れゼロ |

## 3. 明示的な未検証項目（Unverified）

- 実際の OS 時計読み取り（`arb-os`）、SQLite 永続化・ファイルシステム原子リネーム（`arb-storage`）、Android 実機・エミュレータ上での連続キャプチャ統合は別ワークストリーム（W2 以降）の責務であり、本検証記録では純粋ドメイン論理のみを対象としている。
