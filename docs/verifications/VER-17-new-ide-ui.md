---
id: VER-17
type: verification
name: Android Studio IntUI 新プラグイン UI（統合ビューア＆トリムスタジオ・コックピット・画面遷移スパン＆PSI ソースジャンプ）検証記録
verifies:
  - SPEC-03
  - REQ-01
  - REQ-02
  - REQ-03
  - REQ-04
  - REQ-07
  - REQ-08
  - REQ-09
  - REQ-11
  - REQ-12
---

# VER-17: T-4 Android Studio IntUI 新プラグイン UI（統合ビューア＆トリムスタジオ・コックピット・画面遷移スパン＆PSI ソースジャンプ）検証記録

記録日: 2026-11-23（macOS arm64 / Gradle 9.8.1 / Kotlin 2.4.21 / Android Studio SDK `2026.2.2.8` / JDK 27・JVM 25 ターゲット / 実 `arbd` バイナリ結合）

## 1. UI ツールキット選定（Compose/Jewel ではなく IntelliJ Platform ネイティブ IntUI `JBUI` + 高 DPI `Graphics2D` を採用した技術理由）

`plugin/build.gradle.kts` の `composeUI()` / `com.intellij.modules.compose`（Jewel）統合を事前評価した上で、T-4 の `ide-plugin/` には **IntelliJ Platform ネイティブ IntUI (`JBColor` / `JBUI` / `AllIcons` / `JBSplitter` / `JBTabbedPane` / `JBList`) + 高 DPI アンチエイリアス `Graphics2D` カスタムコンポーネント** を採用した。技術理由は以下の通り。

1. **ヘッドレス JUnit 5 直接検証と二重実装の排除**: `JewelComposePanel` は Skiko ネイティブレンダラおよび IntelliJ `Application` コンテキストを要求するため `-Djava.awt.headless=true` の CI ヘッドレステスト内で実パネルを直接生成・操作できない（実際、旧 `plugin/` でも `ReplayToolWindowFactory.kt` 内にテスト専用の非表示 Swing 実装を二重保持する必要があった）。ネイティブ IntUI Swing / `JBUI` を採用することで、本番の `CaptureBarPanel` / `BundleCatalogPanel` / `TimelineViewerPanel` / `VideoViewportCanvas` / `MultiTrackTimelineDock` / `EventLogStreamPanel` をヘッドレス JUnit で直接インスタンス化・操作検証できる。
2. **高頻度 SHM 映像＆マルチトラックタイムラインの描画適合性**: 統合スタジオの主領域は `ShmFrameRingReader` の `BufferedImage` ゼロコピー描画＋ベクタタップ波紋オーバーレイ＋デュアル In/Out ドラッグハンドル・画面遷移スパン・クラッシュマーカー・不連続ハッチ帯であり、Java2D `Graphics2D` で高 DPI アンチエイリアス描画を行う方が軽量かつ Android Studio IntUI Dark/Light テーマへシームレスに追従する。
3. **プラグインサイズ・依存境界の最小化**: `com.intellij.modules.compose` 必須依存を課さず、新規 GPL/AGPL 依存ゼロ、生成プラグイン ZIP は **941 KB** に抑えられた。

## 2. 実装対象（`ide-plugin/` のみ — `core/`・`device-agent/`・`plugin/`・`.github/` 非変更）

| モジュール / ファイル | 役割・主な実装内容 |
| --- | --- |
| `client-core/model/ProtocolModels.kt` & `ProtocolJson.kt` | T-1 ワイヤスキーマ完全一致（`ArbConfig`, `ArbConfigPatch`, `OutputDirPatch` の省略 vs 明示 `null` 区別、`IdeDefaultSaveAction`, `SaveMode`, `BundleKind`, `SaveTriggerKind`, `SaveLogsParams`, `SaveScreenshotParams`, `ScreenshotResult`, `CrashMarker`, `ScreenSpan`, `CrashAutoSaveStatusView`）および T-2B 前方互換 DTO（`ExportShareMp4Params/Response`, `MediaDecodeAudioPcmParams/Response`）。 |
| `client-core/ipc/ArbdRpcClient.kt` | `config.get` / `config.update`（明示 `JsonNull` 保持エンベロープ出力）、`save.screenshot`、`save.logs`、`timeline.crashes`、`timeline.screens` を実装。さらに未対応デーモンで `-32601` を安全に `null` 縮退する `exportShareMp4OrNull` / `mediaDecodeAudioPcmOrNull` を追加。 |
| `client-core/state/ArbFormatters.kt` | **厳格な秒表記**（`直前 300s を保存して開く`、`272s / 300s · 420 MB`、`45s` — 分換算を一切行わない）、汎用日本語コピー（`選択範囲を書き出し` / `MP4 を書き出し` / `トリムを書き出しました`）、実データに忠実な `Tap #N (x, y)` 表示、絵文字コードポイント検査 `containsEmoji`。 |
| `client-core/state/SourceJumpParser.kt` & `ide/SourceJumpResolver.kt` | PC 側のみで動作（端末負荷 0）のスタックトレース（`at pkg.Cls.fn(File.kt:84)`）・`File.kt:42`・Activity `ScreenSpan`・`CrashMarker` パーサ、および `JavaPsiFacade` / `FilenameIndex` / `PsiShortNamesCache` による `IntelliJSourceJumpResolver`（テスト可能な `SourceJumpResolver` インターフェース分離）。 |
| `client-core/state/ArbViewModels.kt` | コックピット状態（`SaveAndOpen` / `SaveOnly` 主副入替・リング秒メータ・正常時バナー 0）、ライブラリフィルタ（`すべて` / `トリム済み` / `クラッシュ` / `ゴミ箱`）、Tier-2 誠実チップ（正常時 0 件、音声オフセット・`ログ UNVERIFIED`・ギャップ件数・`Legacy (RO)` 時のみ表示）、トリム範囲内/外ディミング・`0..300ms` タップ→直後ログ相関・±250ms タップ波紋抽出・`Legacy (RO)` 時の `trashOriginal` 強制無効化。 |
| `ui/ArbTheme.kt`, `ArbQuickConfigDialog.kt`, `CaptureBarPanel.kt`, `FailedSaveRecoveryPanel.kt`, `BundleCatalogPanel.kt`, `ArbToolWindowPanel.kt` | 絵文字ゼロ・`AllIcons`＋`JBColor` IntUI デザインの `ARB` ツールウィンドウ（コックピットカード＋設定ダイアログ＋正常時高さ 0 の Tier-2 バナー＋フィルタ可能高密度ライブラリ）。 |
| `ui/VideoViewportCanvas.kt`, `MultiTrackTimelineDock.kt`, `EventLogStreamPanel.kt`, `TimelineViewerPanel.kt`, `ide/ArbBundleFileEditor.kt` | 統合ビューア＆トリムスタジオ：Tier-1 トップアクションバー＋Tier-2 誠実チップ＋書き出し完了トースト（`[Finder で表示]` `[パスをコピー]` `[新しいタブで開く]`）、`JBSplitter` 左（SHM 映像＋±250ms タップ波紋＋左上浮遊 `近似プレビュー（位置確定前）` ピル＋T-3 `PlaybackController` & `AudioSyncPlayer` 接続トランスポートバー）、右（`操作＆ログ` / `時刻不明 (N)` / `詳細` タブ・トリム範囲同期・`0..300ms` タップログ相関・ホバー `[In]`/`[Out]` スナップ・PSI ソースジャンプ）、下部フル幅マルチトラックタイムライン（フィルムストリップ＋デュアル In/Out ドラッグハンドル＋Activity 画面遷移帯＋タップピン＋クラッシュマーカー＋アンバー斜線ギャップ＋決定論的プリセット `操作区間を選択` / `クラッシュ前後を選択 (-5s/+2s)` / `全選択に戻す`＋非破壊ゴミ箱トグル）。 |

## 3. ローカル検証ゲート実行結果

- `ide-plugin` Gradle テストおよびプラグイン ZIP ビルド（`./gradlew test buildPlugin --no-daemon`）:
  - `:client-core:test`: **59 件全 PASS（失敗 0 / スキップ 0）**
    - `ArbdRealIntegrationTest`: 2 件全 PASS（既存フルラウンドトリップ＋新規 T-1 `config.get`/`config.update` の `output_dir_override` 設定・`null` クリア往復、`timeline.crashes` / `timeline.screens` 実 `arbd` 往復、`save.logs` / `save.screenshot` ワイヤ登録確認、T-2B 未実装 `-32601` の安全な `null` 縮退）
    - `ArbdRpcClientTest`: 5 件全 PASS（T-1 全 RPC および T-2B フォールバックの JSON 往復検証含む）
    - `ArbT4StudioModelTest`: 7 件全 PASS（**全 `ide-plugin` メインソースファイル絵文字ゼロ検査**、厳格秒表記、`SaveAndOpen`/`SaveOnly` 入替、Tier-2 正常時非表示・異常時表示、トリム範囲同期＆`0..300ms` タップ→ログ相関＆誠実 `Tap #N (x, y)` 表示、`SourceJumpParser` 単体検証、`Legacy (RO)` ゴミ箱無効化＆ライブラリフィルタ）
    - 既存スイート（`ArbPathsTest`, `ArbdProcessManagerTest`, `BundleLeaseControllerTest`, `ProtocolJsonTest`, `AudioSyncPlayerTest`, `PlaybackControllerTest`, `ShmFrameRingReaderTest`, `ArbViewModelsTest`）: 45 件全 PASS
  - ルートプラグイン `:test`（`ArbUiComponentsTest`）: **4 件全 PASS**（`VideoViewportCanvas` 近似プレビュー＆タップ波紋、`CaptureBarPanel` コックピット秒表記・主副入替・1 クリック補助ボタン・`FailedSaveRecoveryPanel`・`BundleCatalogPanel` 4 フィルタ、`TimelineViewerPanel` の主/時刻不明分離・`[In]`/`[Out]` スナップ・決定論的プリセット条件表示・`SourceJumpResolver`・`Legacy (RO)` 保護・書き出し完了バー）
  - **合計 63 件全 PASS**、Kotlin コンパイル警告 0 件、`:buildPlugin` ZIP（`ide-plugin/build/distributions/arb-ide-plugin-2.0.0-SNAPSHOT.zip`, **941 KB**）生成成功。
- `./core/scripts/verify.sh`:
  - `cargo fmt --check` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace` / license check / `sara check --strict`（`VER-17` 含む） / REQ→SPEC 被覆 14/14 すべて PASS。

## 4. 明示的な未検証項目（Unverified）

- **実 Android Studio GUI プロセス上でのライブ視覚・操作受入検証**: 本検証はヘッドレス JUnit 5（実 UI コンポーネントインスタンス生成・描画契約・コールバック検証）および実 `arbd` IPC / SHM 結合・`buildPlugin` ZIP 生成までを実施済み。実 Android Studio `2026.2.2.8`（Dark / Light テーマ）へプラグイン ZIP を導入した目視・ドラッグ操作・実プロジェクト PSI ジャンプ確認は後続のマルチ OS GUI 受入フェーズで実施する。
- **Linux / Windows 実機ランタイム実行**: ローカルは macOS arm64 で検証し、Ubuntu（`ide-plugin-tests`）および Windows（`ide-plugin-client-core-windows`）を含む全 7 ジョブの結果は GitHub Actions `v2 CI` で確認する。
