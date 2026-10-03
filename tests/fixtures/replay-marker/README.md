# Android Replay Buffer専用マーカーfixture

[初期版QA手順](../../../docs/verification/initial-version.md)で使う1プロジェクト・2アプリの使い捨てAndroidプロジェクトです。録画・ネットワーク・ストレージ・常駐処理はありません。イベント番号の描画境界を直接扱うため、Android標準のActivity/CanvasをJavaで共有しています。アプリ依存ライブラリ、Compose、Kotlin pluginは追加しません。

| 項目 | 固定値 |
| --- | --- |
| modules / debug applicationId | `appA` / `io.github.shinma06.replaybuffer.qa.appa.debug`、`appB` / `io.github.shinma06.replaybuffer.qa.appb.debug` |
| 共通namespace | `io.github.shinma06.replaybuffer.fixture`。自動対象はこれではなく実applicationIdで照合する |
| AGP / Gradle | 9.1.1 / 9.3.1（Wrapper配布SHA-256固定） |
| SDK / build tools / minSdk | 37.0 / 36.0.0 / 29。これより古い端末はこのfixtureでは検証できない |
| Java source/target | 17。今回のbuild実行には対象IDE同梱JBR25を使用 |
| log tag | `REPLAY_QA`。識別子はpackage + source + run UUID + view UUID + PID + event番号 |

AGP/Gradleの組合せは[公式互換表](https://developer.android.com/build/releases/agp-9-1-0-release-notes)に従います。Android Studioではこのフォルダを独立プロジェクトとして開き、`appA`と`appB`を通常のAndroid App Run configurationで選びます。開く／Sync／Run／インストールは共有GUI leaseとPM割当の後に行ってください。既存ユーザープロジェクトへmoduleを追加しません。

## buildと識別

既存SDKの`platforms;android-37.0`と`build-tools;36.0.0`、JDKを使用します。次はbuildだけです。ADB、install、Emulator、IDE起動は呼びません。

```bash
# repository root。SDK/JDKは準備担当が実環境に合わせる
export JAVA_HOME="/Applications/Android Studio.app/Contents/jbr/Contents/Home"
export ANDROID_HOME="$HOME/Library/Android/sdk"
python3 tests/fixtures/replay-marker/build.py
# 全依存がcacheにあるときだけ --offline を付けられる
```

Windowsでは同じPythonコマンドが`gradlew.bat`を使用します。Windows/Linuxでのbuild/動作確認は未実施です。初回は公式AGP/Gradle依存の取得が必要です。ツールを製品利用者へ手動導入させる手順ではなく、QA fixtureの開発用準備です。

build.pyは初回に専用debug署名鍵を`.private/qa.keystore`へ生成し、2 APKのassembleDebug/lintDebugとnative回帰APKのassembleDebugAndroidTestを実行します。alias/passwordはAndroid標準debug値で、実サービスの認証情報ではありません。鍵自体はGit/PRへ添付せず同じQA runで保持し、署名一致が必要な再インストールで別鍵へ置換しません。buildだけで通常の`~/.android/debug.keystore`を使用しません。IDEから直接Runする前にもこのbuildで専用鍵を準備してください。

APKは各moduleの`build/outputs/apk/debug/`、識別記録は`build/fixture-identity.json`です。記録にGit source SHA、dirty、各APKと回帰APKのSHA-256を残します。画面とlogにも同じsourceを出し、未commit変更があれば`-dirty`を付けます。最終候補はclean commitからbuildし、2 APK・identity記録・署名証明書fingerprintをprivateに保管して固定します。再buildでbytesが変わったAPKへ過去観察を流用しません。Plugin ZIPとfixture APKのhashは別々に記録します。

## マーカーの読み方

「イベント番号を更新」で画面の大きな番号と背景色を変更します。通常の同期測定では1秒以上間隔を空け、各番号の表示とcommit logを確認してから次へ進みます。連打ではREQUESTの一部が描画前に次のイベントへ置き換わり得ます。REQUESTがあるだけで動画にその番号が存在するとは判定しません。

logはJSONで、`phase`を区別します。Activityごとの`view` UUIDで再生成前後の同じ番号を区別します。

- `CREATE`: Activity作成。run UUIDとcounterは同一process内で維持し、回転再生成で同じ番号が再描画され得ます。view UUIDは再生成で変わります。process再起動では新しいrun UUIDになります。
- `REQUEST`: button callbackで更新を要求した時点。
- `DRAW`: Canvasへその番号を描く命令を発行した時点。再描画では初回だけ記録するため、回転後は同じ番号のDRAWが再び出る場合があります。
- `FRAME_COMMIT`: hardware rendererのcommit callbackを受けた時点。callbackのdispatch遅延を含みます。
- `NO_HARDWARE_COMMIT`: hardware renderingなし。commitを観察したと扱わず、同期精度測定の環境条件を見直します。

各phaseに、wall clockサンプルを挟む`elapsed_before_ns / wall_ms / elapsed_after_ns`を記録します。wallとelapsedの対応にはこのサンプリング幅とwallのms分解能が含まれ、logcatヘッダーの時刻やlog書込み時点とも同一とは限りません。壁時計変更・端末再起動前後で単一offsetを流用しません。

DRAWは表示完了ではありません。[frame commitの公式仕様](https://developer.android.com/reference/android/view/ViewTreeObserver#registerFrameCommitCallback(java.lang.Runnable))もswap chainへ提出された時点であり、画面に見えている保証はありません。FRAME_COMMITを真の表示時刻へ置き換えたり、REQUESTを動画の正解PTSにしたりしません。

保存動画で、各番号が最初に見えるフレームPTSとその直前フレームPTSを読みます。package/run/eventを両ログと対応づけ、#13の時計変換と精度基準を適用します。REQUEST→DRAW→commit callbackの差、callback dispatch、render→表示→録画の遅延、フレーム間隔、時計対応のサンプル幅を不確かさとして記録します。閾値より不確かさが大きければ「精度内pass」とせず、測定不能としてblockedまたは設計へ戻します。このfixtureだけで物理ディスプレイの実表示時刻を直接測定することはできません。

## 後片付け

GUI担当の所有と停止確認後、自分が導入した2 package・AVD・sandboxだけを扱います。共有ADB serverや他アプリは止めません。署名鍵、APK、IDEのlocal.properties、録画・logは非追跡で保管します。鍵を失った場合は専用fixtureだけのuninstall/再導入をPMと調整し、通常ユーザーアプリのデータを消しません。

## 描画順・Insetsの回帰

FRAME_COMMIT登録は`OnPreDraw`でそのフレームに描画予定の最新番号へ一度だけ行います。`onDraw`内には登録しません。更新前/終了時は未回収callbackを解除し、pre-draw listenerも終了時に解除します。rendererへ回収済みのcallbackが遅れて返っても古いview UUIDの記録であり、別のActivityへ書き替えずUIを更新しません。[固定AOSPの回収順](https://android.googlesource.com/platform/frameworks/base/+/refs/tags/android-16.0.0_r1/core/java/android/view/ViewRootImpl.java)を参照しました。

rootの16dp余白にsystem bars/cutoutの安全Insetsを毎回加算します。API30以上は型の合成、API29はsystem window Insetsとcutoutの辺ごとの最大値を使用し、前回paddingへ累積加算しません。[edge-to-edge公式仕様](https://developer.android.com/develop/ui/views/layout/edge-to-edge)に従い、識別表示と更新ボタンを保護します。

`RegressionInstrumentation`は追加testライブラリを使わず、実Android上で次を検証します。build.pyによる回帰APK生成と、端末での実行結果は区別します。

- 1回更新した後、追加タップ/描画要求なしでDRAW/FRAME_COMMITが届くこと。
- 同じUI処理内の2更新は最新番号だけを描画しcommitすること。
- 更新直後にActivityを終了し再開しても、新viewの番号とcommitが対応すること。
- system bars/cutoutの異なるInsetsと同じInsetsの再dispatchで、四辺のpaddingが正しく非累積であること。

lease/対象確認とQAの明示割当後にだけ、生成した対象APKとその回帰APKを専用対象へ導入し、各moduleのrunnerを実行します。runnerは自身のpackage/view UUIDのREPLAY_QAだけをlogcatから照合し、生logを出力せず結果を返します。実機/製品Caseのpassへ転記しません。3-button/gesture navigation、縦横/cutoutでボタンとidentityの実表示も別に確認します。
