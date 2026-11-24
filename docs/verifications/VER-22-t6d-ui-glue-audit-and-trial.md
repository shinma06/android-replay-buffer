---
id: VER-22
type: verification
name: T-6D T-6C 機能の Compose / Jewel UI 結線・T-6B Compose / Jewel ライフサイクル深掘り監査・ゼロ zsh macOS arm64 試用プラグイン ZIP の検証記録
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

# VER-22: T-6D T-6C 機能の UI 結線・Compose / Jewel 監査・試用プラグイン ZIP 検証記録

記録日: 2026-11-24（macOS arm64 / Rust workspace `core/` / Gradle 9.8.1 / Kotlin 2.4.21 + `kotlin("plugin.compose")` / Android Studio SDK `2026.2.2.8` `composeUI()` / JDK 27 targeting JVM 25）

## 1. Part 1: T-6C バックエンド機能の Compose / Jewel UI 結線

1. **`CaptureMode::LogsOnly` 切替**
   - ツールウィンドウ: 端末行の直下に静かなモード切替（`映像＋ログ` / `ログのみ`）を追加。`logs_only` 中は `ログのみ常時` の控えめなステータスチップを表示し、音声表示は `映像・音声なし` に変わる。追加の箱型ボタンなし。
   - 切替は `config.update(capture_mode)`。録画中に切り替えた場合は、同じ端末のまま `capture.stop` → `capture.start(capture_mode)` で再開する（モードは開始時にのみ反映されるため）。失敗時は警告バルーンを出し、デーモン切断扱いにはしない。
   - Settings: 「キャプチャモード」コンボボックス（`映像＋ログ（画面録画あり）` / `ログのみ（低負荷・常時記録）`）を追加。
2. **アプリ別ログフィルタ（`app-logs/<package>.jsonl`）**
   - スタジオ右ペイン「操作＆ログ」に `アプリ` ドロップダウン（`全アプリ` ＋ 各パッケージの `N 行 · クラッシュ M` バッジ）を追加。`manifest.app_logs` が空のバンドルや Legacy (RO) バンドルでは表示しない。
   - 選択時は IPC ワーカー上で `timeline.query_logs(package = 選択パッケージ)` を MAIN / UNANCHORED の両バケットに発行し、タップ・クラッシュ・画面遷移は保持したままログのみ差し替える。古い応答（選択変更後に届いた結果）は破棄する。
3. **厳格マルチ端末 `DeviceSelectionPolicy` 結線**
   - 端末ドロップダウンを `client-core` の `DeviceSelectionOutcome` から生成するよう変更した。
     - 0 台: `端末未検出（USB / エミュレータを接続）`（未承認端末のみの場合は `(未承認: USB デバッグを許可)` 表記）
     - 1 台・優先端末なし: 自動選択し、録画・スクリーンショットを有効化
     - 2 台以上・優先端末なし: 先頭に `複数端末が接続中です — 対象を選択してください` を表示し、自動選択しない（従来は先頭端末が選択中に見えていた）
     - 選択した端末が切断し、他の端末がオンライン: `<serial> — 切断中（再接続待ち）` のまま固定し、別端末へ自動で切り替えない
   - ユーザーが明示的に選んだ端末は `config.update(device_serial)` で永続化する。Running Devices ツールバー / `arb-tray` / CLI / MCP も同じ端末を対象にする。自動選択済みの単独端末を選び直しただけでは固定しない。
4. **Logcat スクリーンショット保存先継承 → `save.screenshot(output_dir)`**
   - ツールウィンドウと Running Devices ツールバーのスクリーンショットを、`output_path` の事前合成から `output_dir`（Logcat 継承先、または継承オフ時の専用フォルダ）へ変更した。ファイル名は `arbd` が決めるため CLI / MCP / トレイと同一になる。解決できない場合は `null` を渡し、`arbd` 側の `screenshot_dir_override` → `output_dir_override` → 既定の順で解決する。
   - スタジオの PNG 書き出しは IntelliJ ネイティブの保存ダイアログ（`FileSaverDescriptor`）に置き換え、初期フォルダを同じスクリーンショット保存先にした。
5. **同一パス `arbd` 自動アップグレード（`build_id` / `binary_mtime_secs`）**
   - `ArbdProcessManager.shouldAutoUpgradeBinary` は従来、同一パスで mtime が新しい場合だけを判定していた。`system.hello` `build_id`（`<version>-<size>-<mtime>`）を末尾から解析する `parseBuildIdentity` を追加し、mtime が同じでもサイズが違えば入れ替えと判定するようにした。
   - **実測根拠**: `buildPlugin` の ZIP エントリはすべて固定時刻（1980-02-01）で、展開後の `arbd` も `binary_mtime_secs = 318178800` になった（本書 3. のスモークテストで確認）。そのため mtime 比較だけでは、プラグイン ZIP を上書き再インストールしても新しい `arbd` を検知できない。
   - ループガード `tryClaimIdentityUpgrade()` を追加し、識別子ベースの再起動は 1 セッション 1 回までに制限した。プロトコル非互換（`config.get` 未対応など）による入れ替えは対象外。
   - About ダイアログに `arbd ビルド ID` を表示する。

## 2. Part 2: T-6B Compose / Jewel 深掘り監査（独立監査: Claude Opus）

対象: `ide-plugin/src/main/kotlin/io/github/shinma06/arb/{ide,ui}/**`、`ide-plugin/build.gradle.kts`、`plugin.xml`。全指摘を Trail に登録し、回帰テストを付けて修正した。

- **API 互換性（AS `2026.2.2.8`）**: Kotlin 2.4.21 / Compose コンパイラ / `composeUI()` / `com.intellij.modules.compose` の構成は、実績のある旧 `plugin/` と同一。`JewelComposePanel`、`ListComboBox`、`Tooltip`、`IconActionButton`、`TextField(TextFieldState)`、`CheckboxRow`、`GroupHeader` はすべて同 SDK に対してコンパイルされるため、`NoSuchMethodError` の原因になる版ずれはない。`ListComboBox` のバイトコードも確認し、`onSelectedItemChange` はクリック / キー操作時のみ呼ばれ、合成時の `LaunchedEffect` からは呼ばれないことを確かめた（端末固定が意図せず発火しない根拠）。
- **[High] Settings が `capture_mode` / `device_serial` / `screenshot_dir_override` を破棄**: `ArbSettingsFormModel.currentDraft()` がこの 3 項目を既定値で返していた。そのため `logs_only`・固定端末・CLI の保存先上書きがあるだけで `isModified()` が常に真になり、「適用」で 3 項目がリセット / 消去されていた。キャプチャモードを編集可能にし、残り 2 項目は基準値をそのまま引き継ぐよう修正した。
- **[Medium] 破棄時の `shutdownNow()` が共有 IPC を割り込み**: ツールウィンドウとスタジオ（`worker` / `audioDecodeWorker`）が破棄時に `shutdownNow()` を呼んでいた。共有アプリサービスの `SocketChannel`（割り込み可能チャネル）でブロック中のワーカーに割り込むと `ClosedByInterruptException` でチャネルが閉じ、他の ARB ビューの実行中呼び出しまで巻き込まれる。`shutdown()` に変更し、キュー内タスクは `disposed` を見て終了するようにした。破棄後の投入も `RejectedExecutionException` を EDT へ投げない。
- **[Medium] フレーム変換が EDT 上で行われ、再生中に無制限にキューされる**: `BufferedImage.toComposeImageBitmap()`（全画素コピー）を EDT で毎フレーム実行し、`invokeLater` を無制限に積んでいた。変換をワーカー側へ移し、`LatestValueMailbox` で最新フレームのみを 1 回だけ EDT へ渡すようにした（遅い EDT では古いフレームを飛ばす）。
- **[Medium] PSI ソースジャンプが EDT 上でインデックスを検索**: Compose のクリックハンドラ（EDT）から `JavaPsiFacade` / `FilenameIndex` / `PsiShortNamesCache` を直接呼んでいた（低速処理）。プールスレッドで解決し、ナビゲーションだけを EDT へ戻すようにした。
- **[Low] テーマ色がライト / ダーク切替に追従しない**: `JBColor.toComposeColor()` は Compose の状態を読まないため、読み手が再合成されない。`JewelTheme.isDark` を読む `Tone` に置き換えた。
- **[Low] `ArbTooltip` がツールチップ空のとき `modifier` を捨てる**: `Box(modifier)` で保持するようにした。
- **[Low] ハードコードされた暗色 `Color(0xFF1A1A1A)`**（トリムハンドルのグリップ）: テーマトークン `trimHandleGrip()` に置き換えた。
- **[Low] `LazyColumn` のキー重複でスタジオ全体が白紙になる恐れ**: 同一ソース行由来の複数クラッシュマーカーなどで `IllegalArgumentException` が起き得た。`uniqueKeys` で `#n` を付けて一意化した。
- **[Low] 端末ドロップダウンの誤表示**: 複数端末で未選択のときや、固定端末の切断待ちのとき、先頭端末が選択中に見えていた（Part 1.3 で修正）。
- **[Low] OS 依存の文言「Finder」**: Windows / Linux でも同じ表示にするため「フォルダ」「フォルダを表示」に統一した。
- **視覚監査**: `LineBorder` 0 件、ハードコードされた暗色背景 hex 0 件、`JBColor(` 0 件、絵文字 UI 0 件、`shutdownNow()` 0 件。ソース走査テスト `noLegacyBorderHexOrEmojiChromeInComposeSources` で恒久化した。28dp 上段、枠なしユーティリティ列、単一の `DefaultButton`、`Configurable.NoScroll`（ラベル 156dp ＋ コンボ 236dp で 520px 内）を維持している。
- **ZIP 同梱**: `-PdeviceAgentDir` を `-ParbDeviceAgentDir` の別名として受け付けるようにした。`buildPlugin` は `native/macos-arm64/{arbd,arb-cli,arb-tray}`（`-rwxr-xr-x`）と `device-agent/*.dex.jar` を ZIP 内に同梱する。

## 3. Part 3: ゼロ zsh macOS arm64 試用プラグイン ZIP

- 生成手順: `cargo build --release -p arbd -p arb-cli -p arb-tray`（`core/`）、`device-agent/build.sh --with-scrcpy-audio`（ProtocolTest 19 件 PASS）、`./gradlew test buildPlugin -ParbNativeDir=../core/target/release -PdeviceAgentDir=../device-agent/build`。
- 成果物: `arb-ide-plugin-2.0.0-SNAPSHOT.zip` を試用用パス `arb-v2-ide-plugin-macos-arm64-compose-trial.zip` へコピーした。
- 単体展開スモークテスト（リポジトリなし・zsh なし。`/bin/bash` ＋ `unzip` で一時ディレクトリへ展開）:
  - エントリ: `lib/*.jar`（plugin / client-core / gson / error_prone_annotations / searchableOptions）、`native/macos-arm64/{arbd,arb-cli,arb-tray}`（展開後も `-rwxr-xr-x`）、`device-agent/{arb-companion,replay-scrcpy-audio}.dex.jar`。旧 `plugin/` ディレクトリ 0 件。
  - 隔離した `--data-dir` / `--cache-dir` / `--runtime-dir` で展開済み `arbd` を起動し、`arb-cli status` 成功（`build_id = 0.1.0-7757008-318178800`）を確認した。
  - `arb-cli config get` に `capture_mode` と `screenshot_dir_override` が含まれることを確認した。
  - `arb-cli mcp`: `initialize`（`arb-mcp 0.1.0`）＋ `tools/list` で 17 ツール、stderr 0 B。
  - `arb-cli daemon stop` で `arbd` が終了し、残存プロセス 0 件。

## 4. ローカル品質ゲート

- `(cd ide-plugin && ./gradlew test buildPlugin --no-daemon)`: **ide-plugin 35 passed / client-core 81 passed、0 failed**
  - 新規 `ArbT6dUiGlueAuditTest` 17 件: 0 / 1 / 複数端末、固定端末の切断待ち、明示選択の永続化と自動選択の再選択、録画中 / 待機中のモード切替、`output_dir` 結線と解決、アプリ別フィルタ（陳腐化した応答の破棄、Legacy / 空のとき非表示）、Settings の 3 項目保持、破棄時の割り込みなし、メールボックス合流、キー一意化、テーマ Tone、About ビルド ID、ソース走査
  - client-core 新規 3 件: `build_id` 解析、固定タイムスタンプ ZIP でのサイズ差検知、1 セッション 1 回のループガード
- `./core/scripts/verify.sh`: 6/6 PASS（fmt / clippy `-D warnings` 警告 0 件 / `cargo test` **354 passed, 0 failed, 3 ignored** / license **GPL・AGPL 0 件** / `sara check --strict`（本書 `VER-22` を含む）/ REQ → SPEC 14/14）

## 5. 未検証項目（Unverified）

- **実機 Android Studio `2026.2.2.8` への ZIP ドラッグ＆ドロップ導入と GUI 操作**: このセッションでは IDE を起動していないため、Compose / Jewel 描画、Logcat 継承先の反射解決（Android プラグインのクラスローダー可視性）、IntelliJ のプラグイン展開がパーミッションと mtime を保持するかは未確認。保持されない場合も、`ArbdProcessManager.ensureUnixExecutable` が同梱バイナリに実行権限を付け直す。
- **物理端末 / エミュレータでの `logs_only` 再開、複数端末の固定、`output_dir` への実保存**: 単体テストとスタブ IPC で検証済み。実機確認は `docs/testing/work-device-smoke-checklist.md` で後日行う。
- **Linux / Windows ホスト**: ローカル検証は macOS arm64 のみ。3 OS の同等性は GitHub Actions `v2 CI` で確認する。
