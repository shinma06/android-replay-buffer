---
id: VER-20
type: verification
name: T-6B UI 層の本物 Compose / JetBrains Jewel（composeUI()）全面移行＋Settings CLI/MCP コピー・Logcat スクショ保存先継承・Logcat パネル操作整理の検証記録
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

# VER-20: T-6B Compose / JetBrains Jewel（`composeUI()`）全面移行・Settings CLI/MCP コピー・Logcat スクショ保存先継承 検証記録

記録日: 2026-11-24（macOS arm64 / Gradle 9.8.1 / Kotlin 2.4.21 + `kotlin("plugin.compose")` / Android Studio SDK `2026.2.2.8` `intellijPlatform { composeUI() }` / JDK 27 targeting JVM 25）

## 1. T-6B 実装サマリ

### 1.1 本物の `composeUI()` / JetBrains Jewel 有効化＆T-6A 機能の完全保持
- `ide-plugin/build.gradle.kts` に `kotlin("plugin.compose") version "2.4.21"` および `intellijPlatform { composeUI() }` を追加（旧 `plugin/build.gradle.kts` と同一の Android Studio `2026.2.2.8` 実績構成）。
- `ide-plugin/src/main/resources/META-INF/plugin.xml` に `<depends>com.intellij.modules.compose</depends>` を追加。
- T-6A のゼロ zsh 同梱パッケージング（`native/<os>-<arch>/{arbd,arb-cli,arb-tray}` + `device-agent/*.dex.jar`）、`ArbdProcessManager` 自動起動・`adb` 検出・古い `arbd` 自動入替、`ARB v2: メニューバー (arb-tray) を起動`、2 秒バックグラウンド `device.list` ポーリングおよび物理 USB `unauthorized -> online` 遷移を 100% 維持。
- オーナー確定方針（`06-v1-v2-feature-parity-matrix.md`）に基づき、`RunningDevices`（`StreamingToolbarSecondary`）3 アクションを維持しつつ、`Logcat.PopupActions` および `ArbLogcatTitleActions` を削除（サイドバーにも「Logcat を開く」ボタンなし）。

### 1.2 全 4 サーフェスの JetBrains Jewel Compose 全面移行（Swing フォールバックゼロ化）
- `ArbJewelTheme.kt`: `JewelTheme.globalColors.panelBackground` / `JewelTheme.globalColors.borders.normal` / `JewelTheme.contentColor` / `JewelTheme.defaultTextStyle` のみを使用するトークン群と、ボーダーレス・ホバー反応型の `ArbSubtleAction` / `ArbFilterTab` / `ArbSoftBanner` / `ArbSemanticPill` / `ArbScrollableColumn` を整備（ハードコード暗色背景・角張った矩形 `LineBorder`・絵文字をゼロ化）。
- **ツールウィンドウ（`ARB`: `ArbToolWindowFactory` / `ArbToolWindowController` / `ArbToolWindowContent`）**:
  - 外枠カード線を排し `JewelTheme.globalColors.panelBackground` へ直接描画。
  - 28dp 上段にネイティブ Jewel `ListComboBox`（0 端末時無効プレースホルダ `端末未検出（USB / エミュレータを接続）`、`unauthorized` / `offline` 明示ラベル付き）＋控えめなインライン状態ドット＋ボーダーレス Jewel `IconActionButton` 設定ギア。
  - スリム丸角リング充填バー＋等幅秒表記（`272s / 300s · 420 MB`）、単一 Jewel `DefaultButton` 主 CTA（`直前 {N}s を保存して開く`）、ボーダーレス補助行（`保存のみ` / `スクショ` / `ログのみ` / `開始・停止` / `トレイ`）。
  - 条件付き Tier 2 バナー（`STORAGE_PRESSURE_STOP`、古い `arbd` `-32601` ＋ `arbd を再起動`、失敗保存リカバリ `再試行` / `保存先変更` / `破棄`）。
  - 繊細なインラインタブフィルタ（`すべて` / `トリム済み` / `クラッシュ` / `ゴミ箱` — `ゴミ箱を空にする` / GC は `ゴミ箱` タブ内のみ表示）と外枠レス `LazyColumn`（クリックで統合スタジオを開き、ホバーで `Finder` / `ゴミ箱へ` / `復元`）。
- **統合ビューア＆トリムスタジオ（`ArbBundleFileEditor` / `ArbStudioController` / `ArbStudioContent`）**:
  - 単一行ヘッダ（バンドル名＋条件付き Tier 2 誠実チップ＋ボーダーレス `PNG` / `Finder`＋形式選択＋単一 Jewel `DefaultButton`）。
  - 可変左右スプリッタ：左に SHM `ImageBitmap` ステージ＋`display_width`/`display_height` 正規化ベクタータップ波紋＋ドラッグ時限定 `近似プレビュー（位置確定前）` ピル＋ボーダーレス再生バー、右に 3 タブ（`操作＆ログ` / `時刻不明 (N)` / `詳細`）＋範囲外ディム表示＋0..300ms タップ相関＋ホバー `[In]`/`[Out]` スナップ＋PSI ソースジャンプ。
  - 下部マルチトラックタイムライン（Compose `Canvas`：ルーラー＋フィルムストリップ＋QuickTime 風ゴールド二重トリムハンドル＋画面遷移帯＋タップ／クラッシュ／不連続マーカー＋単一プレイヘッド＋決定論的プリセット＋非破壊 `書き出し後に元のバンドルをゴミ箱へ（復元可）`）。
- **Settings（`ArbSettingsConfigurable` / `Configurable.NoScroll`）**:
  - Compose/Jewel 自己スクロール化により 520px 標準幅で横スクロールゼロ。古い `arbd` 折り返しバナー＋1 クリック `arbd を再起動` 維持。
  - **Logcat スクショ保存先継承（`LogcatScreenshotSettings.java` + `ArbScreenshotDestinationResolver`）**: 既定 ON のチェックボックス `Android Studio の Logcat スクリーンショット保存先を継承する` を追加。リフレクションで `DeviceScreenshotSettings` / `SaveConfigurationResolver` を安全参照し、非 Studio 環境や ABI 変化時は `output_dir` へクリーンにフォールバック。OFF 時は任意フォルダ上書き可能。
  - **1 クリック「CLI / MCP パスをコピー」（`ArbCliMcpPathResolver`）**: 同梱 `<pluginPath>/native/<os>-<arch>/arb-cli` を表示し、`CLI パスをコピー` および `MCP 設定 JSON をコピー`（`{"command": "<path>/arb-cli", "args": ["mcp"]}`、Windows バックスラッシュ含む正規 JSON エスケープ対応）を提供。
- **About ダイアログ（`ArbAboutDialog`）**: Jewel Compose 化＋GPL/AGPL 0 件の選択可能 OSS ライセンス表記を維持。
- 旧 Swing UI 実装ファイル 11 本（`ArbTheme.kt`, `ArbToolWindowPanel.kt`, `CaptureBarPanel.kt`, `BundleCatalogPanel.kt`, `FailedSaveRecoveryPanel.kt`, `TimelineViewerPanel.kt`, `VideoViewportCanvas.kt`, `EventLogStreamPanel.kt`, `MultiTrackTimelineDock.kt`, `DiscontinuityTimelineBar.kt`, `ArbQuickConfigDialog.kt`）および `ArbLogcatTitleActions.kt` を完全削除。

## 2. ローカル品質ゲート検証結果

- **Gradle `ide-plugin`（`(cd ide-plugin && JAVA_HOME="/opt/homebrew/opt/openjdk@27/libexec/openjdk.jdk/Contents/Home" ./gradlew test buildPlugin --no-daemon)`）**:
  - `composeUI()` / JetBrains Jewel コンパイル＆プラグインパッケージング: **PASS**
  - `:client-core:test`: **71/71 PASS**
  - ルートプラグイン `:test`: **18/18 PASS**（`plugin.xml` Compose 依存・RunningDevices 維持・Logcat 削除検証、全 UI ソース絵文字ゼロ／暗色 hex ゼロ／`LineBorder` ゼロ自動監査、Settings CLI/MCP 1 クリックコピー＆Windows パス JSON エスケープ、Logcat スクショ保存先継承＆フォールバック、古い `arbd` `-32601` 検出＋再起動、2 秒バックグラウンド 0→1 端末検出、物理 USB `unauthorized -> online` 遷移、タップ物理スケール正規化＆未マッピング座標スキップ、GC のゴミ箱タブ限定表示、統合スタジオのスナップ／プリセット／PSI ジャンプ／Legacy 保護）
  - **Gradle 合計: 89/89 PASS（失敗 0 件）**
  - ゼロ zsh プラグイン ZIP: `ide-plugin/build/distributions/arb-ide-plugin-2.0.0-SNAPSHOT.zip`（**9,466,178 bytes**、同梱 `native/macos-arm64/{arbd,arb-cli,arb-tray}` 確認済み）
- **Rust ワークスペース＆トレース品質ゲート（`./core/scripts/verify.sh`）**:
  - `[1/6] cargo fmt --check`: PASS
  - `[2/6] cargo clippy --workspace --all-targets -- -D warnings`: PASS
  - `[3/6] cargo test --workspace`: PASS
  - `[4/6] check-licenses.py`: PASS（**GPL / AGPL 違反 0 件**）
  - `[5/6] sara check --strict`: PASS（本書 `VER-20` 含む）
  - `[6/6] REQ -> downstream SPEC coverage`: PASS

## 3. 明示的な未検証項目（Unverified）

- **実 Android Studio GUI ウィンドウでの Light / Dark テーマ目視確認**: Compose/Jewel コンパイル・ヘッドレスコントローラ／モック IPC 結合テスト・プラグイン ZIP 同梱は検証済み。実 Android Studio `2026.2.2.8` 上での描画ピクセルおよびドラッグ操作の最終目視確認はマージ後の実機トライアルで確認する。
- **Linux / Windows ホストでの 3-OS パリティ**: ローカル実行は macOS arm64。Linux / Windows ビルド＆テストは GitHub Actions `v2 CI`（全 7 ジョブ）で検証する。
