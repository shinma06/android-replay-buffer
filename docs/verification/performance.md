# Android負荷を優先する性能比較

[#224](https://github.com/shinma06/android-replay-buffer/issues/224)の測定方法案です。第一目標は、同じ条件のAndroid Studioでlogcatを記録している場合と同等以下のAndroid負荷。その条件下でホスト負荷を改善します。[製品責任](responsibility.md)・既存Caseの品質、保存窓、時刻、欠落表示、失敗時保全を維持します。本書の準備完了は実測・目標達成・第2版受入の完了ではありません。

## 現行経路と比較対象

sourceは `a49bef8ec4e934c24c1d7e16cf7bb9a953dd645f` に固定して調査しました。以下はこの版の事実であり、#206のstack変更や#274/#282/#283の変更後には該当箇所を再照合します。

| 経路 | Android側 | ホスト側・測定上の注意 |
| --- | --- | --- |
| OFF | ARBの画面・ログ・時計取得なし | `ReplayCore` の端末一覧pollは約1秒間隔。復旧journalがあれば所有資源のcleanupがあり得る。OFFを「ADB処理ゼロ」と定義しない |
| 動画＋両ログ | scrcpy 4 server、H.264、max_size=1920、max_fps=30、8,000,000bit/s、audio/control=false、B-frame=0、I-frame間隔指定1秒。実FPS/GOPは実測する | 圧縮packetをdiskへ保持。通常はmain/allのbinary log readerを併用し、allに含まれるmainをホスト側で除外。legacy headerはall readerへfallback。重複転送と出力重複を区別する |
| 時計・アプリ識別 | 開始時5回の時計sample後は約1秒。package/UIDとPID/UID/nameを照会し、各cycle後に1秒待機 | 起動直後と定常状態を分ける。appログは共有ログから導出し、別のapp専用logcatとは数えない |
| 寸法一定のSave | 取得継続 | `SaveWriter` が圧縮packetをremux。動画のdecode/encodeはしない。snapshotをpinし、出力をatomicに公開 |
| 寸法混在のSave | 取得継続 | `MixedVideoNormalizer` がJCodecでdecode/pad/encode。元映像sidecarとPTS/画素検証を保持。現行にホストHW変換モードはない |
| Save反復・失敗 | 取得継続／終了条件を記録 | WRITING中の追加Saveを拒否し、未処理要求を積まない。FAILEDはretry/discardまでpinを保持。失敗・取消し時の試行データも保全するため、反復時の総disk量は別途測る |

調査対象は [DeviceCapture](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/DeviceCapture.kt)、[ReplayCore](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/ReplayCore.kt)、[CaptureStore](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/CaptureStore.kt)、[SaveWriter](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/SaveWriter.kt)、[MixedVideoNormalizer](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/MixedVideoNormalizer.kt) とその呼出し・既存回帰試験です。

| 比較arm | 固定する状態 | 現在の実施可否 |
| --- | --- | --- |
| OFF | IDE/対象アプリ起動、ARB OFF、Studio logcat停止。復旧処理完了後 | 未測定 |
| Studio-logcat | ARB OFF、実際のStudioのlogcat記録経路・buffer/filter・表示/保存設定を記録 | 導入済みIDEで経路の同定が必要。独自のadb redirectをStudio記録と呼ばない |
| ARB-log | ログ中心モード、動画workerなし。ログ範囲・時計・アプリ識別は設定を記録 | 現行未実装。#274の固定成果物後に測定。動画取得後の出力削除で代用しない |
| ARB-AV | 動画＋両ログ＋時計、Studio logcatは停止 | 未測定。ARB-logの結果を転用しない |
| Save-remux / Save-convert | ARB-AV中、同じ保存窓・frame/log量、寸法一定／混在を分ける | 未測定。取得定常値、Save区間、完了後回復を別集計 |

動画のみは原因の切分け用候補です。現行にないモードを測定済みにせず、正式なStudio対比・製品の両ログ保証を置き換えません。

## 既存証拠の適格性

2026-10-17に保持原本を読み戻しました。以下は過去候補での観察であり、現在候補のbaselineや新しいPASSではありません。原証拠・失敗・期限超過の記録は変更しません。

| 出典・固定候補 | 確認できた事実 | 性能比較に使えない範囲 |
| --- | --- | --- |
| [G06観測](https://github.com/shinma06/android-replay-buffer/issues/14#issuecomment-6079086527)、source `1fbf8ef19c797b8e7b14c6b29a59ee2f7f0668ac`、ZIP `60f196accce3da09f2a4e08e5dd6186dc1715f7c5fb2ca40ac4a2ebc655d6de1` | 324 sample。CPUは累積CPU時間の差分で100%=1 core。AVD CPU中央値はOFF前10.382%（有効2点）、ON静止18.499%/18.063%（17/16点）、OFF後9.098%（4点）。入力を含む区間は別集計 | 324独立runではない。Studio armなし、warm-up/温度/順序統制なし。Emulator host CPUを実機Android CPUに換えない。許容差・改善率の根拠にしない |
| 同G06 | 観測内RSS最大はIDE 1,801,617,408B、AVD 2,565,947,392B。観測器自身CPU 3.26146秒、回収した子process CPU 23.435624秒、発行1,558 command。device clock/video identityを確認できないsampleはnull | 観測器のAndroid側負荷と全瞬間peakは不明。RSSはARB単独heapではない。ring/pin/tempの総量・電力・thermal・jankは未測定 |
| [回転保存native218](https://github.com/shinma06/android-replay-buffer/issues/14#issuecomment-6135357142)、source `3a9af24231c3db339c0b3a82ba8aa8eb4cecb189`、ZIP `ca74694b93c0b101e19348c9826a22d03285662fe5ada9e7006a94d2c59c173b` | 2回転・236frame・1Save。source packet 1,195,698B、出力MP4 24,792,461B。Save中の1点はIDE CPU 142.9%、RSS 2,500,272KiB | psの点観測は分離した区間CPUやpeakではない。約80.81秒はSave dispatch前からOFFまでの上限区間で、正確なSave latencyではない。変換時の容量増加を調べる入口に限る |

G06 sample原本SHA-256は `7742d67f54babdfcd2d11cbf2f9d9a354f35c1d58bea3acebf9e8381e9b1bb3e`、native218 reportは `ff430ac2100ea74fdab8f19cf997179fa961697d5e549a57b6942cc5c0a2e3ea`。媒体・raw log・私有pathは公開せず、担当が保持する原本へ対応付けます。過去の同期精度・全frame解析は品質の証拠であり、jank/消費電力/Studio同等負荷の証拠ではありません。

## 環境と再現条件

準備時に確認したhostはmacOS 27.0.1 / 26A434、Apple M5（10CPU core）、RAM 24GB。Android Studioインストール情報は `AI-262.9437.185.2621.16467767`、platform-tools 37.0.1、Emulator 37.1.11、既存Perfetto trace processor v58.2です。確認時点のIDE/Emulator/ADB server/Gradle daemonは起動していませんでした。これは実ロード・接続・測定可能性の証明ではありません。実機の列挙・起動・インストール・更新・録画は行っていません。

各run開始前に次を私有manifestへ固定します。変更があれば別条件とし、対象部分を再測定します。

- source SHA、標準ZIP hash、実ロードplugin版、IDE/JBR、stack/dependency、fixture APK hash・version、実行run ID。
- device model/OS/build/API、physical/Emulator、接続経路、画面寸法/refresh rate/輝度、電源/充電状態・電池残量、開始温度/thermal状態。端末serial等は私有記録のみ。
- ARBの保持秒数、動画設定実値と変更時刻、log buffer/filter、実encoder、対象アプリ、入力時刻列、開始/終了clock、host/device process identityと再起動。
- 観測tool版/hash・設定・周期・trace data source、hostのbuild/indexing/同期等、測定前後の空き容量、drop/loss/unknown。

[共有leaseと専用fixture](../operations.md)を使用します。leaseはGUIの協調予約であってCPU/I/Oの隔離ではありません。PMが#206 build・#281実験との静穏枠を調整し、区間中の予期しない競合は記録して比較対象から分離します。他担当のprocess/daemonを停止しません。汚染runを削除せず、除外理由と再実施を残します。

[既存marker fixture](../../tests/fixtures/replay-marker/README.md)の同じAPK・同じ入力列を利用します。静止、毎秒1回の番号/色変更、回転を別workloadとします。このfixtureの疎な全画面変化は、連続scroll/動画再生等の高運動量画面の代理にはなりません。高運動量と大量logは同じ再生可能な刺激を追加準備してから別条件で測り、私有アプリ/個人データを使いません。

起動直後を除外するだけでwarm-up済みとしません。予備測定でclock取得・buffer占有・CPU・thermalの推移を確認してwarm-up条件と測定長を固定し、本測定では全armに同じ手順を適用します。保持上限到達後とpruneは保持秒数を超えるrun、長時間driftは対応する長時間runで扱います。短時間pilotから900秒保持や長時間安定性を主張しません。

## 指標と観測手段

| 対象 | 記録する値・集計単位 | 手段・限界 |
| --- | --- | --- |
| Android CPU | 全deviceと対象アプリ、SurfaceFlinger、logd、ADB、ARB所有processのCPU秒/区間秒。100%=1 coreを明記 | Perfetto sched sliceをthread/processへ対応付け、区間端をclip。観測できないkernel/IRQ等はcoverageを明記。Emulator host CPUとは別 |
| Android RAM | process別RSS/PSSと全体、中央値・観測最大・開始終了差 | Perfetto process countersや同条件の低頻度dumpsys。RSS/PSS/heapを混同せず、process再起動を分離。dumpsysの影響も校正 |
| 画面/操作 | 対象layerのframe time、jank率・分母、入力→表示遅延 | Android 12+の[FrameTimeline](https://perfetto.dev/docs/data-sources/frametimeline)を利用可能なlayerで照合。app draw callback間隔をsystem present/jankへ読み替えない。未対応layerはunknown |
| 温度・電力 | thermal状態/温度の時系列、rail energy差分/時間、充電条件 | [Perfetto battery/rail](https://perfetto.dev/docs/data-sources/battery-counters)。対応railの範囲を明示。USB充電中の電池残量差やCPU率を総device energyに換算しない。Emulatorで実機の電力は証明しない |
| host CPU/RAM/GPU | IDE全体と関連processを分けた区間CPU、RSS/観測最大、利用可能なGPU値 | 累積CPU差分を単調clockで割る。JVM heapとprocess RSSは別。powermetrics等は実際の権限・対応項目を確認し、未取得を0にしない |
| I/O・転送 | 読書きbyte、disk/ring/pin/temp/成果物別量、転送byte/秒 | process単位が取れなければhost全体と明記。論理ファイル量と物理書込み・USB総量を区別。圧縮packet合計はlog/ADB overheadを含む総転送ではない |
| IDE共存 | 同じ編集/検索等の入力→反映、同じbuildの経過時間、UI stall | 静穏baselineとは別の共存試験。build cache・indexing状態と順序を固定。別実験のbuildを偶然重ねて比較しない |
| Save | 受付→atomic公開、受付→UI ready、継続取得中の遅延、成功/取消し/失敗別 | 自前の区間markerまたは対応する観測で両端を同定。送信前からのwall timeを精密なSave時間と呼ばない。run数と個別値を併記し、少数runのp95を確かなtail保証にしない |

まず低頻度の区間CPU/操作応答観測を共通にし、追加traceあり/なしの同じworkloadを対にして、host/device両側の増分と欠落を校正します。最小観測自体もcommand数・CPU・測定周期の変更による差を記録し、残る影響を不明として残します。全armへ同じ観測器を使うだけでは非線形な影響が相殺されたとは限りません。結果を変える影響なら周期・data sourceを減らして再校正し、品質の独立確認を残します。Android・hostの重いprofilerを常に全部重ねません。

Perfettoは[bufferのloss counters](https://perfetto.dev/docs/concepts/buffers)を確認します。対象区間のftrace overrun、writer packet loss、上書き/廃棄等があれば完全なCPU/jank記録とせず、不足範囲と再測定条件を残します。trace全体のlossが対象区間へ影響しないと主張する場合は、その境界証拠が必要です。

## encoderとHW利用

Androidでは実際のARB取得sessionに対応するMediaCodec生成・component名・codec・設定とprocess identityを記録します。現行はencoderを明示指定せず、manifestに実component名がなく、stderrを読むだけでも取得できるとは未確認です。利用可能なsession診断で同定できなければ「不明」のまま、#282へ最小の計測点を要求します。別途起動したscrcpyのencoderや能力一覧を、ARB sessionの実使用証拠にしません。

API 29+の[MediaCodecInfo.isHardwareAccelerated](https://developer.android.com/reference/android/media/MediaCodecInfo#isHardwareAccelerated())は製造元の分類で、正しさの実検証はされません。`HW報告あり / SW報告あり / 不明`とその根拠、実際の負荷を分けて記録します。名前のprefixだけで全端末のHW利用を保証しません。

ホストHW化は#283の候補で、現行の採用方式ではありません。採用検討時は同じ寸法/codec/画質/時刻条件でSWと比較し、CPU以外にGPU競合・copy・電力・Save時間・IDE応答を測ります。VideoToolboxなら実sessionの[UsingHardwareAcceleratedVideoEncoder](https://developer.apple.com/documentation/videotoolbox/kvtcompressionpropertykey_usinghardwareacceleratedvideoencoder)等で利用を照合し、能力列挙と区別します。

Auto/HW/SWを設ける場合の候補契約は、開始前の非対応・初期化失敗をAutoだけ明示的SW fallback、HW指定は明示失敗、途中失敗は未完成出力を成功公開せず元pinを保持して再試行、取消しは所有processと一時出力だけを停止・保全、です。途中から別encoderへ継ぎ足す暗黙fallbackは採用しません。非対応・開始失敗・途中失敗・取消しの既存失敗回帰へ適合した試験と代表的native経路を用意し、#283で方式決定時に確定します。

## 予算・回数・判定

現行の[ReplaySettings](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/ReplayModels.kt)はindexed video payload 1GiB、ログ会計32MiB、packet数60,000、config packet 64KiB / config memory 8MiB、空き容量判定128MiB、保持1〜900秒（既定180秒）です。混在変換の512MiBはframe/参照buffer見積りの拒否条件です。これらはIDE RSS・全disk量・実peakの上限でも、128MiBの予約でもありません。保護pin、sidecar、失敗試行の保全と同時に成立する容量を測ります。

性能予算は次の段階で確定します。

1. 各arm/workloadの探索pilotで観測の成立、warm-up、分散、温度drift、観測器の影響を確認する。ここでは効果判定をしない。
2. 同一条件の独立runを対にし、順序を入れ替える。予備値から検出可能な差と必要run数・集計量・片側区間の方法を選び、本測定開始前に#224へ固定する。run内の多数sampleを独立反復数にしない。
3. Androidの各主要指標で `ARB − Studio` を実単位と区間推定で示す。厳密な同等以下は不利方向の片側上限が0以下であることを目指す。未確定/不利な指標をCPU平均だけで相殺せず、品質条件もすべて維持する。
4. 実用上の許容差が必要なら、baselineの再現精度と再現性を損なわない用途上の理由を示す。測定ノイズだけで性能悪化を許容せず、「許容差内」と本人指定の厳密な「同等以下」を分ける。結果に合わせて許容差・除外・回数を変更しない。有意差なしを同等の証明にしない。
5. Android条件を満たした候補についてhost予算を提案する。CPU/RAM/Save待ちの数値予算は現時点で未設定。未対応の電力等はunknownとして実機条件/測定手段の不足を返し、全指標達成にしない。

本測定は事前のrun数で終え、都合のよい結果になるまで反復しません。予定外停止、loss、温度条件逸脱、欠落を全件残し、原因と次の検証を決めます。旧baseline再利用にはsource/artifact/設定/fixture/OS/IDE/device/観測方法の該当条件同一性とfreshな等価性確認が必要です。#206や最終buildの変更時は影響範囲を再測定します。

改善候補は仮説として以下へ渡します。効果は未測定です。

| 担当 | 候補 | 採用前に失ってはいけないもの |
| --- | --- | --- |
| #282 | 時計・process照会頻度、main/all重複転送、解像度/fps/bitrate/GOPの比較 | clock精度、再起動検出、両ログ完全性、文字・短い変化・画面遷移。bitrate低下をCPU比例低下と見なさない。ログ専用モードは#274へ |
| #283 | remux経路のcopy/scan、混在変換のSW/HW比較、I/O・pin/queue管理 | 元PTS・窓・gap/loss/uncertain、画素/可読性、atomic公開・失敗保全、取消し/復旧、EDT応答 |

既存の `CaptureStoreSaveTest`（byte ceiling・pin/保持）、`SaveWriterFailureTest`（失敗/取消し/衝突/保全）、`SingleMp4SaveTest`（混在寸法/PTS/全frame）等は保証の回帰根拠です。unit成功をnative性能や全資源上限の測定に換えません。反復SaveはWRITINGの拒否、完了後の再Save、FAILEDのretry/discard、容量不足を分け、要求数・pin・残存試行・全disk量と期待された保全を照合します。無期限失敗反復を無制限に実行せず、残量不足では新規試行を止めて既存データを保全します。

## 次の実測枠

PM調整用の最初の枠は最大40分です。枠外で固定ZIP/fixtureを準備し、既存資源のownerと復元条件を確認します。自動予約・定期実行ではありません。

| 時間配分 | 操作・得られる事実 |
| --- | --- |
| 最初の8分まで | lease、実ロードidentity、対象端末、Studio記録経路、実encoder識別/trace対応、温度・容量を確認。能力が不足したら当該数値を取れない理由を返す |
| 次の6分まで | 同一の疎なmarker操作で最小観測のみ/追加traceありを試し、起動過渡と観測負荷の成立を確認 |
| 次の18分まで | OFF→Studio-logcat→ARB-AVを各3分、その逆順を各3分。各arm冒頭1分は探索用warm-up、残り2分を暫定区間とする。warm-up不成立なら本測定値にしない |
| 残り8分 | trace loss・品質の一次確認、所有した取得の停止、設定/fixture復元、lease解放、未完了を記録 |

これは短時間・疎な画面変化の探索で、Save/長時間/高運動量/ARB-log/実機全指標の合格にはしません。予備runの生データは私有領域に置き、試行前後の容量と所有出力を照合します。取得物の目安上限2GiBを停止条件に使い、正常な停止・保全の余裕を別に確保します。上限は製品の保証値ではありません。静穏枠不足なら途中結果を保持して終了します。

実機/人間確認の既存延期は維持します。Emulatorで実行した場合はhost/手順成立の予備値に限定し、Android負荷目標を達成したとは扱いません。初回枠の結果から必要run数と後続のSave・実機・高運動量・長時間の不足事実を#224へ戻し、#282/#283の採用判断前に確定します。
