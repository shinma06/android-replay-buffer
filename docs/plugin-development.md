# Android Studioプラグインの開発

`plugin/` は独立したKotlin/JVMのIntelliJ Platformプラグインです。Android端末で動くアプリではないため、Android Gradle Plugin、XML View/ViewBinding、Composeは使用しません。現在はToolsメニューの「Android Replay Bufferについて」で開発状況とロードした版を表示するだけです。

## 固定した開発環境

| 項目 | 値 |
| --- | --- |
| Plugin ID | `io.github.shinma06.android-replay-buffer` |
| Kotlin package | `io.github.shinma06.replaybuffer` |
| Android Studio SDK | Rabbit 1 `2026.2.1.8` / `AI-262.9437.185.2621.16467767` |
| 宣言するIDE範囲 | `262.9437.185` 〜 `262.*`。実受入は固定Rabbitで実施し、全patchの動作を保証した意味ではない |
| JDK / JVM target | 25 |
| Kotlin | 2.4.20、IDE同梱stdlibを使用 |
| IntelliJ Platform Gradle Plugin | 2.19.0 |
| Gradle Wrapper | 9.7.1、distribution SHA-256固定 |
| 開発版 | `0.1.0-dev.<source SHA>`。未commit差分があるbuildには `-dirty` が付く |

SDK版は[公式Android Studio一覧](https://plugins.jetbrains.com/docs/intellij/android-studio-releases-list.html)と対象IDEのproduct-infoで照合しています。Kotlinの[公式互換表](https://kotlinlang.org/docs/whatsnew2420.html)はGradle 9.7.0までを明記しており、9.7.1はpatch版をこのプロジェクトで実build検証します。JDK 25は対象IDEの最小要求に合わせています。

[公式のAndroid Studioプラグイン開発手順](https://plugins.jetbrains.com/docs/intellij/android-studio.html)に従いAndroid Studio SDKを使用します。現段階で利用するのは汎用Platform APIだけのため、Android API・Terminal・JCEF等の依存は追加しません。

## ビルド

JDK 25を用意して `JAVA_HOME` に設定します。macOSで対応Android Studioがインストール済みなら、その同梱JBRを使えます。

```bash
export JAVA_HOME="/Applications/Android Studio.app/Contents/jbr/Contents/Home"
python3 scripts/workflow/plugin_check.py
```

既定では固定SDKをGradleが取得します。初回はネットワーク接続とSDK展開用の空き容量が必要です。同じSDKが手元にある場合だけ、明示して取得を省けます。

```bash
REPLAY_PLATFORM_PATH="/Applications/Android Studio.app/Contents" \
  python3 scripts/workflow/plugin_check.py
```

`REPLAY_PLATFORM_PATH` は共通scriptが `-PuseLocalPlatform=true -PplatformPath=...` に変換します。Gradleを直接呼ぶ場合も両方指定します。個人のGradle設定に `platformPath` だけがあっても既定SDKを置き換えません。指定先が固定Rabbitと異なる場合はbuildを失敗させます。

標準コマンドは次と同じです。`check`のtestがNO-SOURCEでも、IDE受入成功とは扱いません。

```bash
plugin/gradlew -p plugin --no-daemon check buildPlugin verifyPluginStructure
```

ZIPは `plugin/build/distributions/` に出ます。原型PythonコードはZIPへ同梱しません。CIの `test` でも同じ変更影響判定から実行し、ZIPを14日保持のActions artifactとして保存します。これは検証用でありMarketplace/Release公開ではありません。長期保存する候補は保持期限内に取得し、SHA-256とsourceを記録してください。

## IDEでロード確認する

[GUI leaseと共有状態の手順](operations.md)を守り、別作業のAndroid Studio・ADB・daemonを操作しないでください。通常利用しているIDEへの上書きインストールより、Gradleの開発用sandboxと使い捨てfixtureを使います。

```bash
# GUI lease取得・固定buildとfixtureの確認後に実行
plugin/gradlew -p plugin runIde
```

`runIde`はsandboxを準備してIDEを起動します。自動起動は行いません。原型CLIの起動やUSB端末への接続もありません。

配布ZIPを受入する場合は、cleanな固定commitから一度 `buildPlugin` した同じZIPをsandbox IDEにInstall Plugin from Diskで導入します。先にZIPのSHA-256を記録し、ロード後にSettings → PluginsおよびTools → Android Replay Bufferについてで `0.1.0-dev.<固定SHA>` を確認します。再buildした別ZIPや `-dirty` 版を同じ候補の証拠へ流用しません。

[Case JSON](verification/changes/issue-3.json)のIDEロード・情報表示・IDE再起動を確認し、観察者・日時・ロード版・ZIP hashを記録します。Plugin VerifierによるAPI互換性検査やこのGUI受入は、ZIP構造検査と別です。初回基盤のGUI受入は未実施です。

## 次の実装順と責務

製品の動作と優先順は[製品要件](requirements.md)を正本とします。CLIの既存設定値や過去の設計から、将来プラグインの方式を自動決定しません。

1. **未決の設計を解決**: 保存失敗時のUX、Logcat内か独立ツールウィンドウかの配置、動画と両ログの同期方式・精度を各調査Issueで決める。共通IDE知見の該当範囲を調査し、実際の設計を `docs/plugin-design.md` に記録する。
2. **取得エンジンと接続契約を定義**: 追加ツールの手動導入不要・複数OSへの展開を満たす方式を選ぶ。原型の `replay_buffer/cli.py`・`ipc.py`・`daemon.py` と既存JSON IPCは調査・再利用候補であり、未変更のPython CLIへの接続を唯一の方式に固定しない。processの所有、接続先、timeout、取消し、入力サイズ・エラー・並行要求、依存ツールの提供を検証する。他の利用者が起動したdaemonを停止しない。
3. **初期版の取得・状態表示・保存を実装**: 有効化トグルと状態復元、接続自動取得、明示的な設定適用、180秒の実時間バッファ、両ログ、1ボタン保存、同期、自動復旧と同一シーケンスを要件に照合する。接続・process待ちはEDTで行わず、dispose後のUI反映を抑止する。macOSを先に実機/Emulatorで検証する。
4. **初期版候補を受入**: 同一ZIPで要件に対応するCaseとAPI互換性を確認し、developからmainへpromotionする。基盤の既存QAを含め、未実施のまま製品完成と扱わない。Marketplace公開は別の依頼範囲。
5. **macOSメニューバー連携へ最初に着手**: IDE内初期版の完成直後に実施する。その後のOS対応はWindows、Linuxの順を基本にし、後続TODOは指定優先度と利用者の計画に従う。

この順番は開発の入口です。未定の接続方式のためにservice/interface/DBや追加ライブラリを先に作りません。現段階の情報表示actionは状態・外部入力・非同期処理・DBを持たないため、入力検証、coroutineの並行性、DB安全性は適用対象外です。projectやViewを保持しないため長寿命参照はありません。Lifecycle・取消し・process停止は接続実装時に検証します。
