# Android Replay Buffer専用マーカーfixture

[初期版QA手順](../../../docs/verification/initial-version.md)で使う1プロジェクト・2アプリの使い捨てAndroidプロジェクトです。録画・ネットワーク・ストレージ・常駐処理はありません。イベント番号の描画境界を直接扱うため、Android標準のActivity/CanvasをJavaで共有しています。アプリ依存ライブラリ、Compose、Kotlin pluginは追加しません。

| 項目 | 固定値 |
| --- | --- |
| modules / debug applicationId | `appA` / `io.github.shinma06.replaybuffer.qa.appa.debug`、`appB` / `io.github.shinma06.replaybuffer.qa.appb.debug` |
| 共通namespace | `io.github.shinma06.replaybuffer.fixture`。自動対象はこれではなく実applicationIdで照合する |
| AGP / Gradle | 9.4.1 / 9.8.1（Wrapper配布SHA-256固定） |
| SDK / build tools / minSdk | 37.2 / 37.0.0 / 29。これより古い端末はこのfixtureでは検証できない |
| Java source/target | 17（Android向けdesugar対象）。build実行はJDK 27 |
| log tag | `REPLAY_QA`。識別子はpackage + source + run UUID + view UUID + PID + event番号 |

AGP/Gradleの組合せは[公式互換表](https://developer.android.com/build/releases/agp-9-4-0-release-notes)に従います。Android Studioではこのフォルダを独立プロジェクトとして開き、`appA`と`appB`を通常のAndroid App Run configurationで選びます。開く／Sync／Run／インストールは共有GUI leaseとPM割当の後に行ってください。既存ユーザープロジェクトへmoduleを追加しません。

## buildと識別

既存SDKの`platforms;android-37.2`と`build-tools;37.0.0`、JDKを使用します。次はbuildだけです。ADB、install、Emulator、IDE起動は呼びません。

```bash
# repository root。SDK/JDKは準備担当が実環境に合わせる
export JAVA_HOME="/opt/homebrew/opt/openjdk/libexec/openjdk.jdk/Contents/Home"
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

## クラッシュ自動保存の専用入力

通常のlauncherは従来の`MarkerActivity`です。画面・番号更新・描画/lifecycle入力は変更せず、別の`CrashTestActivity`を明示的に選んだときだけ専用画面を開きます。新しい固定APKのbuild成功と実端末での入力確認・自動保存PASSは別です。計測中の既存APKや署名鍵を置き換えません。

PMの実行割当・GUI lease・固定source/APK/署名の確認後、Android Studioの「Specified Activity」で`io.github.shinma06.replaybuffer.fixture.CrashTestActivity`を選ぶか、所有端末へ明示起動します。appBを使う場合はapplicationIdだけを変更します。

```bash
adb -s OWNED_SERIAL shell am start -n io.github.shinma06.replaybuffer.qa.appa.debug/io.github.shinma06.replaybuffer.fixture.CrashTestActivity
```

起動自体はクラッシュしません。画面のpackage/source/process/PID/UID/runと、`REPLAY_CRASH_QA`の操作記録を照合してから、1つずつボタンを操作します。

| ボタン / 操作tag | 実際の入力 |
| --- | --- |
| 通常のERRORログ / `ordinary_error` | 対象アプリUIDからERRORを1件出し、画面へ戻る |
| catchする例外 / `caught` | アプリ内で例外をthrow/catchし、stackをログへ出して画面へ戻る |
| このThreadだけのhandlerで例外を処理 / `custom_handler` | 1回だけのworker Threadに個別handlerを設定し、そのThreadの未処理例外を受ける。Thread終了後に画面へ継続状態を表示する |
| この画面を通常終了 / `normal_finish` | `Activity.finish()`を呼ぶ。Androidがprocessを残す場合も、process終了とは偽らない |
| 未処理例外でこのアプリを終了 / `uncaught_main` | main-threadのbutton callbackから例外をthrowし、標準runtimeの処理へ渡す |
| :crash の試験画面を開く / `open_subprocess` | 同じAPKの非exportedな`:crash` Activityを起動する。表示された子processの帰属を確認してから、`uncaught_subprocess`ボタンでそのprocessの未処理例外を起こす |

handlerは[そのThreadにだけ設定](https://developer.android.com/reference/java/lang/Thread#setUncaughtExceptionHandler(java.lang.Thread.UncaughtExceptionHandler))し、process全体のdefault handlerは置き換えません。子Activityは[アプリ専用process](https://developer.android.com/guide/topics/manifest/activity-element#proc)で、外部から直接起動できません。入口は明示起動の専用画面だけで、通常markerにボタンや自動処理を足していません。NDK/ANR、service、shell UIDの偽ログ、常駐処理は追加しません。

最小のnative計画は、同じruntimeでLOG_ONLYのmain processとVIDEO_AND_LOGSの`:crash`を1回ずつ、計2保存です。既存ps/UID/clockの実際の準備を確認してから実行します。同runtimeの上表4 guardと、プラグインの手動対象をappAに保持したままのappB実クラッシュで自動保存しないことを確認します。標準header/typed am_crash/reporterが実際に対応形式でなければUNKNOWNを残し、手製の入力で埋め合わせません。2保存の全FILE検証はGUI操作と分け、[現行責任範囲](../../../docs/verification/responsibility.md)に従います。実行担当・容量・時刻・復元期限はPMが実行前に固定し、提案の所要時間を新しい一律受入条件にしません。

既存`RegressionInstrumentation`には非fatal入力・Thread個別handler・通常終了・子Activity宣言と、専用画面終了後の通常markerを確認する回帰を追加しています。fatalボタンはrunner自体を終了させ得るため、このrunnerでは押しません。回帰APKのcompileと端末での実行を区別し、実行前は未実施として保持します。

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


## 同期測定補助（macOS標準API）

`transitions.swift`はmacOS 26以降の[AVAssetReader output provider](https://developer.apple.com/documentation/avfoundation/avassetreader/outputprovider(for:))でローカルMP4を非同期に全量復号し、sampleのpresentation timestampと背景色の境界を抽出します。read-only pixel bufferの所有された読取り範囲で画素へアクセスし、Swift 6のactor隔離を保ちます。Xcode Command Line ToolsのApple Swift 6.4（Swift 6 language mode）とmacOS標準frameworkだけを使い、新しい依存・製品viewerを追加しません。GUI/録画/ADB/時計変換は呼びません。このOS条件はQA補助専用であり、配布CLIやプラグインの対応OSを変更しません。

```bash
# INPUTはprivateの固定出力。出力先は未存在の専用directoryを指定する
xcrun swift -swift-version 6 tests/fixtures/replay-marker/transitions.swift INPUT.mp4 NEW_PRIVATE_OUTPUT 0.15 0.55
```

座標は復号画像の左上原点、幅/高さで正規化した値です。初回の`transition-0000-first.png`で、5×5 pixel領域が背景内・番号/identity/端/回転後の余白にかからないことを確認してから採用します。背景2色へのRGB距離50以内だけをeven/oddとし、その他はunknownにします。codec/色変換でunknownになったら閾値を黙って緩めず、測定条件を調整して新runへ分けます。新directoryのみへ0600のJSONL/PNGを書き、途中失敗を完成結果にしません。生画像/ログは公開しません。

`frames.jsonl`に全復号frameのsample_index、CMTimeの整数value/timescale、us丸め値、色、境界、直前PTSを残します。色が変わるたびに最初のframeと直前frameをPNGへ保存します。最初の画像は初期状態で、eventの遷移件数には数えません。非単調PTS/復号失敗は停止、unknown件数も集計します。抽出sample数と製品frames indexの対応を確認します。MP4 edit listによりdecoded PTSがmedia/source PTSと同一とは限らず、source originを単純加算しません。製品のpart/edit/prerollとsample index、PTS差分を照合し一意対応できない場合はblockedです。

色のparityだけでは偶数個のイベント欠落を検知できません。全境界pairの**表示番号を人が読み**、package/run/view/eventのDRAW記録へ対応づけます。既知の300イベントを開始100/中間100/終了100（30分連続取得、各1秒以上間隔、各batchを期限内に保存）で発生させ、初期画像＋各遷移の前後2枚、最低601画像を確認します。期待した番号がなければ欠落、同じ番号が複数の初回遷移を持てば重複、unknown/読めない番号/時計対応不明はuncertainとし、いずれも300件の母数から除いて正常を装いません。回転/再生成はview UUIDで別系列に分けます。DRAWされなかったcoalesced REQUESTも別件数で記録します。全frameのVFR PTSを使い、30fps固定としてframe番号から時刻を算出しません。

各eventの最初の表示frame `F`、その直前 `P`を製品clock mappingで共通elapsedへ変換します。表示開始の録画上の区間は`(P,F]`、間隔`F-P`がframe観測の不確かさです。各phaseのelapsedはbefore/afterの中点、幅を保持し、`F-REQUEST`、`F-DRAW`、`F-FRAME_COMMIT`を**別列**へ記録します。callbackのdispatch遅延によりFRAME_COMMITがFより後でも自動補正しません。物理表示の真値は測れません。ログheaderのelapsed/record_id一致、時計sample読取幅/量子化/offset差による誤差も別列に保持し、host受信を発生時刻へ代入しません。

正常時計区間の全300件について、絶対差のnearest-rank p95（昇順285番目）・最大、開始100件と終了100件の符号付き差の中央値差を、REQUEST/DRAW/FRAME_COMMITそれぞれに集計します。#13基準はp95≤100ms、最大≤250ms、開始終了の中央値drift≤50ms、時計見積誤差≤20ms。frame間隔＋時計/phaseの幅が閾値を超える、300件揃わない、番号/PTS対応が曖昧な場合は精度passを出さずblocked/欠落として記録し、設計/実装へ戻します。単なる色境界抽出成功や合成VFR試験を製品精度passにしません。時計jump/sleep/bootは正常300件と別試験にして、境界跨ぎ補間禁止/null/epoch保持を確認します。
