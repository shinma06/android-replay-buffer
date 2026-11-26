# 第三者ソフトウェアの表示（Third-Party Notices）

Android Replay Buffer（ARB）本体は `MIT OR Apache-2.0` のデュアルライセンスです（[`LICENSE-MIT`](LICENSE-MIT) / [`LICENSE-APACHE`](LICENSE-APACHE)）。
このファイルは、配布物に同梱または静的リンクされる主な第三者ソフトウェアとそのライセンスを記録します。GPL / AGPL / LGPL を選択して利用している依存はありません。

## 同梱するバイナリ・コード

| 名称 | 版 | ライセンス | 用途 / 所在 |
| --- | --- | --- | --- |
| scrcpy-server（Genymobile / Romain Vimont） | v5.0.1 | Apache-2.0 | 端末側の画面・音声取得。未改変のバイナリを `third-party/scrcpy/scrcpy-server-v5.0.1` に置き、`device-agent/build.sh --with-scrcpy-audio` が `replay-scrcpy-audio.dex.jar` に取り込みます。ライセンス全文は [`third-party/scrcpy/LICENSE.txt`](third-party/scrcpy/LICENSE.txt)。 |
| 修正版 `AudioRecordReader.java` | scrcpy v5.0.1 由来 | Apache-2.0 | 音声バッファの PTS 計算の修正（[`third-party/scrcpy/AudioRecordReader.java`](third-party/scrcpy/AudioRecordReader.java)）。修正内容は [`device-agent/NOTICE.md`](device-agent/NOTICE.md)。 |
| OpenH264（Cisco）／ Rust クレート `openh264`・`openh264-sys2` | クレート 0.9.x | BSD-2-Clause | H.264 のデコードとエンコード（トリム・共有 MP4・タップ焼き込みの再エンコード）。ソースからビルドして静的リンクします。 |
| `rusty_aac` | 2.x | Apache-2.0 | AAC の処理（音声付き保存・書き出し）。 |
| SQLite（`rusqlite` / `libsqlite3-sys` の bundled ビルド） | rusqlite 0.35 | SQLite 本体はパブリックドメイン、`rusqlite` は MIT | 保存バンドル・カタログのメタデータ。 |
| `serde_path_to_error` | 0.1.x | MIT OR Apache-2.0 | RPC 引数のデコードエラーにフィールド名を付与。 |
| regex-lite | 0.1.x | MIT OR Apache-2.0 | MCP 入力スキーマの文字列パターン検証。 |
| Gson | 2.14.0 | Apache-2.0 | IDE プラグイン（`client-core`）の JSON 処理。 |
| Error Prone Annotations（Gson の推移的依存） | 2.48.0 | Apache-2.0 | IDE プラグイン ZIP の `lib/` に同梱される注釈のみのライブラリ。 |

## 開発・検証専用の依存（配布物に含まれない）

次のツール・ライブラリはテストと検証にだけ使います。プラグイン ZIP・CLI バイナリには同梱もリンクもされません。

| 名称 | 版 | ライセンス | 用途 |
| --- | --- | --- | --- |
| Konsist（`com.lemonappdev:konsist`） | 0.17.3 | Apache-2.0 | `ide-plugin` の `testImplementation`。アーキテクチャ規則 KON-1..5。 |
| Stateright | 0.31 | MIT | `arbd` の `dev-dependencies`。状態モデル S1–S3。 |
| proptest / tempfile | 1.x / 3.x | MIT OR Apache-2.0 | Rust の `dev-dependencies`。プロパティテスト G1–G7。 |
| Kani / Miri | Kani 0.68.0 / nightly | MIT OR Apache-2.0 | CI ツール（`.github/workflows/pdd-kani.yml`）。依存としては組み込みません。 |

OpenH264 について: Cisco 配布のバイナリに付く特許ライセンス（AVC/H.264 特許プール費用の Cisco 負担）は、Cisco が配布したバイナリにのみ適用されます。ARB はソースからビルドして静的リンクするため、H.264 に関する特許上の扱いは利用者・配布者が各自確認してください。

## OpenH264 の再配布条件

```text
Copyright (c) 2013, Cisco Systems
All rights reserved.

Redistribution and use in source and binary forms, with or without modification,
are permitted provided that the following conditions are met:

* Redistributions of source code must retain the above copyright notice, this
  list of conditions and the following disclaimer.

* Redistributions in binary form must reproduce the above copyright notice, this
  list of conditions and the following disclaimer in the documentation and/or
  other materials provided with the distribution.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND
ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR
ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES
(INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES;
LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON
ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```

## Rust 依存クレート

`core/Cargo.lock` に固定した Rust 依存（ワークスペースの本体クレートを含む 353 パッケージ）は、いずれも MIT、Apache-2.0、BSD-2-Clause、BSD-3-Clause、Zlib、Unicode-3.0、Unlicense、0BSD などの許容ライセンスです。ライセンスは `cargo metadata` から取得し、`core/scripts/check-licenses.py`（`core/scripts/verify.sh` の第 4 ゲート、CI の `test` / `os-parity`）で SPDX 式を評価しています。

- 検査結果: `license-check: OK (353 packages checked, 0 GPL/AGPL violations)`
- 複数のライセンスを選べる依存は、許容ライセンスの選択肢で利用します。たとえば `r-efi 6.0.0` は `MIT OR Apache-2.0 OR LGPL-2.1-or-later` の式で、MIT を選択しています。LGPL の選択肢は使っていません。
- 検査は GPL / AGPL / LGPL をいずれも拒否し、許容ライセンスで満たせない依存があると失敗します。

各クレートのライセンス全文は、その crates.io 配布物に含まれます。依存の一覧は次で再生成できます。

```bash
cargo metadata --manifest-path core/Cargo.toml --format-version 1 --locked
python3 core/scripts/check-licenses.py --manifest-path core/Cargo.toml
```

## ビルド時のみ使う道具（配布しません）

- Android SDK の `android.jar`（プラットフォームスタブ、Apache-2.0）と Build-Tools `d8`。端末側 DEX のビルドにだけ使います。
- JetBrains IntelliJ Platform / Android Studio / Jewel / Compose。IDE プラグインは実行時に IDE 側が提供するものを利用し、配布物には含めません。

## 連絡先

ライセンスや帰属表示に関する指摘は <https://github.com/shinma06/android-replay-buffer/issues> へお願いします。
