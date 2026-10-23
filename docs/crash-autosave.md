# 対象アプリのクラッシュ報告による自動保存

[#284](https://github.com/shinma06/android-replay-buffer/issues/284) の契約と受入範囲です。既定OFFの設定を適用すると、取得中の対象アプリについて標準Java/Kotlinの未処理例外報告を照合し、現在の保持窓を既存の保存処理へ渡します。動画＋ログとログ専用の有効モードに従います。main反映・実機受入の完了記録ではありません。

## 検出と受付

単一のcrash bufferレコードにあるERROR/AndroidRuntimeの標準 `FATAL EXCEPTION` / `Process: …, PID: …` と、events bufferのtyped `am_crash`（30039、9フィールド、recoverable=0）を相互確認します。kernelのPID/UID、選択packageの排他的UIDと当時の帰属期間、process名、現在の取得世代・時計epochを照合します。platform報告は既存のps pollで確認したUID1000の一意なsystem_server PIDからに限定し、poll開始から受信まで2秒以内・poll完了済みを要求します。PID/userの先頭2整数は、独立に確認した値と一致する順序を採用します。

必須の型・長さ・終端・identityは厳密に確認します。不要な例外文・stack・file名はopaqueなbytesとして扱い、Modified-UTF-8等を含んでも必須部分が有効なら照合できます。保存する元record ID、payload、decode statusは変更しません。platform行は端末全体ログに残し、アプリの行と偽りません。形式や帰属が不明なら保存せず理由を残します。native crashとANRは初期対応外です。通常ERROR、handled exception、正常終了、単独の偽装ヘッダーでは保存しません。

既存readerから有界な観測領域へ渡し、既存control tickで処理します。既に受信した組を照合してから未対応観測を失効させ、受信時刻を処理時刻へ置き換えません。確実な別アプリ・別reporterは対象の容量・履歴・通知を消費しません。

| 境界 | 契約 |
| --- | --- |
| 観測 | 合計16件、コピーした証拠64KiB以内 |
| 照合 | 受信差と保守的な元時刻差が5秒以内 |
| 重複履歴 | 対象の終端記録256件。古い記録の追出しなし |
| 自動保存 | 明示的な自動保存OFF→ONあたり8受付、受付間隔30秒以上 |
| 上限到達 | 自動保存を停止して表示。手動保存は維持 |

これらは設計上限で、性能実測や出力総容量の保証ではありません。保存済み・失敗試行の出力は累積し、自動削除しません。

取得OFF中は設定だけを保持します。適用・対象変更・再接続・時計境界では古い未受付観測を無効化し、新しい適用境界以降の報告を使います。対象変更や一時的な再接続だけで重複履歴・回数上限を消しません。自動保存のみOFFにしても受付済み保存は完了できます。

## 固定保存と結果

受付ではStore内で一度だけ最新の時計換算を固定し、報告の適格性、出力行、`auto_trigger`へ同じ換算を使います。platform報告の不確かさ全体が、窓境界の不確かさも含めて窓内にある場合だけ1件のpinを作ります。時計不明・窓外・境界不明はpinを作りません。必須のplatform証拠行がログの容量上限で失われた場合も、無関係なログや動画だけで受付せず、元データ不在の理由を残します。受付判定時に手動保存中・失敗pin保持中・保存先なし・上限時は保存せず、その時点で拒否した報告を後から実行するqueueや自動retryは作りません。ログ専用の正常0行手動保存は維持しますが、自動保存は証拠も実データもない空保存を作りません。

基準はplatformの**報告時刻**で、processの死亡時刻ではありません。保存窓は受付時点の窓です。`auto_trigger`は両証拠の元時刻・受信時刻・固定換算・不確かさ・出力内有無、対象/reporting identity、適用revision、判定理由、報告から保存までの差、報告直前の希望区間に対する不足の範囲を記録します。先行Java行が窓外なら不在を明記し、完全なクラッシュ直前区間を保証しません。既存の窓・mode・gapは通常のmetadataを参照します。

`auto_trigger`は固定事実のみです。最終結果・保存先・attempt・完了時刻の予測を埋め込みません。writerは検証済みの結果をrequest/attemptへ先に記録し、control/UI callbackはその後に行います。OFF/disposeでcallbackがなくても結果を失いません。公開前の実際の取消、検証済み完了、rename後の検証失敗、終了待ち中の結果未確定を区別します。取消flagやタイムアウトだけで取消完了にしません。再試行は同じpin・窓・triggerを使い、attemptと出力先だけを新しくします。結果保持は当該coreの寿命内で、IDE異常終了後の復元journalは追加しません。

設定画面は下書き/Applyを維持し、ToolWindowは設定ON/OFFと有効・待機・上限停止、残受付数、対象報告の未保存件数、直近の結果と残る警告を分けて示します。生stackごとのpopupは出しません。失敗後の成功だけで警告を消さず、自動保存の明示的な再設定で解除します。

## 根拠と検証の分担

承認済み[契約v2の独立review](https://github.com/shinma06/android-replay-buffer/issues/284#issuecomment-6232598191)を、#274採用後のdevelop `83f58e96d5ce67ea6d242deb9e6b37cb2d224c09`へ接続します。契約manifest SHA-256は `49a6d379b2f63d0c33cf686e4775ddf11108cf71cfb88bb2410423e6e19e65b6`、契約は `daade070df4f03ca8edb22475486b785fc52cebc27369672d7c92c5c44aba9f4`。82宣言仕様は実行済みテストではありません。

固定AOSP revision `94b4c163b7dfe5ce3607f7bb8456f9573f7de57d` の [RuntimeInit](https://android.googlesource.com/platform/frameworks/base/+/94b4c163b7dfe5ce3607f7bb8456f9573f7de57d/core/java/com/android/internal/os/RuntimeInit.java)、[AppErrors](https://android.googlesource.com/platform/frameworks/base/+/94b4c163b7dfe5ce3607f7bb8456f9573f7de57d/services/core/java/com/android/server/am/AppErrors.java)、[EventLogTags](https://android.googlesource.com/platform/frameworks/base/+/94b4c163b7dfe5ce3607f7bb8456f9573f7de57d/services/core/java/com/android/server/am/EventLogTags.logtags) と [JNI event encoding](https://android.googlesource.com/platform/frameworks/base/+/94b4c163b7dfe5ce3607f7bb8456f9573f7de57d/core/jni/eventlog_helper.h) の保持済み一次資料を採用しています。これは対応候補形式の根拠で、全Android/OEMへの動作保証ではありません。新しいCursor知識は採用していません。

- 決定的検査: CrashProtocolTest / CrashAutosaveTest / CrashSaveLifecycleTest、既存ReplayCoreWireTestと設定・ProjectCapture/UI・保存/所有回帰を使用します。wire bytes、時刻/receipt/epoch、MUTF-8、別対象の大量入力、固定換算境界、遅延drain、単一pin/writer、retryと終了競合を具体入力で検査します。実行結果は固定PRの記録を正本とします。
- native最小計画: 固定source/ZIP/実ロード/端末buildと[専用fixtureの新APK/署名](../tests/fixtures/replay-marker/README.md#クラッシュ自動保存の専用入力)を同定し、標準JVM経路を各modeで1回ずつ観察します。LOG_ONLYはmain process、VIDEO_AND_LOGSは非exportedな`:crash`を使い、2保存でsubprocessも確認します。実際のheader・platform reporterの帰属、1保存、窓/不足と動画有無を確認します。同じruntimeで普通のERROR・handled・Thread個別の非終了custom handler・別アプリ・通常finishを各1回確認します。fixtureのbuildやrunnerのcompileはnative実行/PASSではなく、対応する実報告が得られなければUNKNOWNです。unsupported native用NDK fixtureは作りません。
- FILEと寿命: 上の2保存の全行/元payload/ID/時刻/帰属・triggerと全frame/窓/gap/hashを検査し、既存の完全性保証を保ちます。設定Apply/再openとOFF又は保存競合は、決定的検査後に残る実接続上の不足を1つに限定して同じfixtureで確認します。QA #298の同じbuild/条件の観察と対応づけられますが、別buildのPASSを転記しません。
- 長時間・負荷: 16件/64KiB/256件/8回の決定的上限検査は、RSS・全出力・実端末負荷の実測ではありません。#224/QA #298の担当が同じ候補で既存の有限計画へ自動保存ON/OFFと成功/失敗出力の集計を組み込み、時間・入力量・回数・容量停止条件を実行前に固定します。この割当では実測しません。

[4つの受入Case](verification/changes/issue-284.json)は未実施を保持します。#206の元FAIL/unknown、QA #65、#224/#282/#283と#280の版全体/main反映条件は変更しません。#103は将来のProjectService→ProjectCapture→Coreの利用者として既存のpending/resolving/closedと設定を保持し、この変更でCLI原型やCLI実装順は変えません。

上の#103の順序・CLI原型に関する記述は#284実装当時の範囲です。現在の同梱CLIと明示移行・復旧は[CLI契約](cli.md)を参照し、この文書の固定出典・元受入を新しい実行結果へ読み替えません。
