---
id: VER-27
type: verification
name: T-6I Running Devices（Emulator + Mirroring）ツールバー・完全トリムバンドル出力・Tap/Swipe 焼き込み MP4（burn_taps）・統合通知・スタジオ終了 UX の検証記録
verifies:
  - SPEC-01
  - SPEC-02
  - SPEC-03
  - SPEC-04
  - REQ-04
  - REQ-07
  - REQ-08
  - REQ-09
  - REQ-11
  - REQ-12
---

# VER-27: T-6I Running Devices ツールバー・完全トリムバンドル & `burn_taps` 焼き込み MP4・統合通知・スタジオ終了 UX 検証記録

記録日: 2026-11-24（macOS arm64 / JDK 27 / Rust workspace `core/` / Android Studio `2026.2.2.8` 実 JAR 解析）

## 1. 目的

1. **Part 1 — Running Devices（Embedded Emulator + Physical Device Mirroring 両対応）ツールバー**:
   - Android Studio `2026.2.2.8` の実 `android.jar`（`META-INF/streaming.xml` + `AbstractDevicePanel` / `EmulatorToolWindowPanel` / `DeviceToolWindowPanel` バイトコード）を解析し、両パネル共通の公式外部拡張グループ `StreamingToolbarSecondary` へ重複なく 1 回アタッチされていることを確定。
   - `ArbToggleCaptureAction` を `DumbAwareToggleAction` 化し、全アクションに明示的 `icon="AllIcons..."`、ライブデーモン/端末状態に基づく誠実な `isEnabled` と日本語ツールチップを付与。
2. **Part 2 — 完全トリムバンドル出力 & 人間可読サイドカー & 純 Rust（GPL ゼロ）Tap/Swipe 焼き込み MP4（`burn_taps`）**:
   - 全保存バンドル・トリムバンドルに人間可読サイドカー（`logcat.log`, `app-logs/<package>.log`, `events.jsonl`, 非空時のみ `unanchored.log`）を出力し、`manifest.json` の SHA-256 検証対象に含める。
   - `export.trim` / `export.share_mp4` で省略時 `dest_dir` が `output_dir_override` → `managed_bundles_root()` の順で解決されるよう統一。
   - `export.trim` に `include_share_mp4` / `burn_taps` / `trash_original` を追加し、トリム時も完全なバンドルディレクトリ（無劣化 `video.mp4` + `audio.m4a` + `timeline.sqlite` + 人間可読サイドカー + 共有用 `share.mp4` / `share-taps.mp4`）を出力し `トリム済み` カタログへ即時登録。
   - `arb-media::burn_in` に純 Rust のアンチエイリアス RGBA バウンディングボックス描画 + BT.601 studio-swing YUV420p/NV12 直接合成を実装し、VideoToolbox（macOS）/ OpenH264（全 OS）の `reencode_frame_range_with_overlay` で GPL 依存ゼロのまま `edts=0/elst=0` faststart H.264+AAC MP4 を生成（タッチ 0 件時は無再エンコード高速パス）。
   - タッチ表示は書き出し先の動画に合成する。端末の開発者向け設定は変更しない。
3. **Part 3 — 統合リッチ通知（`ArbNotifications.kt`）**:
   - Tool Window・Studio FileEditor・Running Devices / Tools Menu アクションの全通知を `ArbNotifications` に集約し、HTML エスケープ・破棄済み Project ガード・非 EDT I/O・OS ゴミ箱専用削除（完全削除なし）・1 クリックアクション（`ビューアで開く` / `画像を開く` / `フォルダを開く` / `パスをコピー` / `ゴミ箱へ` + `元に戻す` / `元バンドルを復元` / `ARB パネルを開く`）を統一。
4. **Part 4 — Studio 書き出し後 UX & 明示的終了フロー**:
   - 書き出し中の進捗表示（`焼き込み書き出し中...` / `書き出し中...`）と二重実行防止、書き出し後のデュアルフィードバック（IDE バルーン通知 + インライン完了バナー）、完了バナーの `フォルダを開く` / `パスをコピー` / `結果を開く` / `タブを閉じる`、および書き出し後の未保存確認なし即時クローズと Tool Window カタログ即時更新を実装。

## 2. Android Studio `2026.2.2.8` 実 JAR 解析結果（Part 1）

- 検査対象: `~/.gradle/caches/9.8.1/transforms/.../android-studio-2026.2.2.8-rabbit2-mac_arm/plugins/android/lib/android.jar`
- `META-INF/streaming.xml` の定義:
  - `StreamingToolbarVirtualDevice`: `EmulatorToolWindowPanel` 専用メインツールバー
  - `StreamingToolbarPhysicalDevice`: `DeviceToolWindowPanel` 専用メインツールバー
  - `StreamingToolbarSecondary`: 外部プラグイン向け公式セカンダリツールバー（`AbstractDevicePanel` が `EmulatorToolWindowPanel` と `DeviceToolWindowPanel` の両方で `NOWRAP_STRATEGY` で構築）
- Layout Inspector（`META-INF/layout-inspector.xml`）および Compose UI Inspector も `StreamingToolbarSecondary` のみにアタッチしており、ARB も `io.github.shinma06.arb.RunningDevices` を `StreamingToolbarSecondary` へ 1 回だけアタッチすることで、Embedded Emulator と Physical Device Mirroring の両パネルにボタンを重複なく表示する。

## 3. ローカル検証結果

- **`./core/scripts/verify.sh`**: PASS（`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, GPL/AGPL ライセンスゼロ確認, `sara check --strict`, REQ カバレッジ全通過）。
  - `arb-media::burn_in` 単体テスト（Tap 波紋・Swipe 軌跡・奇数サイズ安全クランプ・0 タッチ高速パス・実ピクセル焼き込み差分 & `edts=0/elst=0` 検証）PASS。
  - `arb-storage` 統合テスト（`logcat.log`, `app-logs/<package>.log`, `events.jsonl`, `unanchored.log` 有無制御, `share-taps.mp4` サイドカー SHA-256 検証）PASS。
  - `arbd` E2E テスト（`output_dir_override` 出力先、完全トリムバンドル構造、`share-taps.mp4` 実ピクセル差分 + AAC 音声保持 + `edts=0/elst=0`、`export.share_mp4` の `-taps.mp4` 非衝突ファイル名、`trash_original` の LegacyRo 拒否と通常バンドル論理ゴミ箱移動）PASS。
  - `arb-cli` / MCP stdio 統合テスト（`--burn-taps`, `--include-share-mp4`, `--trash-original`, MCP `arb_export_trim` の `format="share_mp4"` / `"bundle"` 両対応）PASS。
- **`RUSTDOCFLAGS="-D warnings" cargo doc --manifest-path core/Cargo.toml --workspace --no-deps`**: PASS（警告 0）。
- **`./device-agent/build.sh --with-scrcpy-audio`**: PASS（`ProtocolTest` 19/19、`Aos9CompatTest` 2/2、DEX 2 種ビルド完了）。
- **`(cd ide-plugin && ./gradlew test buildPlugin --no-daemon)`**（JDK 27）: PASS（`client-core` + `ide-plugin` 全テスト通過、`ArbBrandingGuardTest` 100% 通過、`ArbCheckboxAlignmentGuardTest` 100% 通過、`ArbT6iRunningDevicesAndBurnTapsTest` 6/6 通過、`buildPlugin` ZIP 生成完了）。

## 4. 未検証

- 実機 Android 端末（USB / ワイヤレス ADB）および実 Android Studio Rabbit（`2026.2.2.8`）GUI 上での Running Devices（Emulator + Mirroring）ツールバー描画・バルーン通知クリック・Finder/Explorer 起動・`share-taps.mp4` 目視再生は、本自動化環境の範囲外のため未実施（実 JAR バイトコード解析およびヘッドレス単体・統合テストにより検証済み）。
- Windows 実機上での `arb-tray` / Android Studio GUI 目視確認は未実施（GitHub Actions `os-parity (windows-latest)` および `os-parity (macos-latest)` でビルド・全テストを検証）。
