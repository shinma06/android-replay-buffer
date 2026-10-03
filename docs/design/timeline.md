# 動画と両ログの時刻・録画方式

[要件](../requirements.md)のENV-03、BUF-01〜07、LOG-01/02、SYNC-01、SAVE-01/02、VIDEO-01/02/04に対する初期版の実装契約。#13の設計であり、取得機能の実装・実機合格ではない。保存失敗UXは#11、UI配置は#12が正本を持つ。CLI原型のコード・設定・出力は変更しない。

## 採用する方式

**固定scrcpy serverのH.264/PTSをJVMで受け、MP4の連続区間と2種類のJSONLログ、時刻対応JSONを保存する。** Android SDKのadbを絶対パスで使い、server・小さな時計測定用DEX・JCodecをPlugin ZIPへ同梱する。利用者のscrcpy/ffmpeg/Python導入、PATH設定、実行時ダウンロードは不要。音声・操作注入・clipboard連携は無効。独自の録画・H.264実装、専用ビューア、常駐HTTPサーバーは作らない。

| 録画候補 | 比較と判断 |
| --- | --- |
| Android標準screenrecord | 追加hostツール不要。ただし公式に180秒の制限・回転の制約があり、プロセスを繰り返す取得は起動の空白を作る。raw H.264だけでは元PTSを保持できない。初期版の常時取得方式として不採用。[公式adb](https://developer.android.com/tools/adb#screenrecord) |
| 公式scrcpy配布＋ffmpeg | macOS両archの公式scrcpy配布はある。録画・muxの成熟度が利点。一方、通常の録画では最初のPTSを引くため、元PTSのsidecar取得が別途必要。ffmpeg提供・各OSのnative実行物管理も増える。未変更CLIへの接続やプロセス開始時刻での補正はSYNC-01を満たす根拠にならない。[公式PTS変更](https://github.com/Genymobile/scrcpy/commit/1c82c3923d63985655686dd5884a7a9e9407619e)、[公式配布](https://github.com/Genymobile/scrcpy/releases/tag/v4.0) |
| **固定server＋JVM mux** | 元PTS/config/keyframe/session packetを直接保持できる。native host codec不要、OS展開時も同じwire/muxを使える。内部protocolの固定・parser・時計測定・MP4再生受入をこちらで担う。H.264限定の小さな受信処理と既存muxライブラリで成立するため採用。[公式protocol](https://github.com/Genymobile/scrcpy/blob/2322868e9e256eb5fce0b3d659ab2a409f29bae1/doc/develop.md) |

依存の固定値はscrcpy **4.0 / commit `2322868e9e256eb5fce0b3d659ab2a409f29bae1`**、公式`scrcpy-server-v4.0`、SHA-256 `84924bd564a1eb6089c872c7521f968058977f91f5ff02514a8c74aff3210f3a`。host muxは **`org.jcodec:jcodec:0.2.5`**、JAR SHA-256 `890329dad124e8b739c1d6602a59a53c8a474daddff265c2561e21c498496c81`を候補固定する。JCodecは映像の再エンコードには使わず、AVC config処理とMP4 mux/demuxだけを利用する。version更新はwire互換・可変PTS・edit list・再生Caseの再実施を伴う。[JCodec一次source/ライセンス](https://github.com/jcodec/jcodec)、[固定Maven POM](https://repo.maven.apache.org/maven2/org/jcodec/jcodec/0.2.5/jcodec-0.2.5.pom)

実装buildで、公式serverのhash照合、JCodec dependency verification、Apache-2.0のscrcpy LICENSE/NOTICEとFreeBSDのJCodec LICENSE同梱、時計DEXのsource/build/hash記録を行う。JCodecは通常の`implementation`依存としてPlugin ZIPへ同梱し、Plugin専用classloaderでロードする。[JetBrains公式](https://plugins.jetbrains.com/docs/intellij/plugin-class-loaders.html)はIDE/他Pluginと異なるlibrary versionを使えると説明している。shadingは初期版へ追加せず、固定IDEでclassloaderを含め実受入する。実衝突を再現した場合だけ依存境界の修正を比較する。これらのbuild変更は今回行っていない。ZIPに同梱できない依存を後から利用者へインストールさせるfallbackは採用しない。

adbは対象projectの設定済みAndroid SDKの`platform-tools/adb`（Windowsでは`adb.exe`）を解決する。SDK/adbが見つからない場合は設定済みSDKの確認を案内し、PATH検索やHomebrew導入へ流さない。adb serverはIDE等と共有するため`kill-server`は禁止。forward port、device上のDEX/JAR名、serverのscidは所有する取得generation固有とし、他のscrcpy/録画/forwardを停止・削除しない。shell文字列にpackage/serial/pathを無検証で連結しない。端末packetは信頼しない外部入力として読み、video packet上限16MiB、log entry上限5KiBを初期上限にする。整数overflow、unknown flags、header長、payload長、EOF途中、nonce不一致を検証し、巨大alloc/無限待ちを防ぐ。上限を超えたstreamだけを失敗へ移し、正常streamを巻き添えにしない。EDTで接続/read/muxを行わず、取消しと所有processの実終了を別に確認する。無効化後に遅れてできたprocess/socketはそのgenerationの責任で閉じ、古いcallbackを新generation/破棄後のUIへ反映しない。

serverは`audio=false control=false video_codec=h264 send_frame_meta=true send_stream_meta=true`。`raw_stream=true`はPTSを失うため禁止。scidとforwardのdummy/device metadataも固定4.0仕様に従う。session packetでサイズ/configが変わったら別MP4区間を開始する。B-frameなしを`video_codec_options=max-bframes:int=0`で要求し、実際のPTSが非単調なら動画側を不適合として復旧し、ログは継続する。最長辺1920、30fps上限、8Mbps、I-frame間隔1秒を**試験開始値**とする。VIDEO-01の可読性/負荷合格後に実装Issueで固定し、端末encoderが指定を無視した場合も観測値を記録する。実機・Emulatorの対応OS/APIを、この設計だけで保証しない。

## 時計の契約

端末の時計を同じものと思い込まない。日時は表示・フォルダ命名用、取得時刻は対応と実時間窓用、host到着時刻は遅延診断用と分ける。

| 記録する値 | 元の時計・意味 |
| --- | --- |
| `video_pts_us` | serverが`MediaCodec.BufferInfo.presentationTimeUs`を加工せず送る値。config packetには時刻を割り当てない。[固定Streamer](https://github.com/Genymobile/scrcpy/blob/2322868e9e256eb5fce0b3d659ab2a409f29bae1/server/src/main/java/com/genymobile/scrcpy/device/Streamer.java)、[Android API](https://developer.android.com/reference/android/media/MediaCodec.BufferInfo) |
| `device_mono_ns` | 時計DEXの`System.nanoTime()`。Androidではdeep sleep中に止まる時計。映像PTSはこの系との対応を対象端末で校正する。[Android時計の区別](https://developer.android.com/reference/android/os/SystemClock) |
| `device_elapsed_ns` | `SystemClock.elapsedRealtimeNanos()`。端末起動からの経過時間、deep sleepを含む。**同一boot内の共通実時間軸**とする。同じUSB切断/再接続でリセットしない。[Android API](https://developer.android.com/reference/android/os/SystemClock#elapsedRealtimeNanos()) |
| `device_epoch_ns` | binary logcat headerの`sec/nsec`。Epoch日時であり、手動/NTP補正で飛び得る。[AOSP Android 16固定header](https://android.googlesource.com/platform/system/logging/+/refs/tags/android-16.0.0_r1/liblog/include/log/log_read.h) |
| `host_receive_ns` / clock requestのsend/receive | hostの`System.nanoTime()`。端末への輸送・queue遅延を含むため動画/ログの発生時刻に代用しない。host日時`Instant`も診断・保存名用に別記録する |

AOSPのSurface自動timestampはMONOTONICを使うが、全端末のcodec出力が同じ精度・遅延であることの保証ではない。[固定Surface source](https://android.googlesource.com/platform/frameworks/native/+/refs/tags/android-16.0.0_r1/libs/gui/Surface.cpp)。**[要検証] PTSと端末monoの対応は各受入端末で確認する。** `logcat -v monotonic`にも依存しない。AOSPは元のEpochログから変換し、dmesgの同期markerが不足する古いログを疑わしいとしている。[固定logprint source](https://android.googlesource.com/platform/system/logging/+/refs/tags/android-16.0.0_r1/liblog/logprint.cpp)

### 時計測定と変換

時計DEXはapp_processで動く小さな専用helper。端末appの改造・APKインストール・rootを要求しない。hostからnonce付き測定要求を送り、helperは`elapsed before → mono → currentTimeMillis → elapsed after`を返す。clock sampleにはnonce、boot epoch ID、各値、読取幅、host往復幅を保存する。helperの実起動/互換性は未検証。起動不可なら同期不適合を表示し、受入を通さない。

開始/再接続/取得generation変更時に5回測定し最短往復のsampleを初期anchorにする。以後1秒ごとに測定、5秒以上古いanchorは正常同期に使わない。clock sampleの端末読取幅は2ms以下、host往復幅は40ms以下を正常条件にする。offsetのepoch分割は前後sampleの読取幅と1ms量子化による見積誤差を超える不連続で判断し、微小な測定差だけでは分割しない。往復の半分は**対称遅延の推定値**であり、端末内部のmono/elapsed/Epoch変換の保証誤差とは混同しない。

- `E = (elapsed_before + elapsed_after) / 2`。`E - mono`と`E - epoch`を別々に保存する。currentTimeMillisの1ms量子化、読取幅、隣接sampleのoffset差を変換の誤差幅へ加える。
- 同一clock区間では`video_elapsed = video_pts * 1000 + (E - mono)`、`log_elapsed = log_epoch + (E - epoch)`。各sample間でoffsetを線形補間する。同期精度を装うためにhost受信時刻へ置き換えない。
- `E-mono`の変化（端末sleep）、`E-epoch`の急変（日時変更）、boot変化、PTS逆行、clock測定不達を境界としてclock区間を分ける。境界を跨いで補間しない。取得済みの生PTS・Epoch・arrivalを残し、ambiguousなログには`elapsed_ns: null`、推定範囲、`clock_uncertain`を付ける。日時が戻った場面では同じEpoch値が複数の時点を指し得るため、ログ到着順・generationを含めても一意にならないものを無理に割り当てない。
- 相関不明でも取得を捨てない。正常な側は継続し、UI/成果物は「時刻対応を確認できない区間」を示す。未対応をSYNC-01合格に数えない。受信遅延でT以前のデータが保存確定後に届いたら、後の保存だけへ含める。前のimmutable保存対象は書き換えず、保存時点のwatermark/遅延疑いを記録する。

`sequence_time_ns = device_elapsed_ns - initial_elapsed_ns`が同一bootの軸。`sequence_id`は有効化開始から無効化/project closeまで固定し、再接続・取得process再起動・設定適用・rotationで変えない。generationとclock epochだけ増やす。端末再起動ではelapsedが戻るため新しいboot epochを作る。同じsequenceに履歴を残すが、boot間は時刻が厳密につながると主張しない。

boot跨ぎでは新旧epoch間のoffsetを、最後/最初のhost時計sampleから推定し、推定幅を保存する。host sleepや日時変更で継続性を確認できない場合はepoch間の対応をunknownとし、旧bootのrecordを新bootの正確な時刻へ混ぜない。リング全体はリセットせず、旧record/clock/gap履歴とpinを保つ。pruneは推定幅全体が現在窓より前と判定できるrecordだけを落とし、判断不能なrecordはバイト上限内で保持し保存時にも時刻不明として提示する。保持上限によるlossを隠さない。boot間のgapはhost日時差の参考値と`duration_uncertain`を記録し、確定の0秒gapへ変換しない。新epochのsequence座標にはこのoffsetを加え、対応不明なepochを跨ぐ`window_ns`はnullとする。初期版の対応保証にはこの非致命性Caseを含める。

## 保存窓・取得の中断

保存要求を受けた瞬間に`T / N / sequence_id / clock epoch / generation / per-stream watermark / app selection`を凍結する。選択する論理窓は **`[max(sequence開始, T-N), T]`**。cut後の`window_time_ns = elapsed_ns - window_start_ns`は動画と両ログで共通、gapも含めて進む。取得できたN秒を集めるために開始を過去へ広げない。開始20秒ならその20秒を保存する。

| 場面 | T・リング・欠落の規則 |
| --- | --- |
| 接続中/片系障害 | 最新の正常clock anchorから現在の端末elapsedを推定しTを進める。片系障害の間も健康な側を保持・保存。clock anchorも不達なら同期degraded、誤差を増やして範囲不明を明示する |
| 完全切断確定 | 切断を確定したhost時刻に対応する端末elapsed推定値でTを凍結する。実際のUSB抜去瞬間とは区別し、確定の検出遅延と誤差を残す。切断前の最後のsample/frameとT間は未取得のgap。Tとリングpruneも凍結するため長い切断中も保存可能 |
| 同じbootの再接続 | 再測定後Tを現在のelapsedへ戻し、通常のT-Nでpruneする。長い切断前データが現在窓外なら落ちる。同じsequenceの切断/再開履歴とgapは残す。旧未保存区間が欲しければ切断中に保存する旨をUIへ表示する |
| 自動復旧/回転/明示設定適用 | 全体リングを消さずgenerationを増やす。必要なstreamだけ再開。適用前後の取得条件をintervalに残す。N変更後の新規保存は新N、失敗保存のpinは旧N/Tを維持 |
| 無効化/project close | 取得停止、未保存リング・pinを破棄。保存済みフォルダは残す。sequence終了を示し次の有効化は別sequence |
| 全種類0件 | 成功通知/空動画は作らず「保存できるデータなし」。clockだけで記録成功にしない。解決済み保存なしなら次の取得/保存を妨げない |

完全切断はdevice offline/absentと全stream transport喪失の状態判断で確定する。動画だけEOF、logcatだけEOF、clock helperだけ停止は完全切断にしない。復旧は最初の1回を直ちに試し、その後1/2/4/5秒上限で再試行し、接続の正常化通知でも待機を解除する。遅延待ちは取消可能、他streamを停止せず、成功後は待機値を初期化する。動画の初期forward接続はbackend bind前にEOFとなり得るため、同じserver/forwardのままfresh socketでdummy受領を反復する。準備は全体10秒・各接続/読取最長1秒・100ms間隔で取消可能とし、codec/session検証は維持する。継続readは部分packetを再読しない。固定scrcpyは[画面変化時にframeを生成する可変FPS](https://github.com/Genymobile/scrcpy/blob/2322868e9e256eb5fce0b3d659ab2a409f29bae1/doc/video.md#frame-rate)で、Androidの[repeat設定](https://developer.android.com/reference/android/media/MediaFormat#KEY_REPEAT_PREVIOUS_FRAME_AFTER)も無期限のheartbeatを保証しない。完全packet間のidleだけではgap/RECOVERING/強制closeにしない。初期codec/session/初回frameの取得と、packetの最初のbyteからheader/body完了までには10秒の全体期限を設ける。metadataやtrickleで期限を延長せず、process終了/EOF/不正packet/取消しで所有socketを閉じ復旧する。正常idleと無応答のtransportを取得側だけでは区別できない場合、新frame未確認として残し、process/clock生存から動画取得成功を推定しない。監視はstore/disk/adb停止を待たず、clock timeoutの物理終了待ちと動画socket closeを2 workerに分ける。旧監視は新socketを参照しない。実端末の静止・連続変化・負荷時の受入は別に行う。

gapはstream毎に`kind / from / to / reason / boundary_uncertainty / generation`を保存する。ログの無出力だけではgapと判定しない。正常なreaderでもAndroid log bufferのoverflow、binary framing失敗、捨てた件数/不明件数を別のlossとして記録する。正常streamの終了/復旧境界は最後の正常watermarkと再開anchorで表し、その間を収録成功として埋めない。

表示用StreamSnapshot.gapsも現在の論理窓に交差するimmutableコピーを返す。clock gapは全stream、device_log gapはapp_logにも関連付ける。境界または現在窓の時計対応が不明なら窓外と断定せず保持し、完全切断時は凍結した窓の不確実性を使う。SaveSnapshotの端末・app選択・窓内app履歴は固定保存対象から生成し、現在設定や再試行先によって変えない。概要にUID/PIDを出さず、範囲不明はnullのまま示す。

端末側cleanupが再接続待ちになる場合、停止済みreader/clientのUUID・serial・exact forward endpointをcaller指定のstable private cleanupDirectoryへ記録する。session workspaceを探索せず、未知/破損/未来schema/生存file-lock ownerを保全する。新CoreはOFFでも同directoryの保留をローカルで読み戻して表示する。検証済みrecordのlock取得後だけ引継ぎ、解決済みSDKと所有pendingがある場合に限り、OFFのまま有界・取消可能なcleanupを再試行する。取得backend/時計/logcat/recordingは開始せず、pendingなし・破損/未来/lockedのみの初回OFFはADB照会もしない。旧serial自身が接続したときに二重psのowned名確認・forward serial/port/endpoint再照合を行う。port再利用・別device・共有ADBには作用せず、成功recordだけ削除する。IDE callerはsession tempとstable directoryの寿命を分ける。旧tempに残った未参照markerは所有者による既知資源管理へ残し、host-wide探索で移行しない。

#11のPM案（1件のimmutable保存対象、失敗時に同じ対象を再試行/保存先変更/破棄、解決前の新規保存を止め取得は継続、無効化/project closeで未保存破棄）と整合する。pinはraw GOP/log/clock metadataへの所有参照で、pruneがpinを削除しない。snapshotは各raw chunkのbyte長/完全packet数とclock変換版をwatermarkとして固定する。進行中chunkに追記してもsnapshotは固定長より後を読まず、pin中の既存bytesは上書きしない。保存後のclock再校正も固定対象を書き換えない。保存中もwriterは新packet/GOPへ進む。保存対象は失敗しても新しいTへ差し替えない。容量・pin寿命は#11の決定を実装担当が適用し、録画streamを無制限にpinしない。

## 切出しと保存形式

| 形式候補 | 判断 |
| --- | --- |
| **MP4＋JSONL＋session JSON** | 一般の動画プレイヤーとテキストエディタで確認可能。元時計、変換誤差、gap、両ログを保持しやすい。複数ファイルの整合をmanifest/hashで検査するため採用 |
| WebVTT metadata | 動画の秒数に対応する外部text trackに向くが、元の複数時計や欠落の正本には追加schemaが要る。playerのmetadata表示も一定でない。初期版に追加しない。[W3C仕様](https://www.w3.org/TR/2026/CRD-webvtt1-20260520/) |
| Perfetto | 複数時計snapshotの考えは採用。画面エビデンス配布にはMP4やlogcatへの変換・別UIが必要で、新しいtrace収集を増やすため不採用。[公式clock sync](https://perfetto.dev/docs/concepts/clock-sync) |
| MKV metadata track | 柔軟な複数track/可変PTSが利点。初期版では一般のmacOS受取人の再生・metadata閲覧とJVM muxの確認範囲を増やすため不採用。[Matroska仕様](https://www.matroska.org/technical/elements.html) |

保存単位は日時＋一意save IDのフォルダ。`session.json`（schema=1、build/dependencies、sequence、T/N、clock samples/epochs、設定/app選択履歴、watermarks、coverage、loss/gap、全file hash。ただし自己参照を避けsession.json自身をhash一覧から除外）、`logcat-device.jsonl`、`logcat-app.jsonl`、`video-001.mp4`以降、`frames.jsonl`、`README.txt`を含める。動画のない部分保存ではMP4は0件、ログのない種類もファイルは0行としsessionのcoverage/statusで空の理由を示す。完了manifest公開までフォルダを完了扱いしない。再試行・atomic公開・失敗対象の寿命は#11へ接続する。 macOSではIDE同梱JNAからrenamex_npのRENAME_EXCLを使い、同parentのpartialを完成folderへ原子的かつ非上書きで公開する。[Appleの対応volume契約](https://developer.apple.com/documentation/foundation/urlresourcevalues/volumesupportsexclusiverenaming)。既存のempty/nonempty/file/symlinkも置換しない。非macOS・symbol/FS未対応・公開失敗はFAILEDとしてpinを保ち、通常Files.move/ATOMIC_MOVEへfallbackしない。local合成回帰をnetwork FSやIDE実ロードの合格へ転用しない。

両ログは全体の**同じbinary logcat取得**から抽出し、共通の`record_id`と元Epoch/変換情報を持つ。`-b all -B -T 1`を開始候補とし、deviceがshell権限で読めるbufferとheader versionを確認する。取得開始以前のtailは正常窓から除外、再接続で過去をbackfillしたとは主張しない。lidによってtext/event payloadを分け、未対応payloadもraw bytesをJSONのbase64として残す。package→UID/PID/multiprocessの選択・再起動追従はLOG-02実装契約から供給し、PIDだけを永続app IDにしない。対象未確定の区間はapp coverageをunknownとし、全体logの空白へ変えない。security等読めないbufferを「端末の全ログ取得済み」と表現しない。

各ログ行は少なくとも`record_id / clock_epoch / generation / epoch_ns / elapsed_nsまたはnull / window_nsまたはnull / uncertainty_ns / uid / pid / tid / lid / priority / tag / message / app_membership`。ns/usの時刻値は桁落ちを防ぐ10進文字列（null可）、単位はキー末尾で固定、JVMでは範囲確認付きLong変換/加減算を使う。改行をJSON escapeして1record=1行、サイズ上限・unknown header/payload・破断したrecordを検証する。UTF-8不正は元bytesとdecode statusを残す。JSONLは端末内容であり、公開Issue/CI artifactへ自動アップロードしない。

動画はIDRとSPS/PPSから始まるGOP単位でraw packetをリングへ保持。各chunkの元PTS、clock epoch、generation、codec config、寸法、keyframe、byte数をindexに記録する。通常は最後のIDR以前のdecoder用データを1GOPだけ余分に保持する。I-frame指定を端末が無視したら実際のGOP長を使い、容量上限に達したため失った先頭を明示する。

保存時は論理窓に交差するframeを最大限選び、復旧/寸法/config/clock境界の**連続区間ごと**にMP4を作る。gapを詰めて1本へ結合しない。MP4ごとの`file → window開始/終了 → source_pts_origin → media_time/edit_start`をsession/READMEへ書く。受取人がpart 2の0秒をpart 1の直後と誤解しないようgapの秒数を隣に記す。

JCodecのPacket timescaleは常に`1_000_000`。同一generation/session/config/寸法/clock epochで、介在する確定break/drop/時計不明がない完全取得済み隣接frame間は実PTS差をVFR表示durationにする。PTS差の長さだけでpart分割しない。窓を跨ぐ表示frameと必要なIDR/GOPをprune/captureに残し、窓内新frame0でも直前画像を保全する。同一epochの最初の正常clock anchorを4096 sample上限内で保持し、長期静止の元frameの時計対応をsample evictionだけで失わない。epoch/break/clock不明の扱いは維持する。byte/packet/config/disk上限・pinを維持し、失ったframeや別sessionの画像は復活させない。

保存末尾はTで凍結し次frameを待たない。最後の画像をT又は既知の取得境界まで表示保持する場合、新frame未確認の範囲を`video_tail`へ分離する。`from_ns / to_ns / source_pts_us / source_sequence_ns / clock_epoch / generation / display_held / time_axis=sequence_ns / new_frame_confirmed=false`を記録し、時刻不明はnull、ns/usは十進文字列。確定break/clock不明/epoch変更/取得前/容量dropを跨がない。表示保持は、MP4選択と同じIDR/config/世代/session/寸法条件で固定対象から復号を開始できる場合だけtrueにする。IDR喪失でMP4を作れない対象を保持表示済みとは示さない。partの`window_start_ns / window_end_ns`は再生範囲であり、`confirmed_window_end_ns`は隣接frameで確認できる表示区間の終端（区間なしならnull）。画像保持の再生durationを確認済みcoverageに加算しない。末尾未確認だけではgap/loss/video_missing_ranges/missingKindsやCaptureState.PARTIAL/RECOVERINGへ流さず、COMPLETED/session.complete=trueは正常な成果物完成とする。pinを解放し次の保存を可能にする。既知欠落は従来どおり併記する。

`SaveSnapshot.videoTail: VideoTailSnapshot? = null`を末尾へ追加する。immutableな`fromNs / toNs / sourcePtsUs / sourceSequenceNs / clockEpoch / generation / displayHeld`は固定保存対象から生成し、retry/後続frame/現在設定で変更しない。UI callerは保存完了と未確認末尾を別表示する。通常も最終frame時刻<Tとなるため、想定fpsや短い間隔を取得確認の根拠にしない。

通常のmedia PTSは`source_pts - part最初のsource_pts`。長期静止でMP4のduration/edit整数範囲を超える場合は、窓に表示する実durationを保ち、decoder-only時間だけを短縮して`media_timeline_clipped=true`とする。元source PTS/実frame数は不変、`frames.jsonl`の`source_duration_us`（次frameがない末尾はnull）と`display_duration_us / media_pts_us / presentation_pts_us`を別記する。追加画像を作らず、受取人はpartの表示範囲とframe indexで照合する。config packetはframe/PTSに数えない。SPS/PPSを最初のIDRへ渡しAVC configとMP4 headerを確定してから完了とする。破断は完全frameだけを使いlossを保持する。

開始cutがIDR間なら直前IDRから復号するprerollを持ち、MP4 edit listで論理start/endだけを再生対象とする。`frames.jsonl`は`part / sample_index / source_pts_us / media_pts_us / elapsed_ns / window_ns / presented / preroll / uncertainty_ns`を保持する。**ファイル内部には窓外のdecoder prerollが含まれ得る**ことをREADMEに明示し、論理取得時間へ加算しない。窓前の画像を含めてはいけない利用には初期版を適合済みとしない。edit listが受取人の標準playerで不適合なら、開始cutだけJVMで再エンコードする代案を実装Issueで比較・再測定する。cutを次IDRへ黙って丸めてBUF-02の取得済みデータを失うfallbackは禁止。

## 許容精度と受取人の確認

初期版の**受入目標**は、ログに記した描画eventと映像に現れる同じIDの対応誤差が通常時 **p95 ≤ 100ms、最大 ≤ 250ms**。30分継続で前半/後半の誤差中央値差 **≤ 50ms**。端末内clock変換の見積誤差 **≤ 20ms**、mux/indexの丸め誤差 **≤ 1ms**、cut境界は1観測frame間隔＋時計誤差以内。これらはITの場面とログを照合するための設計上の基準で、ベンチマーク結果や全端末保証ではない。アプリのLog出力と描画にはスケジューリング差があるため、時計誤差と描画遅延を別々に測る。

失敗/時計急変区間はnormalから除外して隠さず全件別集計。閾値を超える/推定できない区間は`degraded/uncertain`、gapは`missing`。正常区間の件数・測定条件が足りない場合はpending。精度を満たさない端末でSYNC-01合格や初期版完成を主張しない。

受取人は追加ツールなしで次を行う。

1. README.txtで論理窓の日時/N、動画/全体ログ/appログのcoverage、gap一覧、同期状態、part一覧を見る。全体の成功表示だけで不足を隠さない。
2. OSの動画プレイヤーで指定partを開く。playerの表示秒`p`から、READMEに書かれた`window開始秒 + p`で窓内秒を求める。edit適用後の表示秒を使う。フレーム単位ではframes.jsonlの`presented=true`行でmedia/source/window対応を照合する。
3. テキストエディタで両JSONLの`window_ns`付近を探す。同じapp recordは同じrecord_id/元時刻で全体側にもある。`uncertainty_ns`内の近傍を示し「最も近い1行が同じ操作」と断定しない。
4. 窓内秒がgapなら「その種類は映像/ログなし」。`window_ns=null`なら「時計対応不明」。part間の空白、device日時の飛び、prerollを成功区間へ混ぜない。

保存後のviewアプリやWebVTTは不要。標準プレイヤーのedit/seek/表示秒が契約通りであることは下記Caseで**受取人の環境も含めて**確認する。

## 試作で確認した範囲と残Case

製品コード・deviceを使わないJBR 25/JCodec 0.2.5の純粋試作を実施した。32×32の合成IDR 3枚をPTS `[0,100000,350000]`µs、duration `[100000,250000,50000]`µs、timescale=1e6でmux/demuxし、PTS/durationを一致確認。次に`Edit(duration=350000, mediaTime=50000, rate=1)`を付け、edit metadataを読み戻した。JCodec demuxはedit適用時に後続PTSを `[0,50000,300000]`へ変えるため、編集前のmedia PTSと表示PTSを同一視しない。試作source SHA-256 `2bb56169bc51ac3945a4248b8f088af9b413d08fb91784bd50926db2bdd0c600`。合成映像だけで、端末encoder、socket/parser、標準playerの再生/seek、Plugin classloader、時計DEXの確認には使えない。

実装担当が同じ試作を再現する最小手順は、固定JARをhash照合しJBR25で`MP4Muxer.createMP4MuxerToChannel`→`addVideoTrack(H264, 32×32 YUV420J)`→`H264Encoder.encodeIDRFrame`→上記3つの`Packet.createPacket`（全てtimescale=1e6）→`finish`→`MP4Demuxer.createRawMP4Demuxer`の順。editなしで全PTS/durationをassertし、edit付きではTrakBoxのEdit値をassertする。異なるtimescaleを混ぜた動作、B-frameは未確認・初期版契約外。現実の画面取得でこれを置き換えない。

実装Caseは[issue-13.json](../verification/changes/issue-13.json)に全てpendingで残す。実機は利用者が用意できず受入を保留しており、未実施のまま保持する。実機の再開は利用者による端末準備とPMの割当後とし、この保留を理由にEmulator受入の準備や[#31のコア実装](https://github.com/shinma06/android-replay-buffer/issues/31)を止めない。Emulator/受取人側も現段階では未実施であり、実機のpassを代替しない。設計変更自体はGUIを要しないが、方式採用の製品受入には実機/Emulatorで必要なためGUI requiredとして追跡する。PMは#14/#31の実装と[#27のQA](https://github.com/shinma06/android-replay-buffer/issues/27)へCaseと環境ごとの状態を双方向で引継ぎ、独立レビュー/固定候補promotionを調整する。検証待ちを理由にCLI原型を変更しない。
