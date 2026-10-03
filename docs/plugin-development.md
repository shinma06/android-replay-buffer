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

1. **接続契約を定義**: 原型の `replay_buffer/cli.py`・`ipc.py`・`daemon.py` を調査し、既存daemonの所有、接続先、応答timeout、取消し、設定・出力先を決める。CLI出力の文字列解析より既存JSON IPCを候補にするが、サイズ境界・エラー・並行要求を先に検証する。
2. **状態表示を接続**: 専用fixtureでread-onlyのstatus表示を実装。接続・process待ちはEDTで行わず、project dispose後のUI反映を抑止する。他の利用者が起動したdaemonを停止しない。
3. **保存を接続**: 保存先を明示し、動画未準備のlog-only、タイムアウト、USB切断、保存失敗とデータ保全をCase化する。実機録画で動画/logcat/timelineの時刻対応を確認する。
4. **配布候補を受入**: 同一ZIPで必要なCaseとAPI互換性を確認し、developからmainへpromotionする。Marketplace公開は別の依頼範囲。

この順番は開発の入口です。未定の接続方式のためにservice/interface/DBや追加ライブラリを先に作りません。現段階の情報表示actionは状態・外部入力・非同期処理・DBを持たないため、入力検証、coroutineの並行性、DB安全性は適用対象外です。projectやViewを保持しないため長寿命参照はありません。Lifecycle・取消し・process停止は接続実装時に検証します。
