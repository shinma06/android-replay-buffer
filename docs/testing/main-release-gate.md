# `main` 正式受け入れゲート（G-M1〜G-M9）

`develop` の内容を `main` へ昇格し、`vX.Y.Z` タグで正式版を出す前に、同じコミットで通すゲートの定義と合格基準です。
**`main` へのマージ、タグの作成、`LegacyV1SettingsMigrator.kt` の撤去は、オーナーが最終 GO を出した後の専用 PR だけで行います。** このドキュメントに沿った準備 PR は `develop` へ入れます。

実行は GitHub Actions の `release-gate.yml`（手動起動）と、Devin の Linux / macOS / Windows 環境で行います。テストが通っても、実機 USB 端末・Android Studio の実画面での受入が済んだことにはなりません。どこまで確認したかは `docs/verifications/` に境界つきで残します。

## ゲート一覧

| ID | ゲート | 合格基準 | 実行場所 / 証跡 |
| --- | --- | --- | --- |
| G-M1 | OSS 法務・ライセンス | ルートに `LICENSE-MIT`（Copyright 2026 shinma06）、`LICENSE-APACHE`、`THIRD_PARTY_NOTICES.md` があり、`core/Cargo.toml` の `license = "MIT OR Apache-2.0"` と一致する。3 OS の `arb-<os>-<arch>.zip` と `arb-ide-plugin-*.zip`（`licenses/`）に同梱され、`SHA256SUMS.txt` に載る。GPL / AGPL / LGPL 依存 0 件（`core/scripts/check-licenses.py`）。製品表示に旧版ラベルが無い | `package_distribution.py --self-test`、`verify.sh`、`verify_release_bundle.py` の `bundle-integrity` |
| G-M2 | バージョン整合と Plugin Verifier | `arbd` / `arb-cli` / `arb-tray` の `--version` が `core/Cargo.toml` と一致し、タグ `vX.Y.Z` と食い違う配布物は `release.yml` が公開前に止める。`plugin.xml` に `<change-notes>`、説明文は英字で始まり、任意依存は `config-file` を持つ。`verifyPlugin`（Android Studio `2026.2.2.8` = `AI-262.*`）が互換性エラー 0 件・内部 API 使用 0 件 | `release-gate.yml` の `plugin-verifier`、`./gradlew verifyPluginProjectConfiguration verifyPlugin` |
| G-M3 | 3-OS 配布 ZIP のコールドインストール | 組み立て済み ZIP を隔離ディレクトリへ展開し、`SHA256SUMS.txt` を全件照合、POSIX 実行ビット、ユニバーサルプラグインの 3 OS ネイティブ（`linux-x86_64` / `macos-arm64` / `windows-x86_64`）と DEX を確認。展開済みバイナリで `status` / `config get` / `config set` / `bundles list` / MCP（`tools/list` が 20 ツール、標準エラー 0 B、実バンドルの `timeline_overview` と `contact_sheet`）が通る。旧形式 schema 3 / 4 は読取専用で、操作前後の全ファイル SHA-256 が一致。`arb-tray` の起動維持・単一インスタンス・クリーン終了・残留プロセス 0。ランチャーの構文と実行 | `release-gate.yml` の `gate-linux` / `gate-macos` / `gate-windows`、`verify_release_bundle.py --check-tray` |
| G-M4 | Suite B 性能と焼き込み実画素 | FHD30 の 1 s GOP / 5 s GOP それぞれで `[47..=647]` の GOP 途中スプライス（601 フレーム）を展開済み `arb-cli` でトリムし、`devin_verify_player_matrix.py` が **17/17**（映像 11 + 同梱 `audio.m4a` 6）。`share.mp4`（焼き込みなし）、`share-taps.mp4`（Tap + Swipe 焼き込み）、トリム済み `video.mp4` も各 17/17。`share-taps.mp4` を ffmpeg で RGB に復号し、Tap / Swipe の期待座標に 300 画素以上の変化があり、下 200 px の対照窓は 5 画素以下、`share.mp4` と元 `video.mp4` にはマーカーが無い。親バンドルは SHA-256 不変。open / seek / trim の実測値を表で報告（性能バーは数値付きの指摘であり、超過だけでは FAIL にしない） | `verify_release_bundle.py` の `suite-b` / `burn-in`、`release_gate_summary.py` の 3-OS 比較表 |
| G-M5 | Linux 実環境の IDE GUI | Devin の Cloud Linux で Android Studio `2026.2.2.8` を Xvfb 上に起動し、プラグイン ZIP を導入して ARB ツールウィンドウ、Running Devices のアクション、バンドルの閲覧・トリム・共有 MP4 出力を操作する。動画で証跡を残す（手順: `devin-multi-os-test-plan.md` の Suite D） | Devin、動画と `SUMMARY.md` |
| G-M6 | macOS 実環境の IDE GUI と実機 / エミュレータ | macOS の Android Studio で同じ操作を行い、エミュレータ（API 37）または USB 実機で常時録画・保存・トリム・`share-taps.mp4` を確認する。QuickTime と Chrome で再生する | Devin、動画と `SUMMARY.md` |
| G-M7 | Windows 実 GUI | `windows-latest` で `ide-plugin` の全 Gradle テストが走り、`WindowsPipeSecurityTest.win32CurrentUserSidIsAValidSidOnWindows`（実 Win32 の Named Pipe 所有者 SID）が skip されず成功する。`verify_release_bundle.py --check-tray` が Named Pipe、`MoveFileExW` による保存、MCP 20 ツール、旧形式の読取専用、Suite B（OpenH264 + `nasm`）、17/17、焼き込み実画素、`start-arb.ps1` を通す。`arb-tray.exe` の実起動・単一インスタンスと、Edge・VLC・Media Foundation での `share.mp4` / `share-taps.mp4` 再生を、非黒の画面と録画で示す | `release-gate.yml` の `gate-windows`、アーティファクト `release-gate-windows` |
| G-M8 | 異種クロス監査とオーナー GO | 実装と別のエージェントが `codex review --base develop` と手動監査を行い、`entire trail finding` を実指摘は修正・それ以外は理由つきで解決する。必須チェック（`test`、`os-parity (macos-latest)`、`os-parity (windows-latest)`、`Entire Gates`）が緑。G-M1〜G-M7・G-M9 が同一コミットで緑。オーナーが明示的に GO を出す | Entire Trail、PR、`docs/verifications/` の VER |
| G-M9 | v2 → v2 アップグレード互換 | 下の U3〜U5 がすべて通る | `release-gate.yml`、Devin、`ide-plugin` の Gradle テスト |

### 性能バー（G-M4）

オーナー確認済みの目安は open ≤ 1 s、seek ≤ 0.2〜0.5 s、trim ≤ 数秒です。バーを超えたら FAIL ではなく「性能指摘」として、OS 間の差も含めて値つきで報告します。

## G-M9 v2 → v2 アップグレード互換（U3〜U5）

| ID | 経路 | 確認すること | 自動化 |
| --- | --- | --- | --- |
| U3 | v2-old → v2-new（稼働中 `arbd`、状態の保持） | 起動中の旧 `arbd` を新しい `start-arb.sh` / `start-arb.ps1` が置換する。`build_id` が新 `arbd` の SHA-256 に切り替わり、旧 `arbd` の残留 0。`config.json`（新フィールド欠落・未知の余剰フィールド、`capture_enabled=true`、任意の出力先）が保たれ不足分は安全な既定値で補われる。`catalog.sqlite` の全状態（保存済・トリム済・ゴミ箱・再試行待ち）が残り、アップグレード後の `bundle.restore` が成功する。旧 5 カラムの `timeline.sqlite`（T-6H 以前）が開ける。IDE 側は固定 mtime の ZIP 上書きでも `build_id` の SHA-256 不一致で `system.shutdown` → 新 `arbd` 起動となり、`ArbLocalSettingsState`（`trayEnabled` / `burnTapsOnExport` ほか）が残る | `verify_release_bundle.py` の `upgrade`、`ArbdInPlaceUpgradeIntegrationTest`、`ArbUpgradeRegressionTest` |
| U4 | `arb-tray` の `arbd` 再起動を跨ぐ継続 | 稼働中の `arb-tray` が `arbd` の置換・再起動の間もクラッシュせず、二重起動せず、ロックが固着せず、新 `arbd` へ「再接続しました」と出る | `verify_release_bundle.py --check-tray` の `upgrade` |
| U5 | 公開済みの前リリース `early` → 新ビルド（実バイナリ） | `release-gate.yml` が `early` の `arb-<os>-<arch>.zip` を取得し、`--upgrade-from-zip` で本物の旧 `arbd` を先に起動して新ビルドへ置換する。旧と新の `arbd` が同一 SHA-256 なら成立しない（失敗とする）。取得できない場合も自己検証へはフォールバックせず、失敗理由を明記して止める（`allow_missing_previous` を明示したときだけ「未実施」と警告して続行） | `release-gate.yml` の `gate-*` の「Fetch the previous release」と `--upgrade-from-zip` |

### ランチャーによる `arbd` の置換規則

`start-arb.sh` と `start-arb.ps1` は、同梱の `arb-cli` が選択した実行ディレクトリの `arbd` に接続します。CLI / MCP モードは起動引数の `--runtime-dir`、`--data-dir`、`--cache-dir` を置換先にも適用します。`system.hello` の `build_id` と同梱バイナリの SHA-256 が同じなら、IDE が起動したデーモンもそのまま使います。

SHA-256 が異なる場合、同梱版の SemVer 優先順位が稼働中の版以上なら置換します。ビルドメタデータは順位に影響しません。ハッシュを返さない古いビルドは、`system.hello` の `daemon_version` より同梱版が新しい場合に置換します。同じ IPC 接続で `system.shutdown` を送り、単一インスタンスロックの解放を最大 15 秒待ちます。

## ローカルでの実行

```bash
./core/scripts/verify.sh
cargo doc --manifest-path core/Cargo.toml --workspace --no-deps          # RUSTDOCFLAGS="-D warnings"
./device-agent/build.sh --with-scrcpy-audio
(cd ide-plugin && ./gradlew test buildPlugin verifyPluginProjectConfiguration verifyPlugin)
python3 core/scripts/package_distribution.py --self-test
python3 core/scripts/verify_release_bundle.py --self-test

# 配布 ZIP の実走（自 OS 向け）
cargo build --manifest-path core/Cargo.toml --release -p arbd -p arb-cli -p arb-tray
cargo build --manifest-path core/Cargo.toml --release -p arbd --example arb_gate_bundle
python3 core/scripts/package_distribution.py --target-os macos --arch arm64 --version 2.0.0-dev.local \
  --bin-dir core/target/release --device-agent-dir device-agent/build \
  --plugin-zip ide-plugin/build/distributions/arb-ide-plugin-2.0.0-SNAPSHOT.zip --out-dir dist
python3 core/scripts/verify_release_bundle.py --bundle-zip dist/arb-macos-arm64.zip \
  --plugin-zip ide-plugin/build/distributions/arb-ide-plugin-2.0.0-SNAPSHOT.zip --plugin-scope host-only \
  --check-tray --upgrade-from-zip <前リリースの arb-macos-arm64.zip> --out-dir release-bundle-out
```

既定ブランチへの初回昇格前は `gh workflow run ci.yml --ref <候補ブランチ> -f release_gate=true` で、前 `early` を使う 3 OS ゲートを起動できます。昇格後のGitHub の Actions から `Release Gate` を手動起動します（入力 `ref`、`version_label`、`previous_release_tag`、`allow_missing_previous`）。

## 実機・エミュレータが要るもの / 要らないもの

要らない（自動で通せる）: G-M1〜G-M4、G-M7 のうち Gradle テストと配布物の検査、G-M9 の U3〜U5 のうち ZIP とバイナリで完結する部分。
要る: 実 USB 端末の Android 9 / 13 での録画と音声、Android Studio の実画面（G-M5、G-M6）、macOS・Windows のデスクトップ上での目視再生。これらは Devin（`devin-multi-os-test-plan.md`）が動画つきで確認し、未実施の項目は VER に理由つきで残します。

## `main` 昇格 PR のチェックリスト（オーナー GO の後だけ）

1. **`LegacyV1SettingsMigrator` の撤去**（単一ファイルに隔離済み）
   - `ide-plugin/src/main/kotlin/io/github/shinma06/arb/ide/LegacyV1SettingsMigrator.kt` を削除
   - `ide-plugin/src/test/kotlin/io/github/shinma06/arb/ide/LegacyV1SettingsMigratorTest.kt` を削除
   - `ArbStartupActivity.kt` の `LegacyV1SettingsMigrator.migrateOnceIfNeeded(project)` の 1 行と、その KDoc の記述を削除
   - `ArbLocalSettingsState.kt` の `migratedFromV1`（`State` の項目、プロパティ、`loadState` の 1 行）を削除
   - VER / SPEC に残る migrator の記述を更新
2. **版の引き上げ**: `core/Cargo.toml` の `[workspace.package] version` を `X.Y.Z` に、`core/Cargo.lock` を更新、`ide-plugin/gradle.properties` の `devBaseVersion` を揃える。`release.yml` は `package_distribution.py --check-core-version` でタグと `Cargo.toml` の食い違いを止める
3. G-M1〜G-M9 を昇格コミットで再実行し、`release-gate.yml` の結果と Devin の SUMMARY を `docs/verifications/` の VER にまとめる（`sara check --strict` が通ること）
4. VM での IDE・エミュレータ受入が済んでから、`main` 上にタグ `vX.Y.Z` を打つ（SemVer）。プレリリースは `-alpha.N` / `-beta.N` / `-rc.N`
5. `early` は `develop` に追従するため手で編集しない
