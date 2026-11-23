---
id: VER-18
type: verification
name: T-5 Android Studio ネイティブ統合・T-2B share_mp4/音声再生配線・arb-tray 刷新＆Windows フレーク修正 検証記録
verifies:
  - SPEC-01
  - SPEC-03
  - REQ-01
  - REQ-04
  - REQ-07
  - REQ-08
  - REQ-09
  - REQ-11
  - REQ-12
---

# VER-18: T-5 Android Studio ネイティブ統合・T-2B `share_mp4`/音声再生配線・`arb-tray` 刷新＆Windows フレーク修正 検証記録

記録日: 2026-11-23（macOS arm64 / Gradle 9.8.1 / Kotlin 2.4.21 / Android Studio SDK `2026.2.2.8` / Rust workspace `core/` / 実 `arbd` バイナリ結合）

## 1. 実装対象（許可範囲 `ide-plugin/**`・`core/crates/arb-tray/**`・本書のみ）

### 1.1 Android Studio ネイティブ統合（`ide-plugin/`）
- **Running Devices ツールバー（`StreamingToolbarSecondary`）**: JetBrains `AllIcons` 使用・絵文字ゼロの 3 アクションを追加（`ARB v2: キャプチャ開始 / 停止`、`ARB v2: 直前の記録を保存`（`ArbConfig.ide_default_save_action` と `save_window_secs` を尊重）、`ARB v2: スクリーンショットを保存`（`save.screenshot`＋Finder リンク付きバルーン通知））。
- **Logcat ツールウィンドウ（`Logcat.PopupActions`＋`ArbLogcatTitleActions` `ToolWindowManagerListener`）**: 映像キャプチャを中断せずに `save.logs`（`save_window_secs` 秒）を実行する `ARB v2: 直前のログのみ保存` をタイトルバーおよびポップアップメニューへ登録。
- **Settings → Tools → Android Replay Buffer（`projectConfigurable` id `io.github.shinma06.arb.settings`, `parentId="tools"`）**: `arbd` の `config.get` / `config.update` に直結する IntUI ネイティブ設定画面（`ArbSettingsPanel` / `ArbSettingsConfigurable`）を実装（`save_window_secs` 5〜600s、`ring_budget_bytes` / `min_free_disk_bytes` MiB・GiB 指定、`video_max_size`、`video_max_fps`、`audio_enabled`、`crash_autosave_enabled`、`crash_autosave_cooldown_secs`、`output_dir_override`、`ide_default_save_action`、および IntelliJ `PersistentStateComponent` `ArbLocalSettingsState` へ保存して `ArbdProcessManager` で PATH より優先解決する任意 `arbd` バイナリパス上書き）。デーモン未起動時はオフライン通知バナーを表示しつつローカルの `arbd` パス設定のみ編集可能に縮退。
- **バンドル自動オープン（`ArbBundleFileType` / `ArbBundleFileTypeDetector` / `ArbBundleFileEditorProvider`）＆ `ArbBundlePreCloseCheck`（`VirtualFilePreCloseCheck`）**: `manifest.json`（v2）および旧形式 `replay.json` / `session.json` について、親ディレクトリの兄弟ファイルと ARB マニフェスト固有フィールドの両方を厳格検査し、通常の `package.json` や PWA/Chrome `manifest.json` を乗っ取らずに ARB 統合スタジオで開く。さらに `TrimCloseState` により未書き出しのカスタム In/Out 範囲があるタブを閉じる時のみ確認ダイアログを表示し、未変更（全選択／初期セッション範囲）または書き出し済みの場合は無音で閉じる。
- **About & OSS ライセンスダイアログ（`ToolsMenu` → `Android Replay Buffer (v2) について`）**: プラグインバージョン、`arbd` デーモンバージョン・PID・接続状態、IPC プロトコルバージョン、および全 OSS ライセンス方針（MIT / Apache-2.0 / BSD-2-Clause / Unlicense、**GPL / AGPL 依存 0 件**）を表示。
- **T-2B `export.share_mp4` & `media.decode_audio_pcm` の統合スタジオ本配線**:
  - `ArbdRpcClient` と `TimelineViewerPanel` / `ArbBundleFileEditor` に `export.share_mp4` を既定書き出し経路（`共有用 MP4` / `WORM のみ` 切替可）として接続。高速開始 `elst=0` H.264+AAC `share.mp4`（トリム時は不変 WORM バンドル併設）を生成し、`trash_original` は非 Legacy バンドルのみ許可。完了バー（`Finder で表示` / `パスをコピー` / `新しいタブで開く`）を表示し、書き出し完了で `TrimCloseState` をクリーン化。
  - 音声トラックありのバンドルを開いた際、`ArbBundleFileEditor` が別ワーカースレッドで非同期に `media.decode_audio_pcm` を呼び出し、返却 `wav_path` を T-3 `WavFilePcmSource` → `AudioSyncPlayer`（実 `av_offset_ms` 適用）へ読み込み、`PlaybackController.attachAudioPlayer` で安全にアタッチ（音声なし／ホスト音声デバイス不在時はエラーなしで動画のみ再生へ縮退）。

### 1.2 メニューバー / システムトレイ（`core/crates/arb-tray/`）刷新＆Windows フレーク修正
- **`05-ui-ux-redesign-spec.md §3.4` 準拠（絵文字ゼロ・厳格秒表記）**:
  - `config.get` をポーリング対象へ追加し、`save_window_secs` および `video_max_fps` からステータス行（`REC <device> · 272s / 300s` / `待機中` / `arbd オフライン`）および主アクション（`直前 {N}s を保存`、`スクリーンショットを保存`、`直前のログのみ保存`）を秒表記で生成。
  - `最近の保存` サブメニューを最新 5 件のアクティブバンドルへ絞り込み、1 クリックで OS ファイルマネージャ（Finder / Explorer / xdg-open）を開く構成へ整理。
  - 録画トグル（`録画を開始` / `録画を停止`）および `トレイのみ終了（録画継続）` / `録画デーモンごと終了` へ統一。バックグラウンド `arb-tray-ipc` ワーカースレッドとコンテンツ安定 `MenuId`（`arb-tray:save:<N>` / `arb-tray:screenshot` / `arb-tray:save-logs:<N>` 他）を維持。
- **Windows CI フレーク（`tests/ipc_integration.rs` 再接続テスト）の根本原因と修正**:
  - **根本原因**: Windows Named Pipe 実装（`arb-os/src/ipc.rs`）ではサーバー側最終ハンドル Drop 時に `DisconnectNamedPipe` が実行され、パイプバッファ内の未読バイトが破棄される。`tests/ipc_integration.rs` のモックサーバーは Connection 1 の末尾のみ `writer.drain()` を呼んでいたが、**Connection 2 の末尾で `writer.drain()` を行わずに即 Drop していた**ため、Windows CI 上でクライアントが Connection 2 最終 RPC（`bundle.list`）を読む前にパイプが切断され、`TrayController::fetch_state` がオフラインへフォールバックして `assert!(recovered_state.daemon_online)`（290 行目）が間欠失敗していた。加えて Connection 1 の Drop 完了を待たずにクライアント側ステップ 4 を開始するタイミング競合も存在した。
  - **修正**: Connection 1 切断完了を `mpsc` チャネルで決定論的に同期し、Connection 2 末尾に `writer.drain()` とクライアント読了同期チャネルを追加。さらに 5 連続切断→再接続を繰り返す決定論的統合テスト `tray_controller_reconnects_deterministically_across_repeated_drops` を追加し、アサーションを一切弱めずに 3 OS 決定論化を担保。

## 2. ローカル品質ゲート検証結果

- `ide-plugin` Gradle テスト＆プラグイン ZIP ビルド（`(cd ide-plugin && ./gradlew test buildPlugin --no-daemon)`）:
  - `:client-core:test`: **65 件全 PASS（失敗 0 / スキップ 0）**（実 `arbd` バイナリ結合テスト 3 件含む：`t2bShareMp4WithTrashOriginalAndDecodeAudioPcmToWavPcmSource` で実 AAC-LC → `media.decode_audio_pcm` → `WavFilePcmSource` → `AudioSyncPlayer` / `PlaybackController` 接続、`export.share_mp4` + `trash_original`、Legacy RO 拒否を検証；`ArbT5IntegrationModelTest` 3 件；`ArbdProcessManagerTest` カスタムバイナリ優先順；`ArbT4StudioModelTest` 全 `.kt` 絵文字ゼロ検査含む）
  - ルートプラグイン `:test`: **9 件全 PASS（失敗 0 / スキップ 0）**（`ArbT5IdeIntegrationTest` 5 件：`FileTypeDetector` 誤検出防止、`PreCloseCheck`、`SettingsConfigurable` オンライン往復＆オフライン縮退、`About` OSS 表記、`plugin.xml` 拡張点＆絵文字ゼロ検査；`ArbUiComponentsTest` 4 件）
  - **Gradle 合計 74 件全 PASS**、プラグイン ZIP `ide-plugin/build/distributions/arb-ide-plugin-2.0.0-SNAPSHOT.zip`（**1,059,470 bytes / 1034.6 KiB**）生成成功。
- Rust ワークスペース品質ゲート（`./core/scripts/verify.sh` および `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`）:
  - `[1/6] cargo fmt --check`: PASS
  - `[2/6] cargo clippy --workspace --all-targets -- -D warnings`: PASS（警告 0 件）
  - `[3/6] cargo test --workspace`: PASS（`arb-tray` 全テスト PASS：lib 単体 24 件（秒表記・最新 5 件・`save.screenshot`/`save.logs`・絵文字ゼロ監査含む）＋ bin 3 件 ＋ 実 IPC 結合 2 件）
  - `[4/6] check-licenses.py`: PASS（GPL/AGPL 違反 **0 件**）
  - `[5/6] sara check --strict`: PASS（本書 `VER-18` 含む）
  - `[6/6] REQ → SPEC カバレッジ`: PASS（14/14）
  - `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`: PASS（警告 0 件）

## 3. 明示的な未検証項目（Unverified）

- **実 Android Studio GUI プロセス上でのライブ操作確認**: Running Devices / Logcat タイトルバー / Settings / Project からの `manifest.json` ダブルクリック / PreCloseCheck ダイアログ / About ダイアログ / `arb-tray` メニューバーの視覚・手動操作確認はヘッドレス JUnit 5 および実 `arbd` IPC 結合テストで契約・実装を検証済みであり、実 GUI 確認は後続のマルチ OS GUI 受入フェーズで実施する。
- **Linux / Windows 実機ランタイム実行**: ローカル検証は macOS arm64 で実施。Windows Named Pipe 再接続テストの決定論性および Ubuntu/Windows 3-OS パリティは GitHub Actions `v2 CI`（全 7 ジョブ）で確認する。
