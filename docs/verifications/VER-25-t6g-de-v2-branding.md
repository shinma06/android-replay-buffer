---
id: VER-25
type: verification
name: T-6G v2 製品ブランディング除去と docs/v2 フラット化（純 Android Replay Buffer 化）の検証記録
verifies:
  - SPEC-01
  - SPEC-04
---

# VER-25: T-6G De-v2 ブランディング整理・`docs/` フラット化 検証記録

記録日: 2026-11-24（macOS arm64 / JDK 27 / Rust workspace `core/`）

## 1. 目的

`develop` / `main` へ昇格させる製品コードベースおよびユーザー向けサーフェス（Android Studio プラグイン、CLI、MCP、トレイ、配布物・リリース ZIP、ドキュメントディレクトリ）から製品名としての「v2」表記を完全除去し、純粋な **Android Replay Buffer (ARB)** として統一する。

内部のストレージ／ワイヤ互換識別子（`schema_version = 2`、`ManifestV2`、`V2BundleReader`、`CatalogFormatVersion::V2`、`BundleFormat::V2`、`LegacySchema3` / `LegacySchema4`、および SemVer バージョン番号）は破壊せずそのまま保持する。

## 2. 変更内容

- **IDE プラグイン（`ide-plugin/`）**:
  - `ide-plugin/build.gradle.kts`・`plugin.xml`・`ArbActions.kt`・`ArbAboutDialog.kt`・`ArbBundleFileType.kt`・`ArbToolWindowController.kt`・`ArbLocalSettingsState.kt`・`ArbViewModels.kt` のユーザー向け名称・アクション・通知グループ・設定ストレージ・バッジ・来歴表示から `v2` / `(v2)` 表記を除去（`Android Replay Buffer` / `ARB: ...` / `ARB` / `WORM` / `arb-local.xml`）。
  - 回帰防止の自動ガードテスト `ArbBrandingGuardTest` を追加し、`plugin.xml`・`build.gradle.kts`・`src/main` 全ソース・ランタイム UI 文字列・`docs/` フラット化を常時検査。
- **Rust コア（`core/`）**:
  - `arb-tray`（`--help`、最近の保存バッジ `ARB`）、`arb-cli`（`--help`、MCP `SERVER_INSTRUCTIONS` および `arb_save` / `arb_save_logs` ツール説明）、全 8 クレートの `Cargo.toml` 説明文・クレート doc コメントから `v2` 製品表記を除去。
  - `core/scripts/package_distribution.py` の配布 ZIP 命名を `arb-v2-<os>-<arch>.zip` から `arb-<os>-<arch>.zip` へ統一し、ランチャー・`README-TRIAL.md`・`--self-test` を更新。
  - `.github/workflows/ci.yml`・`.github/workflows/release.yml` の成果物アップロード glob を `dist/*.zip` に更新（必須チェック名 `test` / `os-parity (macos-latest)` / `os-parity (windows-latest)` は不変）。また `ci.yml` の `early` ジョブを `develop`（`early` タグ）と `v2-develop`（`v2-early` タグ、昇格時に削除予定）の両ブランチへ対応させ、初回リリース作成と stale-head ガードを追加。
- **ドキュメントフラット化（`docs/v2/` → `docs/`）**:
  - `git mv` で `docs/v2/{model.yaml,sara.toml,requirements,specs,testing,verifications}` をすべて `docs/` 直下へ移動し、`docs/v2/` を削除。
  - `docs/sara.toml`、`core/scripts/verify.sh`、`core/scripts/package_distribution.py`、`README.md`、`AGENTS.md`、`docs/specs/SPEC-04-testing-and-harness.md`、`docs/requirements/REQ-05.md`・`REQ-07.md`、およびアクティブ参照パス（`VER-11` / `VER-21` / `VER-22`）を更新。

## 3. 検証結果

- **ゼロ `"v2"` 製品ブランディング検証**: `ide-plugin/src/main/`、`ide-plugin/build.gradle.kts`、`core/crates/`、`README.md`、`AGENTS.md`、`.github/workflows/` 全体で `ARB v2` / `(v2)` / `Android Replay Buffer v2` / `arb-v2-` が 0 件、かつ `docs/v2/` が存在しないことを確認。
- **`./core/scripts/verify.sh`**（macOS arm64）: 6/6 PASS（`cargo fmt` / `cargo clippy -D warnings` / `cargo test` / ライセンス検査 0 GPL/AGPL / `sara check --strict -c docs/sara.toml` / REQ→SPEC カバレッジ 14/14）。
- **`RUSTDOCFLAGS="-D warnings" cargo doc --manifest-path core/Cargo.toml --workspace --no-deps`**: PASS（警告 0）。
- **`python3 core/scripts/package_distribution.py --self-test`**: PASS（3 OS 配布 ZIP `arb-<os>-<arch>.zip`・ユニバーサルプラグイン結合・ゼロ `v2` 表記・SHA256SUMS 検証）。
- **`./device-agent/build.sh --with-scrcpy-audio`**: PASS（Java ホストプロトコルテスト全件 PASS、両 DEX JAR 生成）。
- **`(cd ide-plugin && ./gradlew test buildPlugin --no-daemon)`**（JDK 27）: PASS（`ArbBrandingGuardTest` を含む全テスト PASS、プラグイン ZIP 生成）。

## 4. 未検証

- 実機（USB）・Android Studio の GUI 目視受入は本変更の自動化範囲外のため未実施。
- `release.yml` のタグ実行および `develop` への push で動作する `early` ジョブは、本 PR のマージ先が `v2-develop` のため PR 時点では未実行（CI の `test` / `os-parity (macos-latest)` / `os-parity (windows-latest)` で 3 OS ビルド・テスト・パッケージングを検証）。
