---
id: VER-19
type: verification
name: T-6A レビューフォローアップ（タップ物理画面スケール・マルチエポック対応・IDE/Settings UI 改善・ゼロ zsh 自己完結プラグイン ZIP・3-OS 配布パッケージ）＆ API 37 実エミュレータフルスタック E2E 検証記録
verifies:
  - SPEC-01
  - SPEC-02
  - SPEC-03
  - SPEC-04
  - REQ-01
  - REQ-04
  - REQ-07
  - REQ-08
  - REQ-09
  - REQ-11
  - REQ-12
---

# VER-19: T-6A レビューフォローアップ・ゼロ zsh 自己完結プラグイン ZIP・3-OS 配布パッケージ＆API 37 実エミュレータフルスタック E2E 検証記録

記録日: 2026-11-23（macOS arm64 / Android API 37 Emulator `arb_api37_arm64` (`sdk_gphone64_arm64`, 物理画面 `1080x2400`) / Rust workspace `core/` / Gradle 9.8.1 / Kotlin 2.4.21 / Android Studio SDK `2026.2.2.8` / `ffprobe` & `ffmpeg`）

## 1. T-6A 実装・改善サマリ

### 1.1 タップ座標の物理ディスプレイ解像度スケーリング（`display_width` / `display_height`）
- `arb-core` `VideoStreamMeta` および `manifest.json`、`arbd` `timeline.overview` / `bundle.open` へ任意の `display_width: Option<u32>` / `display_height: Option<u32>` を追加（0 値拒否バリデーション＋旧バンドル互換の `None` フォールバック）。
- `arb-adb` `AdbClient::query_display_size`（`adb shell wm size` の `Physical size:` 優先、`Override size:` フォールバック）をライブセッション開始時に実行し、保存バンドル・トリム後バンドルの両方へ伝播。
- `arbd` `media.inspect_frames`（`contact_sheet` / `single` / `tap_crop`）および `ide-plugin` `VideoViewportCanvas.mapTapToCanvasPoint` の両方で、物理タッチ座標 `(x, y)` を `display_width x display_height` から縮小動画解像度へ正確にスケール変換（旧バンドル時は動画解像度へフォールバック）。

### 1.2 マルチエポック（端末再起動跨ぎ）フレーム範囲マッピング
- `arbd` `timeline.summary` / `timeline.overview` に `EpochFrameRange { epoch_index, boot_id, start_frame_index, end_frame_index, start_boottime_ns, end_boottime_ns }` を追加。
- `arb-core` クラッシュマーカー抽出、`arbd` `inspect_export` のタップ／ログ→フレーム対応、および `ide-plugin` `TimelineViewerState`（総尺計算・フレーム↔boottime 変換・タップリップル・タップ→後続ログ 300ms 相関・時系列ソート）をすべて `boot_id` 単位のエポック分離へ統一し、再起動跨ぎのクロスポルーションを防止。

### 1.3 `ide-plugin/` レビューフォローアップ＆実 Android Studio トライアル UX 修正
- **Logcat タイトルバー非破壊追加（`ArbLogcatTitleActions`）**: 既存タイトルアクションを保持しつつ重複なく ARB アクションを追記。
- **長時間エクスポート用パーコール RPC タイムアウト（`ArbdRpcClient`）**: 既定 30s を維持しつつ `export.share_mp4` / `export.trim` / `media.decode_audio_pcm` に 120s タイムアウトを適用。
- **Settings 適用後の即時 UI 反映（`ArbdDaemonService` `ArbDaemonStateListener`）**: `config.update` およびカスタム `arbd` パス変更時にツールウィンドウ・Running Devices / Logcat アクションへ即時ブロードキャスト。
- **誠実な `buffered_secs` 表示**: `arbd` `system.status` が実リング滞留秒数 `buffered_secs` を返却し、`ide-plugin`（蓄積中ラベル付き）および `arb-tray` で優先消費。
- **ツールウィンドウ空端末ドロップダウン＆上段整列**: 0 端末時は無効プレースホルダ `端末未検出（USB / エミュレータを接続）` を表示して空ポップアップを抑止。上段 3 要素（端末コンボ `weightx=1.0`・ステータスピル・`28x28` 正方形歯車ボタン）を `GridBagLayout` 単一垂直中央ベースラインへ整列。
- **Settings 画面の折り返しバナー・横スクロールゼロ化＆古い `arbd` 検出＋1 クリック再起動**: 警告バナーを折り返し `JTextArea` 化し、全数値スピナー・コンボ・パス欄を 520px 標準幅内に収束。JSON-RPC `-32601`（`method not found: config.get`）を接続不能と分離し、`古いバージョンの arbd が起動中です（config.get 非対応）。新しい arbd へ再起動してください。` と 1 クリック **`arbd を再起動`** ボタンを提供。
- **ゼロ zsh 自己完結プラグイン ZIP（`ide-plugin/build.gradle.kts` & `ArbdProcessManager`）**: `prepareSandbox` / `buildPlugin` が `native/<os>-<arch>/{arbd,arb-cli,arb-tray}`（Unix `0755` 権限付き）と `device-agent/*.dex.jar` を `arb-ide-plugin-*.zip` に直接同梱。`ArbdProcessManager` は `PluginManagerCore` 経由で同梱バイナリと DEX JAR を自動検出し、GUI 起動（最小 `PATH`）向けに `ARB_ADB_PATH`（および `arb-adb` 側の `$HOME/Library/Android/sdk` / `$HOME/Android/Sdk` / `%LOCALAPPDATA%\Android\Sdk` フォールバック）を自動解決、古い `arbd` 検出時の自動アップグレード、およびターミナル不要でメニューバーを起動する `ARB v2: メニューバー (arb-tray) を起動` アクションを追加。
- **物理 USB / ワイヤレス ADB・エミュレータの 2 秒バックグラウンド自動検出**: `ArbToolWindowPanel` が 2 秒間隔（オフ EDT、dispose 時確実停止）で `device.list` をポーリングし、0→1 接続や切替時にオンライン端末を自動選択。未承認（`unauthorized`）や `offline` の物理端末も明示ラベル（`未承認: USB デバッグを許可` / `オフライン`）と操作ガード付きで表示し、`online` 遷移と同時に操作を有効化。

### 1.4 3-OS 自己完結配布パッケージ＆`v2-ci.yml` 統合
- `core/scripts/package_distribution.py`（Python 3 標準ライブラリのみ・UTF-8 安全）を追加し、`arb-v2-<os>-<arch>.zip`（`arb-ide-plugin-*.zip`、`bin/{arbd,arb-cli,arb-tray}`、`device-agent/*.dex.jar`、古い `arbd` 自動停止付き `start-arb.sh` / `start-arb-tray.command` / `start-arb.ps1`、`README-TRIAL.md`、`docs/first-run-os-permissions.md`、`SHA256SUMS.txt`）および `--self-test` を実装。
- `.github/workflows/v2-ci.yml`（全 7 ジョブ維持）に `--self-test`、ゼロ zsh 自己完結 `arb-ide-plugin-*.zip` 検証、および配布 ZIP アーティファクトアップロードを統合。

## 2. API 37 実エミュレータ フルスタック E2E 実測結果（`arb_api37_arm64`）

### 2.1 `core/scripts/e2e_emulator.sh`（9 ステップ一気通貫＋Player Matrix）
- **対象端末**: `emulator-5554`（`arb_api37_arm64`, `sdk_gphone64_arm64`, API 37, 物理画面 `1080x2400`）
- **ライブ取得結果**:
  - `buffered_secs = 9s`、`video_frames = 166`、`keyframe_count = 3`、`audio_frames = 439`、`log_records = 1888`（注入 `ARB_E2E` マーカー 6/6 件回収、`logcat_completeness = verified_no_overflow`）
  - `streams.video.display_width = 1080`, `display_height = 2400`（動画実寸 `460x1024`）
  - `epoch_frame_ranges`: 1 エポック（`epoch_index = 0`, `frames 0..=165`, `boot_id = 0ef72b19-17c7-4d5f-9b0d-18177487cfb9`）
- **実 IPC + SHM `scrub-bench`**:
  - `filmstrip`: 3 枚生成 `51.76 ms`
  - `scrub`（近似 SHM `80x180`, `is_approximate=true`）: **`0.38 ms`**
  - `seek_exact`（精確 SHM `460x1024`, `is_approximate=false`, frame 83）: **`95.03 ms`**
  - `step_forward`（frame 84, cache hit 1）: **`0.36 ms`**
  - `step_backward`（frame 82, cache hits 2）: **`4.27 ms`**
- **Mid-GOP トリム（frame `30..=112`, 83 フレーム）**:
  - 所要時間: **`132.9 ms`**（VideoToolbox バックエンド、再エンコード 30f ＋ ストリームコピー 53f、`edts = 0`, `elst = 0`）
  - `ffprobe` 独立検査: `h264`, `460x1024`, `nb_read_frames = 83`, トップボックス `['ftyp', 'moov', 'mdat']`, `edts = 0`, `elst = 0`
  - 音声チェック: 保存版 `audio.m4a = 152,944 bytes`（`av_offset_ms = 26`, 441 AAC-LC パケット 48kHz 2ch）、トリム版 `audio.m4a = 61,021 bytes`（`av_offset_ms = -18`, 175 パケット）
  - `devin_verify_player_matrix.py`: 保存版 **17/17 PASS**、トリム版 **17/17 PASS**（`ffmpeg` 全フレームデコードエラー 0 件）
- **PNG 書き出し（frame 30）**: `460x1024`, `202,049 bytes`, `tEXt` 証跡キー `['arb:boottime_ns', 'arb:frame_index', 'arb:source_bundle_id']`

### 2.2 T-1 / T-2B / T-6A ライブ機能スモーク実測（`arb_api37_arm64`）
- **`config.get` / `config.update` 往復**: `save_window_secs` `300 -> 120 -> 300` **PASS**
- **ライブ `system.status`**: `buffered_secs = 7s`, `video_frames = 139`, `audio_frames = 340`, `logs = 1377` **PASS**
- **ライブ `save.screenshot`**: `1080x2400` PNG（`96,245 bytes`, SHA-256 `0e420a92102d7feb...`）**PASS**
- **ライブ `save.logs`（ログのみ WORM バンドル）**: `bundle_kind = logs_only`, `logs.jsonl = 1,388 lines`, `unanchored.jsonl` 同梱、`video = absent` **PASS**
- **ライブ `export.share_mp4`（単一共有 MP4）**: `1,127,714 bytes`, `codecs = ['aac', 'h264']`, `top_boxes = ['ftyp', 'moov', 'mdat']`, `edts = 0`, `elst = 0`, `ffmpeg -v error -f null` デコードエラー **0 件** **PASS**
- **ライブ `media.decode_audio_pcm`**: `48,000 Hz`, `2 ch`, `duration_ns = 7,808,000,000`, `av_offset_ms = 57`, RIFF/WAVE PCM `1,499,180 bytes`, 1 回目 `cached = false` → 2 回目 `cached = true` **PASS**
- **ライブ `arb-cli mcp`（stdio JSON-RPC 2.0）**:
  - `initialize` + `tools/list`: **17 ツール** 正常列挙
  - `arb_timeline_overview`: `display_width = 1080`, `display_height = 2400`, `epoch_frame_ranges = 1`
  - `arb_inspect_frames`（`mode = "contact_sheet"`, `max_tiles = 4`, `include_tap_marker = true`）: 有効な base64 PNG（`1024x588`, `168,823 bytes`）返却 **PASS**

## 3. ローカル品質ゲート・パッケージング検証結果

- **ゼロ zsh 自己完結プラグイン ZIP（macOS arm64）**:
  - `ide-plugin/build/distributions/arb-ide-plugin-2.0.0-SNAPSHOT.zip`（**6,135,988 bytes**）
  - 同梱検証: `native/macos-arm64/arbd`（`7,554,080 bytes`, `0o755`）、`arb-cli`（`946,608 bytes`, `0o755`）、`arb-tray`（`1,858,128 bytes`, `0o755`）、`device-agent/arb-companion.dex.jar`（`26,696 bytes`）、`device-agent/replay-scrcpy-audio.dex.jar`（`735,101 bytes`）
- **配布パッケージ（`core/scripts/package_distribution.py`）**:
  - `--self-test`（`linux-x86_64`, `macos-arm64`, `windows-x86_64`、POSIX `0755` 権限、古い `arbd` 自動再起動ランチャー、UTF-8 ドキュメント、全 `SHA256SUMS.txt`）: **PASS**
  - 実 macOS arm64 ビルド成果物パッケージング: `arb-v2-macos-arm64.zip`（**11,147,923 bytes**, 11 エントリ）**PASS**
- **Gradle `ide-plugin` テスト（`(cd ide-plugin && ./gradlew test buildPlugin --no-daemon)`）**:
  - `:client-core:test`: **70 件全 PASS**（新規: マルチエポックマッピング、誠実 `buffered_secs`、120s パーコールタイムアウト、同梱バイナリ／DEX JAR／GUI `adb` 解決、古い `arbd` 自動アップグレード判定）
  - ルートプラグイン `:test`: **17 件全 PASS**（新規: Logcat タイトル非破壊追記、物理画面スケールタップ変換、Settings 即時反映ブロードキャスト、空端末プレースホルダ＆上段 28px 整列レイアウト、Settings 520px 幅内収束＆折り返しバナー、`-32601` 古い `arbd` 診断＋1 クリック再起動、バックグラウンド 0→1 端末検出、物理 USB `unauthorized → online` 遷移）
  - **Gradle 合計 87 件全 PASS（絵文字ゼロ監査含む）**
- **Rust ワークスペース品質ゲート（`./core/scripts/verify.sh` ＋ `cargo doc`）**:
  - `[1/6] cargo fmt --check`: PASS
  - `[2/6] cargo clippy --workspace --all-targets -- -D warnings`: PASS（警告 0 件）
  - `[3/6] cargo test --workspace`: PASS（失敗 0 件）
  - `[4/6] check-licenses.py`: PASS（**GPL / AGPL 違反 0 件**）
  - `[5/6] sara check --strict`: PASS（本書 `VER-19` 含む）
  - `[6/6] REQ → SPEC カバレッジ`: PASS（14/14）
  - `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`: PASS（警告 0 件）

## 4. 明示的な未検証項目（Unverified）

- **実 Android Studio GUI ウィンドウ上での目視・手動操作再確認**: 5 件の UI レイアウト・バナー折り返し・空端末プレースホルダ・`-32601` 診断＋再起動・物理 USB `unauthorized → online` 遷移は Swing 実レイアウトテストおよびモック IPC 結合テストで検証済みであり、実 Android Studio 上の最終目視確認はマージ前トライアルで実施する。
- **物理 Android 実機（USB / ワイヤレス ADB）での実録画**: ライブ E2E は API 37 エミュレータ（`arb_api37_arm64`）で実施。物理 USB 実機特有のベンダー別 `getevent` / `unauthorized` 実承認フローは単体・モック結合テストによる検証のみ。
- **Linux / Windows ホストでの実ランタイム動作**: ローカル検証は macOS arm64 で実施。Linux / Windows 3-OS パリティおよび配布 ZIP 生成は GitHub Actions `v2 CI`（全 7 ジョブ）で確認する。
