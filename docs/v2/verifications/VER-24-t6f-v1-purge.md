---
id: VER-24
type: verification
name: T-6F 旧 v1 プラグイン・スクリプト・文書の全削除と CI / リリース workflow の純 v2 化の検証記録
verifies:
  - SPEC-01
  - SPEC-04
  - REQ-06
---

# VER-24: T-6F v1 完全パージと CI / リリースの純 v2 化 検証記録

記録日: 2026-11-24（macOS arm64 / JDK 27 / Rust workspace `core/`）

## 1. 目的

`v2-develop` を、旧 v1（`plugin/` の Kotlin / Gradle プラグインと周辺の運用文書・スクリプト）を一切含まない純粋な ARB v2 リポジトリにする。v2 が実在のユーザー機能として持つ**旧スキーマ 3/4 バンドルの読み取り専用互換**（`arb-storage`）は残す。

## 2. 削除したもの

- `plugin/`（旧プラグイン一式。Gradle ビルド、Kotlin ソース、CLI 同梱資源、テスト）
- `scripts/`（`scripts/workflow/plugin_check.py` と `product_check.py`。どちらも旧 `plugin/` 専用で、v2 の検証は `core/scripts/` にある）
- `docs/` の旧文書（`docs/v2/` 以外すべて）: `android-studio.md`、`cli-origin.md`、`cli.md`、`crash-autosave.md`、`design/`、`legacy-ops/`、`mcp.md`、`plugin-design.md`、`plugin-development.md`、`project.md`、`release.md`、`requirements.md`、`requirements/`、`verification/`
- `.github/workflows/v2-ci.yml`（`ci.yml` に統合）
- ルートの `.gitattributes` / `.gitignore` の旧 `plugin/` 専用行

`sara`（`docs/v2/sara.toml`）と `core/scripts/verify.sh` は `docs/v2/` のみを参照しており、削除した文書に依存しない。

## 3. v2 が使っていた資産の移設（`git mv`、内容は不変）

`plugin/` 配下に置かれていたが v2 が実際に読んでいた資産を、ルート直下の専用ディレクトリへ移した。

| 旧パス | 新パス | 利用者 |
| --- | --- | --- |
| `plugin/src/test/resources/replay-trim-parity/` | `fixtures/legacy-schema4/replay-trim-parity/` | `arb-core` / `arb-storage` / `arbd` の Rust テスト、`ide-plugin` の Kotlin テスト |
| `plugin/src/test/resources/aac-playback-sync/stereo-44100.m4a` | `fixtures/aac-playback-sync/stereo-44100.m4a` | `ide-plugin/client-core` の実 IPC テスト |
| `plugin/src/main/resources/replay/deps/scrcpy-server-v5.0.1` | `third-party/scrcpy/scrcpy-server-v5.0.1` | `device-agent/build.sh --with-scrcpy-audio`（SHA-256 `764eb6f7…c536` の照合は不変） |
| `plugin/src/scrcpy/java/.../AudioRecordReader.java` | `third-party/scrcpy/AudioRecordReader.java` | 同上 |
| `plugin/src/main/resources/replay/licenses/scrcpy-LICENSE.txt` | `third-party/scrcpy/LICENSE.txt` | `device-agent/NOTICE.md`（Apache-2.0） |

パス文字列だけを更新した: `core/crates/arb-core/tests/legacy_fixtures.rs`、`core/crates/arb-storage/tests/storage_integration.rs`、`core/crates/arbd/tests/e2e_scenarios.rs`、`ide-plugin/client-core/.../ArbdRealIntegrationTest.kt`（リポジトリルート判定を `core/` と `fixtures/` に変更）、`ide-plugin/src/test/.../ArbT5IdeIntegrationTest.kt`、`device-agent/build.sh`、`device-agent/NOTICE.md`。製品コード・テストの期待値は変更していない。`VER-01/02/04/05` の該当パス表記も新パスに更新した。

## 4. CI / リリース workflow

必須チェック名は GitHub ルールセット（`develop`: `test` / `os-parity (macos-latest)` / `os-parity (windows-latest)`、`main`: `test`）に合わせて不変にした。

| workflow | ジョブ | 内容 |
| --- | --- | --- |
| `ci.yml`（push / PR: `main` `develop` `v2-develop`） | `test`（Ubuntu） | `core/scripts/verify.sh` 6 ゲート（fmt / clippy / test / ライセンス / `sara check --strict` / REQ カバレッジ）、`cargo doc`、補助スクリプト自己テスト、`device-agent/build.sh --with-scrcpy-audio`（`ProtocolTest: ALL 19 TESTS PASSED`）、`./gradlew test buildPlugin` とプラグイン ZIP 内容検査、Linux バンドル作成 |
| 同上 | `os-parity (macos-latest)` / `os-parity (windows-latest)` | Rust ワークスペースの fmt / clippy / test / doc / ライセンス検査 / 自己テスト / リリースバイナリ、macOS は ide-plugin 全テスト、Windows は `:client-core:test`、OS 別バンドル作成 |
| 同上 | `early`（`develop` への push のみ、`shinma06/android-replay-buffer` 本流限定） | `package_distribution.py --assemble-release` で 3 OS ネイティブ同梱のユニバーサルプラグイン ZIP と `device-agent/*.dex.jar` 同梱の対称 3 OS バンドルを組み立てて `early` プレリリースを置き換え |
| `release.yml`（タグ `vX.Y.Z` のみ） | `verify-tag` → `bundle-linux` / `bundle-native` → `publish` | SemVer タグ形式と `main` 祖先性を確認し、`--assemble-release` でユニバーサル 3 OS プラグイン ZIP と対称 3 OS バンドルを GitHub Release に添付 |

`v2-ci.yml` の 7 チェック（`rust-3os-parity` ×3、`sara-and-traceability`、`device-agent-host-tests`、`ide-plugin-tests`、`ide-plugin client-core tests (Windows)`）の検証内容は、`test` と `os-parity` が重複なく包含する。これにより同一コミットで CI が二重に走らない。`release.yml` はブランチ push（`v2-develop` を含む）では起動しない。

## 5. 検証結果

- `git ls-files plugin scripts` と `docs/` の `docs/v2/` 以外: いずれも 0 件。`git grep 'plugin/'`（`ide-plugin/`・`docs/v2/` の歴史的記述を除く）で旧 `plugin/` への参照が残らないことを確認した。
- `./core/scripts/verify.sh`（macOS arm64）: 6/6 PASS。Rust テスト 354 件 PASS・失敗 0、clippy 警告 0、ライセンス検査 `330 packages checked, 0 GPL/AGPL violations`、`sara check --strict` PASS（本書を含む Verification 23 件・Requirement 14 件・Specification 4 件）、REQ→SPEC カバレッジ 14/14。
- `./device-agent/build.sh --with-scrcpy-audio`（`plugin/` なし）: `ProtocolTest: ALL 19 TESTS PASSED`、`arb-companion.dex.jar` と `replay-scrcpy-audio.dex.jar`（scrcpy-server の SHA-256 照合 PASS）を生成。
- `(cd ide-plugin && ./gradlew test buildPlugin)`（JDK 27）: 116 件 PASS・失敗 0・スキップ 0、`arb-ide-plugin-*.zip` に `native/macos-arm64/{arbd,arb-cli,arb-tray}` と `device-agent/*.dex.jar` を同梱。旧スキーマ 3/4 フィクスチャを使う Rust・Kotlin テストも新パスで PASS。
- `ci.yml` / `release.yml` の YAML 構文を読み込んで検証した。GitHub Actions 上の実行結果は PR のチェックで確認する（`test` / `os-parity (macos-latest)` / `os-parity (windows-latest)`）。

## 6. 未検証

- GitHub Actions 上の Windows・Linux 実行は、本書作成時点では未実行（ローカルは macOS のみ）。PR の CI 結果で確認する。
- `release.yml` のタグ実行と `early` ジョブの新アセット構成は、`develop` 取り込み後・タグ付け時にしか実行できないため未検証。
- 実機（USB）・Android Studio の GUI 受入は本変更の対象外で、行っていない。
