---
id: VER-21
type: verification
name: T-6C オーナー確定最終 v1 パリティ（logs_only 常時録画・フォアグラウンドアプリ別ログ抽出・厳格マルチ端末自動切替禁止＆Wi-Fi/ミラーリング耐性・screenshot_dir_override・T-6A ハードニング補完）検証記録
verifies:
  - SPEC-01
  - SPEC-02
  - SPEC-03
  - SPEC-04
  - REQ-01
  - REQ-02
  - REQ-04
  - REQ-06
  - REQ-07
  - REQ-08
  - REQ-09
  - REQ-10
  - REQ-11
  - REQ-12
---

# VER-21: T-6C オーナー確定最終 v1 パリティ検証記録

記録日: 2026-11-24（macOS arm64 / Rust workspace `core/` / Gradle 9.8.1 / Kotlin 2.4.21 / JDK 27）

## 1. T-6C 実装サマリ（5 項目）

1. **低負荷 `logs_only` 常時録画モード (`CaptureMode::VideoAndLogs | CaptureMode::LogsOnly`)**
   - `arb-core` `ArbConfig`（後方互換 serde デフォルト `video_and_logs`）、`arbd` `capture.start` / `system.status`、`arb-cli config set --capture-mode` / `capture start --mode` / `capture-start --mode`、`arb-cli mcp`、`arb-tray` ステータス表示（`REC (ログのみ)` / `待機中 (ログのみ)`、絵文字ゼロ）、および `ide-plugin/client-core` モデルに実装。
   - `CaptureMode::LogsOnly` 時は `arbd` が `scrcpy-server` の配備・起動を完全スキップ（端末側 MediaCodec 負荷ゼロ）し、軽量 `logcat` ＋クロックプローブ＋ `am_crash` 検知＋画面遷移解析のみを実行。`save.trigger`（およびクラッシュ自動保存）は自動的に `SaveMode::LogsOnly` へ変換し、エラーなく整合性検証済みの WORM `logs_only` バンドルを書き出す。

2. **フォアグラウンド操作アプリ・クラッシュアプリ別ログ自動抽出（`app-logs/<package>.jsonl` ＋ フィルタ RPC / MCP）**
   - PC 側処理のみで `timeline.screens`（`android` / `com.android.systemui` / ランチャー類を除外）および `timeline.crashes` から対象パッケージを特定し、軽量 `cmd package list packages -U`（`AdbClient::query_package_uids`）と PID/イベント解決器（`LogPackageResolver`）でログ行を帰属判定。
   - WORM バンドル内に `app-logs/<sanitized_package>.jsonl` を自動生成し、`manifest.json` の `app_logs: Vec<AppLogSummary { package_name, relative_path, line_count, crash_count, sha256 }>` で SHA-256 整合性検証（バックグラウンドの `com.google.android.gms` 等のノイズファイルは生成しない）。
   - `timeline.query_logs(package=...)`、`timeline.summary`、`timeline.overview`、MCP `arb_query_logs(package=...)` / `arb_timeline_overview`、および `ide-plugin/client-core`（`AppLogSummary` / `ArbdRpcClient.timelineQueryLogs(packageName=...)` / `TimelineViewerState.appLogs`）へ統一反映。

3. **厳格マルチ端末「自動切替禁止」ルール ＋ 4 経路対応（USB / Wi-Fi ADB / Running Devices ミラーリング / エミュレータ）**
   - Rust (`arb_core::device::DeviceSelectionPolicy`) と Kotlin (`io.github.shinma06.arb.state.DeviceSelectionPolicy`) の両方で共通ポリシーを強制：
     - `preferred_serial` 未設定かつオンライン 1 台 → 自動選択。
     - `preferred_serial` 未設定かつオンライン 2 台以上 → `MultipleDevicesNeedSelection`（決して自動選択・自動録画開始しない）。
     - `preferred_serial` 設定済みで切断された場合 → 他端末がオンラインでも決して別端末へ切り替えず `WaitingForPreferredDevice` で再接続待機。
   - Wi-Fi ADB シリアル検証（`192.168.1.50:5555`、`[fe80::1]:5555`、`adb-R3CN1234-AbCdEf._adb-tls-connect._tcp` を許可し、空白・制御文字・シェルメタ文字・`..`・先頭 `-` を拒否）を `arb-core` / `arbd` / `arb-cli` / `client-core` へ統一。
   - Running Devices ミラーリング HW エンコーダ競合の誠実な診断：`arbd/src/live_adb.rs` で `scrcpy-server` の `stderr`（8 KiB 上限バッファ）を収集し、`MediaCodec` / `Configure failed` / `RESOURCE_EXHAUSTED` / `ERROR_INSUFFICIENT_RESOURCES` 等を検出した場合に Running Devices ミラーリングとの H.264 HW エンコーダ競合の可能性および対処（`video_max_size=1024` または `logs_only` モード）を明示。
   - `docs/v2/testing/work-device-smoke-checklist.md` に Running Devices 同時録画・Wi-Fi ADB・複数端末自動切替禁止の 30 秒実機確認項目を追記。

4. **`screenshot_dir_override` ＆ `save.screenshot(output_dir)`**
   - `ArbConfig.screenshot_dir_override`（絶対パス・`..` 拒否・`data_dir` 内部状態ファイル上書き拒否）および `save.screenshot` の `output_dir` パラメータ（`output_path` と排他）を追加。保存先解決順：`output_path` → `output_dir` → `screenshot_dir_override` → `output_dir_override` → `<data_dir>/screenshots`。
   - `arb-cli config set --screenshot-dir-override` / `screenshot --out-dir`、MCP `arb_config_update` / `arb_screenshot`、`ide-plugin/client-core`（`ArbConfig` / `ArbConfigPatch` / `SaveScreenshotParams` / `ArbSettingsDraft`）まで貫通。

5. **T-6A フォローアップ・ハードニング（2 件）**
   - `arb-adb` `parse_wm_size_output`：`Override size: WxH` がある場合は `Physical size: WxH` より優先して返すよう改善。
   - `arbd` `system.hello`：`build_id: Option<String>` と `binary_mtime_secs: Option<u64>` を後方互換で追加し、`ide-plugin/client-core` `ArbdProcessManager.shouldAutoUpgradeBinary` が同一パス上書き更新時も安全に自動アップグレード判定可能に。

## 2. ローカル品質ゲート検証結果

- **Rust ワークスペース（`./core/scripts/verify.sh` ＋ `cargo doc`）**:
  - `[1/6] cargo fmt --check`: PASS
  - `[2/6] cargo clippy --workspace --all-targets -- -D warnings`: PASS（警告 0 件）
  - `[3/6] cargo test --workspace`: **350 passed, 0 failed, 3 ignored**（新規: `device.rs` 3 件、`config.rs` / `bundle.rs` / `analytics.rs` 各 1 件、`arb-adb` `wm size` Override ＆ `query_package_uids`、`live_adb.rs` HW エンコーダ競合分類、`arb-cli` T-6C フラグ解析、`arb-tray` `logs_only` 表示＆マルチ端末ガード、`arbd` 実デーモン E2E `t6c_final_parity_e2e` 2 件、`arb-cli` MCP stdio `app_logs` ＋ `package` フィルタ検証）
  - `[4/6] check-licenses.py`: PASS（**GPL / AGPL 違反 0 件**・新規外部クレート追加 0 件）
  - `[5/6] sara check --strict`: PASS（本書 `VER-21` 含む）
  - `[6/6] REQ → SPEC カバレッジ`: PASS（14/14）
  - `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`: PASS（警告 0 件）
- **Kotlin `ide-plugin:client-core`（`./gradlew :client-core:test --no-daemon`）**:
  - **75 passed, 0 failed**（新規: `DeviceSelectionPolicyTest` 5 件 — 厳格マルチ端末自動切替禁止、`CaptureBarReducer` 状態遷移＋`logs_only`/エンコーダ警告バナー、USB/Wi-Fi IPv4/IPv6/mDNS シリアル検証、T-6C JSON-RPC/Config/AppLogSummary/Hello `build_id` 往復、同一パスバイナリ mtime 自動アップグレード判定）
  - `ide-plugin/src/**` / `ide-plugin/build.gradle.kts` / `plugin/**` / `.github/**` への変更ゼロ（並列 T-6B とのファイル所有境界を厳守）。

## 3. 明示的な未検証項目（Unverified）

- **物理 Android 実機での Running Devices ミラーリング同時録画・Wi-Fi ADB 実機接続**: 実機固有のベンダー別 H.264 HW エンコーダ上限や職場ネットワークでの mDNS TLS 接続は、単体・実 `arbd` E2E・モック検証済みとし、実機確認は `docs/v2/testing/work-device-smoke-checklist.md`（項目 7・8）にて後日実施する。
- **Linux / Windows ホストでの実ランタイム動作**: ローカル検証は macOS arm64 で実施。Linux / Windows 3-OS パリティは GitHub Actions `v2 CI`（全 7 ジョブ）で確認する。
