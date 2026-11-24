# Android Replay Buffer (ARB) v2

Android 端末の画面・logcat・タップ・時刻情報を常時バッファし、不具合を見つけた瞬間の「直近 N 秒」をバンドルとして保存する開発者向けツールです。Android Studio（IntelliJ）プラグイン、CLI、MCP サーバー、トレイが同じ常駐デーモンと同じ保存処理を共有し、GUI・CLI・MCP の挙動は同一です。

ARB v2 は Rust コア・Android 端末エージェント・Compose / Jewel プラグインの 3 層で構成されます。製品の要件・仕様・検証記録は [`docs/v2/`](docs/v2/) にあります。

## 構成

| ディレクトリ | 内容 |
| --- | --- |
| [`core/`](core/) | Rust ワークスペース。`arbd`（常駐デーモン）、`arb-cli`（CLI と 17 ツールの MCP サーバー）、`arb-tray`（トレイ）、`arb-core` / `arb-os` / `arb-adb` / `arb-media` / `arb-storage`（共有ライブラリ） |
| [`device-agent/`](device-agent/) | 端末側 Companion（Java、`arb-companion.dex.jar`）と、パッチ済み scrcpy 音声取得 DEX |
| [`ide-plugin/`](ide-plugin/) | Android Studio / IntelliJ プラグイン（Kotlin、Compose / Jewel）。`arbd` / `arb-cli` / `arb-tray` と DEX を同梱した自己完結 ZIP（zsh 不要） |
| [`docs/v2/`](docs/v2/) | 要件（REQ）・仕様（SPEC）・検証記録（VER）。[`sara`](https://crates.io/crates/sara-cli) でトレーサビリティを検査 |
| [`fixtures/`](fixtures/) | 旧スキーマ（schema 3/4）バンドルの読み取り専用互換テスト用フィクスチャ |
| [`third-party/scrcpy/`](third-party/scrcpy/) | Apache-2.0 の scrcpy-server v5.0.1（SHA-256 固定）と音声取得パッチ |

保存済みバンドルは v2 形式（schema 2）です。旧形式（schema 3 `session.json` / schema 4 `replay.json`）のバンドルは、`arb-storage` が**読み取り専用**で開きます（書き換えません）。

## 使う

配布物は [Releases](https://github.com/shinma06/android-replay-buffer/releases) にあります。`early` は `develop` の最新ビルド（プレリリース）です。

- `arb-v2-<OS>-<arch>.zip`: `arbd` / `arb-cli` / `arb-tray` と導入手順（Linux・macOS・Windows）
- `arb-ide-plugin-<version>.zip`: Android Studio の `Settings → Plugins → 歯車 → Install Plugin from Disk...` で導入

署名なしの配布物を初めて起動するときの OS 側の確認は [`first-run-os-permissions.md`](docs/v2/testing/first-run-os-permissions.md) を参照してください。

## 開発する

必要なもの: Rust（stable）、Python 3、JDK 27（`JAVA_HOME`）、`sara-cli`（`cargo install sara-cli --version 0.12.3 --no-default-features --locked`）、Android SDK の `android-37.2`（device-agent のビルド時、`ANDROID_HOME`）。

```bash
# Rust: fmt / clippy / test / ライセンス検査（GPL・AGPL なし）/ sara strict / REQ→SPEC カバレッジ
./core/scripts/verify.sh

# device-agent: ビルドとホスト側プロトコルテスト
./device-agent/build.sh --with-scrcpy-audio

# ide-plugin: テストとプラグイン ZIP（core/target/release と device-agent/build があれば自動で同梱）
cargo build --manifest-path core/Cargo.toml --release -p arbd -p arb-cli -p arb-tray
(cd ide-plugin && ./gradlew test buildPlugin)
```

CI（`.github/workflows/ci.yml`）は `test`（Linux の全検証）と `os-parity (macos-latest)` / `os-parity (windows-latest)`（Rust ワークスペースと ide-plugin の OS 同等性）を実行します。`develop` への push で `early` を更新し、`main` 上のタグ `vX.Y.Z` で正式版を公開します（`release.yml`）。

テストが通ることは、実機・IDE での受入済みを意味しません。未検証の項目は検証記録（`docs/v2/verifications/`）に理由付きで残します。進め方は [AGENTS.md](AGENTS.md) を参照してください。
