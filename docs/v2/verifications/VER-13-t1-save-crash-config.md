---
id: VER-13
type: verification
name: Core v1 Parity A（永続設定・スクリーンショット・ログのみ保存・am_crash 自動保存＆マーカー・画面遷移スパン）検証記録
verifies:
  - SPEC-02
  - SPEC-03
  - REQ-01
  - REQ-02
  - REQ-04
  - REQ-09
  - REQ-10
---

# VER-13: T-1 Core v1 Parity A（永続設定・スクリーンショット・ログのみ保存・`am_crash` 自動保存＆マーカー・画面遷移スパン）検証記録

記録日: 2026-11-23

## 1. 実装・検証範囲（`arb-core` / `arb-storage` / `arbd` / `arb-cli`）

`arb-media` / `ide-plugin` / `plugin` / `.github` には一切触れず、既存 API・既存 WORM v2 バンドル・旧形式読取互換を維持したまま、新規クレート依存ゼロ・GPL/AGPL 非依存で以下 5 機能を実装した。

1. **永続ユーザー設定（`arb-core::config` + `arb-storage::config::ConfigStore` + `config.get` / `config.update` RPC + `arb-cli config`）**:
   - スキーマバージョン付き `ArbConfig`（`config_version = 1`、`save_window_secs: 5..=600` (default `300`)、`ring_budget_bytes` (default `1_610_612_736` = 1.5 GiB)、`min_free_disk_bytes` (default `3_221_225_472` = 3 GiB)、`video_max_size` (default `1920`)、`video_max_fps` (default `30`)、`audio_enabled` (default `true`)、`crash_autosave_enabled` (default `true`)、`crash_autosave_cooldown_secs` (default `15`)、`output_dir_override: Option<PathBuf>`、`ide_default_save_action: SaveAndOpen | SaveOnly`）。
   - `ConfigStore` が `<data_dir>/config.json` を兄弟一時ファイル書出 → `fsync_file` → 原子的 `rename` → `fsync_dir` で永続化。不在時はデフォルトにフォールバックし、範囲違反・相対パス・`arbd` プライベートディレクトリ指定は `RPC_ERR_INVALID_PARAMS` (`-32602`) で拒否して既存ファイルを無傷保護。更新時は稼働中 `RingStore` ポリシーへ即座に反映。
2. **ワンクリック現在画面スクリーンショット（`arbd::screenshot` + `save.screenshot` RPC + `arb-cli screenshot`）**:
   - 注入可能な `ScreenshotProvider`（本番 `AdbScreenshotProvider` は `adb -s <serial> exec-out screencap -p` をタイムアウト・64 MiB 上限付きで実行、テストは決定的モック PNG を注入）。
   - 純 Rust `validate_png_bytes` で PNG シグネチャ `\x89PNG\r\n\x1a\n`・`IHDR` 寸法・`IEND` 終端を検証し、原子的書き出し（デフォルトは非上書き保護 `Conflict`、明示 `overwrite: true` 時のみ原子的置換）と SHA-256 算出を実行し `{ path, width, height, sha256, captured_at_unix_ms, serial }` を返却。
3. **ログのみ保存（`BundleKind::LogsOnly` + `save.logs` RPC + `save.trigger` `mode = "logs_only"` + `arb-cli save-logs`）**:
   - 映像録画中であってもキャプチャを停止せず・`video.mp4` / `audio.m4a` を一切生成せず、`timeline.sqlite` + `logs.jsonl` + `unanchored.jsonl` + SHA-256 付きの正規 WORM v2 バンドル（`bundle_kind = "logs_only"`、`video.status = "absent"`、`audio.status = "absent"`）を原子的に書き出す。
   - REQ-01（実時間ウィンドウ）・REQ-02（`unanchored` 隔離）・REQ-04（非破壊 Trash / Restore / Lease 保護 / GC）をフルバンドルと完全同一に適用。
4. **`am_crash` クラッシュ検知・構造化マーカー・デバウンス自動保存（`arb-core::analytics` + `timeline.crashes` RPC + `arbd` 背景ワーカー）**:
   - バイナリイベントログ（tag `30039`、8 フィールド API 28–33 および 9 フィールド API 34+）とテキスト `am_crash: [...]`（メッセージ内カンマ許容、API 34+ `recoverable != 0` の非致命イベント除外）および `AndroidRuntime: FATAL EXCEPTION:` フォールバック（単一複数行・連続レコード双方＋ 5 秒重複排除）を純 Rust で決定的に解析。
   - `timeline.sqlite` に `crashes` テーブルとして永続化し（旧 v2 / レガシーバンドルは読取時に決定的に導出）、`timeline.crashes` RPC で `Vec<CrashMarker>` を返却。
   - ライブキャプチャ中に致命的 `am_crash` を受信すると、`CrashAutoSaveGate` による `crash_autosave_cooldown_secs` デバウンスと `STORAGE_PRESSURE_STOP` 厳格抑止を経て、背景スレッドがロック非保持で WORM 保存（`trigger = "crash_auto_save"`）を実行し、`system.status.crash_autosave` と `save.list_jobs` に状態を公開。
5. **PC 側 Activity / 画面遷移スパン抽出（`arb-core::analytics::extract_screen_spans` + `timeline.screens` RPC、デバイス負荷ゼロ）**:
   - バンドル保存済み logcat（`wm_on_resume_called` / `wm_resumed_activity` / `ActivityTaskManager: Displayed` / `ActivityManager: Displayed`、バイナリイベント `30022` / `30043` 復号含む）を純 Rust で解析し、連続重複遷移を畳み込んで `Vec<ScreenSpan { start_ns, end_ns, start_frame_index, end_frame_index, package_name, activity_name, short_name }>` を `timeline.screens` RPC で返却。

## 2. テスト・品質ゲート実行結果

ローカルで `./core/scripts/verify.sh`（全 6 ゲート）、`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`、および Windows ターゲット向け `cargo clippy --target x86_64-pc-windows-msvc` を実行して全合格を確認した（詳細はコミット時点の CI 実行ログおよび `t1_save_crash_config_e2e.rs` / 各クレート単体テストを参照）。

- 新規単体テスト:
  - `arb-core::config::tests`（3 件: デフォルト値＆往復検証、範囲外・相対パス拒否、パッチ適用＆`null` クリア）
  - `arb-core::analytics::tests`（5 件: 8/9 フィールドバイナリ＆カンマ入りテキスト `am_crash`、`FATAL EXCEPTION` フォールバック＆重複排除＆フレームヒント、`CrashAutoSaveGate` デバウンス＆ストレージ逼迫抑止、`wm_on_resume_called`/`Displayed`/`wm_resumed_activity` 画面スパン畳み込み、PNG シグネチャ/IHDR/IEND 検証）
  - `arb-storage::config::tests`（2 件: `ConfigStore` 原子保存・再読込・不正設定時の既存ファイル保護、`sha256_hex`）
  - `arb-storage::bundle_io::tests::logs_only_v2_bundle_writes_jsonl_sidecars_persists_crashes_and_verifies_sha256`（1 件: `logs_only` WORM 往復、`logs.jsonl`/`unanchored.jsonl` SHA-256 改ざん検知、`crashes` 永続化）
  - `arbd::screenshot::tests::atomic_screenshot_write_validates_png_and_protects_existing_files`（1 件: 原子書出・非上書き保護・明示上書き・不正 PNG 拒否）
  - `arb-cli::tests::t1_parses_screenshot_save_logs_and_config_commands`（1 件: `screenshot` / `save-logs` / `config get` / `config set` 引数パース・相対パス絶対化）
- 新規 `arbd` 実 IPC E2E テスト（`core/crates/arbd/tests/t1_save_crash_config_e2e.rs`、5 シナリオ全合格）:
  1. `t1_config_get_update_persistence_and_validation`
  2. `t1_save_screenshot_atomic_validation_and_overwrite_guard`
  3. `t1_save_logs_and_logs_only_trigger_keep_video_running_and_honor_worm_leases`
  4. `t1_crash_autosave_debounce_and_timeline_crashes_screens`
  5. `t1_crash_autosave_suppressed_under_storage_pressure_stop`

## 3. 明示的な未検証項目（Unverified）

- **Android 実機／エミュレータ上の実 `adb exec-out screencap -p` および実アプリクラッシュとの結合実行**: 本検証は実 `arbd` IPC サーバ＋決定的モック `ScreenshotProvider`・バイナリ `logcat -B` 相当 `am_crash` / `wm_*` イベントによる自動テストで確認済み。実端末上の実 `screencap`・実クラッシュ自動保存はローカル実機／エミュレータ受入フェーズで検証予定。
- **Windows / Linux 実機ランタイム実行**: ローカルは macOS arm64 環境（＋ `x86_64-pc-windows-msvc` クロス clippy）で検証し、Windows / Linux 実実行は GitHub Actions `v2 CI` マルチ OS マトリクスで検証する。
