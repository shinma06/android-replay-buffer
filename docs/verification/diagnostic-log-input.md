# 製品logcat原入力の有限診断（#106）

これは保存ログ欠落の切分けであり、原因確定・修正済み・正式QAの合格を表さない。固定基準05827d4と診断HEAD/ZIP/実ロード版を区別する。[Case](changes/issue-106.json)とIssue #106の具体的割当を正本とする。今回の実装だけでGUI/ADB/raw採取/導入・再起動を開始しない。

## 記録の境界

`DeviceCapture.logs`の既存Processが返す同じInputStreamを、明示opt-in時だけwrapする。追加read/reader/collector/ADB接続やSDK置換をしない。各readで返ったbyteだけをparserより先にRAMへコピーする。delegate内部で読まれたが例外で返らなかったbyte、未読pipe、logdの未送信まで観測したとは主張しない。

parser通過後の初期live条件を残し、旧tailとしてstoreへ渡さなかったrecordを区別する。storeの返すIDはgeneration条件を通りlogsへ追加した証拠であり、その後のbyte上限・prune・Save窓内保持の保証ではない。Saveの前後をcontrol側で記録し、FrozenCaptureの実record ID集合を保存結果と照合する。固定中に並行したstore完了は順序不明、Save後の到着は遅着として保持する。

時計2ms/40ms/5秒/20ms、parser条件、record ID、通常保存形式、CLIは変更しない。property未指定ではwrapper/RAM/fileがなく、元InputStreamを使う。診断callbackに製品の判定権を与えない。

## private opt-inと上限

QA本人が用意した新local private directoryの**symlinkを含まない絶対path**を、専用IDE profileのVM option `-Dreplay.diagnostic.logInputRoot=<owned-private-root>` に指定する。rootは本人所有0700でなければ拒否する。通常profileへ設定しない。root内の新規0600 `input.claim` が1回の採取を予約し、同rootの再利用・既存file上書きを拒否する。親symlink・root identity/owner/権限の変化も検査する。

- 取得中: raw最大8MiB、binary metadata最大4MiB、read receipt最大131072件、診断object作成から最大240秒。固定capacityのRAMへ保持する。metadata snapshotを作る終了処理の一時copyも有限。hot pathのdisk write/flushと診断workerはない。
- raw/metadata/read/time上限で古いbyteを上書きせず、reasonと不完全性を保持し、製品へは元byte/countを返し続ける。上限後を完全証拠として扱わない。
- 2接続目は診断不完全とし、そのInputStreamはwrapしない。異なる接続を1つの原streamと称さない。製品の既存復旧を改変しない。
- OFFは既存readerの終了を先に確認し、その後control workerからdumpする。reader停止確認の失敗は従来の製品例外として保持し、実行中readerのbufferを完成証拠にしない。診断書込/close失敗はpartialを保持して製品OFFを妨げず、自動再試行しない。通常closeのownerはOwnedAdbのまま。
- 有限byte量はdisk I/Oの時間保証ではない。OFF/dumpが終了しなければ未終了のownerと次条件を引き継ぎ、他processの一括killやlease横取りをしない。

停止後に`input.bin.partial`、`receipt.bin.partial`、最後に`receipt.json`をCREATE_NEW/0600で保存する。後者にsource/dirty/version、generation、private serial、connections、byte数、上限/例外のreason、完全性、両binaryのSHA256がある。`complete`は記録自体の完全性であり、全event存在・parser成功・正式同期精度の合格ではない。OFFによる最後の未完recordは原byteとして残り得る。書込失敗や設定不適合でreceipt.jsonがない場合も診断未達。以前のreceiptを代用しない。

生ログ・録画・binary・path・serial/host/token・秘密値はGit/Issue/PR/IDE log/通知/clipboard/remoteへ公開しない。原入力は他tagの秘密も含み得るので、採取の具体的な承認にはこの範囲を含める。公開結果は状態・件数・opaque参照のみ。

## binary receipt（schema 1）

DataOutputStreamのbig endian。型1byteに続いて以下を読む。時刻はhost monotonicであり端末event時刻に代用しない。read範囲は原binaryのbyte offsetで、parserが消費する順序を表す。文字列はwriteUTF、製品IDは正規UUID36文字。

| 型 | body |
| --- | --- |
| R | begin:long、returned count:int（EOFは-1）、read開始/終了:long×2 |
| P | begin/end:long×2、時刻:long、live:boolean（storeへ渡す初期gate結果） |
| S | begin:long、store開始/完了:long×2、accepted:boolean、trueならrecord ID:UTF |
| B | Save固定処理直前:long |
| N | 固定対象なしの時刻:long |
| F | 固定後時刻:long、save ID:UTF、sequence:UTF、窓start/end:long×2、generation:long、row count:int、全record ID:UTF×count |

原streamをofflineで読む場合は同じ診断ZIPの既存readLogを利用し、新parserや途中再同期を作らない。原stream不完全時や固定中の順序不明を正常母数から除外しない。

## 有限nativeの予定（別途承認後）

build/check/reviewと新profile/root準備を先に完了する。shared leaseは[operations](../operations.md)に従う。専用新profileで標準診断ZIPを配置1、IDE起動1・終了1、既存IDE再起動0・plugin再導入0。既存QA fixture AのAPKは更新0、Emulator再起動0。診断product HEADとfixture05827d4の組合せを明記し、同sourceを要求する正式受入へ流用しない。

全体600秒、取得240秒、lease更新0、追加試行0。初期準備180秒以内に実ロードidentity/標準SDK adb/fixture source・署名/run/view/PID/current E/lease/rootを確認し、対象A・N180・private保存先を設定してON1。準備未達なら入力0で中止し復元する。

既存controllerの最小適合版でfixture sourceと診断product/lease HEADを別bindingにする。SDK adbと観察commandsは維持する。controllerの別logcat dumpはfixture存在確認であり同じ製品原入力の代用ではない。100入力（E+1〜E+100）、15×6+10 block、間隔最低1.2秒、全300phaseの母数とguard/deadlineを保持し、入力区間最大150秒。未入力/欠落/unknownも残す。

最初の入力から160秒以内を目標上限にSave最大1。実FrozenCaptureで窓内を検査し、拒否/失敗を追加Saveで救済しない。clock gap/unknownは保持し、canSave falseを強行しない。全体420秒以内にOFF1へ進み、停止/dumpを確認する。残る180秒を新profile設定/opt-in解除、診断IDE通常終了、fixture HOME1・元QA IDE focus復元1、所有reader/pin/未保存確認、lease release1に確保する。元QA profileと既存IDE/Emulator/ADB serverを変更・停止しない。

## 判定と残条件

fixtureに存在するが完全な原streamにない場合は、parserへ返る前のdelivery/read境界まで。原byteにありparser通過にない場合はframing/取得例外まで。store受理完了がSave前なのにFrozenにない場合は保持/窓選別まで。Frozenにあり保存IDにない場合はwriter/publicationまで範囲を絞る。初期gate・遅着・固定中の順序不明・複数接続・上限・書込失敗は別に保持する。未再現なら旧欠落は未解決のまま。

既存CaptureProtocolTest、CaptureStoreSaveTest、ReplayCoreWireTestを適合し、JVMとnativeを別証拠にする。固定HEAD/baseの独立reviewは別セッション。#27/#87の正式300event/30分・人間/REAL・同期・GOP等の未完了は維持し、本診断だけでIssue close/main promotionへ進まない。
