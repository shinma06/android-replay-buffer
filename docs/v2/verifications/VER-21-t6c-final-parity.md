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
   - Wi-Fi ADB シリアル検証（`192.168.1.50:5555`、`[fe80::1]:5555`、`[fe80::1%wlan0]:5555`、`adb-R3CN1234-AbCdEf._adb-tls-connect._tcp` を許可し、空白・制御文字・シェルメタ文字・非 ASCII・`..`・先頭 `-`・括弧不整合・括弧外の `%` を拒否）を `arb-core` / `arbd` / `arb-cli` / `client-core` で 1:1 に統一（同一テストベクタで Rust / Kotlin 双方を検証）。
   - Running Devices ミラーリング HW エンコーダ競合の誠実な診断：`arbd/src/live_adb.rs` で `scrcpy-server` の `stderr`（8 KiB 上限バッファ）を収集し、`MediaCodec` / `Configure failed` / `RESOURCE_EXHAUSTED` / `ERROR_INSUFFICIENT_RESOURCES` 等を検出した場合に Running Devices ミラーリングとの H.264 HW エンコーダ競合の可能性および対処（`video_max_size=1024` または `logs_only` モード）を明示。
   - `docs/v2/testing/work-device-smoke-checklist.md` に Running Devices 同時録画・Wi-Fi ADB・複数端末自動切替禁止の 30 秒実機確認項目を追記。

4. **`screenshot_dir_override` ＆ `save.screenshot(output_dir)`**
   - `ArbConfig.screenshot_dir_override`（絶対パス・`..` 拒否・`data_dir` 内部状態ファイル上書き拒否）および `save.screenshot` の `output_dir` パラメータ（`output_path` と排他）を追加。保存先解決順：`output_path` → `output_dir` → `screenshot_dir_override` → `output_dir_override` → `<data_dir>/screenshots`。
   - `arb-cli config set --screenshot-dir-override` / `screenshot --out-dir`、MCP `arb_config_update` / `arb_screenshot`、`ide-plugin/client-core`（`ArbConfig` / `ArbConfigPatch` / `SaveScreenshotParams` / `ArbSettingsDraft`）まで貫通。

5. **T-6A フォローアップ・ハードニング（2 件）**
   - `arb-adb` `parse_wm_size_output`：`Physical size:` / `Override size:` の行順・不正行に依存せず解析。タップ写像に使う raw `getevent` 座標は物理パネル座標系のため `Physical size` を優先し、`Override size` は Physical が無効な場合のフォールバック（T-6A VER-19 の設計を維持。レビュー指摘で Override 優先への退行を修正）。
   - `arbd` `system.hello`：`build_id: Option<String>` と `binary_mtime_secs: Option<u64>` を後方互換で追加し、`ide-plugin/client-core` `ArbdProcessManager.shouldAutoUpgradeBinary` が同一パス上書き更新時も安全に自動アップグレード判定可能に。

## 2. ローカル品質ゲート検証結果

- **Rust ワークスペース（`./core/scripts/verify.sh` ＋ `cargo doc`）**:
  - `[1/6] cargo fmt --check`: PASS
  - `[2/6] cargo clippy --workspace --all-targets -- -D warnings`: PASS（警告 0 件）
  - `[3/6] cargo test --workspace`: **354 passed, 0 failed, 3 ignored**（レビュー修正後・`origin/v2-develop`（T-6B）マージ後）（新規: `device.rs` 3 件、`config.rs` / `bundle.rs` / `analytics.rs` 各 1 件、`arb-adb` `wm size` Override ＆ `query_package_uids`、`live_adb.rs` HW エンコーダ競合分類、`arb-cli` T-6C フラグ解析、`arb-tray` `logs_only` 表示＆マルチ端末ガード、`arbd` 実デーモン E2E `t6c_final_parity_e2e` 2 件、`arb-cli` MCP stdio `app_logs` ＋ `package` フィルタ検証）
  - `[4/6] check-licenses.py`: PASS（**GPL / AGPL 違反 0 件**・新規外部クレート追加 0 件）
  - `[5/6] sara check --strict`: PASS（本書 `VER-21` 含む）
  - `[6/6] REQ → SPEC カバレッジ`: PASS（14/14）
  - `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`: PASS（警告 0 件）
- **Kotlin `ide-plugin:client-core`（`./gradlew :client-core:test --no-daemon`）**:
  - **78 passed, 0 failed**（`ide-plugin` 本体 `test` 18 passed。新規: `DeviceSelectionPolicyTest` 7 件 — 厳格マルチ端末自動切替禁止、`CaptureBarReducer` 状態遷移＋`logs_only`/エンコーダ警告バナー、USB/Wi-Fi IPv4/IPv6/mDNS シリアル検証、T-6C JSON-RPC/Config/AppLogSummary/Hello `build_id` 往復、同一パスバイナリ mtime 自動アップグレード判定）
  - `ide-plugin/src/**` / `ide-plugin/build.gradle.kts` / `plugin/**` / `.github/**` への変更ゼロ（並列 T-6B とのファイル所有境界を厳守）。

## 2.1 独立レビュー（Claude Opus）指摘と修正

Trail #105 に登録し、すべて回帰テスト（修正前に失敗することを確認済み）付きで修正した。

- **[Medium] app-logs ファイル名衝突**: 大文字小文字だけが異なるパッケージ（`com.Example.app` / `com.example.app`）が macOS / Windows で同一ファイルに上書きされ manifest の sha256 と不整合になる問題、および Windows 予約デバイス名（`CON` / `NUL` / `COM1` など）で保存全体が失敗する問題を修正。重複除去を大文字小文字非区別にし、予約名には `_` 接頭辞を付与。`ManifestV2::validate` も大文字小文字非区別の重複 `app_logs[].relative_path` を拒否。
- **[Medium] 共有 / システム UID の誤帰属**: `cmd package list packages -U` の uid 1000（`system_server`）や `sharedUserId` の UID を列挙順先頭のパッケージへ割り当てていたため、`system_server` のログ（`am_crash` 等）が別アプリ扱いで対象アプリの app-logs から欠落し得た。app id < 10000 と複数パッケージ共有 UID をマップから除外。
- **[Medium] `wm size` の退行**: 上記 5. の通り Physical 優先へ戻した。
- **[Medium] Kotlin / Rust シリアル検証の不一致**: Kotlin が Unicode 文字・`%`・256 文字・括弧不整合を許可していた。Rust 規則を 1:1 移植し、IPv6 ゾーン ID（括弧内の単一 `%zone`）は双方で許可。Kotlin `evaluate` も Rust 同様 online シリアルを trim。
- **[Low] scrcpy stderr 収集の停止**: 不正 UTF-8 で `read_line` が失敗すると読み取りを止め、パイプ満杯で `scrcpy-server` がブロックし得た。バイト単位読み取り＋lossy 変換で EOF まで排出。

## 3. 明示的な未検証項目（Unverified）

- **物理 Android 実機での Running Devices ミラーリング同時録画・Wi-Fi ADB 実機接続**: 実機固有のベンダー別 H.264 HW エンコーダ上限や職場ネットワークでの mDNS TLS 接続は、単体・実 `arbd` E2E・モック検証済みとし、実機確認は `docs/v2/testing/work-device-smoke-checklist.md`（項目 7・8）にて後日実施する。
- **Linux / Windows ホストでの実ランタイム動作**: ローカル検証は macOS arm64 で実施。Linux / Windows 3-OS パリティは GitHub Actions `v2 CI`（全 7 ジョブ）で確認する。
