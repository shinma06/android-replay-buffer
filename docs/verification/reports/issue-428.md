# 第2版固定候補の保証別適用 — Issue 428 Draft

固定候補0.2.0へ、保持済みの成功事実と現在の影響評価を同じproof PRで集約する。旧draftへの独立評価F1/F2を訂正し、支持差分と既存whole結果の正式形式を接続する。**この文書と証明は未承認Draftであり、Case PASS・trusted証明採用・製品promotionではない。** 原資料は上書きせず、後継差分として保存する。

## 固定対象と登録範囲

| 対象 | 固定値 |
|---|---|
| source | `2e576ce3c724dc95ccf0688b8a1bf7cd92d774a8` |
| version | `0.2.0` |
| 正式ZIP SHA-256 | `f3274d7501f9d173ece5de6b1cb729a7fb40d247cfe2a17c6c937fcc085a2c88` |
| main JAR SHA-256 | `5fa8de88f0b3efd37ac6da99f5a9231d75adfaa9ef37a4e8c98ca512c9d40080` |
| 実trusted base | `fb3c417a27079a503ab28db380b4042dec994a3f`（exact登録PR427採用） |
| 登録集合 | 37候補commit／177 Case／246出典、初期139＋通常38 |
| 独立適用評価 | report SHA-256 `3d48390a225dcf56c2e4e1344e62e9a93b92b7d1c495393b7e5dace621b97905` |

[親280](https://github.com/shinma06/android-replay-buffer/issues/280)・[既存配布271](https://github.com/shinma06/android-replay-buffer/issues/271)の責任を維持する。初期139の明示延期を通常38へ輸入せず、原契約・原source/ZIP/actor/time・FAIL/UNKNOWNを保持する。独立評価の読取りbaseだったfb2を過去へ書き換えない。後続作業には採用後fb3cを使い、policy/schema/責任契約が不変なことを確認した。

初期139の有効groupと通常38の全原条項は[固定計画](../amendments/initial-agent.json)・[exact登録](../amendments/initial-agent-continuation.json)を正本とする。この文書は新しいCase台帳・plan・判定器ではない。実schemaは[保証単位の影響証明](../README.md#修正影響による保証単位の証拠再利用246)を使う。

## F1/F2とD4 copyの後継訂正

### F1：416の実form labelForは支持済み

原`416:SETTINGS-INNER-ACCESSIBILITY` step1の実Configurable EDT生成、wrapper内実editorの名前、JLabel.labelForからeditorへの直接接続と正確label文字列、手動保存先への有効切替後の同じ検査は、既存の実`ReplaySettingsLayoutTest`がassertする。元JUnit SHA-256は`50ee425b736afe64e9405183596490a481d1c11dc30ad9359608e995da61cd12`、元時刻2026-11-06T00:30:34.565Z、1 test、failure/error/skipは0。元tested sourceと現2e/f327のSettings production/test source・全14 class一致を独立担当が照合した。

したがって旧draftの「exact exported labelFor未支持」を訂正する。この構造保証のための新native試験は不要。step2の既存native focus/label/Helpとは役割を分け、206の永続化や416全体を自動合格にはしない。

### F2：clipboardの原契約は104へ戻す

`325:SCREENSHOT-ACTIONS`はOpen folder／所有PNG Delete／実通知の失敗説明と撮影client寿命を要求する。clipboardをその追加gateにしない。読取詳細の選択とコピーは`104:GUI-ACCESS` step2へ帰属させ、UNKNOWNと元の条件を保存する。実通知可視性・OS Openの未接合は325に残る。

### 104の該当詳細Copy-contentは条件付き支持

既存D4の別担当評価SHA-256 `2d5bd78a674e2214f846d2abd78d56499805ee9f41e58b91514fca72104aab6b`を直接接合した。原948/a582のOFF詳細欄でclick／Cmd+A、2026-11-05T22:45:56.359Zの1回のCmd+Cが記録され、その後の実bridge読取りはcount1316・item1・3 canonical typeの229 UTF-8 byteが固定expected SHA-256 `389ff5cf5f919be56541b2b5110c6c7f3abdfca24757d43c86da15d8cb5a8e1b`と一致する。今回も保存済みsnapshotをdecodeして一致を確認した。

これは**該当詳細欄の使用可能な選択・コピー内容**を支持する。遅延読取りを旧Copy瞬間の本文一致・全選択範囲・現在のclipboard安定性へ拡張しない。元v2/v3、restore conflict、別slotのUNKNOWNと後の元baseline復元は別記録のまま残す。復元の不明だけを理由に新Copyを要求しない。G4をclipboard一般の全機能義務や104 whole Case PASSへ広げず、同じnative担当の固定packetで追加の資格を結合する。

## 支持済み保証の現在適用

| 保証 | 適用する保持事実と現在binding | 保持する限界 |
|---|---|---|
| environment/current identity | f327 installed3JAR hash/size/mode、実loaded paths/JBR、About版/fullsource、OFF常設理由の初期→Close後全文→native再表示、通常返却。自然SDK→A2 CLI ON/reader/2Save/STOP | raw snapshotの正確wall-clock瞬間は不明。process unit範囲と適用評価時刻を区別。f327上の全操作再実行にはしない |
| markers/precision/content | 143の全149 source/output sample・pixel/番号・全FILE、Clockの実境界回帰・mapper/epoch/unknown、C全84sample/PTS/editと全logを役割別に共有 | Cの32 unknown/52未校正候補を画像真値にしない。元physical FAIL・未校正を保持。旧総合精度・長時間数値を一律gateへ戻さない |
| buffer180 | LIFEの実clock293に対応する289.73195345秒・正確180秒窓／LOG_ONLY完成5FILEと、共通T/N/prune/pinおよびAV固有GOP/VFR/IDRの実回帰 | LOG_ONLYを180秒AV録画と呼ばない。追加性能・heap・長時間を発注しない |
| PARTIAL/recovery | attempt4/eviction2のvideo EOF・log EOF・clock不達中の健康T/受理、実eviction1行→同完成FILE/metadataの4境界。通常close・B同boot復旧・所有停止・atomic/pin/retry回帰 | fake SDK/loopback・実production classでありAndroid DEX成功ではない。Homebrew JDK/hash資格、attempt3原因UNKNOWN、元compile/Gson/stale/eviction失敗を保持 |
| CLI理由/寿命 | 実busy/failed-pin拒否、accepted client切断→同writer完了→Capture/Service/Notifications.TOPIC、独立status/FILE、A2重複保存。422修復RED→fixed/FAILED pin案内と現class等価 | G1のGUI action・同attempt表示・同pin UI接続は未着。Service配送だけを画面表示にしない。stale busy原FAILは保持 |
| Logcat | A-only開閉/外部追従/Tab-Space/幅、native fresh B→Replay Open→native fresh A、424修復と193/193class binding、f327理由全文 | 原empty label FAIL/hover UNKNOWNを保存。SDK内部object/5秒を直接測定したとはしない。一律新instrumentation不要 |
| Settings/Screenshot | 実Store/Configurable/Capture回帰、Apply/Cancel/reopen/通常close復元、416 focus/label/Help、R1実失敗案内/PNG保全とR2client/readers停止/queue/旧action拒否 | 内部resetとnativeCancel、fake SDKと自然SDK、callbackと実OS通知を区別。USB固有条件は下記 |
| recipient/interop | 元全内容FILE・代表player、TEXT2の実open/read、既存consumer観察を保証ごとに共有 | 全FILEと代表interopは別。全player/全seekやnative encoded raw追加採取を一律義務にしない |
| AUTO | G04実external reporter/target、unchanged guard/境界回帰、LOG_ONLY・subprocess AVの2成功FILE、BOUNDS元whole判断 | G04writerFAILED、negative/counter未達、個別帰属の資格を維持。successごとのkernel認証・追加mode×subprocess matrix不要 |

PARTIALの4辺は古いB/B1の未接合文言を、後続の固定実行証拠が補ったもの。元文書・失敗を消さず、同じ辺を「未観察」に戻さない。C単独のraw未取得を、既知入力の全frame回帰＋143画像/全FILEという分担を無視した追加採取義務にしない。#373で終了した追加性能作業は再開しない。

## 同じPRに集約する既存whole結果

通常338／341の元948 normal whole result、nested original pending/ordinary assessment、限定独立評価は不変で保持する。現在2eへの関連history/blob/modeとSettings/Service/Screenshot影響評価は、この2保証を破る差分を示していないという独立支持を受領した。元候補の承認と現在固定PRの正式承認は別である。

`284:CRASH-AUTO-BOUNDS`も元948 whole decision SHA-256 `319c2c0bf0518ccfdd2afed25796975e665b1a4b18f204e1103f7ca51638be29`と原normal resultを同じPRへ接続する。原preconditions/step1/step2/expectedの実分担は、89回帰、有限manualSUCCESS1/autoFAILED1、8段階会計と出力保全。autoSUCCESS同時native・飽和・heapを新要件にせず、DETECTのnegative不足をBOUNDSで埋めない。

3件の証明は[所有Issue受入JSON](../changes/issue-428.json)の既存`evidence_certificates`形式に置く。review.statusはpending、実reviewer/report/completecontributors/executors/originは未成立。現在assessmentもpendingであり、元whole resultを現在Case PASSへ付け替えない。正式適用の証明登録を成立させるための完成部分であり、2件だけ先行mergeして残りを別準備へ直列化するものではない。

## 残る結合点と担当

| 残り | 正確な辺 | 担当・扱い |
|---|---|---|
| G1a | 103 SHARED step1のGUI actionとCLI要求→同writer制約 | 既存native担当。原action trace接合優先、なければその辺だけ。結果未着でpending |
| G1b | 同step2／OWNERSHIP step2の同attempt native表示とFILE | client切断/Service/FILEは支持済み。表示の実接続だけ既存native担当で確認、先取りしない |
| G1c | 同step3の同pin UI retry又はdiscard→同request/attempt | 既存native担当。内部pin/retryの支持を保持しGUI原条項を消さない |
| G2 | 106 PARTITIONの有限予定main/nonmain陽性入力集合→全対象FILE分母 | 既存有限nonGUI担当。4tupleの支持を維持し、100/300や全wire不在を新gateにしない |
| G3 | 325 ACTIONSの実OS Open／実通知失敗説明 | 既存native担当。R1/R2の支持を維持し実OS辺だけ。clipboardは含めない |
| G4 | 104 step2の該当詳細選択・Copy-content | D4の条件付き229byte支持へ更新済み。同担当の最終固定資格を結合。別通知や全clipboard機能へ広げず、新Copy不要の方向 |
| G5 | 284 DETECT step2/expectedのnegative/別対象→counter不変 | 既存有限nonGUI担当。元negative/accountingだけを扱い、BOUNDSや2成功で先取りしない |

REAL/humanは一括「全体未完」にせず、初期139の既存34REAL等の明示延期だけを保持する。通常`325:SCREENSHOT-USB`はpreconditionsの実USB端末と各stepの固有条件が未充足であり、本人保留・物理利用不可・実観察欠測を区別してpending。`333:SCREENSHOT-DEVICE` step2は利用可能な対応端末の支持/非対応拒否を原条件で適用し、EMUをUSB成功へ換えない。`377:SCREENSHOT-MIRROR`の「物理画面」はlogical displayとの区別であり、一律physical USB端末の新要件にしない。通常38へ初期延期を輸入せず、各原条項で判定する。

## 独立性・origin・統合条件

実独立適用担当は供給された25 actor・11 origin root・58原record byte-rangeを照合し、本人との重複なしを確認した。人数だけで未記録actorの不存在は証明しない。writerは既存意味/方法寄与者かつ本proof authorであり、combined独立review不可。今回の独立評価はreview行為であり、元証明案の共同writerへ読み替えない。

原recordは公開safe hash/要約へ対応付け、private原byte・source range・canonical origin・activity/timeをPM custodyに保全する。header作成時刻をexecution時刻にしない。現在はorigin adapter receiptを発行していない。未成立のapproved値・実merge SHA・CI成功を記入しない。

同じ固定PRで変化部分とactual base/全feedback/required CIを確認し、同じ独立担当が正式評価・実originを結合する。承認欄が未成立の証明はAcceptanceの成功/merge条件を満たさない。証明main採用後の実commit/path/id/canonical hashだけをassessment.impact_reuseへ接続する。製品promotionと既存271/QA/release/cleanupの終了確認は別であり、本PRから実施しない。

新test/build/GUI/SDK/ADB/device/lease、製品/契約/schema/validator変更は行っていない。正式PRの分類別check・通常hookは実施記録をPRへ残し、同じcleanHEADで不要なsuite反復をしない。未着factの監視loop・別の準備レビュー・全map再監査は追加しない。

## 固定参照のhash

| 参照 | SHA-256 |
|---|---|
| 独立適用report | `3d48390a225dcf56c2e4e1344e62e9a93b92b7d1c495393b7e5dace621b97905` |
| 同handoff | `a082622625f5630a6395e6e7e6bd9d5fffc430bc59254cffba25bf4d70b5c526` |
| D4内容評価 | `2d5bd78a674e2214f846d2abd78d56499805ee9f41e58b91514fca72104aab6b` |
| BOUNDS元whole評価 | `319c2c0bf0518ccfdd2afed25796975e665b1a4b18f204e1103f7ca51638be29` |
| 現在候補application原本 | `138f7dd0e2d6c3d1b34a798e8bd4583c8ee7f66d96ae47526ea60dc0e800ba9c` |

媒体・raw log/diagnostics・private path・canonical origin UUIDは公開しない。原file/result/factの対応は独立reviewのprivate bundleで照合する。


<details><summary>未採用のcandidate-specific assessment（既存results形式）</summary>

```json
{
  "results": {
    "338:SCREENSHOT-DESTINATION": {
      "status": "pending",
      "head": "2e576ce3c724dc95ccf0688b8a1bf7cd92d774a8",
      "artifact_sha256": "f3274d7501f9d173ece5de6b1cb729a7fb40d247cfe2a17c6c937fcc085a2c88",
      "actor": "gpt",
      "observer": "codex-session-sha256:7b2456c4ac692a16695fa3b6f035ac2149e76f23bd63e479f43f81ca687087e1",
      "at": "2026-11-06T13:58:56.017172+00:00",
      "evidence": "Candidate-specific normal338 application draft; fixed original canonical-sha256=aabe7ad54113619d1e55406ade28e562ace1b059860c90b67383dd9f30c564be; independent current application not approved.",
      "loaded_identity": "保存済み実行者原本のinstalled 3 JAR hash/size/mode、loaded paths/JBR、About version0.2.0/full source2e576ceを照合。保存画像のOFFと常設理由全文を確認。現在のGUIや新実行を主張しない。",
      "reason": "Issue428の候補別正式assessment案。元whole結果と限定独立適用支持を保持するが、固定PR全feedback/実originを含む正式承認は未成立。",
      "responsibility_revision": "85296114925d6947be93158e3eca90fcd69e561b3d8a5d1e6845407b73108f61",
      "responsibility_evidence": {
        "contract_coverage": "Candidate2e/formalf327 draft 338/contract_coverage; facts=RETAINED_NORMAL_338_WHOLE_RESULT",
        "artifact_binding": "Candidate2e/formalf327 draft 338/artifact_binding; facts=CURRENT_F327_LOAD",
        "failure_attribution": "Candidate2e/formalf327 draft 338/failure_attribution; facts=RETAINED_NORMAL_338_WHOLE_RESULT",
        "preconditions": "Candidate2e/formalf327 draft 338/preconditions; facts=RETAINED_NORMAL_338_WHOLE_RESULT",
        "step_1": "Candidate2e/formalf327 draft 338/step_1; facts=RETAINED_NORMAL_338_WHOLE_RESULT",
        "step_2": "Candidate2e/formalf327 draft 338/step_2; facts=RETAINED_NORMAL_338_WHOLE_RESULT",
        "step_3": "Candidate2e/formalf327 draft 338/step_3; facts=RETAINED_NORMAL_338_WHOLE_RESULT",
        "expected": "Candidate2e/formalf327 draft 338/expected; facts=RETAINED_NORMAL_338_WHOLE_RESULT"
      },
      "responsibility_assessment": {
        "source_candidate": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
        "source_result": {
          "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
          "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
          "actor": "gpt",
          "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
          "at": "2026-11-05T22:34:06.869289+00:00",
          "evidence": "Private338wholeproofsha256=8b6952d29ff65103dc68b65a4105de13b8de6765e8fef0b9d24dce851997b873; actualtrustednormalresolver=ca863083d3db673cbc1b6f3c79b5b44de69681f5",
          "loaded_identity": "Retained actual currentPNG load source948/formalZIPa582/mainJARd620, installed6/loadedJARs at StudioPID28904/AI-262.10968.63.2622.16730706 and emulator5602. No present host/lease or physicalUSB claim.",
          "reason": "Private final normal current-result proposal after actual fixed independent approval; canonical observer metadata corrected. Original fact assessment times and original native records preserved. Formal adoption is not performed here.",
          "responsibility_revision": "85296114925d6947be93158e3eca90fcd69e561b3d8a5d1e6845407b73108f61",
          "responsibility_evidence": {
            "contract_coverage": "Private338clause contract_coverage; proofsha256=8b6952d29ff65103dc68b65a4105de13b8de6765e8fef0b9d24dce851997b873; facts=ACTUAL_CURRENT_LOAD,CURRENT_DEFAULT_REPLACEMENT,ACTUAL_SELECTED_ROOT_AND_SETTING_CONNECTION,ACTUAL_DIRECT_COMPLETE_PNG,ACTUAL_SAME_NOTIFICATION_OWNED_ACTIONS,CURRENT_SETTINGS_AND_REQUEST_SEMANTICS,CURRENT_OWNED_IMAGE_DEVICE_CANCEL_LIFETIME_SEMANTICS,ACTUAL_BOUNDED_NONINTERFERENCE,PRESERVED_FAILURE_ATTRIBUTION",
            "artifact_binding": "Private338clause artifact_binding; proofsha256=8b6952d29ff65103dc68b65a4105de13b8de6765e8fef0b9d24dce851997b873; facts=ACTUAL_CURRENT_LOAD,CURRENT_SETTINGS_AND_REQUEST_SEMANTICS,CURRENT_OWNED_IMAGE_DEVICE_CANCEL_LIFETIME_SEMANTICS",
            "failure_attribution": "Private338clause failure_attribution; proofsha256=8b6952d29ff65103dc68b65a4105de13b8de6765e8fef0b9d24dce851997b873; facts=PRESERVED_FAILURE_ATTRIBUTION,ACTUAL_BOUNDED_NONINTERFERENCE,CURRENT_DEFAULT_REPLACEMENT",
            "preconditions": "Private338clause preconditions; proofsha256=8b6952d29ff65103dc68b65a4105de13b8de6765e8fef0b9d24dce851997b873; facts=ACTUAL_CURRENT_LOAD,CURRENT_DEFAULT_REPLACEMENT",
            "step_1": "Private338clause step_1; proofsha256=8b6952d29ff65103dc68b65a4105de13b8de6765e8fef0b9d24dce851997b873; facts=CURRENT_DEFAULT_REPLACEMENT,ACTUAL_SELECTED_ROOT_AND_SETTING_CONNECTION,CURRENT_SETTINGS_AND_REQUEST_SEMANTICS,PRESERVED_FAILURE_ATTRIBUTION",
            "step_2": "Private338clause step_2; proofsha256=8b6952d29ff65103dc68b65a4105de13b8de6765e8fef0b9d24dce851997b873; facts=ACTUAL_DIRECT_COMPLETE_PNG,ACTUAL_SELECTED_ROOT_AND_SETTING_CONNECTION,ACTUAL_BOUNDED_NONINTERFERENCE,CURRENT_OWNED_IMAGE_DEVICE_CANCEL_LIFETIME_SEMANTICS",
            "step_3": "Private338clause step_3; proofsha256=8b6952d29ff65103dc68b65a4105de13b8de6765e8fef0b9d24dce851997b873; facts=ACTUAL_SAME_NOTIFICATION_OWNED_ACTIONS,CURRENT_OWNED_IMAGE_DEVICE_CANCEL_LIFETIME_SEMANTICS",
            "expected": "Private338clause expected; proofsha256=8b6952d29ff65103dc68b65a4105de13b8de6765e8fef0b9d24dce851997b873; facts=ACTUAL_CURRENT_LOAD,CURRENT_DEFAULT_REPLACEMENT,ACTUAL_SELECTED_ROOT_AND_SETTING_CONNECTION,ACTUAL_DIRECT_COMPLETE_PNG,ACTUAL_SAME_NOTIFICATION_OWNED_ACTIONS,CURRENT_SETTINGS_AND_REQUEST_SEMANTICS,CURRENT_OWNED_IMAGE_DEVICE_CANCEL_LIFETIME_SEMANTICS,ACTUAL_BOUNDED_NONINTERFERENCE,PRESERVED_FAILURE_ATTRIBUTION"
          },
          "responsibility_assessment": {
            "source_candidate": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
            "source_result": {
              "status": "pending",
              "reason": "writerはsource/fake/filesystem/Swing回帰のみ。実IDE/通知/OSの観察は未実施。"
            },
            "source_result_sha256": "00709385d646897b8d322279bd6a58f4837d0f0b107ec8fb0529dbb8493c52d3",
            "source_record_evidence": "git:948b1ad2f18ee266c79935327a403dd7471fdc3b:docs/verification/changes/issue-338.json#/cases/0/gpt canonicalsha256=00709385d646897b8d322279bd6a58f4837d0f0b107ec8fb0529dbb8493c52d3",
            "facts": {
              "ACTUAL_CURRENT_LOAD": {
                "status": "pass",
                "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                "actor": "gpt",
                "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                "at": "2026-11-05T17:04:42.284394+00:00",
                "loaded_identity": "Retained actual currentPNG load source948/formalZIPa582/mainJARd620, installed6/loadedJARs at StudioPID28904/AI-262.10968.63.2622.16730706 and emulator5602. No present host/lease or physicalUSB claim.",
                "evidence": "Current338selectedrefs 07cb7ab27859051617e7801ee8d6249623eacb64ba2d8420c40c2df6d1322898,c4d66219ce24883207addf655012b417fbdc6c32a5b7285ec99cd4e2597f3c61; originalPNGindependentclauses G01,G10",
                "reason": "Current formal artifact, native loaded runtime and actual owned fixture identified. LIMIT: Retained actual currentPNG load source948/formalZIPa582/mainJARd620, installed6/loadedJARs at StudioPID28904/AI-262.10968.63.2622.16730706 and emulator5602. No present host/lease or physicalUSB claim."
              },
              "CURRENT_DEFAULT_REPLACEMENT": {
                "status": "pass",
                "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                "actor": "gpt",
                "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                "at": "2026-11-05T17:04:42.284394+00:00",
                "loaded_identity": "Fresh private current338 source/contract/retained-measurement applicability assessment; source executions keep original identities.",
                "evidence": "Current338selectedrefs 0d54f6871ddf9c9c70be938bbab5476e1957057349f48154eaeb160014354af6; originalPNGindependentclauses ",
                "reason": "Original338 common-default clause retained as history and explicitly replaced by adopted341 Logcat-follow/manual choice; continuing338 guarantees retained. LIMIT: Actual fixedsource/doc/public original records. Not338 erasure, newinitialoverride, independentapproval or currentolddefault observation."
              },
              "ACTUAL_SELECTED_ROOT_AND_SETTING_CONNECTION": {
                "status": "pass",
                "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                "actor": "gpt",
                "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                "at": "2026-11-05T17:04:42.284394+00:00",
                "loaded_identity": "Retained actual currentPNG load source948/formalZIPa582/mainJARd620, installed6/loadedJARs at StudioPID28904/AI-262.10968.63.2622.16730706 and emulator5602. No present host/lease or physicalUSB claim.",
                "evidence": "Current338selectedrefs 07cb7ab27859051617e7801ee8d6249623eacb64ba2d8420c40c2df6d1322898,098729fe085668231e703226266876306e1f691d241c14ccac66e90e27ed0df6; originalPNGindependentclauses G03,G04",
                "reason": "Native Screenshotssave setting changed Desktop to ownedroot/Apply; fresh reopened ARB AXroot agrees; next single productPNG uses thatdirectroot. LIMIT: Resolvedroot is offscreen AXvalue, not visible renderedpath pixels. Existingopen value remainedcachedDesktop untilreopen; preserveG04 and PMdisposition, no always-live guarantee invented."
              },
              "ACTUAL_DIRECT_COMPLETE_PNG": {
                "status": "pass",
                "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                "actor": "gpt",
                "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                "at": "2026-11-05T17:04:42.284394+00:00",
                "loaded_identity": "Retained actual currentPNG load source948/formalZIPa582/mainJARd620, installed6/loadedJARs at StudioPID28904/AI-262.10968.63.2622.16730706 and emulator5602. No present host/lease or physicalUSB claim.",
                "evidence": "Current338selectedrefs 07cb7ab27859051617e7801ee8d6249623eacb64ba2d8420c40c2df6d1322898,6c9ecfa8aa2f175600171de469992ae0895a3f1156ff7fdd8312b2cdfa71add9; originalPNGindependentclauses G02,G03",
                "reason": "Exactly1 currentdirectroot screenshot-timestamp-UUID.png82049bytes0600/no wrapper/dialog; full1080x2400/all2592000pixels independently decoded and fixture package/source/run/view/event0/colour matched. LIMIT: CurrentPNGhashf3b75a8ad9c10eb0d095428eae5ba2705e7b71036920e6bf26cb6dfa4563efc0; no oldPNG promoted to newcapture."
              },
              "ACTUAL_SAME_NOTIFICATION_OWNED_ACTIONS": {
                "status": "pass",
                "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                "actor": "gpt",
                "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                "at": "2026-11-05T17:04:42.284394+00:00",
                "loaded_identity": "Retained actual currentPNG load source948/formalZIPa582/mainJARd620, installed6/loadedJARs at StudioPID28904/AI-262.10968.63.2622.16730706 and emulator5602. No present host/lease or physicalUSB claim.",
                "evidence": "Current338selectedrefs 07cb7ab27859051617e7801ee8d6249623eacb64ba2d8420c40c2df6d1322898; originalPNGindependentclauses G05,G10",
                "reason": "Same completed notification opens actualimageparent; ownedDelete removes thatPNG only; parent/sentinel/privatecopy/12completed outputs preserved. LIMIT: Actual effects qualify uncertain delivered_unverified ACK. ActionsafterOFF; noticecontains nofilename/path. No second native request/oldnotice branch or genericnotificationCopy claim."
              },
              "CURRENT_SETTINGS_AND_REQUEST_SEMANTICS": {
                "status": "pass",
                "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                "actor": "gpt",
                "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                "at": "2026-11-05T17:04:42.284394+00:00",
                "loaded_identity": "Fresh private current338 source/contract/retained-measurement applicability assessment; source executions keep original identities.",
                "evidence": "Current338selectedrefs 6698ad057b891a2fab18f196df232110a5be9bee340c7e8004e5ef2951da4cd1; originalPNGindependentclauses ",
                "reason": "Current source/class applicability of actualService/Configurable/Store regression: manualpath persistence, ApplyCancel/asyncstale-unchecked/invalid-unavailable refusal/nonfallback, migration/CLI import and root snapshot. LIMIT: Originalf62 macOSXML/times/artifact remain original. Actual currentnative settingroot path independently observed. No newly executednative manual/error matrix or341wholeproofapproval borrowed."
              },
              "CURRENT_OWNED_IMAGE_DEVICE_CANCEL_LIFETIME_SEMANTICS": {
                "status": "pass",
                "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                "actor": "gpt",
                "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                "at": "2026-11-05T17:04:42.284394+00:00",
                "loaded_identity": "Fresh private current338 source/contract/retained-measurement applicability assessment; source executions keep original identities.",
                "evidence": "Current338selectedrefs 6698ad057b891a2fab18f196df232110a5be9bee340c7e8004e5ef2951da4cd1; originalPNGindependentclauses ",
                "reason": "Current source/class applicability of production directPNG/nonoverwrite/collision/concurrent/requestnotificationidentity/replace-symlink-deletion/devicepin/OFF/interruptcleanup and alive/disposal callback guards. LIMIT: Originalknown-input regressions plus currentService source. No fresh screenshot-inflight nativeclose implied; separateLOGONLY heldclose packet not consumed to create that fact."
              },
              "ACTUAL_BOUNDED_NONINTERFERENCE": {
                "status": "pass",
                "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                "actor": "gpt",
                "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                "at": "2026-11-05T17:04:42.284394+00:00",
                "loaded_identity": "Retained actual currentPNG load source948/formalZIPa582/mainJARd620, installed6/loadedJARs at StudioPID28904/AI-262.10968.63.2622.16730706 and emulator5602. No present host/lease or physicalUSB claim.",
                "evidence": "Current338selectedrefs 07cb7ab27859051617e7801ee8d6249623eacb64ba2d8420c40c2df6d1322898,6698ad057b891a2fab18f196df232110a5be9bee340c7e8004e5ef2951da4cd1; originalPNGindependentclauses G06,G10",
                "reason": "Same current acquisition progresses77590 to246822storedbytes, exact77590-byte inodeprefix preserved; currentsettings/installed6/12outputs preserve, joined to independent screenshot client/root dataflow and identified regression semantics. LIMIT: Only representative healthy screenshot destination noninterference. All3bareH264decode183/noSPSPPS/logunexported remain; no currentfullvideo/log content/sync proof, old377positive native facts not used."
              },
              "PRESERVED_FAILURE_ATTRIBUTION": {
                "status": "pass",
                "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                "actor": "gpt",
                "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                "at": "2026-11-05T17:04:42.284394+00:00",
                "loaded_identity": "Retained actual currentPNG load source948/formalZIPa582/mainJARd620, installed6/loadedJARs at StudioPID28904/AI-262.10968.63.2622.16730706 and emulator5602. No present host/lease or physicalUSB claim.",
                "evidence": "Current338selectedrefs 07cb7ab27859051617e7801ee8d6249623eacb64ba2d8420c40c2df6d1322898,ab54c8034632bd8a5df191496e8c7f4756941364e1edad951ec5fd169d197e19,1feee54149e4c4cfd6392492c4e379c20c3abf8af53f0764ce4f9ed446ba79c8,098729fe085668231e703226266876306e1f691d241c14ccac66e90e27ed0df6; originalPNGindependentclauses G04,G07,G10",
                "reason": "Actual cachedAX limitation, rawH264/log omissions, summary corrections, tool/parser/stale/timeout failures and scopednormalreturn retained with original attribution; existingG04 contractualdisposition applied. LIMIT: Successfulprivate attribution/preservation assessment only; no failing tool/content observation reclassifiedpass, noUSB/human/REAL/otherCase status cleared."
              }
            },
            "coverage": {
              "contract_coverage": [
                "ACTUAL_CURRENT_LOAD",
                "CURRENT_DEFAULT_REPLACEMENT",
                "ACTUAL_SELECTED_ROOT_AND_SETTING_CONNECTION",
                "ACTUAL_DIRECT_COMPLETE_PNG",
                "ACTUAL_SAME_NOTIFICATION_OWNED_ACTIONS",
                "CURRENT_SETTINGS_AND_REQUEST_SEMANTICS",
                "CURRENT_OWNED_IMAGE_DEVICE_CANCEL_LIFETIME_SEMANTICS",
                "ACTUAL_BOUNDED_NONINTERFERENCE",
                "PRESERVED_FAILURE_ATTRIBUTION"
              ],
              "artifact_binding": [
                "ACTUAL_CURRENT_LOAD",
                "CURRENT_SETTINGS_AND_REQUEST_SEMANTICS",
                "CURRENT_OWNED_IMAGE_DEVICE_CANCEL_LIFETIME_SEMANTICS"
              ],
              "failure_attribution": [
                "PRESERVED_FAILURE_ATTRIBUTION",
                "ACTUAL_BOUNDED_NONINTERFERENCE",
                "CURRENT_DEFAULT_REPLACEMENT"
              ],
              "preconditions": [
                "ACTUAL_CURRENT_LOAD",
                "CURRENT_DEFAULT_REPLACEMENT"
              ],
              "step_1": [
                "CURRENT_DEFAULT_REPLACEMENT",
                "ACTUAL_SELECTED_ROOT_AND_SETTING_CONNECTION",
                "CURRENT_SETTINGS_AND_REQUEST_SEMANTICS",
                "PRESERVED_FAILURE_ATTRIBUTION"
              ],
              "step_2": [
                "ACTUAL_DIRECT_COMPLETE_PNG",
                "ACTUAL_SELECTED_ROOT_AND_SETTING_CONNECTION",
                "ACTUAL_BOUNDED_NONINTERFERENCE",
                "CURRENT_OWNED_IMAGE_DEVICE_CANCEL_LIFETIME_SEMANTICS"
              ],
              "step_3": [
                "ACTUAL_SAME_NOTIFICATION_OWNED_ACTIONS",
                "CURRENT_OWNED_IMAGE_DEVICE_CANCEL_LIFETIME_SEMANTICS"
              ],
              "expected": [
                "ACTUAL_CURRENT_LOAD",
                "CURRENT_DEFAULT_REPLACEMENT",
                "ACTUAL_SELECTED_ROOT_AND_SETTING_CONNECTION",
                "ACTUAL_DIRECT_COMPLETE_PNG",
                "ACTUAL_SAME_NOTIFICATION_OWNED_ACTIONS",
                "CURRENT_SETTINGS_AND_REQUEST_SEMANTICS",
                "CURRENT_OWNED_IMAGE_DEVICE_CANCEL_LIFETIME_SEMANTICS",
                "ACTUAL_BOUNDED_NONINTERFERENCE",
                "PRESERVED_FAILURE_ATTRIBUTION"
              ]
            },
            "missing_requirements": [],
            "unresolved_owned_failures": []
          },
          "status": "pass"
        },
        "source_result_sha256": "aabe7ad54113619d1e55406ade28e562ace1b059860c90b67383dd9f30c564be",
        "source_record_evidence": "Immutable normal338 result projection byte-sha256=bc88f515a03684dc129831e9e6481b5ad7e2afcb4cefdacd9bfbe9ac628342bd; result canonical-sha256=aabe7ad54113619d1e55406ade28e562ace1b059860c90b67383dd9f30c564be; original nested pending/result/facts and qualified review preserved.",
        "facts": {
          "RETAINED_NORMAL_338_WHOLE_RESULT": {
            "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
            "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
            "actor": "gpt",
            "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
            "at": "2026-11-05T22:34:06.869289+00:00",
            "evidence": "Private338wholeproofsha256=8b6952d29ff65103dc68b65a4105de13b8de6765e8fef0b9d24dce851997b873; actualtrustednormalresolver=ca863083d3db673cbc1b6f3c79b5b44de69681f5",
            "loaded_identity": "Retained actual currentPNG load source948/formalZIPa582/mainJARd620, installed6/loadedJARs at StudioPID28904/AI-262.10968.63.2622.16730706 and emulator5602. No present host/lease or physicalUSB claim.",
            "reason": "Private final normal current-result proposal after actual fixed independent approval; canonical observer metadata corrected. Original fact assessment times and original native records preserved. Formal adoption is not performed here.",
            "responsibility_revision": "85296114925d6947be93158e3eca90fcd69e561b3d8a5d1e6845407b73108f61",
            "responsibility_evidence": {
              "contract_coverage": "Private338clause contract_coverage; proofsha256=8b6952d29ff65103dc68b65a4105de13b8de6765e8fef0b9d24dce851997b873; facts=ACTUAL_CURRENT_LOAD,CURRENT_DEFAULT_REPLACEMENT,ACTUAL_SELECTED_ROOT_AND_SETTING_CONNECTION,ACTUAL_DIRECT_COMPLETE_PNG,ACTUAL_SAME_NOTIFICATION_OWNED_ACTIONS,CURRENT_SETTINGS_AND_REQUEST_SEMANTICS,CURRENT_OWNED_IMAGE_DEVICE_CANCEL_LIFETIME_SEMANTICS,ACTUAL_BOUNDED_NONINTERFERENCE,PRESERVED_FAILURE_ATTRIBUTION",
              "artifact_binding": "Private338clause artifact_binding; proofsha256=8b6952d29ff65103dc68b65a4105de13b8de6765e8fef0b9d24dce851997b873; facts=ACTUAL_CURRENT_LOAD,CURRENT_SETTINGS_AND_REQUEST_SEMANTICS,CURRENT_OWNED_IMAGE_DEVICE_CANCEL_LIFETIME_SEMANTICS",
              "failure_attribution": "Private338clause failure_attribution; proofsha256=8b6952d29ff65103dc68b65a4105de13b8de6765e8fef0b9d24dce851997b873; facts=PRESERVED_FAILURE_ATTRIBUTION,ACTUAL_BOUNDED_NONINTERFERENCE,CURRENT_DEFAULT_REPLACEMENT",
              "preconditions": "Private338clause preconditions; proofsha256=8b6952d29ff65103dc68b65a4105de13b8de6765e8fef0b9d24dce851997b873; facts=ACTUAL_CURRENT_LOAD,CURRENT_DEFAULT_REPLACEMENT",
              "step_1": "Private338clause step_1; proofsha256=8b6952d29ff65103dc68b65a4105de13b8de6765e8fef0b9d24dce851997b873; facts=CURRENT_DEFAULT_REPLACEMENT,ACTUAL_SELECTED_ROOT_AND_SETTING_CONNECTION,CURRENT_SETTINGS_AND_REQUEST_SEMANTICS,PRESERVED_FAILURE_ATTRIBUTION",
              "step_2": "Private338clause step_2; proofsha256=8b6952d29ff65103dc68b65a4105de13b8de6765e8fef0b9d24dce851997b873; facts=ACTUAL_DIRECT_COMPLETE_PNG,ACTUAL_SELECTED_ROOT_AND_SETTING_CONNECTION,ACTUAL_BOUNDED_NONINTERFERENCE,CURRENT_OWNED_IMAGE_DEVICE_CANCEL_LIFETIME_SEMANTICS",
              "step_3": "Private338clause step_3; proofsha256=8b6952d29ff65103dc68b65a4105de13b8de6765e8fef0b9d24dce851997b873; facts=ACTUAL_SAME_NOTIFICATION_OWNED_ACTIONS,CURRENT_OWNED_IMAGE_DEVICE_CANCEL_LIFETIME_SEMANTICS",
              "expected": "Private338clause expected; proofsha256=8b6952d29ff65103dc68b65a4105de13b8de6765e8fef0b9d24dce851997b873; facts=ACTUAL_CURRENT_LOAD,CURRENT_DEFAULT_REPLACEMENT,ACTUAL_SELECTED_ROOT_AND_SETTING_CONNECTION,ACTUAL_DIRECT_COMPLETE_PNG,ACTUAL_SAME_NOTIFICATION_OWNED_ACTIONS,CURRENT_SETTINGS_AND_REQUEST_SEMANTICS,CURRENT_OWNED_IMAGE_DEVICE_CANCEL_LIFETIME_SEMANTICS,ACTUAL_BOUNDED_NONINTERFERENCE,PRESERVED_FAILURE_ATTRIBUTION"
            },
            "responsibility_assessment": {
              "source_candidate": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
              "source_result": {
                "status": "pending",
                "reason": "writerはsource/fake/filesystem/Swing回帰のみ。実IDE/通知/OSの観察は未実施。"
              },
              "source_result_sha256": "00709385d646897b8d322279bd6a58f4837d0f0b107ec8fb0529dbb8493c52d3",
              "source_record_evidence": "git:948b1ad2f18ee266c79935327a403dd7471fdc3b:docs/verification/changes/issue-338.json#/cases/0/gpt canonicalsha256=00709385d646897b8d322279bd6a58f4837d0f0b107ec8fb0529dbb8493c52d3",
              "facts": {
                "ACTUAL_CURRENT_LOAD": {
                  "status": "pass",
                  "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                  "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                  "actor": "gpt",
                  "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                  "at": "2026-11-05T17:04:42.284394+00:00",
                  "loaded_identity": "Retained actual currentPNG load source948/formalZIPa582/mainJARd620, installed6/loadedJARs at StudioPID28904/AI-262.10968.63.2622.16730706 and emulator5602. No present host/lease or physicalUSB claim.",
                  "evidence": "Current338selectedrefs 07cb7ab27859051617e7801ee8d6249623eacb64ba2d8420c40c2df6d1322898,c4d66219ce24883207addf655012b417fbdc6c32a5b7285ec99cd4e2597f3c61; originalPNGindependentclauses G01,G10",
                  "reason": "Current formal artifact, native loaded runtime and actual owned fixture identified. LIMIT: Retained actual currentPNG load source948/formalZIPa582/mainJARd620, installed6/loadedJARs at StudioPID28904/AI-262.10968.63.2622.16730706 and emulator5602. No present host/lease or physicalUSB claim."
                },
                "CURRENT_DEFAULT_REPLACEMENT": {
                  "status": "pass",
                  "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                  "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                  "actor": "gpt",
                  "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                  "at": "2026-11-05T17:04:42.284394+00:00",
                  "loaded_identity": "Fresh private current338 source/contract/retained-measurement applicability assessment; source executions keep original identities.",
                  "evidence": "Current338selectedrefs 0d54f6871ddf9c9c70be938bbab5476e1957057349f48154eaeb160014354af6; originalPNGindependentclauses ",
                  "reason": "Original338 common-default clause retained as history and explicitly replaced by adopted341 Logcat-follow/manual choice; continuing338 guarantees retained. LIMIT: Actual fixedsource/doc/public original records. Not338 erasure, newinitialoverride, independentapproval or currentolddefault observation."
                },
                "ACTUAL_SELECTED_ROOT_AND_SETTING_CONNECTION": {
                  "status": "pass",
                  "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                  "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                  "actor": "gpt",
                  "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                  "at": "2026-11-05T17:04:42.284394+00:00",
                  "loaded_identity": "Retained actual currentPNG load source948/formalZIPa582/mainJARd620, installed6/loadedJARs at StudioPID28904/AI-262.10968.63.2622.16730706 and emulator5602. No present host/lease or physicalUSB claim.",
                  "evidence": "Current338selectedrefs 07cb7ab27859051617e7801ee8d6249623eacb64ba2d8420c40c2df6d1322898,098729fe085668231e703226266876306e1f691d241c14ccac66e90e27ed0df6; originalPNGindependentclauses G03,G04",
                  "reason": "Native Screenshotssave setting changed Desktop to ownedroot/Apply; fresh reopened ARB AXroot agrees; next single productPNG uses thatdirectroot. LIMIT: Resolvedroot is offscreen AXvalue, not visible renderedpath pixels. Existingopen value remainedcachedDesktop untilreopen; preserveG04 and PMdisposition, no always-live guarantee invented."
                },
                "ACTUAL_DIRECT_COMPLETE_PNG": {
                  "status": "pass",
                  "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                  "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                  "actor": "gpt",
                  "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                  "at": "2026-11-05T17:04:42.284394+00:00",
                  "loaded_identity": "Retained actual currentPNG load source948/formalZIPa582/mainJARd620, installed6/loadedJARs at StudioPID28904/AI-262.10968.63.2622.16730706 and emulator5602. No present host/lease or physicalUSB claim.",
                  "evidence": "Current338selectedrefs 07cb7ab27859051617e7801ee8d6249623eacb64ba2d8420c40c2df6d1322898,6c9ecfa8aa2f175600171de469992ae0895a3f1156ff7fdd8312b2cdfa71add9; originalPNGindependentclauses G02,G03",
                  "reason": "Exactly1 currentdirectroot screenshot-timestamp-UUID.png82049bytes0600/no wrapper/dialog; full1080x2400/all2592000pixels independently decoded and fixture package/source/run/view/event0/colour matched. LIMIT: CurrentPNGhashf3b75a8ad9c10eb0d095428eae5ba2705e7b71036920e6bf26cb6dfa4563efc0; no oldPNG promoted to newcapture."
                },
                "ACTUAL_SAME_NOTIFICATION_OWNED_ACTIONS": {
                  "status": "pass",
                  "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                  "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                  "actor": "gpt",
                  "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                  "at": "2026-11-05T17:04:42.284394+00:00",
                  "loaded_identity": "Retained actual currentPNG load source948/formalZIPa582/mainJARd620, installed6/loadedJARs at StudioPID28904/AI-262.10968.63.2622.16730706 and emulator5602. No present host/lease or physicalUSB claim.",
                  "evidence": "Current338selectedrefs 07cb7ab27859051617e7801ee8d6249623eacb64ba2d8420c40c2df6d1322898; originalPNGindependentclauses G05,G10",
                  "reason": "Same completed notification opens actualimageparent; ownedDelete removes thatPNG only; parent/sentinel/privatecopy/12completed outputs preserved. LIMIT: Actual effects qualify uncertain delivered_unverified ACK. ActionsafterOFF; noticecontains nofilename/path. No second native request/oldnotice branch or genericnotificationCopy claim."
                },
                "CURRENT_SETTINGS_AND_REQUEST_SEMANTICS": {
                  "status": "pass",
                  "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                  "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                  "actor": "gpt",
                  "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                  "at": "2026-11-05T17:04:42.284394+00:00",
                  "loaded_identity": "Fresh private current338 source/contract/retained-measurement applicability assessment; source executions keep original identities.",
                  "evidence": "Current338selectedrefs 6698ad057b891a2fab18f196df232110a5be9bee340c7e8004e5ef2951da4cd1; originalPNGindependentclauses ",
                  "reason": "Current source/class applicability of actualService/Configurable/Store regression: manualpath persistence, ApplyCancel/asyncstale-unchecked/invalid-unavailable refusal/nonfallback, migration/CLI import and root snapshot. LIMIT: Originalf62 macOSXML/times/artifact remain original. Actual currentnative settingroot path independently observed. No newly executednative manual/error matrix or341wholeproofapproval borrowed."
                },
                "CURRENT_OWNED_IMAGE_DEVICE_CANCEL_LIFETIME_SEMANTICS": {
                  "status": "pass",
                  "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                  "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                  "actor": "gpt",
                  "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                  "at": "2026-11-05T17:04:42.284394+00:00",
                  "loaded_identity": "Fresh private current338 source/contract/retained-measurement applicability assessment; source executions keep original identities.",
                  "evidence": "Current338selectedrefs 6698ad057b891a2fab18f196df232110a5be9bee340c7e8004e5ef2951da4cd1; originalPNGindependentclauses ",
                  "reason": "Current source/class applicability of production directPNG/nonoverwrite/collision/concurrent/requestnotificationidentity/replace-symlink-deletion/devicepin/OFF/interruptcleanup and alive/disposal callback guards. LIMIT: Originalknown-input regressions plus currentService source. No fresh screenshot-inflight nativeclose implied; separateLOGONLY heldclose packet not consumed to create that fact."
                },
                "ACTUAL_BOUNDED_NONINTERFERENCE": {
                  "status": "pass",
                  "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                  "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                  "actor": "gpt",
                  "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                  "at": "2026-11-05T17:04:42.284394+00:00",
                  "loaded_identity": "Retained actual currentPNG load source948/formalZIPa582/mainJARd620, installed6/loadedJARs at StudioPID28904/AI-262.10968.63.2622.16730706 and emulator5602. No present host/lease or physicalUSB claim.",
                  "evidence": "Current338selectedrefs 07cb7ab27859051617e7801ee8d6249623eacb64ba2d8420c40c2df6d1322898,6698ad057b891a2fab18f196df232110a5be9bee340c7e8004e5ef2951da4cd1; originalPNGindependentclauses G06,G10",
                  "reason": "Same current acquisition progresses77590 to246822storedbytes, exact77590-byte inodeprefix preserved; currentsettings/installed6/12outputs preserve, joined to independent screenshot client/root dataflow and identified regression semantics. LIMIT: Only representative healthy screenshot destination noninterference. All3bareH264decode183/noSPSPPS/logunexported remain; no currentfullvideo/log content/sync proof, old377positive native facts not used."
                },
                "PRESERVED_FAILURE_ATTRIBUTION": {
                  "status": "pass",
                  "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                  "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                  "actor": "gpt",
                  "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                  "at": "2026-11-05T17:04:42.284394+00:00",
                  "loaded_identity": "Retained actual currentPNG load source948/formalZIPa582/mainJARd620, installed6/loadedJARs at StudioPID28904/AI-262.10968.63.2622.16730706 and emulator5602. No present host/lease or physicalUSB claim.",
                  "evidence": "Current338selectedrefs 07cb7ab27859051617e7801ee8d6249623eacb64ba2d8420c40c2df6d1322898,ab54c8034632bd8a5df191496e8c7f4756941364e1edad951ec5fd169d197e19,1feee54149e4c4cfd6392492c4e379c20c3abf8af53f0764ce4f9ed446ba79c8,098729fe085668231e703226266876306e1f691d241c14ccac66e90e27ed0df6; originalPNGindependentclauses G04,G07,G10",
                  "reason": "Actual cachedAX limitation, rawH264/log omissions, summary corrections, tool/parser/stale/timeout failures and scopednormalreturn retained with original attribution; existingG04 contractualdisposition applied. LIMIT: Successfulprivate attribution/preservation assessment only; no failing tool/content observation reclassifiedpass, noUSB/human/REAL/otherCase status cleared."
                }
              },
              "coverage": {
                "contract_coverage": [
                  "ACTUAL_CURRENT_LOAD",
                  "CURRENT_DEFAULT_REPLACEMENT",
                  "ACTUAL_SELECTED_ROOT_AND_SETTING_CONNECTION",
                  "ACTUAL_DIRECT_COMPLETE_PNG",
                  "ACTUAL_SAME_NOTIFICATION_OWNED_ACTIONS",
                  "CURRENT_SETTINGS_AND_REQUEST_SEMANTICS",
                  "CURRENT_OWNED_IMAGE_DEVICE_CANCEL_LIFETIME_SEMANTICS",
                  "ACTUAL_BOUNDED_NONINTERFERENCE",
                  "PRESERVED_FAILURE_ATTRIBUTION"
                ],
                "artifact_binding": [
                  "ACTUAL_CURRENT_LOAD",
                  "CURRENT_SETTINGS_AND_REQUEST_SEMANTICS",
                  "CURRENT_OWNED_IMAGE_DEVICE_CANCEL_LIFETIME_SEMANTICS"
                ],
                "failure_attribution": [
                  "PRESERVED_FAILURE_ATTRIBUTION",
                  "ACTUAL_BOUNDED_NONINTERFERENCE",
                  "CURRENT_DEFAULT_REPLACEMENT"
                ],
                "preconditions": [
                  "ACTUAL_CURRENT_LOAD",
                  "CURRENT_DEFAULT_REPLACEMENT"
                ],
                "step_1": [
                  "CURRENT_DEFAULT_REPLACEMENT",
                  "ACTUAL_SELECTED_ROOT_AND_SETTING_CONNECTION",
                  "CURRENT_SETTINGS_AND_REQUEST_SEMANTICS",
                  "PRESERVED_FAILURE_ATTRIBUTION"
                ],
                "step_2": [
                  "ACTUAL_DIRECT_COMPLETE_PNG",
                  "ACTUAL_SELECTED_ROOT_AND_SETTING_CONNECTION",
                  "ACTUAL_BOUNDED_NONINTERFERENCE",
                  "CURRENT_OWNED_IMAGE_DEVICE_CANCEL_LIFETIME_SEMANTICS"
                ],
                "step_3": [
                  "ACTUAL_SAME_NOTIFICATION_OWNED_ACTIONS",
                  "CURRENT_OWNED_IMAGE_DEVICE_CANCEL_LIFETIME_SEMANTICS"
                ],
                "expected": [
                  "ACTUAL_CURRENT_LOAD",
                  "CURRENT_DEFAULT_REPLACEMENT",
                  "ACTUAL_SELECTED_ROOT_AND_SETTING_CONNECTION",
                  "ACTUAL_DIRECT_COMPLETE_PNG",
                  "ACTUAL_SAME_NOTIFICATION_OWNED_ACTIONS",
                  "CURRENT_SETTINGS_AND_REQUEST_SEMANTICS",
                  "CURRENT_OWNED_IMAGE_DEVICE_CANCEL_LIFETIME_SEMANTICS",
                  "ACTUAL_BOUNDED_NONINTERFERENCE",
                  "PRESERVED_FAILURE_ATTRIBUTION"
                ]
              },
              "missing_requirements": [],
              "unresolved_owned_failures": []
            },
            "status": "pass"
          },
          "CURRENT_F327_LOAD": {
            "status": "pass",
            "head": "2e576ce3c724dc95ccf0688b8a1bf7cd92d774a8",
            "artifact_sha256": "f3274d7501f9d173ece5de6b1cb729a7fb40d247cfe2a17c6c937fcc085a2c88",
            "actor": "gpt",
            "observer": "codex-session-sha256:7b2456c4ac692a16695fa3b6f035ac2149e76f23bd63e479f43f81ca687087e1",
            "at": "2026-11-06T13:21:06.184603+00:00",
            "evidence": "Retained current native receipt sha256=a80b9fdcc7619824e74b39f5fd5bc27baa89eba29d28a629377b641de617e8b3; original outcome sha256=b7376a368dd90fcaa48f26e279cd492f60ec8976bae798d18c7a3bc39bf8d2de; original manifest sha256=99673eefcfabfe8cc0144f9883f11f47b3c0e566e65a0878b173b88fdbcffa16",
            "loaded_identity": "保存済み実行者原本のinstalled 3 JAR hash/size/mode、loaded paths/JBR、About version0.2.0/full source2e576ceを照合。保存画像のOFFと常設理由全文を確認。現在のGUIや新実行を主張しない。",
            "reason": "候補適用draft作成者による現在artifact bindingの限定事実評価。atは本評価時刻、native再実行時刻ではない。元実行者abcfとunit process時刻範囲は原本に保持。新ON/Save/設定操作/USBの証拠にしない。"
          }
        },
        "coverage": {
          "contract_coverage": [
            "RETAINED_NORMAL_338_WHOLE_RESULT"
          ],
          "artifact_binding": [
            "CURRENT_F327_LOAD"
          ],
          "failure_attribution": [
            "RETAINED_NORMAL_338_WHOLE_RESULT"
          ],
          "preconditions": [
            "RETAINED_NORMAL_338_WHOLE_RESULT"
          ],
          "step_1": [
            "RETAINED_NORMAL_338_WHOLE_RESULT"
          ],
          "step_2": [
            "RETAINED_NORMAL_338_WHOLE_RESULT"
          ],
          "step_3": [
            "RETAINED_NORMAL_338_WHOLE_RESULT"
          ],
          "expected": [
            "RETAINED_NORMAL_338_WHOLE_RESULT"
          ]
        },
        "missing_requirements": [],
        "unresolved_owned_failures": []
      }
    },
    "341:SCREENSHOT-DESTINATION": {
      "status": "pending",
      "head": "2e576ce3c724dc95ccf0688b8a1bf7cd92d774a8",
      "artifact_sha256": "f3274d7501f9d173ece5de6b1cb729a7fb40d247cfe2a17c6c937fcc085a2c88",
      "actor": "gpt",
      "observer": "codex-session-sha256:7b2456c4ac692a16695fa3b6f035ac2149e76f23bd63e479f43f81ca687087e1",
      "at": "2026-11-06T13:58:56.017172+00:00",
      "evidence": "Candidate-specific normal341 application draft; fixed original canonical-sha256=4990345d01afe0a4ad4e97f52c2bb63180b6b4a3873265fc2030593e12861f38; independent current application not approved.",
      "loaded_identity": "保存済み実行者原本のinstalled 3 JAR hash/size/mode、loaded paths/JBR、About version0.2.0/full source2e576ceを照合。保存画像のOFFと常設理由全文を確認。現在のGUIや新実行を主張しない。",
      "reason": "Issue428の候補別正式assessment案。元whole結果と限定独立適用支持を保持するが、固定PR全feedback/実originを含む正式承認は未成立。",
      "responsibility_revision": "85296114925d6947be93158e3eca90fcd69e561b3d8a5d1e6845407b73108f61",
      "responsibility_evidence": {
        "contract_coverage": "Candidate2e/formalf327 draft 341/contract_coverage; facts=RETAINED_NORMAL_341_WHOLE_RESULT",
        "artifact_binding": "Candidate2e/formalf327 draft 341/artifact_binding; facts=CURRENT_F327_LOAD",
        "failure_attribution": "Candidate2e/formalf327 draft 341/failure_attribution; facts=RETAINED_NORMAL_341_WHOLE_RESULT",
        "preconditions": "Candidate2e/formalf327 draft 341/preconditions; facts=RETAINED_NORMAL_341_WHOLE_RESULT",
        "step_1": "Candidate2e/formalf327 draft 341/step_1; facts=RETAINED_NORMAL_341_WHOLE_RESULT",
        "step_2": "Candidate2e/formalf327 draft 341/step_2; facts=RETAINED_NORMAL_341_WHOLE_RESULT",
        "step_3": "Candidate2e/formalf327 draft 341/step_3; facts=RETAINED_NORMAL_341_WHOLE_RESULT",
        "expected": "Candidate2e/formalf327 draft 341/expected; facts=RETAINED_NORMAL_341_WHOLE_RESULT"
      },
      "responsibility_assessment": {
        "source_candidate": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
        "source_result": {
          "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
          "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
          "actor": "gpt",
          "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
          "at": "2026-11-05T22:34:05.374083+00:00",
          "evidence": "Private whole341 proof candidate sha256=725e0ccda90aa50e4e56483a2dc8efca70c85f88c79fcd9508929b925ff69858; adoptedmain resolver=ca863083d3db673cbc1b6f3c79b5b44de69681f5",
          "loaded_identity": "Current actual PNG execution source948b/formalZIPa582/mainJARd620; installed6+loadedJARs bound at Studio PID28904/AI-262.10968.63.2622.16730706, emulator5602. This is a retained execution, not a present GUI/lease assertion.",
          "reason": "Private final normal current-result proposal after actual fixed independent approval; canonical observer metadata corrected. Original fact assessment times and original native records preserved. Formal adoption is not performed here.",
          "responsibility_revision": "85296114925d6947be93158e3eca90fcd69e561b3d8a5d1e6845407b73108f61",
          "responsibility_evidence": {
            "contract_coverage": "Private341 whole-clause proof candidate: contract_coverage; clause-matrix sha256=725e0ccda90aa50e4e56483a2dc8efca70c85f88c79fcd9508929b925ff69858; factIDs=CURRENT_LOAD,NATIVE_ROOT_NEXT_REQUEST,COMPLETE_CURRENT_PNG,SAME_NOTIFICATION_ACTIONS,CURRENT_SETTINGS_REGRESSION_SEMANTICS,CURRENT_OWNED_IMAGE_TARGET_LIFETIME_SEMANTICS,BOUNDED_NONINTERFERENCE,FAILURE_SCOPE_AND_DISPOSITION",
            "artifact_binding": "Private341 whole-clause proof candidate: artifact_binding; clause-matrix sha256=725e0ccda90aa50e4e56483a2dc8efca70c85f88c79fcd9508929b925ff69858; factIDs=CURRENT_LOAD,CURRENT_SETTINGS_REGRESSION_SEMANTICS,CURRENT_OWNED_IMAGE_TARGET_LIFETIME_SEMANTICS",
            "failure_attribution": "Private341 whole-clause proof candidate: failure_attribution; clause-matrix sha256=725e0ccda90aa50e4e56483a2dc8efca70c85f88c79fcd9508929b925ff69858; factIDs=FAILURE_SCOPE_AND_DISPOSITION,BOUNDED_NONINTERFERENCE",
            "preconditions": "Private341 whole-clause proof candidate: preconditions; clause-matrix sha256=725e0ccda90aa50e4e56483a2dc8efca70c85f88c79fcd9508929b925ff69858; factIDs=CURRENT_LOAD",
            "step_1": "Private341 whole-clause proof candidate: step_1; clause-matrix sha256=725e0ccda90aa50e4e56483a2dc8efca70c85f88c79fcd9508929b925ff69858; factIDs=NATIVE_ROOT_NEXT_REQUEST,CURRENT_SETTINGS_REGRESSION_SEMANTICS,FAILURE_SCOPE_AND_DISPOSITION",
            "step_2": "Private341 whole-clause proof candidate: step_2; clause-matrix sha256=725e0ccda90aa50e4e56483a2dc8efca70c85f88c79fcd9508929b925ff69858; factIDs=NATIVE_ROOT_NEXT_REQUEST,COMPLETE_CURRENT_PNG,BOUNDED_NONINTERFERENCE,CURRENT_OWNED_IMAGE_TARGET_LIFETIME_SEMANTICS",
            "step_3": "Private341 whole-clause proof candidate: step_3; clause-matrix sha256=725e0ccda90aa50e4e56483a2dc8efca70c85f88c79fcd9508929b925ff69858; factIDs=SAME_NOTIFICATION_ACTIONS,CURRENT_OWNED_IMAGE_TARGET_LIFETIME_SEMANTICS",
            "expected": "Private341 whole-clause proof candidate: expected; clause-matrix sha256=725e0ccda90aa50e4e56483a2dc8efca70c85f88c79fcd9508929b925ff69858; factIDs=CURRENT_LOAD,NATIVE_ROOT_NEXT_REQUEST,COMPLETE_CURRENT_PNG,SAME_NOTIFICATION_ACTIONS,CURRENT_SETTINGS_REGRESSION_SEMANTICS,CURRENT_OWNED_IMAGE_TARGET_LIFETIME_SEMANTICS,BOUNDED_NONINTERFERENCE,FAILURE_SCOPE_AND_DISPOSITION"
          },
          "responsibility_assessment": {
            "source_candidate": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
            "source_result": {
              "status": "pending",
              "reason": "writerは固定native class/所有Mock・fake adb/filesystem/Swing回帰のみ。実IDEの現在の設定値・OS Desktop・通知/Finderは未観察。"
            },
            "source_result_sha256": "506b7cbd07b8b43d4e692cb7d49f534cc4e7f9a37359af30169899da7849ab69",
            "source_record_evidence": "git:948b1ad2f18ee266c79935327a403dd7471fdc3b:docs/verification/changes/issue-341.json#/cases/0/gpt canonicalsha256=506b7cbd07b8b43d4e692cb7d49f534cc4e7f9a37359af30169899da7849ab69",
            "facts": {
              "CURRENT_LOAD": {
                "status": "pass",
                "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                "actor": "gpt",
                "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                "at": "2026-11-05T16:41:21.113752+00:00",
                "loaded_identity": "Current actual PNG execution source948b/formalZIPa582/mainJARd620; installed6+loadedJARs bound at Studio PID28904/AI-262.10968.63.2622.16730706, emulator5602. This is a retained execution, not a present GUI/lease assertion.",
                "evidence": "Selected immutable private inputs: 9dcc2f3f323b48483c24cad1a1ec509257f793ac0b1a4bcb37067f28f06f2281; c4d66219ce24883207addf655012b417fbdc6c32a5b7285ec99cd4e2597f3c61; PNG independent clauses G01,G10",
                "reason": "Current formal artifact and observed loaded current runtime/fixture. LIMIT: Current actual PNG execution source948b/formalZIPa582/mainJARd620; installed6+loadedJARs bound at Studio PID28904/AI-262.10968.63.2622.16730706, emulator5602. This is a retained execution, not a present GUI/lease assertion."
              },
              "NATIVE_ROOT_NEXT_REQUEST": {
                "status": "pass",
                "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                "actor": "gpt",
                "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                "at": "2026-11-05T16:41:21.113752+00:00",
                "loaded_identity": "Current actual PNG execution source948b/formalZIPa582/mainJARd620; installed6+loadedJARs bound at Studio PID28904/AI-262.10968.63.2622.16730706, emulator5602. This is a retained execution, not a present GUI/lease assertion.",
                "evidence": "Selected immutable private inputs: 9dcc2f3f323b48483c24cad1a1ec509257f793ac0b1a4bcb37067f28f06f2281; 098729fe085668231e703226266876306e1f691d241c14ccac66e90e27ed0df6; PNG independent clauses G03,G04",
                "reason": "Native Screenshots setting Desktop to owned absolute root/Apply; fresh reopened ARB AX resolved root agrees; next single product request writes directly in the changed root. LIMIT: Reopened path is offscreen AX confirmation, not a visible rendered-path observation; cached already-open value remains originalG04."
              },
              "COMPLETE_CURRENT_PNG": {
                "status": "pass",
                "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                "actor": "gpt",
                "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                "at": "2026-11-05T16:41:21.113752+00:00",
                "loaded_identity": "Current actual PNG execution source948b/formalZIPa582/mainJARd620; installed6+loadedJARs bound at Studio PID28904/AI-262.10968.63.2622.16730706, emulator5602. This is a retained execution, not a present GUI/lease assertion.",
                "evidence": "Selected immutable private inputs: 9dcc2f3f323b48483c24cad1a1ec509257f793ac0b1a4bcb37067f28f06f2281; 6c9ecfa8aa2f175600171de469992ae0895a3f1156ff7fdd8312b2cdfa71add9; PNG independent clauses G02",
                "reason": "Exactly1/1 current product PNG,82049bytes,1080x2400; whole2592000pixels independently decoded and matched to current appA package/source/run/view/event0/colour. LIMIT: Current PNG hashf3b75a8ad9c10eb0d095428eae5ba2705e7b71036920e6bf26cb6dfa4563efc0. One emulator scene; no USB/secure/general scene timing claim."
              },
              "SAME_NOTIFICATION_ACTIONS": {
                "status": "pass",
                "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                "actor": "gpt",
                "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                "at": "2026-11-05T16:41:21.113752+00:00",
                "loaded_identity": "Current actual PNG execution source948b/formalZIPa582/mainJARd620; installed6+loadedJARs bound at Studio PID28904/AI-262.10968.63.2622.16730706, emulator5602. This is a retained execution, not a present GUI/lease assertion.",
                "evidence": "Selected immutable private inputs: 9dcc2f3f323b48483c24cad1a1ec509257f793ac0b1a4bcb37067f28f06f2281; PNG independent clauses G05,G10",
                "reason": "Same completed notification opens actual image parent; Delete removes only that completed image; parent/sentinel/preserved copy/12completed outputs survive. LIMIT: Actions were after acquisition OFF. Notification contains no filename/path; unique request/output/effects establish binding. No second native screenshot/old-notice branch invented."
              },
              "CURRENT_SETTINGS_REGRESSION_SEMANTICS": {
                "status": "pass",
                "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                "actor": "gpt",
                "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                "at": "2026-11-05T16:41:21.113752+00:00",
                "loaded_identity": "Fresh read-only current-source/class applicability assessment; original f62 execution remains separately identified.",
                "evidence": "Selected immutable private inputs: 8527e57996a40cec93d7f2f5b0a305de54cb81c0d87f5bd5e19d2718197237f5; PNG independent clauses ",
                "reason": "Current actual source/class applicability of retained realService/Configurable native APP/PROJECT, migration, manual Apply/Cancel, invalid/unavailable nonfallback and snapshot/next-request production regression assertions. LIMIT: Fresh offline source/class observation at composition time. Original f62 XML, JUnit suite times and DEV artifact identity remain separate; no fresh current native manual/invalid/Cancel execution is asserted."
              },
              "CURRENT_OWNED_IMAGE_TARGET_LIFETIME_SEMANTICS": {
                "status": "pass",
                "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                "actor": "gpt",
                "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                "at": "2026-11-05T16:41:21.113752+00:00",
                "loaded_identity": "Fresh read-only current-source/class applicability assessment; original f62 execution remains separately identified.",
                "evidence": "Selected immutable private inputs: 8527e57996a40cec93d7f2f5b0a305de54cb81c0d87f5bd5e19d2718197237f5; PNG independent clauses ",
                "reason": "Current actual source/class applicability of production nonoverwrite/collision/replace/symlink/concurrency/interrupt cleanup, selected-device/OFF/staleSDK/busy and callback-lifetime guards. LIMIT: Fresh source/byte applicability observation. Actual macOS known-input regressions remain f62; current native healthy onePNG/actions separately qualified. Project disposal guard reviewed inService455-483, not a newly executed active-close native branch."
              },
              "BOUNDED_NONINTERFERENCE": {
                "status": "pass",
                "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                "actor": "gpt",
                "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                "at": "2026-11-05T16:41:21.113752+00:00",
                "loaded_identity": "Current actual PNG execution source948b/formalZIPa582/mainJARd620; installed6+loadedJARs bound at Studio PID28904/AI-262.10968.63.2622.16730706, emulator5602. This is a retained execution, not a present GUI/lease assertion.",
                "evidence": "Selected immutable private inputs: 9dcc2f3f323b48483c24cad1a1ec509257f793ac0b1a4bcb37067f28f06f2281; 8527e57996a40cec93d7f2f5b0a305de54cb81c0d87f5bd5e19d2718197237f5; PNG independent clauses G06,G10",
                "reason": "Current same-session stored bytes progress77590 to246822 and original77590-byte inode prefix remains exact; current acquisition state/target/settings and12completed output preservation join unchanged per-request screenshot source/dataflow and relevant production regression semantics. LIMIT: Satisfies representative healthy screenshot destination noninterference only. Three bare H264 snapshots allfail183/noSPSPPS, no memory-log export; no complete current video/log content or synchronization claim. Old377 positive facts are not used."
              },
              "FAILURE_SCOPE_AND_DISPOSITION": {
                "status": "pass",
                "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                "actor": "gpt",
                "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                "at": "2026-11-05T16:41:21.113752+00:00",
                "loaded_identity": "Current actual PNG execution source948b/formalZIPa582/mainJARd620; installed6+loadedJARs bound at Studio PID28904/AI-262.10968.63.2622.16730706, emulator5602. This is a retained execution, not a present GUI/lease assertion.",
                "evidence": "Selected immutable private inputs: ab54c8034632bd8a5df191496e8c7f4756941364e1edad951ec5fd169d197e19; 1feee54149e4c4cfd6392492c4e379c20c3abf8af53f0764ce4f9ed446ba79c8; 098729fe085668231e703226266876306e1f691d241c14ccac66e90e27ed0df6; PNG independent clauses G04,G07,G10",
                "reason": "Original cached AX limitation, decode/log omissions, summary corrections, tool/parser/timeout failures and normal return retained; explicit PM/Omil G04 existing-contract disposition. LIMIT: Successful attribution/preservation observation, not a claim that failed decoder/tool observations passed. Distinct unobserved branches retain their original Case statuses and owners."
              }
            },
            "coverage": {
              "contract_coverage": [
                "CURRENT_LOAD",
                "NATIVE_ROOT_NEXT_REQUEST",
                "COMPLETE_CURRENT_PNG",
                "SAME_NOTIFICATION_ACTIONS",
                "CURRENT_SETTINGS_REGRESSION_SEMANTICS",
                "CURRENT_OWNED_IMAGE_TARGET_LIFETIME_SEMANTICS",
                "BOUNDED_NONINTERFERENCE",
                "FAILURE_SCOPE_AND_DISPOSITION"
              ],
              "artifact_binding": [
                "CURRENT_LOAD",
                "CURRENT_SETTINGS_REGRESSION_SEMANTICS",
                "CURRENT_OWNED_IMAGE_TARGET_LIFETIME_SEMANTICS"
              ],
              "failure_attribution": [
                "FAILURE_SCOPE_AND_DISPOSITION",
                "BOUNDED_NONINTERFERENCE"
              ],
              "preconditions": [
                "CURRENT_LOAD"
              ],
              "step_1": [
                "NATIVE_ROOT_NEXT_REQUEST",
                "CURRENT_SETTINGS_REGRESSION_SEMANTICS",
                "FAILURE_SCOPE_AND_DISPOSITION"
              ],
              "step_2": [
                "NATIVE_ROOT_NEXT_REQUEST",
                "COMPLETE_CURRENT_PNG",
                "BOUNDED_NONINTERFERENCE",
                "CURRENT_OWNED_IMAGE_TARGET_LIFETIME_SEMANTICS"
              ],
              "step_3": [
                "SAME_NOTIFICATION_ACTIONS",
                "CURRENT_OWNED_IMAGE_TARGET_LIFETIME_SEMANTICS"
              ],
              "expected": [
                "CURRENT_LOAD",
                "NATIVE_ROOT_NEXT_REQUEST",
                "COMPLETE_CURRENT_PNG",
                "SAME_NOTIFICATION_ACTIONS",
                "CURRENT_SETTINGS_REGRESSION_SEMANTICS",
                "CURRENT_OWNED_IMAGE_TARGET_LIFETIME_SEMANTICS",
                "BOUNDED_NONINTERFERENCE",
                "FAILURE_SCOPE_AND_DISPOSITION"
              ]
            },
            "missing_requirements": [],
            "unresolved_owned_failures": []
          },
          "status": "pass"
        },
        "source_result_sha256": "4990345d01afe0a4ad4e97f52c2bb63180b6b4a3873265fc2030593e12861f38",
        "source_record_evidence": "Immutable normal341 result projection byte-sha256=d4b98df7bdaa0c21b06e94cd104bc903d3d2f5a1a69189c037eb23bab2d7a175; result canonical-sha256=4990345d01afe0a4ad4e97f52c2bb63180b6b4a3873265fc2030593e12861f38; original nested pending/result/facts and qualified review preserved.",
        "facts": {
          "RETAINED_NORMAL_341_WHOLE_RESULT": {
            "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
            "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
            "actor": "gpt",
            "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
            "at": "2026-11-05T22:34:05.374083+00:00",
            "evidence": "Private whole341 proof candidate sha256=725e0ccda90aa50e4e56483a2dc8efca70c85f88c79fcd9508929b925ff69858; adoptedmain resolver=ca863083d3db673cbc1b6f3c79b5b44de69681f5",
            "loaded_identity": "Current actual PNG execution source948b/formalZIPa582/mainJARd620; installed6+loadedJARs bound at Studio PID28904/AI-262.10968.63.2622.16730706, emulator5602. This is a retained execution, not a present GUI/lease assertion.",
            "reason": "Private final normal current-result proposal after actual fixed independent approval; canonical observer metadata corrected. Original fact assessment times and original native records preserved. Formal adoption is not performed here.",
            "responsibility_revision": "85296114925d6947be93158e3eca90fcd69e561b3d8a5d1e6845407b73108f61",
            "responsibility_evidence": {
              "contract_coverage": "Private341 whole-clause proof candidate: contract_coverage; clause-matrix sha256=725e0ccda90aa50e4e56483a2dc8efca70c85f88c79fcd9508929b925ff69858; factIDs=CURRENT_LOAD,NATIVE_ROOT_NEXT_REQUEST,COMPLETE_CURRENT_PNG,SAME_NOTIFICATION_ACTIONS,CURRENT_SETTINGS_REGRESSION_SEMANTICS,CURRENT_OWNED_IMAGE_TARGET_LIFETIME_SEMANTICS,BOUNDED_NONINTERFERENCE,FAILURE_SCOPE_AND_DISPOSITION",
              "artifact_binding": "Private341 whole-clause proof candidate: artifact_binding; clause-matrix sha256=725e0ccda90aa50e4e56483a2dc8efca70c85f88c79fcd9508929b925ff69858; factIDs=CURRENT_LOAD,CURRENT_SETTINGS_REGRESSION_SEMANTICS,CURRENT_OWNED_IMAGE_TARGET_LIFETIME_SEMANTICS",
              "failure_attribution": "Private341 whole-clause proof candidate: failure_attribution; clause-matrix sha256=725e0ccda90aa50e4e56483a2dc8efca70c85f88c79fcd9508929b925ff69858; factIDs=FAILURE_SCOPE_AND_DISPOSITION,BOUNDED_NONINTERFERENCE",
              "preconditions": "Private341 whole-clause proof candidate: preconditions; clause-matrix sha256=725e0ccda90aa50e4e56483a2dc8efca70c85f88c79fcd9508929b925ff69858; factIDs=CURRENT_LOAD",
              "step_1": "Private341 whole-clause proof candidate: step_1; clause-matrix sha256=725e0ccda90aa50e4e56483a2dc8efca70c85f88c79fcd9508929b925ff69858; factIDs=NATIVE_ROOT_NEXT_REQUEST,CURRENT_SETTINGS_REGRESSION_SEMANTICS,FAILURE_SCOPE_AND_DISPOSITION",
              "step_2": "Private341 whole-clause proof candidate: step_2; clause-matrix sha256=725e0ccda90aa50e4e56483a2dc8efca70c85f88c79fcd9508929b925ff69858; factIDs=NATIVE_ROOT_NEXT_REQUEST,COMPLETE_CURRENT_PNG,BOUNDED_NONINTERFERENCE,CURRENT_OWNED_IMAGE_TARGET_LIFETIME_SEMANTICS",
              "step_3": "Private341 whole-clause proof candidate: step_3; clause-matrix sha256=725e0ccda90aa50e4e56483a2dc8efca70c85f88c79fcd9508929b925ff69858; factIDs=SAME_NOTIFICATION_ACTIONS,CURRENT_OWNED_IMAGE_TARGET_LIFETIME_SEMANTICS",
              "expected": "Private341 whole-clause proof candidate: expected; clause-matrix sha256=725e0ccda90aa50e4e56483a2dc8efca70c85f88c79fcd9508929b925ff69858; factIDs=CURRENT_LOAD,NATIVE_ROOT_NEXT_REQUEST,COMPLETE_CURRENT_PNG,SAME_NOTIFICATION_ACTIONS,CURRENT_SETTINGS_REGRESSION_SEMANTICS,CURRENT_OWNED_IMAGE_TARGET_LIFETIME_SEMANTICS,BOUNDED_NONINTERFERENCE,FAILURE_SCOPE_AND_DISPOSITION"
            },
            "responsibility_assessment": {
              "source_candidate": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
              "source_result": {
                "status": "pending",
                "reason": "writerは固定native class/所有Mock・fake adb/filesystem/Swing回帰のみ。実IDEの現在の設定値・OS Desktop・通知/Finderは未観察。"
              },
              "source_result_sha256": "506b7cbd07b8b43d4e692cb7d49f534cc4e7f9a37359af30169899da7849ab69",
              "source_record_evidence": "git:948b1ad2f18ee266c79935327a403dd7471fdc3b:docs/verification/changes/issue-341.json#/cases/0/gpt canonicalsha256=506b7cbd07b8b43d4e692cb7d49f534cc4e7f9a37359af30169899da7849ab69",
              "facts": {
                "CURRENT_LOAD": {
                  "status": "pass",
                  "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                  "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                  "actor": "gpt",
                  "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                  "at": "2026-11-05T16:41:21.113752+00:00",
                  "loaded_identity": "Current actual PNG execution source948b/formalZIPa582/mainJARd620; installed6+loadedJARs bound at Studio PID28904/AI-262.10968.63.2622.16730706, emulator5602. This is a retained execution, not a present GUI/lease assertion.",
                  "evidence": "Selected immutable private inputs: 9dcc2f3f323b48483c24cad1a1ec509257f793ac0b1a4bcb37067f28f06f2281; c4d66219ce24883207addf655012b417fbdc6c32a5b7285ec99cd4e2597f3c61; PNG independent clauses G01,G10",
                  "reason": "Current formal artifact and observed loaded current runtime/fixture. LIMIT: Current actual PNG execution source948b/formalZIPa582/mainJARd620; installed6+loadedJARs bound at Studio PID28904/AI-262.10968.63.2622.16730706, emulator5602. This is a retained execution, not a present GUI/lease assertion."
                },
                "NATIVE_ROOT_NEXT_REQUEST": {
                  "status": "pass",
                  "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                  "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                  "actor": "gpt",
                  "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                  "at": "2026-11-05T16:41:21.113752+00:00",
                  "loaded_identity": "Current actual PNG execution source948b/formalZIPa582/mainJARd620; installed6+loadedJARs bound at Studio PID28904/AI-262.10968.63.2622.16730706, emulator5602. This is a retained execution, not a present GUI/lease assertion.",
                  "evidence": "Selected immutable private inputs: 9dcc2f3f323b48483c24cad1a1ec509257f793ac0b1a4bcb37067f28f06f2281; 098729fe085668231e703226266876306e1f691d241c14ccac66e90e27ed0df6; PNG independent clauses G03,G04",
                  "reason": "Native Screenshots setting Desktop to owned absolute root/Apply; fresh reopened ARB AX resolved root agrees; next single product request writes directly in the changed root. LIMIT: Reopened path is offscreen AX confirmation, not a visible rendered-path observation; cached already-open value remains originalG04."
                },
                "COMPLETE_CURRENT_PNG": {
                  "status": "pass",
                  "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                  "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                  "actor": "gpt",
                  "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                  "at": "2026-11-05T16:41:21.113752+00:00",
                  "loaded_identity": "Current actual PNG execution source948b/formalZIPa582/mainJARd620; installed6+loadedJARs bound at Studio PID28904/AI-262.10968.63.2622.16730706, emulator5602. This is a retained execution, not a present GUI/lease assertion.",
                  "evidence": "Selected immutable private inputs: 9dcc2f3f323b48483c24cad1a1ec509257f793ac0b1a4bcb37067f28f06f2281; 6c9ecfa8aa2f175600171de469992ae0895a3f1156ff7fdd8312b2cdfa71add9; PNG independent clauses G02",
                  "reason": "Exactly1/1 current product PNG,82049bytes,1080x2400; whole2592000pixels independently decoded and matched to current appA package/source/run/view/event0/colour. LIMIT: Current PNG hashf3b75a8ad9c10eb0d095428eae5ba2705e7b71036920e6bf26cb6dfa4563efc0. One emulator scene; no USB/secure/general scene timing claim."
                },
                "SAME_NOTIFICATION_ACTIONS": {
                  "status": "pass",
                  "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                  "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                  "actor": "gpt",
                  "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                  "at": "2026-11-05T16:41:21.113752+00:00",
                  "loaded_identity": "Current actual PNG execution source948b/formalZIPa582/mainJARd620; installed6+loadedJARs bound at Studio PID28904/AI-262.10968.63.2622.16730706, emulator5602. This is a retained execution, not a present GUI/lease assertion.",
                  "evidence": "Selected immutable private inputs: 9dcc2f3f323b48483c24cad1a1ec509257f793ac0b1a4bcb37067f28f06f2281; PNG independent clauses G05,G10",
                  "reason": "Same completed notification opens actual image parent; Delete removes only that completed image; parent/sentinel/preserved copy/12completed outputs survive. LIMIT: Actions were after acquisition OFF. Notification contains no filename/path; unique request/output/effects establish binding. No second native screenshot/old-notice branch invented."
                },
                "CURRENT_SETTINGS_REGRESSION_SEMANTICS": {
                  "status": "pass",
                  "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                  "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                  "actor": "gpt",
                  "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                  "at": "2026-11-05T16:41:21.113752+00:00",
                  "loaded_identity": "Fresh read-only current-source/class applicability assessment; original f62 execution remains separately identified.",
                  "evidence": "Selected immutable private inputs: 8527e57996a40cec93d7f2f5b0a305de54cb81c0d87f5bd5e19d2718197237f5; PNG independent clauses ",
                  "reason": "Current actual source/class applicability of retained realService/Configurable native APP/PROJECT, migration, manual Apply/Cancel, invalid/unavailable nonfallback and snapshot/next-request production regression assertions. LIMIT: Fresh offline source/class observation at composition time. Original f62 XML, JUnit suite times and DEV artifact identity remain separate; no fresh current native manual/invalid/Cancel execution is asserted."
                },
                "CURRENT_OWNED_IMAGE_TARGET_LIFETIME_SEMANTICS": {
                  "status": "pass",
                  "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                  "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                  "actor": "gpt",
                  "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                  "at": "2026-11-05T16:41:21.113752+00:00",
                  "loaded_identity": "Fresh read-only current-source/class applicability assessment; original f62 execution remains separately identified.",
                  "evidence": "Selected immutable private inputs: 8527e57996a40cec93d7f2f5b0a305de54cb81c0d87f5bd5e19d2718197237f5; PNG independent clauses ",
                  "reason": "Current actual source/class applicability of production nonoverwrite/collision/replace/symlink/concurrency/interrupt cleanup, selected-device/OFF/staleSDK/busy and callback-lifetime guards. LIMIT: Fresh source/byte applicability observation. Actual macOS known-input regressions remain f62; current native healthy onePNG/actions separately qualified. Project disposal guard reviewed inService455-483, not a newly executed active-close native branch."
                },
                "BOUNDED_NONINTERFERENCE": {
                  "status": "pass",
                  "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                  "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                  "actor": "gpt",
                  "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                  "at": "2026-11-05T16:41:21.113752+00:00",
                  "loaded_identity": "Current actual PNG execution source948b/formalZIPa582/mainJARd620; installed6+loadedJARs bound at Studio PID28904/AI-262.10968.63.2622.16730706, emulator5602. This is a retained execution, not a present GUI/lease assertion.",
                  "evidence": "Selected immutable private inputs: 9dcc2f3f323b48483c24cad1a1ec509257f793ac0b1a4bcb37067f28f06f2281; 8527e57996a40cec93d7f2f5b0a305de54cb81c0d87f5bd5e19d2718197237f5; PNG independent clauses G06,G10",
                  "reason": "Current same-session stored bytes progress77590 to246822 and original77590-byte inode prefix remains exact; current acquisition state/target/settings and12completed output preservation join unchanged per-request screenshot source/dataflow and relevant production regression semantics. LIMIT: Satisfies representative healthy screenshot destination noninterference only. Three bare H264 snapshots allfail183/noSPSPPS, no memory-log export; no complete current video/log content or synchronization claim. Old377 positive facts are not used."
                },
                "FAILURE_SCOPE_AND_DISPOSITION": {
                  "status": "pass",
                  "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                  "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                  "actor": "gpt",
                  "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                  "at": "2026-11-05T16:41:21.113752+00:00",
                  "loaded_identity": "Current actual PNG execution source948b/formalZIPa582/mainJARd620; installed6+loadedJARs bound at Studio PID28904/AI-262.10968.63.2622.16730706, emulator5602. This is a retained execution, not a present GUI/lease assertion.",
                  "evidence": "Selected immutable private inputs: ab54c8034632bd8a5df191496e8c7f4756941364e1edad951ec5fd169d197e19; 1feee54149e4c4cfd6392492c4e379c20c3abf8af53f0764ce4f9ed446ba79c8; 098729fe085668231e703226266876306e1f691d241c14ccac66e90e27ed0df6; PNG independent clauses G04,G07,G10",
                  "reason": "Original cached AX limitation, decode/log omissions, summary corrections, tool/parser/timeout failures and normal return retained; explicit PM/Omil G04 existing-contract disposition. LIMIT: Successful attribution/preservation observation, not a claim that failed decoder/tool observations passed. Distinct unobserved branches retain their original Case statuses and owners."
                }
              },
              "coverage": {
                "contract_coverage": [
                  "CURRENT_LOAD",
                  "NATIVE_ROOT_NEXT_REQUEST",
                  "COMPLETE_CURRENT_PNG",
                  "SAME_NOTIFICATION_ACTIONS",
                  "CURRENT_SETTINGS_REGRESSION_SEMANTICS",
                  "CURRENT_OWNED_IMAGE_TARGET_LIFETIME_SEMANTICS",
                  "BOUNDED_NONINTERFERENCE",
                  "FAILURE_SCOPE_AND_DISPOSITION"
                ],
                "artifact_binding": [
                  "CURRENT_LOAD",
                  "CURRENT_SETTINGS_REGRESSION_SEMANTICS",
                  "CURRENT_OWNED_IMAGE_TARGET_LIFETIME_SEMANTICS"
                ],
                "failure_attribution": [
                  "FAILURE_SCOPE_AND_DISPOSITION",
                  "BOUNDED_NONINTERFERENCE"
                ],
                "preconditions": [
                  "CURRENT_LOAD"
                ],
                "step_1": [
                  "NATIVE_ROOT_NEXT_REQUEST",
                  "CURRENT_SETTINGS_REGRESSION_SEMANTICS",
                  "FAILURE_SCOPE_AND_DISPOSITION"
                ],
                "step_2": [
                  "NATIVE_ROOT_NEXT_REQUEST",
                  "COMPLETE_CURRENT_PNG",
                  "BOUNDED_NONINTERFERENCE",
                  "CURRENT_OWNED_IMAGE_TARGET_LIFETIME_SEMANTICS"
                ],
                "step_3": [
                  "SAME_NOTIFICATION_ACTIONS",
                  "CURRENT_OWNED_IMAGE_TARGET_LIFETIME_SEMANTICS"
                ],
                "expected": [
                  "CURRENT_LOAD",
                  "NATIVE_ROOT_NEXT_REQUEST",
                  "COMPLETE_CURRENT_PNG",
                  "SAME_NOTIFICATION_ACTIONS",
                  "CURRENT_SETTINGS_REGRESSION_SEMANTICS",
                  "CURRENT_OWNED_IMAGE_TARGET_LIFETIME_SEMANTICS",
                  "BOUNDED_NONINTERFERENCE",
                  "FAILURE_SCOPE_AND_DISPOSITION"
                ]
              },
              "missing_requirements": [],
              "unresolved_owned_failures": []
            },
            "status": "pass"
          },
          "CURRENT_F327_LOAD": {
            "status": "pass",
            "head": "2e576ce3c724dc95ccf0688b8a1bf7cd92d774a8",
            "artifact_sha256": "f3274d7501f9d173ece5de6b1cb729a7fb40d247cfe2a17c6c937fcc085a2c88",
            "actor": "gpt",
            "observer": "codex-session-sha256:7b2456c4ac692a16695fa3b6f035ac2149e76f23bd63e479f43f81ca687087e1",
            "at": "2026-11-06T13:21:06.184603+00:00",
            "evidence": "Retained current native receipt sha256=a80b9fdcc7619824e74b39f5fd5bc27baa89eba29d28a629377b641de617e8b3; original outcome sha256=b7376a368dd90fcaa48f26e279cd492f60ec8976bae798d18c7a3bc39bf8d2de; original manifest sha256=99673eefcfabfe8cc0144f9883f11f47b3c0e566e65a0878b173b88fdbcffa16",
            "loaded_identity": "保存済み実行者原本のinstalled 3 JAR hash/size/mode、loaded paths/JBR、About version0.2.0/full source2e576ceを照合。保存画像のOFFと常設理由全文を確認。現在のGUIや新実行を主張しない。",
            "reason": "候補適用draft作成者による現在artifact bindingの限定事実評価。atは本評価時刻、native再実行時刻ではない。元実行者abcfとunit process時刻範囲は原本に保持。新ON/Save/設定操作/USBの証拠にしない。"
          }
        },
        "coverage": {
          "contract_coverage": [
            "RETAINED_NORMAL_341_WHOLE_RESULT"
          ],
          "artifact_binding": [
            "CURRENT_F327_LOAD"
          ],
          "failure_attribution": [
            "RETAINED_NORMAL_341_WHOLE_RESULT"
          ],
          "preconditions": [
            "RETAINED_NORMAL_341_WHOLE_RESULT"
          ],
          "step_1": [
            "RETAINED_NORMAL_341_WHOLE_RESULT"
          ],
          "step_2": [
            "RETAINED_NORMAL_341_WHOLE_RESULT"
          ],
          "step_3": [
            "RETAINED_NORMAL_341_WHOLE_RESULT"
          ],
          "expected": [
            "RETAINED_NORMAL_341_WHOLE_RESULT"
          ]
        },
        "missing_requirements": [],
        "unresolved_owned_failures": []
      }
    },
    "284:CRASH-AUTO-BOUNDS": {
      "status": "pending",
      "head": "2e576ce3c724dc95ccf0688b8a1bf7cd92d774a8",
      "artifact_sha256": "f3274d7501f9d173ece5de6b1cb729a7fb40d247cfe2a17c6c937fcc085a2c88",
      "actor": "gpt",
      "observer": "codex-session-sha256:7b2456c4ac692a16695fa3b6f035ac2149e76f23bd63e479f43f81ca687087e1",
      "at": "2026-11-06T13:58:56.017172+00:00",
      "evidence": "Candidate2e/formalf327 BOUNDS application; original948 whole result canonical-sha256=2d22c2339d0ab6c12177a2f1881a4669a0286c93778ba4a5a33f39f631e5731e",
      "loaded_identity": "保存済み実行者原本のinstalled 3 JAR hash/size/mode、loaded paths/JBR、About version0.2.0/full source2e576ceを照合。保存画像のOFFと常設理由全文を確認。現在のGUIや新実行を主張しない。",
      "reason": "Issue428の候補別正式assessment案。元whole結果と限定独立適用支持を保持するが、固定PR全feedback/実originを含む正式承認は未成立。",
      "responsibility_revision": "85296114925d6947be93158e3eca90fcd69e561b3d8a5d1e6845407b73108f61",
      "responsibility_evidence": {
        "contract_coverage": "Draft284BOUNDS/contract_coverage; facts=RETAINED_NORMAL_284_BOUNDS_WHOLE_RESULT",
        "artifact_binding": "Draft284BOUNDS/artifact_binding; facts=CURRENT_F327_LOAD",
        "failure_attribution": "Draft284BOUNDS/failure_attribution; facts=RETAINED_NORMAL_284_BOUNDS_WHOLE_RESULT",
        "preconditions": "Draft284BOUNDS/preconditions; facts=RETAINED_NORMAL_284_BOUNDS_WHOLE_RESULT",
        "step_1": "Draft284BOUNDS/step_1; facts=RETAINED_NORMAL_284_BOUNDS_WHOLE_RESULT",
        "step_2": "Draft284BOUNDS/step_2; facts=RETAINED_NORMAL_284_BOUNDS_WHOLE_RESULT",
        "expected": "Draft284BOUNDS/expected; facts=RETAINED_NORMAL_284_BOUNDS_WHOLE_RESULT"
      },
      "responsibility_assessment": {
        "source_candidate": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
        "source_result": {
          "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
          "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
          "actor": "gpt",
          "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
          "at": "2026-11-05T22:37:25.036933+00:00",
          "evidence": "Private284BOUNDSwholeproofsha256=3f3c52154cac4e61ca7ab3ab840a9476fd0f82f826ec824c2048a79872ed84f3; trustednormalresolver=ca863083d3db673cbc1b6f3c79b5b44de69681f5",
          "loaded_identity": "Retainedactual948/formalZIPa582/mainJARd620 installed6/loaded3JAR, Rabbit2 AI-262.10968.63.2622.16730706/JBR25.0.4, sameproject/emulator5602, currentLOG_ONLY autoON finiteexecution. Notpresenthost/lease orUSB evidence.",
          "reason": "Private normal284BOUNDS contentcandidate; new wholeindependentreview/formaladoptionoutstanding. NoCasePASSissued.",
          "responsibility_revision": "85296114925d6947be93158e3eca90fcd69e561b3d8a5d1e6845407b73108f61",
          "responsibility_evidence": {
            "contract_coverage": "Normal284BOUNDS contract_coverage; proofsha256=3f3c52154cac4e61ca7ab3ab840a9476fd0f82f826ec824c2048a79872ed84f3; facts=CURRENT_ARTIFACT_LOAD,CURRENT_BOUNDED_DETECTOR_SEMANTICS,CURRENT_SINGLE_WRITER_NO_QUEUE_SEMANTICS,ACTUAL_AUTO_FAILED_FIXED_REQUEST,ACTUAL_FINITE_PLAN_AND_RESOURCE_OBSERVATIONS,ACTUAL_PRESERVATION_AND_OWNED_PRODUCT_CLEANUP,CURRENT_FAILURE_OUTPUT_PRESERVATION_SEMANTICS,RETAINED_FAILURE_ATTRIBUTION",
            "artifact_binding": "Normal284BOUNDS artifact_binding; proofsha256=3f3c52154cac4e61ca7ab3ab840a9476fd0f82f826ec824c2048a79872ed84f3; facts=CURRENT_ARTIFACT_LOAD,CURRENT_BOUNDED_DETECTOR_SEMANTICS,CURRENT_SINGLE_WRITER_NO_QUEUE_SEMANTICS",
            "failure_attribution": "Normal284BOUNDS failure_attribution; proofsha256=3f3c52154cac4e61ca7ab3ab840a9476fd0f82f826ec824c2048a79872ed84f3; facts=RETAINED_FAILURE_ATTRIBUTION,ACTUAL_FINITE_PLAN_AND_RESOURCE_OBSERVATIONS,ACTUAL_PRESERVATION_AND_OWNED_PRODUCT_CLEANUP",
            "preconditions": "Normal284BOUNDS preconditions; proofsha256=3f3c52154cac4e61ca7ab3ab840a9476fd0f82f826ec824c2048a79872ed84f3; facts=CURRENT_ARTIFACT_LOAD",
            "step_1": "Normal284BOUNDS step_1; proofsha256=3f3c52154cac4e61ca7ab3ab840a9476fd0f82f826ec824c2048a79872ed84f3; facts=CURRENT_BOUNDED_DETECTOR_SEMANTICS,CURRENT_SINGLE_WRITER_NO_QUEUE_SEMANTICS,CURRENT_FAILURE_OUTPUT_PRESERVATION_SEMANTICS",
            "step_2": "Normal284BOUNDS step_2; proofsha256=3f3c52154cac4e61ca7ab3ab840a9476fd0f82f826ec824c2048a79872ed84f3; facts=ACTUAL_FINITE_PLAN_AND_RESOURCE_OBSERVATIONS,ACTUAL_AUTO_FAILED_FIXED_REQUEST,ACTUAL_PRESERVATION_AND_OWNED_PRODUCT_CLEANUP,CURRENT_FAILURE_OUTPUT_PRESERVATION_SEMANTICS,RETAINED_FAILURE_ATTRIBUTION",
            "expected": "Normal284BOUNDS expected; proofsha256=3f3c52154cac4e61ca7ab3ab840a9476fd0f82f826ec824c2048a79872ed84f3; facts=CURRENT_ARTIFACT_LOAD,CURRENT_BOUNDED_DETECTOR_SEMANTICS,CURRENT_SINGLE_WRITER_NO_QUEUE_SEMANTICS,ACTUAL_AUTO_FAILED_FIXED_REQUEST,ACTUAL_FINITE_PLAN_AND_RESOURCE_OBSERVATIONS,ACTUAL_PRESERVATION_AND_OWNED_PRODUCT_CLEANUP,CURRENT_FAILURE_OUTPUT_PRESERVATION_SEMANTICS,RETAINED_FAILURE_ATTRIBUTION"
          },
          "responsibility_assessment": {
            "source_candidate": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
            "source_result": {
              "status": "pending",
              "reason": "実装と決定的検査の結果は固定PRへ記録する。新候補のnative/FILE/寿命/負荷受入はこの割当で未実施。"
            },
            "source_result_sha256": "700f1a71194a669f4ff2adc25facdd837ad0049440c54e58687d6b3120464143",
            "source_record_evidence": "git:948b1ad2f18ee266c79935327a403dd7471fdc3b:docs/verification/changes/issue-284.json#/cases/id=CRASH-AUTO-BOUNDS/gpt canonicalsha256=700f1a71194a669f4ff2adc25facdd837ad0049440c54e58687d6b3120464143",
            "facts": {
              "CURRENT_ARTIFACT_LOAD": {
                "status": "pass",
                "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                "actor": "gpt",
                "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                "at": "2026-11-05T22:18:30.509146+00:00",
                "loaded_identity": "Retainedactual948/formalZIPa582/mainJARd620 installed6/loaded3JAR, Rabbit2 AI-262.10968.63.2622.16730706/JBR25.0.4, sameproject/emulator5602, currentLOG_ONLY autoON finiteexecution. Notpresenthost/lease orUSB evidence.",
                "evidence": "Selected284source/native refs 764ed6c77ce924f3d032df3a200f5062acfa1a1ba90699b984215a22b0ba13de,c4d66219ce24883207addf655012b417fbdc6c32a5b7285ec99cd4e2597f3c61; scopedindependentguarantees G01",
                "reason": "Current exactformal source/artifact/actualruntime and ownedfixture identified. LIMIT: Retainedactual948/formalZIPa582/mainJARd620 installed6/loaded3JAR, Rabbit2 AI-262.10968.63.2622.16730706/JBR25.0.4, sameproject/emulator5602, currentLOG_ONLY autoON finiteexecution. Notpresenthost/lease orUSB evidence."
              },
              "CURRENT_BOUNDED_DETECTOR_SEMANTICS": {
                "status": "pass",
                "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                "actor": "gpt",
                "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                "at": "2026-11-05T22:18:30.509146+00:00",
                "loaded_identity": "Fresh current284 source/class/identifiedoriginalregression applicability assessment, notnewtest/nativeexecution.",
                "evidence": "Selected284source/native refs 9088341b9e3a8fa312a0bb383605af103a2dd37a19acc70485d73693b79843c6; scopedindependentguarantees ",
                "reason": "Current source/class applicability of original16observations/64KiBaccountedcopiedevidence,256terminal/noeviction,300othergroups/no targetaccounting,8admissions andexact30scooldown/explicitOFFONrearm/retainedmanual semantics. LIMIT: Actualdeterministic macOS execution remains f62; no native saturation/heap measurement invented."
              },
              "CURRENT_SINGLE_WRITER_NO_QUEUE_SEMANTICS": {
                "status": "pass",
                "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                "actor": "gpt",
                "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                "at": "2026-11-05T22:18:30.509146+00:00",
                "loaded_identity": "Fresh current284 source/class/identifiedoriginalregression applicability assessment, notnewtest/nativeexecution.",
                "evidence": "Selected284source/native refs 9088341b9e3a8fa312a0bb383605af103a2dd37a19acc70485d73693b79843c6; scopedindependentguarantees ",
                "reason": "Current singlepending/writer, no secondcrashreader/autoretry or replay of BUSY/FAILED_PIN rejectedpair; original manualbusy/failedpin/retry/firstunassessedpair semantics. LIMIT: Bounded undecided observations may first be assessed aftermanual settles; that is not prohibited retryqueue. Manualsave remains independent of autosuspend."
              },
              "ACTUAL_AUTO_FAILED_FIXED_REQUEST": {
                "status": "pass",
                "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                "actor": "gpt",
                "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                "at": "2026-11-05T22:18:30.509146+00:00",
                "loaded_identity": "Retainedactual948/formalZIPa582/mainJARd620 installed6/loaded3JAR, Rabbit2 AI-262.10968.63.2622.16730706/JBR25.0.4, sameproject/emulator5602, currentLOG_ONLY autoON finiteexecution. Notpresenthost/lease orUSB evidence.",
                "evidence": "Selected284source/native refs 764ed6c77ce924f3d032df3a200f5062acfa1a1ba90699b984215a22b0ba13de,2d4248ebbc5d9292dead1a94fe624029606409934da3a532a1c4626b5982faa8; scopedindependentguarantees G05,G06",
                "reason": "Current autoFAILED actualwriter attempt2 andsame request/sequence180swindow/target/LOG_ONLY/error held>=22.912342582989368s. LIMIT: Globalattempt2 follows manualattempt1, not retry. pending0 notpin0; no auto completed session/trigger/nsfile invented."
              },
              "ACTUAL_FINITE_PLAN_AND_RESOURCE_OBSERVATIONS": {
                "status": "pass",
                "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                "actor": "gpt",
                "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                "at": "2026-11-05T22:18:30.509146+00:00",
                "loaded_identity": "Retainedactual948/formalZIPa582/mainJARd620 installed6/loaded3JAR, Rabbit2 AI-262.10968.63.2622.16730706/JBR25.0.4, sameproject/emulator5602, currentLOG_ONLY autoON finiteexecution. Notpresenthost/lease orUSB evidence.",
                "evidence": "Selected284source/native refs 2d4248ebbc5d9292dead1a94fe624029606409934da3a532a1c4626b5982faa8,e7794302f4f8903e0b6a971d15d83bd23e45f2e624588c09cc6439edbdf6a843; scopedindependentguarantees G01,G05,G06,G08",
                "reason": "Fixedbeforeexecution quantities/time/capacity/observers andactual8stage RSS/process/storage reflect manualSUCCESS andautoFAIL/nativeLOG_ONLY/sourcebounds union. LIMIT: No currentrunautoSUCCESS or bothmode nativeclaim; nopeak/leak/performance/heapquota inference. Unmeasured/adverse facts retained."
              },
              "ACTUAL_PRESERVATION_AND_OWNED_PRODUCT_CLEANUP": {
                "status": "pass",
                "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                "actor": "gpt",
                "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                "at": "2026-11-05T22:18:30.509146+00:00",
                "loaded_identity": "Retainedactual948/formalZIPa582/mainJARd620 installed6/loaded3JAR, Rabbit2 AI-262.10968.63.2622.16730706/JBR25.0.4, sameproject/emulator5602, currentLOG_ONLY autoON finiteexecution. Notpresenthost/lease orUSB evidence.",
                "evidence": "Selected284source/native refs 2d4248ebbc5d9292dead1a94fe624029606409934da3a532a1c4626b5982faa8,764ed6c77ce924f3d032df3a200f5062acfa1a1ba90699b984215a22b0ba13de; scopedindependentguarantees G08,G06",
                "reason": "Original19digestpreservation, new5stat/writerhash qualification, sameheldfailure activeclose, ownedcapture/readers/forwardcleanup andnormalprocessreturn supported. LIMIT: New5prefaultindependentdigests absent; partial0 not positivepartialfailure proof. OriginalclipboardcustodyUNKNOWN is separate andkept."
              },
              "CURRENT_FAILURE_OUTPUT_PRESERVATION_SEMANTICS": {
                "status": "pass",
                "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                "actor": "gpt",
                "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                "at": "2026-11-05T22:18:30.509146+00:00",
                "loaded_identity": "Fresh current284 source/class/identifiedoriginalregression applicability assessment, notnewtest/nativeexecution.",
                "evidence": "Selected284source/native refs 9088341b9e3a8fa312a0bb383605af103a2dd37a19acc70485d73693b79843c6; scopedindependentguarantees ",
                "reason": "Current exactsource/class applies productionfailure/partial/foreignreplacement/pinning andactualtermination-before-release regressions. LIMIT: Known-input originalexecution notnewnativefaultmatrix; source/ring limits do not autodelete completed/failedoutputs or bound all disk."
              },
              "RETAINED_FAILURE_ATTRIBUTION": {
                "status": "pass",
                "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                "actor": "gpt",
                "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                "at": "2026-11-05T22:18:30.509146+00:00",
                "loaded_identity": "Retainedactual948/formalZIPa582/mainJARd620 installed6/loaded3JAR, Rabbit2 AI-262.10968.63.2622.16730706/JBR25.0.4, sameproject/emulator5602, currentLOG_ONLY autoON finiteexecution. Notpresenthost/lease orUSB evidence.",
                "evidence": "Selected284source/native refs 4ef9c5b4bf4d61646491e8acf00711e7b97b799d1e867f274569c9daa507a330,a65b0a160393400eb1ea6ce8c4f9e2662a7dac1007383b581d3b8c4a8eb21cf9,2d4248ebbc5d9292dead1a94fe624029606409934da3a532a1c4626b5982faa8; scopedindependentguarantees G01,G05,G06,G08",
                "reason": "Rawtimings/FAIL/UNKNOWN, resource-returnpreterminal correction, earlyactiveproject_closed, toolerrors/notification andclipboard limits retained. LIMIT: Correctedinterpretation doesnotedit originalrecords or turnintentionalfaultinto successfulsave; unrelatedDETECT/SAVE/LIFETIME/humanREAL/main remain."
              }
            },
            "coverage": {
              "contract_coverage": [
                "CURRENT_ARTIFACT_LOAD",
                "CURRENT_BOUNDED_DETECTOR_SEMANTICS",
                "CURRENT_SINGLE_WRITER_NO_QUEUE_SEMANTICS",
                "ACTUAL_AUTO_FAILED_FIXED_REQUEST",
                "ACTUAL_FINITE_PLAN_AND_RESOURCE_OBSERVATIONS",
                "ACTUAL_PRESERVATION_AND_OWNED_PRODUCT_CLEANUP",
                "CURRENT_FAILURE_OUTPUT_PRESERVATION_SEMANTICS",
                "RETAINED_FAILURE_ATTRIBUTION"
              ],
              "artifact_binding": [
                "CURRENT_ARTIFACT_LOAD",
                "CURRENT_BOUNDED_DETECTOR_SEMANTICS",
                "CURRENT_SINGLE_WRITER_NO_QUEUE_SEMANTICS"
              ],
              "failure_attribution": [
                "RETAINED_FAILURE_ATTRIBUTION",
                "ACTUAL_FINITE_PLAN_AND_RESOURCE_OBSERVATIONS",
                "ACTUAL_PRESERVATION_AND_OWNED_PRODUCT_CLEANUP"
              ],
              "preconditions": [
                "CURRENT_ARTIFACT_LOAD"
              ],
              "step_1": [
                "CURRENT_BOUNDED_DETECTOR_SEMANTICS",
                "CURRENT_SINGLE_WRITER_NO_QUEUE_SEMANTICS",
                "CURRENT_FAILURE_OUTPUT_PRESERVATION_SEMANTICS"
              ],
              "step_2": [
                "ACTUAL_FINITE_PLAN_AND_RESOURCE_OBSERVATIONS",
                "ACTUAL_AUTO_FAILED_FIXED_REQUEST",
                "ACTUAL_PRESERVATION_AND_OWNED_PRODUCT_CLEANUP",
                "CURRENT_FAILURE_OUTPUT_PRESERVATION_SEMANTICS",
                "RETAINED_FAILURE_ATTRIBUTION"
              ],
              "expected": [
                "CURRENT_ARTIFACT_LOAD",
                "CURRENT_BOUNDED_DETECTOR_SEMANTICS",
                "CURRENT_SINGLE_WRITER_NO_QUEUE_SEMANTICS",
                "ACTUAL_AUTO_FAILED_FIXED_REQUEST",
                "ACTUAL_FINITE_PLAN_AND_RESOURCE_OBSERVATIONS",
                "ACTUAL_PRESERVATION_AND_OWNED_PRODUCT_CLEANUP",
                "CURRENT_FAILURE_OUTPUT_PRESERVATION_SEMANTICS",
                "RETAINED_FAILURE_ATTRIBUTION"
              ]
            },
            "missing_requirements": [],
            "unresolved_owned_failures": []
          },
          "status": "pass"
        },
        "source_result_sha256": "2d22c2339d0ab6c12177a2f1881a4669a0286c93778ba4a5a33f39f631e5731e",
        "source_record_evidence": "Original normal284BOUNDS projection byte-sha256=00aa6ac89fdeb49f31a93b6ea826a500635598ee8b7223dea2b415f8f7d9dc90; whole limited independent decision sha256=319c2c0bf0518ccfdd2afed25796975e665b1a4b18f204e1103f7ca51638be29",
        "facts": {
          "RETAINED_NORMAL_284_BOUNDS_WHOLE_RESULT": {
            "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
            "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
            "actor": "gpt",
            "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
            "at": "2026-11-05T22:37:25.036933+00:00",
            "evidence": "Private284BOUNDSwholeproofsha256=3f3c52154cac4e61ca7ab3ab840a9476fd0f82f826ec824c2048a79872ed84f3; trustednormalresolver=ca863083d3db673cbc1b6f3c79b5b44de69681f5",
            "loaded_identity": "Retainedactual948/formalZIPa582/mainJARd620 installed6/loaded3JAR, Rabbit2 AI-262.10968.63.2622.16730706/JBR25.0.4, sameproject/emulator5602, currentLOG_ONLY autoON finiteexecution. Notpresenthost/lease orUSB evidence.",
            "reason": "Private normal284BOUNDS contentcandidate; new wholeindependentreview/formaladoptionoutstanding. NoCasePASSissued.",
            "responsibility_revision": "85296114925d6947be93158e3eca90fcd69e561b3d8a5d1e6845407b73108f61",
            "responsibility_evidence": {
              "contract_coverage": "Normal284BOUNDS contract_coverage; proofsha256=3f3c52154cac4e61ca7ab3ab840a9476fd0f82f826ec824c2048a79872ed84f3; facts=CURRENT_ARTIFACT_LOAD,CURRENT_BOUNDED_DETECTOR_SEMANTICS,CURRENT_SINGLE_WRITER_NO_QUEUE_SEMANTICS,ACTUAL_AUTO_FAILED_FIXED_REQUEST,ACTUAL_FINITE_PLAN_AND_RESOURCE_OBSERVATIONS,ACTUAL_PRESERVATION_AND_OWNED_PRODUCT_CLEANUP,CURRENT_FAILURE_OUTPUT_PRESERVATION_SEMANTICS,RETAINED_FAILURE_ATTRIBUTION",
              "artifact_binding": "Normal284BOUNDS artifact_binding; proofsha256=3f3c52154cac4e61ca7ab3ab840a9476fd0f82f826ec824c2048a79872ed84f3; facts=CURRENT_ARTIFACT_LOAD,CURRENT_BOUNDED_DETECTOR_SEMANTICS,CURRENT_SINGLE_WRITER_NO_QUEUE_SEMANTICS",
              "failure_attribution": "Normal284BOUNDS failure_attribution; proofsha256=3f3c52154cac4e61ca7ab3ab840a9476fd0f82f826ec824c2048a79872ed84f3; facts=RETAINED_FAILURE_ATTRIBUTION,ACTUAL_FINITE_PLAN_AND_RESOURCE_OBSERVATIONS,ACTUAL_PRESERVATION_AND_OWNED_PRODUCT_CLEANUP",
              "preconditions": "Normal284BOUNDS preconditions; proofsha256=3f3c52154cac4e61ca7ab3ab840a9476fd0f82f826ec824c2048a79872ed84f3; facts=CURRENT_ARTIFACT_LOAD",
              "step_1": "Normal284BOUNDS step_1; proofsha256=3f3c52154cac4e61ca7ab3ab840a9476fd0f82f826ec824c2048a79872ed84f3; facts=CURRENT_BOUNDED_DETECTOR_SEMANTICS,CURRENT_SINGLE_WRITER_NO_QUEUE_SEMANTICS,CURRENT_FAILURE_OUTPUT_PRESERVATION_SEMANTICS",
              "step_2": "Normal284BOUNDS step_2; proofsha256=3f3c52154cac4e61ca7ab3ab840a9476fd0f82f826ec824c2048a79872ed84f3; facts=ACTUAL_FINITE_PLAN_AND_RESOURCE_OBSERVATIONS,ACTUAL_AUTO_FAILED_FIXED_REQUEST,ACTUAL_PRESERVATION_AND_OWNED_PRODUCT_CLEANUP,CURRENT_FAILURE_OUTPUT_PRESERVATION_SEMANTICS,RETAINED_FAILURE_ATTRIBUTION",
              "expected": "Normal284BOUNDS expected; proofsha256=3f3c52154cac4e61ca7ab3ab840a9476fd0f82f826ec824c2048a79872ed84f3; facts=CURRENT_ARTIFACT_LOAD,CURRENT_BOUNDED_DETECTOR_SEMANTICS,CURRENT_SINGLE_WRITER_NO_QUEUE_SEMANTICS,ACTUAL_AUTO_FAILED_FIXED_REQUEST,ACTUAL_FINITE_PLAN_AND_RESOURCE_OBSERVATIONS,ACTUAL_PRESERVATION_AND_OWNED_PRODUCT_CLEANUP,CURRENT_FAILURE_OUTPUT_PRESERVATION_SEMANTICS,RETAINED_FAILURE_ATTRIBUTION"
            },
            "responsibility_assessment": {
              "source_candidate": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
              "source_result": {
                "status": "pending",
                "reason": "実装と決定的検査の結果は固定PRへ記録する。新候補のnative/FILE/寿命/負荷受入はこの割当で未実施。"
              },
              "source_result_sha256": "700f1a71194a669f4ff2adc25facdd837ad0049440c54e58687d6b3120464143",
              "source_record_evidence": "git:948b1ad2f18ee266c79935327a403dd7471fdc3b:docs/verification/changes/issue-284.json#/cases/id=CRASH-AUTO-BOUNDS/gpt canonicalsha256=700f1a71194a669f4ff2adc25facdd837ad0049440c54e58687d6b3120464143",
              "facts": {
                "CURRENT_ARTIFACT_LOAD": {
                  "status": "pass",
                  "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                  "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                  "actor": "gpt",
                  "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                  "at": "2026-11-05T22:18:30.509146+00:00",
                  "loaded_identity": "Retainedactual948/formalZIPa582/mainJARd620 installed6/loaded3JAR, Rabbit2 AI-262.10968.63.2622.16730706/JBR25.0.4, sameproject/emulator5602, currentLOG_ONLY autoON finiteexecution. Notpresenthost/lease orUSB evidence.",
                  "evidence": "Selected284source/native refs 764ed6c77ce924f3d032df3a200f5062acfa1a1ba90699b984215a22b0ba13de,c4d66219ce24883207addf655012b417fbdc6c32a5b7285ec99cd4e2597f3c61; scopedindependentguarantees G01",
                  "reason": "Current exactformal source/artifact/actualruntime and ownedfixture identified. LIMIT: Retainedactual948/formalZIPa582/mainJARd620 installed6/loaded3JAR, Rabbit2 AI-262.10968.63.2622.16730706/JBR25.0.4, sameproject/emulator5602, currentLOG_ONLY autoON finiteexecution. Notpresenthost/lease orUSB evidence."
                },
                "CURRENT_BOUNDED_DETECTOR_SEMANTICS": {
                  "status": "pass",
                  "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                  "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                  "actor": "gpt",
                  "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                  "at": "2026-11-05T22:18:30.509146+00:00",
                  "loaded_identity": "Fresh current284 source/class/identifiedoriginalregression applicability assessment, notnewtest/nativeexecution.",
                  "evidence": "Selected284source/native refs 9088341b9e3a8fa312a0bb383605af103a2dd37a19acc70485d73693b79843c6; scopedindependentguarantees ",
                  "reason": "Current source/class applicability of original16observations/64KiBaccountedcopiedevidence,256terminal/noeviction,300othergroups/no targetaccounting,8admissions andexact30scooldown/explicitOFFONrearm/retainedmanual semantics. LIMIT: Actualdeterministic macOS execution remains f62; no native saturation/heap measurement invented."
                },
                "CURRENT_SINGLE_WRITER_NO_QUEUE_SEMANTICS": {
                  "status": "pass",
                  "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                  "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                  "actor": "gpt",
                  "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                  "at": "2026-11-05T22:18:30.509146+00:00",
                  "loaded_identity": "Fresh current284 source/class/identifiedoriginalregression applicability assessment, notnewtest/nativeexecution.",
                  "evidence": "Selected284source/native refs 9088341b9e3a8fa312a0bb383605af103a2dd37a19acc70485d73693b79843c6; scopedindependentguarantees ",
                  "reason": "Current singlepending/writer, no secondcrashreader/autoretry or replay of BUSY/FAILED_PIN rejectedpair; original manualbusy/failedpin/retry/firstunassessedpair semantics. LIMIT: Bounded undecided observations may first be assessed aftermanual settles; that is not prohibited retryqueue. Manualsave remains independent of autosuspend."
                },
                "ACTUAL_AUTO_FAILED_FIXED_REQUEST": {
                  "status": "pass",
                  "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                  "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                  "actor": "gpt",
                  "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                  "at": "2026-11-05T22:18:30.509146+00:00",
                  "loaded_identity": "Retainedactual948/formalZIPa582/mainJARd620 installed6/loaded3JAR, Rabbit2 AI-262.10968.63.2622.16730706/JBR25.0.4, sameproject/emulator5602, currentLOG_ONLY autoON finiteexecution. Notpresenthost/lease orUSB evidence.",
                  "evidence": "Selected284source/native refs 764ed6c77ce924f3d032df3a200f5062acfa1a1ba90699b984215a22b0ba13de,2d4248ebbc5d9292dead1a94fe624029606409934da3a532a1c4626b5982faa8; scopedindependentguarantees G05,G06",
                  "reason": "Current autoFAILED actualwriter attempt2 andsame request/sequence180swindow/target/LOG_ONLY/error held>=22.912342582989368s. LIMIT: Globalattempt2 follows manualattempt1, not retry. pending0 notpin0; no auto completed session/trigger/nsfile invented."
                },
                "ACTUAL_FINITE_PLAN_AND_RESOURCE_OBSERVATIONS": {
                  "status": "pass",
                  "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                  "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                  "actor": "gpt",
                  "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                  "at": "2026-11-05T22:18:30.509146+00:00",
                  "loaded_identity": "Retainedactual948/formalZIPa582/mainJARd620 installed6/loaded3JAR, Rabbit2 AI-262.10968.63.2622.16730706/JBR25.0.4, sameproject/emulator5602, currentLOG_ONLY autoON finiteexecution. Notpresenthost/lease orUSB evidence.",
                  "evidence": "Selected284source/native refs 2d4248ebbc5d9292dead1a94fe624029606409934da3a532a1c4626b5982faa8,e7794302f4f8903e0b6a971d15d83bd23e45f2e624588c09cc6439edbdf6a843; scopedindependentguarantees G01,G05,G06,G08",
                  "reason": "Fixedbeforeexecution quantities/time/capacity/observers andactual8stage RSS/process/storage reflect manualSUCCESS andautoFAIL/nativeLOG_ONLY/sourcebounds union. LIMIT: No currentrunautoSUCCESS or bothmode nativeclaim; nopeak/leak/performance/heapquota inference. Unmeasured/adverse facts retained."
                },
                "ACTUAL_PRESERVATION_AND_OWNED_PRODUCT_CLEANUP": {
                  "status": "pass",
                  "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                  "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                  "actor": "gpt",
                  "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                  "at": "2026-11-05T22:18:30.509146+00:00",
                  "loaded_identity": "Retainedactual948/formalZIPa582/mainJARd620 installed6/loaded3JAR, Rabbit2 AI-262.10968.63.2622.16730706/JBR25.0.4, sameproject/emulator5602, currentLOG_ONLY autoON finiteexecution. Notpresenthost/lease orUSB evidence.",
                  "evidence": "Selected284source/native refs 2d4248ebbc5d9292dead1a94fe624029606409934da3a532a1c4626b5982faa8,764ed6c77ce924f3d032df3a200f5062acfa1a1ba90699b984215a22b0ba13de; scopedindependentguarantees G08,G06",
                  "reason": "Original19digestpreservation, new5stat/writerhash qualification, sameheldfailure activeclose, ownedcapture/readers/forwardcleanup andnormalprocessreturn supported. LIMIT: New5prefaultindependentdigests absent; partial0 not positivepartialfailure proof. OriginalclipboardcustodyUNKNOWN is separate andkept."
                },
                "CURRENT_FAILURE_OUTPUT_PRESERVATION_SEMANTICS": {
                  "status": "pass",
                  "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                  "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                  "actor": "gpt",
                  "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                  "at": "2026-11-05T22:18:30.509146+00:00",
                  "loaded_identity": "Fresh current284 source/class/identifiedoriginalregression applicability assessment, notnewtest/nativeexecution.",
                  "evidence": "Selected284source/native refs 9088341b9e3a8fa312a0bb383605af103a2dd37a19acc70485d73693b79843c6; scopedindependentguarantees ",
                  "reason": "Current exactsource/class applies productionfailure/partial/foreignreplacement/pinning andactualtermination-before-release regressions. LIMIT: Known-input originalexecution notnewnativefaultmatrix; source/ring limits do not autodelete completed/failedoutputs or bound all disk."
                },
                "RETAINED_FAILURE_ATTRIBUTION": {
                  "status": "pass",
                  "head": "948b1ad2f18ee266c79935327a403dd7471fdc3b",
                  "artifact_sha256": "a582052844272ad004f3e3429d50b12a7ec6b6783bc686457465a02f20546316",
                  "actor": "gpt",
                  "observer": "codex-session-sha256:026c095d10bdce839daf168b7283a3ded18bc72cd97d11483aac53f43f663847",
                  "at": "2026-11-05T22:18:30.509146+00:00",
                  "loaded_identity": "Retainedactual948/formalZIPa582/mainJARd620 installed6/loaded3JAR, Rabbit2 AI-262.10968.63.2622.16730706/JBR25.0.4, sameproject/emulator5602, currentLOG_ONLY autoON finiteexecution. Notpresenthost/lease orUSB evidence.",
                  "evidence": "Selected284source/native refs 4ef9c5b4bf4d61646491e8acf00711e7b97b799d1e867f274569c9daa507a330,a65b0a160393400eb1ea6ce8c4f9e2662a7dac1007383b581d3b8c4a8eb21cf9,2d4248ebbc5d9292dead1a94fe624029606409934da3a532a1c4626b5982faa8; scopedindependentguarantees G01,G05,G06,G08",
                  "reason": "Rawtimings/FAIL/UNKNOWN, resource-returnpreterminal correction, earlyactiveproject_closed, toolerrors/notification andclipboard limits retained. LIMIT: Correctedinterpretation doesnotedit originalrecords or turnintentionalfaultinto successfulsave; unrelatedDETECT/SAVE/LIFETIME/humanREAL/main remain."
                }
              },
              "coverage": {
                "contract_coverage": [
                  "CURRENT_ARTIFACT_LOAD",
                  "CURRENT_BOUNDED_DETECTOR_SEMANTICS",
                  "CURRENT_SINGLE_WRITER_NO_QUEUE_SEMANTICS",
                  "ACTUAL_AUTO_FAILED_FIXED_REQUEST",
                  "ACTUAL_FINITE_PLAN_AND_RESOURCE_OBSERVATIONS",
                  "ACTUAL_PRESERVATION_AND_OWNED_PRODUCT_CLEANUP",
                  "CURRENT_FAILURE_OUTPUT_PRESERVATION_SEMANTICS",
                  "RETAINED_FAILURE_ATTRIBUTION"
                ],
                "artifact_binding": [
                  "CURRENT_ARTIFACT_LOAD",
                  "CURRENT_BOUNDED_DETECTOR_SEMANTICS",
                  "CURRENT_SINGLE_WRITER_NO_QUEUE_SEMANTICS"
                ],
                "failure_attribution": [
                  "RETAINED_FAILURE_ATTRIBUTION",
                  "ACTUAL_FINITE_PLAN_AND_RESOURCE_OBSERVATIONS",
                  "ACTUAL_PRESERVATION_AND_OWNED_PRODUCT_CLEANUP"
                ],
                "preconditions": [
                  "CURRENT_ARTIFACT_LOAD"
                ],
                "step_1": [
                  "CURRENT_BOUNDED_DETECTOR_SEMANTICS",
                  "CURRENT_SINGLE_WRITER_NO_QUEUE_SEMANTICS",
                  "CURRENT_FAILURE_OUTPUT_PRESERVATION_SEMANTICS"
                ],
                "step_2": [
                  "ACTUAL_FINITE_PLAN_AND_RESOURCE_OBSERVATIONS",
                  "ACTUAL_AUTO_FAILED_FIXED_REQUEST",
                  "ACTUAL_PRESERVATION_AND_OWNED_PRODUCT_CLEANUP",
                  "CURRENT_FAILURE_OUTPUT_PRESERVATION_SEMANTICS",
                  "RETAINED_FAILURE_ATTRIBUTION"
                ],
                "expected": [
                  "CURRENT_ARTIFACT_LOAD",
                  "CURRENT_BOUNDED_DETECTOR_SEMANTICS",
                  "CURRENT_SINGLE_WRITER_NO_QUEUE_SEMANTICS",
                  "ACTUAL_AUTO_FAILED_FIXED_REQUEST",
                  "ACTUAL_FINITE_PLAN_AND_RESOURCE_OBSERVATIONS",
                  "ACTUAL_PRESERVATION_AND_OWNED_PRODUCT_CLEANUP",
                  "CURRENT_FAILURE_OUTPUT_PRESERVATION_SEMANTICS",
                  "RETAINED_FAILURE_ATTRIBUTION"
                ]
              },
              "missing_requirements": [],
              "unresolved_owned_failures": []
            },
            "status": "pass"
          },
          "CURRENT_F327_LOAD": {
            "status": "pass",
            "head": "2e576ce3c724dc95ccf0688b8a1bf7cd92d774a8",
            "artifact_sha256": "f3274d7501f9d173ece5de6b1cb729a7fb40d247cfe2a17c6c937fcc085a2c88",
            "actor": "gpt",
            "observer": "codex-session-sha256:7b2456c4ac692a16695fa3b6f035ac2149e76f23bd63e479f43f81ca687087e1",
            "at": "2026-11-06T13:21:06.184603+00:00",
            "evidence": "Retained current native receipt sha256=a80b9fdcc7619824e74b39f5fd5bc27baa89eba29d28a629377b641de617e8b3; original outcome sha256=b7376a368dd90fcaa48f26e279cd492f60ec8976bae798d18c7a3bc39bf8d2de; original manifest sha256=99673eefcfabfe8cc0144f9883f11f47b3c0e566e65a0878b173b88fdbcffa16",
            "loaded_identity": "保存済み実行者原本のinstalled 3 JAR hash/size/mode、loaded paths/JBR、About version0.2.0/full source2e576ceを照合。保存画像のOFFと常設理由全文を確認。現在のGUIや新実行を主張しない。",
            "reason": "候補適用draft作成者による現在artifact bindingの限定事実評価。atは本評価時刻、native再実行時刻ではない。元実行者abcfとunit process時刻範囲は原本に保持。新ON/Save/設定操作/USBの証拠にしない。"
          }
        },
        "coverage": {
          "contract_coverage": [
            "RETAINED_NORMAL_284_BOUNDS_WHOLE_RESULT"
          ],
          "artifact_binding": [
            "CURRENT_F327_LOAD"
          ],
          "failure_attribution": [
            "RETAINED_NORMAL_284_BOUNDS_WHOLE_RESULT"
          ],
          "preconditions": [
            "RETAINED_NORMAL_284_BOUNDS_WHOLE_RESULT"
          ],
          "step_1": [
            "RETAINED_NORMAL_284_BOUNDS_WHOLE_RESULT"
          ],
          "step_2": [
            "RETAINED_NORMAL_284_BOUNDS_WHOLE_RESULT"
          ],
          "expected": [
            "RETAINED_NORMAL_284_BOUNDS_WHOLE_RESULT"
          ]
        },
        "missing_requirements": [],
        "unresolved_owned_failures": []
      }
    }
  }
}
```

</details>
