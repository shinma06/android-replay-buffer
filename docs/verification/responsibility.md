# 製品の保証範囲と最小の十分な検証

製品が保証するのは、取得した対象データを正しく保全・保存・説明し、自分の操作・状態・失敗処理が外部APIと正しく接続することです。外部player、editor、IDE、OS内部のすべての挙動を保証しません。既存・予定・今後のCLI/JVM/FILE/native/ハーネス試験へ適用します。

[Issue #182](https://github.com/shinma06/android-replay-buffer/issues/182)の本人指示による正式改訂です。[固定元要件](https://github.com/shinma06/android-replay-buffer/blob/36de4f7e3c3d4cff1fa7c5a50287a7f98ce4447c/docs/requirements.md)、[同期設計](https://github.com/shinma06/android-replay-buffer/blob/36de4f7e3c3d4cff1fa7c5a50287a7f98ce4447c/docs/design/timeline.md)、[旧受入手順](https://github.com/shinma06/android-replay-buffer/blob/36de4f7e3c3d4cff1fa7c5a50287a7f98ce4447c/docs/verification/initial-version.md)の外部実装保証と過剰な実施方法を、以下と[正式計画](amendments/initial-agent.json)の`responsibility_revision`で明示的に置換します。固定原Case・原計画・過去の観察/失敗/未達は履歴です。削除/統合/置換を観測PASSにしません。

## 保証と証拠の分担

| 自製品が保証すること | 最小の十分な検証 | 外部側との境界 |
|---|---|---|
| 全受理データ・log帰属/record ID・原packet/config/bytes・順序・欠落/unknown | production parser/Store/Writerの回帰と全対象FILE照合。予定母数を先に固定し欠落を除外しない | logcat/encoderが出さなかった内容を捏造せず、取得不明/未対応を説明する |
| 単一MP4/track・sample/index/headers・PTS/duration/edit・窓/IDR prefix/既知gap/未知duration/held/terminal | 全file/sampleを忠実な独立readerで読み、edit未適用mediaと適用後movieの対応を元PTS/indexへ照合。全frameを検査 | ファイル非破損だけで内容/seekを証明しない。readerのedit無視・PTS再配置も自製品の時刻へ混同しない |
| 内容・原寸/aspect/文字/色/余白・全frame対応 | 元packet/rawと復号画素・全選択frameの対応。混在変換は原rectangleと品質を検査し全画素一致を再圧縮へ誤適用しない。文字可読性は代表実画像も確認 | 全Playや全playerでの全pixel目視は不要。native実表示の代表確認と全内容検査を分ける |
| 同期精度・実時間窓・時計不確実性・長時間drift | 全300event/30分、開始/中間/終了各100、p95≤100ms・最大≤250ms・drift≤50ms・clock≤20ms、cut≤1観測frame間隔＋時計誤差、mux/index≤1msを維持。全欠落/重複/unknownを別集計 | 実描画時刻をREQUEST/commitやplayerのlabelで推定しない。長時間依存を短い回帰で代替しない |
| 固定T/N/対象/pin・retry・全出力完成・partial/error/atomic非上書き | production故障回帰、所有filesystemの実atomic API、代表native失敗→固定retryとUIの結合。後半欠落ならFAILED＋pin、成功公開なし | OS/FS未対応を成功と呼ばず、既存出力/未知所有者を保全する。ホスト全体を埋める試験はしない |
| process/reader所有・generation・取消し・実停止・Lifecycle/EDT/設定 | fake SDK/loopbackで全race/故障、実IDEでは代表起動・取得・通常再接続・OFF/close・復元・Apply/Cancel・通知/不足表示を共有 | 外部IDE/model/dialog内部の全組合せの再試験はしない。自分のglueの実call/表示/失敗保全は検証する |
| 通常利用に必要な導入・外部起動・読める案内・accessibility | 同梱依存/hash/license、実ロード・代表native操作と可読性/Tab/狭幅/theme/DPI、READMEと標準editorの代表open/read | playerの全seek精度やeditorの全件Find動作を製品保証にしない |

相互運用は代表open・短いPlay・前後seekを一度確認します。既存の観察を有効条件へ対応付けて再利用でき、一Caseごとの新GUI枠は作りません。同条件の複数Caseへ共有でき、全再生・全境界精密seekを繰り返しません。異常は自動的に製品bug又は外部bugにせず、生成container/index/edit/timing/content違反、reader実装差、未帰属に分けます。製品違反と未帰属は未完了。根拠付き外部差は診断履歴と利用上の制約に残し、全player一致のためだけに出力を改変しません。実interopを省略したFILE検査だけでsmoke成功を主張しません。

## 全135 Caseの監査と有効条件

正式計画の9 groupが全135 keyを重複なく対応付けます。204出典と原Case全文hash、原plan canonical `a5c470bf6bdbbbe77d2001bcb287667e4a6d399ee935ebd3fc23388be2f37441`は保持します。34 keyには必要な個別overrideがあります。原Case JSONとそのpending/blocked/failは書き換えません。

| group | Case数 | 判断と理由 |
|---|---:|---|
| environment | 6 | 維持。同梱依存/SDK/PATH非依存とロードの実integrationが必要 |
| markers | 2 | 維持。fixtureの表示/phase/view/coalescingを誤ると測定oracleが成立しない |
| precision | 7 | 統合。同じ30分/300eventを13/31/41/27で共有。裾の外れ/経時driftは維持 |
| recipient | 6 | 置換。全FILE/内容と代表interop/editorへ分担し外部全Play/全Find保証を除外 |
| buffer | 9 | 統合。180秒超/開始20秒/重複保存の実工程とproduction境界回帰/GOP6証拠を共有 |
| recovery | 54 | 統合。所有・失敗・clock/log/window/保存不変性を保持し、全faultは隔離回帰、glueは代表native |
| ui-settings | 36 | 統合。自製品入力/復元/帰属/表示の契約を保持し、全race/数値境界は回帰、代表操作はnative |
| video | 12 | 置換。全packet/frame/画素・VFR/unknown/Levelを保持し外部精密seekはFILE＋代表smokeへ |
| log-reader | 3 | 維持。header/lid/payload/readerの全境界とnative有限母数の用途が異なる |

個別の有効preconditions/steps/expected、Agent条件、延期、人間指定の変更は同blockの`overrides`だけを使います。未記載の機能条件は維持します。実施分担はgroupの`methods`が原文のnative/GUI全種反復指定を置換し、元の意味・全種の期待結果を回帰で検証します。41/27/71の実保存失敗→固定retry等、実integrationでしか分からない代表工程は残ります。45:VFR-BREAKは全5種類の前/中/後保存をproduction隔離fixtureへ明示割当し、別nativeの取得/復旧/表示を束ねます。52 UNKNOWN、49 ACQUIRE、106のfaultも合成とnativeを混同しません。

85の通常3event/両log9phase、PRE3＋POST3/両log18phase、N300は保持します。240秒の事前host余裕は段取り例へ降格し、保存後の固定snapshot T・個別frameの支持mapping/誤差で全対象の窓包含を証明します。窓外・欠落・unknownは救済しません。106の100入力/300phaseは有限reader比較の欠落検出として維持し、同期300event/30分を代替しません。

一律601画像のhuman手読取りと16 Caseの最終human actor指定は除外します。全300遷移/前後frameの番号識別と測定は維持し、独立校正された自動oracleで代替できます。元fixtureの画像真値とpackage/run/view/event ID、誤認/欠落/重複/coalescing/unknownの悪い入力、全関連frame/PTS/誤差、画像とlogの独立性、識別不能時の拒否を検証します。既存`transitions.swift`の色/parity補助は番号を復号せず偶数個の欠落を見逃すため、この代替oracleではありません。期待log順序やconfidenceだけから画像番号を推定しません。曖昧箇所は実画像確認し、識別不能/誤差過大なら未完了です。現時点で自動oracleの校正や既存画像判定をPASSとしたものではありません。REAL34、元の実機条件、3:PLUGIN-LOADの証拠付き操作経路延期は保持します。

## 既存の自動試験と今後の担当依頼

CLIの2 testはthreadtime正常/不正行のparser互換だけを示します。ADB/daemon/録画・時刻/保存/端末の成功を主張しません。既存JVM20ファイルはProtocol/Clock/Store/Writer、Wire/fake SDK、所有cleanup/資源、Settings/IDE接続/UI説明に分かれます。全packet/画素、整数/悪い入力、原子公開/停止やpinの回帰は自製品責任であり削除しません。IDE bridge/settings回帰を外部IDE全体の合格に換えません。fixture instrumentationはoracleの健全性に使い、製品動画の合格とは別です。

ハーネス群はGit保護/影響判定、出典/plan/gate/listing/reuse、registry/lease/worker/handoff/QA、秘密/参照の整合を検査します。候補変更のためだけの無条件再build/全native再試験は要求せず、下記の証拠と変更影響で判断します。テストを読んだ事実と実行成功を区別し、既存の未変更suiteを理由なく反復しません。

担当へは既存Issue/計画から目的・自製品の観測可能な欠陥・有効条件・最小手段・全データ母数・既存証拠・停止条件を渡します。新しい台帳/毎操作フォーム/承認gateを作りません。単に試せる、原文に書いてある、長いという理由で追加/削除せず、固有の欠陥を検出できるかで選びます。shared lease・privacy・実tool権限・所有fixture・復旧は全方法で維持します。

## trusted-main改訂と旧観察の再評価

改訂を独立レビュー付きtooling PRでmainへ入れ、gateと候補付き一覧は固定main baseの同blockを読みます。candidate側のみの改訂、欠落/不一致plan、135集合や原plan/hashの改変、symlink/重複JSON keyは拒否します。Caseのresolverと一覧は、原計画そのものを持つ旧candidateにもこの限定改訂を適用でき、製品コード/fixture/buildを変えたことにはしません。promotionの候補後metadata制限・全commit出典/strict baseは維持します。main文書を取り込むため既存の専用develop同期と新source候補が必要な場合も、不変製品/build/fixture・同artifactを実証して旧観察を再評価でき、source SHAが進んだだけで再build/再ロード/全GUI再試験を命じません。意味が変わる試験に不足する事実は追加実施します。製品修正/不明な依存変更は新候補と影響再試験へ戻します。

新結果は現planの`stage_revision`に加え、観察の`responsibility_revision`（blockのcanonical SHA-256）、`responsibility_evidence`に`contract_coverage / artifact_binding / failure_attribution`とgroupの`evidence_keys`の全参照を持ちます。全有効条件への対応、実生成/全JAR/元source/成果物binding、既知fail/未帰属の処理、group別の実行証拠を独立レビューします。参照文字列の存在だけでは内容を証明しません。

旧成功の再評価は既存の`reused`形式と同じキーで`status: reassessed`を使います。`source_result`に元の`initial-pass`又は全体`pass`をそのまま保存し、`source_candidate`は現在と同じでも構いません。`confirmation`は新しい試験実行ではなく、新条件への対応確認です。現candidate/hash・実確認者/日時・loaded identity・equivalence/environment evidenceと上記の責任改訂証拠を要求します。元planは`previous_plan_sha256`に一致し、元の実行経路・GOP/人間証拠を検証します。祖先関係・不変製品/build/fixture/設定と途中revertを含む既存history gateを維持します。

旧fail/pending/blocked/deferredの**判定そのもの**の再利用、再利用の連鎖、未確認の原結果、旧日時より前の確認は拒否します。部分報告を旧wholeCase PASSへ昇格させません。一方、旧wholeCaseが外部側の旧義務でpendingでも、その報告で独立に実証された成功事実は捨てません。新しい`initial_observation`又は通常`pass`の現在評価として、`contract_coverage`に各旧事実の元候補/成果物/観察者/日時/固定証拠と全有効条件への対応を記録します。`reason`で再評価日時と元の実行日時を区別し、新GUI実行と主張しません。`artifact_binding`には不変artifact/製品/build/fixture/条件の実証、`failure_attribution`には旧fail/pendingの理由と未解決の自製品違反を保全します。欠ける事実だけ追加実施し、未解決の自製品失敗があれば新判定もfailです。これは旧判定のcopyではなく、別のレビュー対象です。この経路は観察に`responsibility_assessment`を追加し、`source_candidate / source_result / source_result_sha256 / source_record_evidence`に固定の原判定を保持します。`facts`はID付きの実成功観察（通常の観察形式、原candidate/hash/実施者/日時/ロード/証拠）、`coverage`は全責任証拠keyからそのfact IDへの対応です。`missing_requirements / unresolved_owned_failures`は空でなければ成功不可。gateは原plan/不変履歴・元結果hash・factの候補/成果物/成功/日時・対応の欠落を検査し、独立レビューが原記録の一致と全条件の実質を確認します。改訂hashを旧結果に足すだけでは成功にしません。通常の`reused`は同じ契約の例外のままです。

gateと一覧は有効条件/原文履歴/再評価を分けて示します。初期段階の合格は完全受入ではなく、後続の全体gateには全135の有効条件を復元します。責任改訂そのものはCase PASS・GUI実行・初期版完成・main製品受入を意味しません。旧候補/ZIP/15件観察/正式PASS0は履歴として保持し、再評価後の件数は別記録とします。QA27/65/87とrelease10の担当・残条件を双方向readbackした後に次の試験を割り当てます。
