# 製品の保証範囲と最小の十分な検証

[Issue #223](https://github.com/shinma06/android-replay-buffer/issues/223)の全試験改訂、[Issue #230](https://github.com/shinma06/android-replay-buffer/issues/230)のconsumer別方法に、[本人の同期要件を反映する #299](https://github.com/shinma06/android-replay-buffer/issues/299)を適用した現行条件です。**保証目的→必要な証拠と精度→実行可能な最小方法**の順で決めます。旧手順に書かれた操作・秒数・回数は、それだけで維持理由になりません。全139 Case/208出典、105 Agent/34 REALと原記録を保持し、[正式計画](amendments/initial-agent.json)の`responsibility_revision.method_revision`を有効条件とします。下の過去改訂にある30分/300event・旧画像精度値・GOP6証拠・固定操作数等は履歴で、明示的に置換した条件を現行gateへ戻しません。

## 全試験の目的と方法

正式計画の次の9 groupが全139 keyを重複なく対応付けます。前提も目的群へ置換し、OFF設定/初期既定値へAPK/取得中fault、受取人へ新録画を一律に要求しません。Case固有の実入力/対象はその保証に必要な範囲だけ用意します。Case固有の期待結果は保持し、明示的に置換した条件とgroupの方法を適用します。groupは関連保証/方法の集合で、各Caseに無関係な目的を新たな依存にしません。短時間保存の完了へN超取得・精度系列・回転・別失敗retryを要求しません。CLI/JVM/FILE/native/ハーネスも同じ判断を使います。

| group / Case数 | 担保すること・必要な証拠 | 最小の方法と置換理由 |
|---|---|---|
| environment / 6 | 同梱依存・SDK adb・実ロードと追加導入不要 | ZIP/hash/licenseとSDK解決回帰＋実起動command/CodeSource/保存。PATH改造や20秒取得は保証と無関係 |
| markers / 2 | 画面とlogの独立ID真値、表示/非表示・coalescing/viewの区別 | fixture回帰＋代表実表示/両log。欠陥を区別する最小入力を用い、各5回/1秒等は固定しない |
| precision / 7 | streamの元PTS・実画像ID/両logと、支持clock変換・窓/索引・unknown/欠落説明の整合 | 有限の開始/後半の予定event/全phase・全FILEを照合し、独立した画像IDと時計モデル/適用限界を示す。長時間/sample更新/4096履歴/時計境界は既知時刻回帰へ。streamが独立に提供しない画像時刻の追加校正を一律gateにしない |
| recipient / 6 | 全保存内容・時刻・不足説明と受取人の利用 | 既知入力の全byte/画素回帰＋native成果の全FILE/sample/両logと、保証ごとに共有する代表player・editor観察。全GUI検索/全Play/全native raw exportは不要 |
| buffer / 9 | 実T/N窓、未満時の最大保存・prune/IDR prefix/pin、保存中取得/次保存 | N未満・N超・重複Saveを別工程で共有。0/N前後・GOP前中後・VFR/品質/容量はproduction回帰＋FILE。20秒/200秒/10秒後/GOP6本は方法例 |
| recovery / 57 | 所有/世代/終了、gap/loss/unknown、固定失敗retry・atomic非上書き/保全 | 全fault/raceはfake SDK/loopback/所有filesystemの回帰。実IDEは代表取得/復旧/OFF/close/保存・理由表示、固定失敗場所→UI consumer→retryは同ZIP隔離public経路、OS atomicは実callを共有 |
| ui-settings / 37 | 一括Apply/Cancel/復元、対象解決/帰属、EDT/最新状態/可読性/Tab/Help | 全入力/raceはproduction回帰、代表の実操作・表示・通常close後の永続設定。Apply/SaveAll直後のXML即時一致は非同期保存の保証ではない |
| video / 12 | VFR/unknown/decodeRun・全frame/PTS/geometry/Level・単一MP4/失敗pin | 既知source全packet/config/画素と全内部境界は回帰、nativeは通常/静止/実回転混在・再接続/表示と全FILE。独立工程を短い同runへ詰めない |
| log-reader / 3 | main/非mainの範囲・identity/順序/二重取込防止、復旧/実停止 | Protocol/Wireの全header/fragment/fault回帰＋有限のmain/非main陽性入力の全FILEと代表OFF/再接続。100入力/300phaseは固定しない |

場面とログの対応、unknownを既知へ見せないこと、取得済み内容と窓を忠実に保存することを保証します。clock変換モデルの見積誤差≤20ms・読取幅≤2ms・RTT≤40ms・鮮度≤5秒、支持された対応でのcut≤1観測frame間隔＋時計誤差、mux/index丸め≤1msを維持します。これらはAndroidの描画/取得/encoder/物理表示を含む総合精度ではありません。時計モデルの根拠とruntimeの未確認を明記し、長時間内部時刻/履歴境界は既知入力回帰で検査します。log順から画像番号を推定せず、独立した実画像IDと全対象の保存内容を確認します。

#299は[本人の要求範囲](https://github.com/shinma06/android-replay-buffer/issues/206#issuecomment-6232582806)の改訂です。独立画像時刻のp95≤100ms・最大≤250ms・中央値drift≤50msを現在のAgent/後続実装/性能baselineの一律前提から外し、代替数値を発明しません。元REQUEST/DRAWのFAIL・未校正・原因未帰属、回転境界unknownやconsumer制約はそのまま保持します。FRAME_COMMITへの付替え、旧結果のPASS化、Android原因の断定はしません。具体的な誤変換・誤帰属・黙示的欠落・破損・既存出力上書き・失敗pin喪失・虚偽の時刻/成功表示は引き続き影響する受入を止めます。旧画像時刻の未帰属だけをその自製品違反と同一視しません。

全byte/画素/headers/edit/PTS/IDR prefix/後半欠落の厳密さは、保存対象の内容と時刻を失わないためです。既知入力と真値を持つ同ZIP production class回帰へ分担し、nativeの未公開raw/configを無条件に追加採取しません。native成果は全sample/行/index/manifestを検査し、実操作/実表示と結び付けます。再圧縮の全画素一致を誤適用せず、原rectangle/aspect/文字/色/全frame対応を確認します。source読解だけは実行証拠ではありません。

## 実際の停止例の訂正と証拠の再評価

- 短時間保存は`0<取得時間<N`で取得済み分を保存し、未取得期間を成功にしないことです。「約20秒」は例です。現在候補の34.474秒/22frame/831device行という保持事実は、この目的への証拠として先に評価します。20秒目標超過という旧方法失敗は残し、20秒で実施したと書き換えません。陽性app行・精度系列・別目的の不足は別に扱います。
- 長時間保持は180秒を超える実取得と現在窓内/窓外の所属が目的です。200秒の上限、回転を短時間保存と同時に行う条件、host/native時計500ms校正やready同期は不要です。固定窓境界の正確さは実T/PTS/FILEとproduction回帰で確認します。
- 保存完了と取得継続、回転/混在内容、通常再接続、失敗固定retryは独立保証です。同じartifactと前提へのbindingがあれば別工程/別runを束ねます。保存失敗の実glueは、同ZIPの実public経路で所有partialからSaveSnapshot/UI consumer/retryまで実行した証拠とnativeの安全な理由表示/通常保存を束ねられます。source/文字列assertだけで伝播を証明せず、未実施/未知の時刻/欠落をPASSへ換えません。
- cleanupは所有process/reader/forwardの実停止と、識別済み残留の安全な保全/通常再接続での回収です。remote JAR/journalの即時消失を一律gateにしません。現在候補で通常reopenにより回収できた事実を使えます。生存/未知/別projectや完成出力を削除せず、失敗保存attemptの保全は取得root回収とは別です。
- 設定はApplyによるStore反映、Cancelの不変、通常close/reopen後の永続化を保証します。現在候補では通常closeの2 GUI入力とclose後の元設定復元・所有thread終了・既存出力保全が独立確認されています。これは一つの実証済み方法です。全試験の所要時間短縮やactive/失敗中closeの合格までは示しません。

REAL34の初期段階延期と利用者の実機保留を保持し、共通host/IDE/code目的の18件は同artifact証拠を共有、実機固有残差の16件は保留解除後の少数共有smokeへ分担します。34回や16回の新実機工程を要求しません。実機固有取得/USB/描画/負荷をEMU又は合成から成功としません。内部数値・faultの保証をproduction回帰へ再分担した分と、実機の残差を区別します。既存の再起動後About操作経路延期を拡張せず、現行保証に必要な実画像IDの識別不足・未帰属/欠落/過大な不確かさは未完了です。旧延期文言に残る画像精度値は履歴として読み、再開時も現行の明示改訂と残るREAL固有契約を適用します。

## 既存試験群と今後の依頼

同梱CLIの試験はthreadtime parser互換や入力・出力経路の既知条件を担保します。対象は[CLI仕様](../cli.md)と[自動検査](../plugin-development.md)で確認し、ADB/daemon/録画全体の受入とは区別します。JVM群はClock/Protocol/Wire、Store/Writer、所有/寿命、Settings/IDE/UIの既知入力/全内部境界を担います。instrumentationは測定oracleの表示真値を担い、製品動画合格とは別です。FILEは対象の全行/frame/sample/hashと対応を担い、nativeは実ロード/API/glue/表示・代表相互運用を担います。ハーネス群は出典/plan/trusted gate/listing/reuse、Git保護/影響、registry/lease/handoff/秘密/参照を担い、製品合格を出しません。新たな役割・依存・全GUI matrixを作りません。

担当とレビュアーは既存Issue/計画から「どの自製品欠陥を検出するか、必要精度/真値、最小の実行可能方法、全予定対象、既存事実と不足」を確認します。手順・回数・所要時間・同runを残すときは固有の欠陥を見逃す具体的理由を示します。必要な境界は維持し、既に十分な証拠は共有します。不要な操作期限や再確認、追加承認を作らず、実tool権限・privacy・lease・所有fixture・復旧を全方法で維持します。

### GUI手段の必要性を先に評価する

検証が必要なことと、手動GUI操作が必要なことを分けます。担当とレビュアーは目的と必要な観測事実を決め、適格な既存証拠、production回帰/FILE、導入済みのAPI/IPC/CLIと実画像・render結果を読む経路で、その事実を確認できるか先に評価します。そこで未確認の具体的な自製品UI/OS接続・表示・consumer相互運用だけに、最小のGUI操作を選びます。選んだ手段の理由と残る保証を既存の割当/レビューへ短く記録し、長い比較表・新台帳・追加承認を要求しません。明示された実施経路、実tool権限、privacy・lease・所有/停止/復旧は維持します。

既存ツールの能力が不足する場合も、消去法でGUIへ進まず、目的に合う検証ハーネス/CLI/API/rendererの追加を積極的に検討します。具体的な不足を埋める最小のツールを選び、公式の能力・版・互換性と導入/保守費用を、繰り返すGUIの負担と照合します。管理は極力Homebrewを優先し、許可された検証環境/範囲内の導入は毎回本人承認を取り直しません。実OS/toolの同意・権限と所有/保全条件には従います。十分な既存経路を先に使い、試すだけの導入や無関係なツール探索はしません。hostの検証ツールと製品の依存を区別し、製品の追加導入不要という保証や製品依存の選定条件を変えません。

動画では「標準player」をOS同梱アプリの銘柄や手動操作の指定と解釈しません。該当する再生・前後移動・表示の保証を満たすなら、既存mpvのJSON IPCと実画像/renderの読取りを優先できます。[mpvの公式command仕様](https://github.com/mpv-player/mpv/blob/v0.41.0/DOCS/man/input.rst)が提供する要求/応答や画像取得を使っても、command成功・`vo=null`・エラーなしだけでは表示の正しさや要求sceneへの到達を証明しません。取得画像の種類が目的の内容/実表示を覆うか確認し、実画像・実際の再生/移動結果を読む必要があります。全内容・時刻/窓の真値はproduction回帰と全FILEへ分担し、playerのtime-posを同期精度へ転用しません。

OS標準という名前やdecoderの多様性だけで別player/GUIを追加しません。必要な自製品相互運用・OS callback・実画面の未確認部分を示せる場合だけ対応する経路を使います。mpv等の失敗/不明は保持し、その条件に残る欠陥を解明する有限の代替方法を選びます。一部のedit/時刻解釈差から全動画へ手動GUI fallbackを課したり、逆に外部差として自製品違反/未帰属を除外したりしません。

### 動画とテキストのconsumer証拠を別々に評価する

代表playerとeditorを毎回一組で割り当てません。動画のcontainer/codec/sample-description/geometryと、テキストのencoding/本文形式/利用導線/表示条件を別々に評価します。新しい混在動画でplayer観察が必要でも、それだけでeditorを再実行しません。本文のID・数値・理由が増えたことや出力が新しいことだけでは、新editor観察の理由になりません。各出力の全行・hash・参照・内容の正しさはproduction回帰とFILEで確認します。

割当と独立レビューでは、追加consumer操作で検出する具体的な自製品欠陥と、production回帰・FILE・既存consumer証拠で確認できない部分を示します。同じ製品artifactへのbindingと、その保証に関係する生成形式・導線・reader/runtime・表示条件の不変を実証した適格な実観察を共有します。不明・変更なら影響する部分だけを再確認し、新しいencodingや表示切断、open導線の破損が残る場合は最小の実open/readを行います。log-onlyでも実production出力のMP4数0・説明・参照/hashをFILEで確認し、形式・導線・条件が不変と確認できたeditor証拠だけを共有します。実通知のopen/copy callback・失敗表示など自製品glueの実接続は別に確認します。アプリの銘柄や全player matrixを保証にせず、FILE成功だけで未観察の相互運用を成功にしません。

これは#230の正式method改訂です。原Case/観察・失敗を保持し、通常reuseと保持事実の再評価はそれぞれ既存の証拠・独立レビュー条件に従います。`interop`のcoverageにはCaseの動画/テキスト各保証に該当する実観察と関連条件を個別に対応付け、FILEだけで未確認consumerを成功にしません。改訂を新実行やPASSにしません。

## trusted-main改訂と現在候補

`method_revision`はIssue299と直前230版のcanonical hash `d544da51fd949b415001c2b4c939e1701fa6d7608eccfed2991b7db544ffd896`を固定します。`previous_precision`にprecisionの旧目的/方法/除外/前提、共通未完条件の旧文言と旧drift overrideを保持し、変更した7 expectedだけを戻して230版の全文を復元します。既存`previous_methods`から223版 `3ee1d401997722423b4db5f52c07d8edb4b6e377bc1f6f5456c66fab8460af57`、method blockを除いて登録139計画 `993e76bb7a0bd6f9f864c2974b3ef4d4392665de51a4791c9cc0c094c394b50c`も正確に復元・照合します。過去hashを改変後の文書から付け直しません。全139key/208出典・原hash/延期、全Caseを一度ずつ覆うgroupを共通resolverで検証します。置換は固定現契約だけに適用し、後続製品Caseの変更を旧overrideで消しません。

改訂は独立レビュー後の固定main baseだけから読みます。候補830の正確な旧計画全文、登録223/230版と現行299版を許可し、要件/方法だけの更新のため新build/導入/全GUIを要求しません。candidate単独改訂・任意の旧計画・欠落/不一致・symlink/重複JSON keyは拒否します。通常のmain→develop管理同期/promotion条件は別にPMが引き継ぎます。製品/build/fixture変更や途中revert・不明な条件は既存history gateで拒否します。

現結果には現plan/revisionとgroup別全証拠を要求します。GOPの6個の旧証拠欄は現行bufferの`window_and_gop`へ統合し、1秒prune反例・IDR前中後・VFR/品質/容量/切出しを全て覆う回帰/FILEの参照を必須にします。旧観察のGOP6証拠は歴史上の旧条件で照合し保全します。旧aggregate pending/blocked/failからの成功事実は既存`responsibility_assessment`で原JSON/hash、正確な元plan、実成功facts、全有効条件へのcoverage、不変artifact/条件、未解決自製品失敗なしを確認します。文字列の存在だけで証拠の内容は保証できず、独立レビューが対応内容を確認します。方法変更や構造checkは遡及PASSではありません。

本改訂から採用件数は増えません。#223当時の3/105と4件の再評価候補は履歴です。その後のsource830評価も自動で新revisionへ移行せず、PM/QAが各保持事実を現条件へ実際に対応付け、独立レビュー後に再集計します。未達/REAL・QA27/65/87/release10を引き継ぎ、計画改訂完了と製品受入完了を分けます。

#206は初期139件の登録対象ではありません。QA294の旧aggregate FAILを`reassessed`で包んだり、`responsibility_assessment`を付けて機械promotion成功にしたりしません。採用後はPMが既存#206/QA294/#280へ、採用revision・固定D/ZIP・原観察者/実行日時・全3 Caseの保証対応・独立評価・新しい評価日時を記録し、旧観察と分けて読み戻します。現在要件の受入により#103は通常の#206→#103順、#224の性能受入は、[2026-10-27の本人方針を反映する #361](https://github.com/shinma06/android-replay-buffer/issues/361)に従い、完成版を本人の環境で比較して「問題ない」と判断した時点で完了とします。現在は本人評価未実施で、性能PASSではありません。自動の純正比較と比較専用の準備・研究は停止し、旧定量比較・再profileを実装や性能受入の必須前提にしません。品質と負荷を明示的に選べる実装を進め、内容・保存窓・clock・pin・OFF/LOG_ONLYの動画非起動・エラー処理等の機能保証と、過去の原観察・FAIL/unknownは保持します。QA65のhuman/REAL・#104最終GUI・#280最終v2の全Case/main条件は残り、完了済み初期版#10は再開しません。新しいpromotion adapterやQAを本改訂のために作りません。

## 過去改訂の記録

以下は#182以降の原契約と改訂経緯を残した履歴です。現行の実施方法は上記とmethod_revisionを使います。原観察/失敗を変えず、過去の方法指定を現行の追加gateへ転用しません。


製品が保証するのは、取得した対象データを正しく保全・保存・説明し、自分の操作・状態・失敗処理が外部APIと正しく接続することです。外部player、editor、IDE、OS内部のすべての挙動を保証しません。既存・予定・今後のCLI/JVM/FILE/native/ハーネス試験へ適用します。

[Issue #182](https://github.com/shinma06/android-replay-buffer/issues/182)の本人指示による正式改訂です。[固定元要件](https://github.com/shinma06/android-replay-buffer/blob/36de4f7e3c3d4cff1fa7c5a50287a7f98ce4447c/docs/requirements.md)、[同期設計](https://github.com/shinma06/android-replay-buffer/blob/36de4f7e3c3d4cff1fa7c5a50287a7f98ce4447c/docs/design/timeline.md)、[旧受入手順](https://github.com/shinma06/android-replay-buffer/blob/36de4f7e3c3d4cff1fa7c5a50287a7f98ce4447c/docs/verification/initial-version.md)の外部実装保証と過剰な実施方法を、以下と[正式計画](amendments/initial-agent.json)の`responsibility_revision`で明示的に置換します。固定原Case・原計画・過去の観察/失敗/未達は履歴です。削除/統合/置換を観測PASSにしません。

## 保証と証拠の分担

| 自製品が保証すること | 最小の十分な検証 | 外部側との境界 |
|---|---|---|
| 全受理データ・log帰属/record ID・原packet/config/bytes・順序・欠落/unknown | production parser/Store/Writerの回帰と全対象FILE照合。予定母数を先に固定し欠落を除外しない | logcat/encoderが出さなかった内容を捏造せず、取得不明/未対応を説明する |
| 単一MP4/track・sample/index/headers・PTS/duration/edit・窓/IDR prefix/既知gap/未知duration/held/terminal | 全file/sampleを忠実な独立readerで読み、edit未適用mediaと適用後movieの対応を元PTS/indexへ照合。全frameを検査 | ファイル非破損だけで内容/seekを証明しない。readerのedit無視・PTS再配置も自製品の時刻へ混同しない |
| 内容・原寸/aspect/文字/色/余白・全frame対応 | 元packet/rawと復号画素・全選択frameの対応。混在変換は原rectangleと品質を検査し全画素一致を再圧縮へ誤適用しない。文字可読性は代表実画像も確認 | 全Playや全playerでの全pixel目視は不要。native実表示の代表確認と全内容検査を分ける |
| 同期精度・実時間窓・時計不確実性・長時間drift | 全300event/30分、開始/中間/終了各100、p95≤100ms・最大≤250ms・drift≤50ms・clock≤20ms、cut≤1観測frame間隔＋時計誤差、mux/index≤1msを維持。全欠落/重複/unknownを別集計 | 実描画時刻をREQUEST/commitやplayerのlabelで推定しない。長時間依存を短い回帰で代替しない |
| 固定T/N/対象/pin・retry・全出力完成・partial/error/atomic非上書き | production故障回帰、所有filesystemの実atomic API、代表native失敗→固定retryとUIの結合。後半欠落ならFAILED＋pin、成功公開なし。失敗・取消し保存attemptは削除せず残留保全 | OS/FS未対応を成功と呼ばず、既存出力/未知所有者を保全する。ホスト全体を埋める試験はしない |
| process/reader所有・generation・取消し・実停止・Lifecycle/EDT/設定 | fake SDK/loopbackで全race/故障、実IDEでは代表起動・取得・通常再接続・OFF/close・復元・Apply/Cancel・通知/不足表示を共有 | 外部IDE/model/dialog内部の全組合せの再試験はしない。自分のglueの実call/表示/失敗保全は検証する |
| 通常利用に必要な導入・外部起動・読める案内・accessibility | 同梱依存/hash/license、実ロード・代表native操作と可読性/Tab/狭幅/theme/DPI、READMEと標準editorの代表open/read | playerの全seek精度やeditorの全件Find動作を製品保証にしない |

相互運用は代表open・短いPlay・前後seekを一度確認します。既存の観察を有効条件へ対応付けて再利用でき、一Caseごとの新GUI枠は作りません。同条件の複数Caseへ共有でき、全再生・全境界精密seekを繰り返しません。異常は自動的に製品bug又は外部bugにせず、生成container/index/edit/timing/content違反、reader実装差、未帰属に分けます。製品違反と未帰属は未完了。根拠付き外部差は診断履歴と利用上の制約に残し、全player一致のためだけに出力を改変しません。実interopを省略したFILE検査だけでsmoke成功を主張しません。

## 全139 Caseの監査と有効条件

正式計画の9 groupが全139 keyを重複なく対応付けます。原204出典と追加4出典（計208）のCase全文hash、原plan canonical `a5c470bf6bdbbbe77d2001bcb287667e4a6d399ee935ebd3fc23388be2f37441`は保持します。37 keyには必要な個別overrideがあります。原Case JSONとそのpending/blocked/failは書き換えません。

| group | Case数 | 判断と理由 |
|---|---:|---|
| environment | 6 | 維持。同梱依存/SDK/PATH非依存とロードの実integrationが必要 |
| markers | 2 | 維持。fixtureの表示/phase/view/coalescingを誤ると測定oracleが成立しない |
| precision | 7 | 統合。同じ30分/300eventを13/31/41/27で共有。裾の外れ/経時driftは維持 |
| recipient | 6 | 置換。全FILE/内容と代表interop/editorへ分担し外部全Play/全Find保証を除外 |
| buffer | 9 | 統合。180秒超/開始20秒/重複保存の実工程とproduction境界回帰/GOP6証拠を共有 |
| recovery | 57 | 統合。所有・失敗・clock/log/window/保存不変性を保持し、全faultは隔離回帰、glueは代表native |
| ui-settings | 37 | 統合。自製品入力/復元/帰属/表示の契約を保持し、全race/数値境界は回帰、代表操作はnative |
| video | 12 | 置換。全packet/frame/画素・VFR/unknown/Levelを保持し外部精密seekはFILE＋代表smokeへ |
| log-reader | 3 | 維持。header/lid/payload/readerの全境界とnative有限母数の用途が異なる |

#201は27:SAVE-FAIL-02-EMU/REALの保存attempt削除義務だけを明示置換します。失敗・取消し出力はpath削除せず残し、衝突entry/所有不明物/差替え/symlink/完成物を保全します。作成済みsessionのcomplete:falseと同identityの実在場所の案内はbest effortで、書込み不能やidentity不明を保証しません。取消しmessageの場所保持と既存OFF/closeのUI抑止を両立し、残留容量増加・自動回収なしを明記します。固定T/N/対象/pin・新規retry、成功時atomic非上書き、取消しの既存参照解放/Lifecycleは維持します。#198の停止済み旧取得root回収と通常取得/remote cleanupは別の責務で、保存attempt残留を回収しません。原cleanup条件と旧成功/失敗/未達は履歴のままです。

個別の有効preconditions/steps/expected、Agent条件、延期、人間指定の変更は同blockの`overrides`だけを使います。未記載の機能条件は維持します。実施分担はgroupの`methods`が原文のnative/GUI全種反復指定を置換し、元の意味・全種の期待結果を回帰で検証します。41/27/71の実保存失敗→固定retry等、実integrationでしか分からない代表工程は残ります。45:VFR-BREAKは全5種類の前/中/後保存をproduction隔離fixtureへ明示割当し、別nativeの取得/復旧/表示を束ねます。52 UNKNOWN、49 ACQUIRE、106のfaultも合成とnativeを混同しません。

85の通常3event/両log9phase、PRE3＋POST3/両log18phase、N300は保持します。240秒の事前host余裕は段取り例へ降格し、保存後の固定snapshot T・個別frameの支持mapping/誤差で全対象の窓包含を証明します。窓外・欠落・unknownは救済しません。106の100入力/300phaseは有限reader比較の欠落検出として維持し、同期300event/30分を代替しません。

一律601画像のhuman手読取りと16 Caseの最終human actor指定は除外します。全300遷移/前後frameの番号識別と測定は維持し、独立校正された自動oracleで代替できます。元fixtureの画像真値とpackage/run/view/event ID、誤認/欠落/重複/coalescing/unknownの悪い入力、全関連frame/PTS/誤差、画像とlogの独立性、識別不能時の拒否を検証します。既存`transitions.swift`の色/parity補助は番号を復号せず偶数個の欠落を見逃すため、この代替oracleではありません。期待log順序やconfidenceだけから画像番号を推定しません。曖昧箇所は実画像確認し、識別不能/誤差過大なら未完了です。現時点で自動oracleの校正や既存画像判定をPASSとしたものではありません。REAL34、元の実機条件、3:PLUGIN-LOADの証拠付き操作経路延期は保持します。

## 既存の自動試験と今後の担当依頼

CLIの自動試験は[同梱CLI](../cli.md)の既知入力と互換経路を検査し、ADB/daemon/録画・時刻/保存/端末の成功を主張しません。JVMの試験群はProtocol/Clock/Store/Writer、Wire/fake SDK、所有cleanup/資源、Settings/IDE接続/UI説明に分かれます。全packet/画素、整数/悪い入力、原子公開/停止やpinの回帰は自製品責任であり削除しません。IDE bridge/settings回帰を外部IDE全体の合格に換えません。fixture instrumentationはoracleの健全性に使い、製品動画の合格とは別です。

ハーネス群はGit保護/影響判定、出典/plan/gate/listing/reuse、registry/lease/worker/handoff/QA、秘密/参照の整合を検査します。候補変更のためだけの無条件再build/全native再試験は要求せず、下記の証拠と変更影響で判断します。テストを読んだ事実と実行成功を区別し、既存の未変更suiteを理由なく反復しません。

担当へは既存Issue/計画から目的・自製品の観測可能な欠陥・有効条件・最小手段・全データ母数・既存証拠・停止条件を渡します。新しい台帳/毎操作フォーム/承認gateを作りません。単に試せる、原文に書いてある、長いという理由で追加/削除せず、固有の欠陥を検出できるかで選びます。shared lease・privacy・実tool権限・所有fixture・復旧は全方法で維持します。

## trusted-main改訂と旧観察の再評価

以下の不変artifact/history条件は従来の責任改訂経路です。製品修正を跨ぐ現在の成功事実再利用には、別の[保証単位の影響証明](README.md#修正影響による保証単位の証拠再利用246)を適用できます。全ZIP不変/元promotion必須をその新経路へ戻さず、原記録/hash/失敗と全Case/plan/延期を保持し、現在load・影響部分のfresh factsと独立実質評価を要求します。方法改訂と実行、元identityと現在identityを区別します。

#190は`plan_extension`に旧required ancestorと追加Caseを明記し、原135 entryを再構成して上記の原plan hashへ照合します。#182のgroup/override/分担は保持し、recoveryへ`186:CLOCK-BATCH-SAVE`を1件追加します。全5手順の同ZIP Clock/Store/Wire回帰と実IDEの保存・継続取得・失敗固定retryをQA74/27の同候補/同条件の観察で共有し、最大4096履歴の長時間GUI反復を追加しません。新required ancestorはPR189実統合です。旧135計画は歴史上の有効計画として保持しますが、新136計画と一致するものへ扱い直しません。今回のclock製品class変更をまたぐ旧観察の再利用は既存history gateが拒否し、旧pending/fail/延期を新PASSへ変更しません。

#201は#190と同じplan_extensionにPR199/200実develop mergeの197/198 Caseを累積追加し、required ancestorを `4d278e65dec0c55e71d584ff24c7f6fa407586cb` へ進めます。previous_required_ancestorは原135計画の `5bf9e84a887171df5f0287b036e9ba288fbfae9f`、previous_plan_sha256も原135hashのままです。追加3keyを除いて原135全文hashを照合し、直前136 entry/205出典も保全します。全138/207・recovery57・REAL34と固定source/hashを共通resolverで検証し、candidate-only改訂や欠落/不一致を拒否します。新Case全手順の同ZIP製品class回帰と代表native失敗場所表示/固定retry・起動回復/設定復元・通常取得/Save/継続/OFF/closeはQA27の同候補観察へ束ね、全故障のGUI反復・新human/REAL延期・新QAを追加しません。main統合後の専用develop同期・新候補/標準ZIP固定、QA27/65・release10の双方向移管/readbackはPMの後続作業です。契約/計画統合を製品受入にしません。

#219は既存plan_extensionへ `216:SETTINGS-WIDTH-RECOVERY` を累積追加し、PR218の実develop merge `06419bd17dddc00a1b57c086be4e86d6184e945f` をrequired ancestorにする。全139/208・Agent105/REAL34、ui-settings37となり、旧138 entry/207出典・group方法/override/責任契約/延期は変更しない。新Case全2手順と期待を限定同ZIP Settings製品class回帰＋QA27/175の代表native幅復帰・編集/Apply/Cancel/reopen/復元へ分担し、元173等の全有効条件が同条件で満たされる観察だけを共有する。新human義務/延期やGUI全matrixは追加しない。

plan_extensionのprevious_plan_sha256は登録旧138計画 `6d11e3cdee0b6189c743fb1a4c8ee3a4e582d72af7e9643bb001536dd4e0c79f` に固定する。追加Caseとui-settings所属を除き、ancestor/extensionを#201へ戻した全文を独立固定hashで照合する。#208 installation_revisionは、その登録旧138計画から追加3 overrideを除いて固定13bdへ照合し、#214の自己申告hash拒否を保持する。原135hash/旧136計画も不変。旧138計画の保持事実/元PASSは出典として照合できるが、新候補は139計画の正確なmain同期を要し、未知の旧計画やcandidate側だけの改訂を認めない。製品JAR変更を跨ぐ旧707等の事実移植はhistory gateが引き続き拒否する。計画統合後の専用develop同期・新固定候補/標準ZIP・native検証、QA27/175/65・release10と元216への双方向移管はPMが行い、登録だけで受入完了にしない。

#208の本人指示で通常導入を[公式ローカルZIPのターミナル配置](../operations.md#固定plugin-zipのターミナル導入)へ変更します。原Case/cases欄・全138/207・104 Agent/REAL34・全出典hash・延期・失敗を不変で保持し、`3:PLUGIN-LOAD`の有効overrideだけでInstall Plugin from DiskとSettings → Pluginsの版探索義務を除外します。自製品Settingsの表示/入力/Apply/Cancel/検証/アクセシビリティ/寿命、初回自製品About/action、全3JAR/ZIP/source/version/PID/profileの実ロード照合、取得OFF、通常再起動は維持します。既存の再起動後About再読取り/dialog終了画面確認だけを延期し、新たな延期は追加しません。

#208時点の`installation_revision`はIssue 208と直前138計画全文のcanonical hash `13bd1e294519915d8c8a66be2770811a4d6b28465432061e7c2eae91a87fd8a3`を登録します。このfieldと追加した3のoverrideを除くと、その旧計画の全文へ完全一致しなければ拒否します。#182の原135 `previous_plan_sha256`、#201のancestor/extensionは変更しません。#208時点では候補側は当時の計画又はこの登録旧計画全文だけを許可し、任意の古い計画/候補単独改訂は認めません。707の固定source/ZIP/製品class/全JARと実条件が不変なら、元planと原記録を保持したまま以下の再評価へ対応付けられます。方法改訂だけで再build/再導入/無関係な全再試験を要求せず、不足する実ロード/UI/復旧等の事実だけを実施します。既存history gateは製品/build/fixture/Case変更と途中revertを引き続き拒否します。707のCase PASSは本改訂から作らず、独立レビューが全有効条件・不変artifact・失敗帰属を確認します。

改訂を独立レビュー付きtooling PRでmainへ入れ、gateと候補付き一覧は固定main baseの同blockを読みます。candidate側のみの改訂、欠落/不一致plan、全Case集合や原plan/hashの改変、symlink/重複JSON keyは拒否します。候補は現在のrequired_ancestorとtrusted計画の一致条件を満たす必要があります。#182当時の旧candidateへの適用は当時の計画に対する履歴であり、今回の追加Caseを免除しません。promotionの候補後metadata制限・全commit出典/strict baseは維持します。main文書を取り込むため既存の専用develop同期と新source候補が必要な場合も、不変製品/build/fixture・同artifactを実証して旧観察を再評価でき、source SHAが進んだだけで再build/再ロード/全GUI再試験を命じません。意味が変わる試験に不足する事実は追加実施します。製品修正/不明な依存変更は新候補と影響再試験へ戻します。

新結果は現planの`stage_revision`に加え、観察の`responsibility_revision`（blockのcanonical SHA-256）、`responsibility_evidence`に`contract_coverage / artifact_binding / failure_attribution`とgroupの`evidence_keys`の全参照を持ちます。全有効条件への対応、実生成/全JAR/元source/成果物binding、既知fail/未帰属の処理、group別の実行証拠を独立レビューします。参照文字列の存在だけでは内容を証明しません。

旧成功の再評価は既存の`reused`形式と同じキー（`source_record`による固定promotion記録との一致を含む）で`status: reassessed`を使います。`source_result`に元の`initial-pass`又は全体`pass`をそのまま保存し、`source_candidate`は現在と同じでも構いません。`confirmation`は新しい試験実行ではなく、新条件への対応確認です。現candidate/hash・実確認者/日時・loaded identity・equivalence/environment evidenceと上記の責任改訂証拠を要求します。元planは#182の`previous_plan_sha256`又は#208の登録直前計画hashに一致し、元の実行経路・GOP/人間証拠を検証します。祖先関係・不変製品/build/fixture/設定と途中revertを含む既存history gateを維持します。

旧fail/pending/blocked/deferredの**判定そのもの**の再利用、再利用の連鎖、未確認の原結果、旧日時より前の確認は拒否します。部分報告を旧wholeCase PASSへ昇格させません。一方、旧wholeCaseが外部側の旧義務でpendingでも、その報告で独立に実証された成功事実は捨てません。新しい`initial_observation`又は通常`pass`の現在評価として、`contract_coverage`に各旧事実の元候補/成果物/観察者/日時/固定証拠と全有効条件への対応を記録します。`reason`で再評価日時と元の実行日時を区別し、新GUI実行と主張しません。`artifact_binding`には不変artifact/製品/build/fixture/条件の実証、`failure_attribution`には旧fail/pendingの理由と未解決の自製品違反を保全します。欠ける事実だけ追加実施し、未解決の自製品失敗があれば新判定もfailです。これは旧判定のcopyではなく、別のレビュー対象です。この経路は観察に`responsibility_assessment`を追加し、`source_candidate / source_result / source_result_sha256 / source_record_evidence`に固定の原判定を保持します。`facts`はID付きの実成功観察（通常の観察形式、原candidate/hash/実施者/日時/ロード/証拠）、`coverage`は全責任証拠keyからそのfact IDへの対応です。`missing_requirements / unresolved_owned_failures`は空でなければ成功不可。fact元候補のplanは、正式改訂に登録した原plan、#208の登録直前計画、#219の登録旧138計画、#223の登録直前139計画（source830）、#230の登録直前223版又は現在trusted有効planの全文と完全一致するものに限ります。docs-only同期後の同じ正式改訂は許可し、任意の別改訂は拒否します。gateはこのplan照合/不変履歴・元結果hash・factの候補/成果物/成功/日時・対応の欠落を検査し、独立レビューが原記録の一致と全条件の実質を確認します。改訂hashを旧結果に足すだけでは成功にしません。通常の`reused`はresponsibility_revisionも同じ契約の例外です。元promotionの記録がない成功を埋め込みreuseで作りません。旧pending内の成功事実は上記の原記録/固定hashと独立レビューを要する別経路として保持し、過去の正式Case PASSがあったと主張しません。

gateと一覧は有効条件/原文履歴/再評価を分けて示します。初期段階の合格は完全受入ではなく、後続の全体gateには全139の有効条件を復元します。責任改訂そのものはCase PASS・GUI実行・初期版完成・main製品受入を意味しません。旧候補/ZIP/15件観察/正式PASS0は履歴として保持し、再評価後の件数は別記録とします。QA27/65/87とrelease10の担当・残条件を双方向readbackした後に次の試験を割り当てます。

後続PRが同じCase IDを更新した完全受入では、候補内の最新Caseを用い、履歴行で置き換えません。旧Case用の置換overrideはその固定原契約のhashが一致する時だけ適用します。追加された最新契約はgroup分担/共通条件を維持した上で、観察に`case_sha256`として有効Caseのcanonical hashを要求します。履歴は現契約がない時の補完と原文表示に保持します。
