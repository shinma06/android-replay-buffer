# Android Replay Buffer (ARB)

Android 端末の画面・logcat・タップ・時刻情報を常時バッファし、不具合を見つけた瞬間の「直近 N 秒」をバンドルとして保存する開発者向けツールです。Android Studio（IntelliJ）プラグイン、CLI、MCP サーバー、トレイが同じ常駐デーモンと同じ保存処理を共有し、GUI・CLI・MCP の挙動は同一です。

Android Replay Buffer（ARB）は Rust コア・Android 端末エージェント・Compose / Jewel プラグインの 3 層で構成されます。製品の要件・仕様・検証記録は [`docs/`](docs/) にあります。

## 構成

| ディレクトリ | 内容 |
| --- | --- |
| [`core/`](core/) | Rust ワークスペース。`arbd`（常駐デーモン）、`arb-cli`（CLI と 21 ツールの MCP サーバー）、`arb-tray`（トレイ）、`arb-core` / `arb-os` / `arb-adb` / `arb-media` / `arb-storage`（共有ライブラリ） |
| [`device-agent/`](device-agent/) | 端末側 Companion（Java、`arb-companion.dex.jar`）と、パッチ済み scrcpy 音声取得 DEX |
| [`ide-plugin/`](ide-plugin/) | Android Studio / IntelliJ プラグイン（Kotlin、Compose / Jewel）。`arbd` / `arb-cli` / `arb-tray` と DEX を同梱した自己完結 ZIP（zsh 不要） |
| [`docs/`](docs/) | 要件（REQ）・仕様（SPEC）・検証記録（VER）。[`sara`](https://crates.io/crates/sara-cli) でトレーサビリティを検査 |
| [`fixtures/`](fixtures/) | 旧形式の保存済みバンドル（schema 3/4）に対する読み取り専用互換テスト用フィクスチャ |
| [`third-party/scrcpy/`](third-party/scrcpy/) | Apache-2.0 の scrcpy-server v5.0.1（SHA-256 固定）と音声取得パッチ |

標準の保存バンドル形式（schema 2）に加え、旧形式の保存済みバンドル（schema 3 `session.json` / schema 4 `replay.json`）は `arb-storage` が**読み取り専用**で開きます（書き換えません）。

## サポート対象（Support Matrix）

### Android OS

| 区分 | 対象バージョン | 利用できる機能 |
| --- | --- | --- |
| **最小サポート（コア機能保証）** | **Android 9（API 28）以上** | 画面録画・logcat・タップ・スクリーンショット・クラッシュ自動保存・画面遷移スパン（`am_*` / `wm_*`）・アプリ別ログ・トリム・MCP（既定では API 28〜32 の端末音声を自動的に `audio=false` / `Unavailable` へフォールバック） |
| **互換音声（明示選択）** | **Android 11（API 30）以上** | Settings の「音声キャプチャ方式」で「出力キャプチャ互換」を選ぶと、端末出力を録音します。録音中は端末スピーカーが消音されます。 |
| **推奨（全機能）** | **Android 13（API 33）以上** | 上記すべて + 端末音声キャプチャ（端末側の再生音を止めない `audio_source=playback audio_dup=true`）・音声付き A/V 再生・音声付き `share.mp4` 出力 |
| **非サポート** | **Android 8.1（API 27）以下** | 動作保証外（接続時に非ブロッキング警告を表示） |

CLI は `arb-cli config set --audio-capture-mode output_compat`、MCP は `arb_config_update` の `audio_capture_mode` で切り替えます。既定に戻す値は `playback_dup` です。録画中に切り替えると、同じ端末へ再接続して選んだ方式を試します。

### Android Studio プラグイン / 他 IDE

| 区分 | 対象環境 | 利用方法 |
| --- | --- | --- |
| **Android Studio プラグイン対応** | **Android Studio Rabbit（2026.2 / IntelliJ Platform `262.9437.185`〜`262.*`）** | 現行安定版の IntelliJ / Java 25 + JetBrains Jewel `composeUI()` 世代に追従。`arb-ide-plugin-<version>.zip` を導入 |
| **旧 Android Studio（Panda / Quail 等）・他 IDE（VS Code / Cursor 等）** | Linux / macOS / Windows | IDE 非依存のネイティブ **`arb-tray` + `arb-cli` + MCP** で全機能を利用可能（`arb-<OS>-<arch>.zip`） |

## 使う

配布物は [Releases](https://github.com/shinma06/android-replay-buffer/releases) にあります。`early` は `develop` の最新ビルド（プレリリース）です。

- `arb-<OS>-<arch>.zip`: `arbd` / `arb-cli` / `arb-tray` と導入手順（Linux・macOS・Windows）
- `arb-ide-plugin-<version>.zip`: Android Studio の `Settings → Plugins → 歯車 → Install Plugin from Disk...` で導入

署名なしの配布物を初めて起動するときの OS 側の確認は [`first-run-os-permissions.md`](docs/testing/first-run-os-permissions.md) を参照してください。

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

テストが通ることは、実機・IDE での受入済みを意味しません。未検証の項目は検証記録（`docs/verifications/`）に理由付きで残します。進め方は [AGENTS.md](AGENTS.md) を参照してください。

## ライセンス

Android Replay Buffer は **`MIT OR Apache-2.0`** のデュアルライセンスです。どちらか好きな方の条件で利用できます。

- [`LICENSE-MIT`](LICENSE-MIT)
- [`LICENSE-APACHE`](LICENSE-APACHE)
- [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md): 同梱する scrcpy-server（Apache-2.0）、OpenH264（BSD-2-Clause）、`rusty_aac`（Apache-2.0）、SQLite などの第三者ソフトウェアと帰属表示

この 3 つのファイルは、各 `arb-<OS>-<arch>.zip` と `arb-ide-plugin-<version>.zip`（`licenses/`）にも同梱されます。
