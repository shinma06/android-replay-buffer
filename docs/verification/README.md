# 受入・固定候補・QA

現行の全139 Caseの方法は[保証目的とconsumer別方法の改訂 #223/#230](responsibility.md)と正式計画の`method_revision`を使います。以下の段階受入/履歴にある固定30分/300event・操作回数・GOP6証拠などは原記録であり、現行groupの必要証拠へ置換します。精度・データ/窓/保全・実接続・REAL未達を保持し、方法だけの更新を実行PASSにしません。


**現在の保証境界**: [保証目的と最小検証（#223/#230）](responsibility.md)と正式計画の`responsibility_revision.method_revision`を適用します。原Case/旧計画/以下の過去改訂説明は履歴として保持し、外部player全Play/精密seek・一律human番号読取り・native故障全種反復・無変更artifactの再build義務は明示改訂後の有効条件で判断します。未達をPASSにしません。gateと一覧は同じresolverを使います。

`changes/issue-N.json` はIssueの受入、`promotion.json` はmainへ入れる固定候補と全Caseの観察の正本です。過去の観察を別SHA/buildのpassへ流用しません。未実施はpending、環境不足はblocked、製品不具合はfailと区別します。

## Caseの記録

GUI不要なら具体的理由と実行したCLI検証を記録します。

```json
{
  "schema": 1,
  "issue": 12,
  "gui_required": false,
  "reason": "管理文書だけの変更で製品の実行経路を変更しない",
  "cli_checks": ["python3 scripts/check.py: pass"],
  "cases": []
}
```

実機/IDE観察が必要なら`gui_required: true`とし、各Caseに`id / artifact / change / preconditions / steps / expected / provenance / gpt / human / fix_issue / fix_pr / recheck / next_action`を記入します。`gpt`はschema互換キーでモデル指定ではありません。`gpt/human`には`status`と`reason`を置き、未観察をpassにしません。failには別の実在open修正Issueが必要です。Computer Use経路自体が要件なら`required_execution: computer_use`を指定します。

CLIではUSB切断/再接続、直前区間の動画/logcat時刻整合、log-only保存、保存失敗、daemon停止等を変更範囲に応じて選びます。プラグインではさらにEDT/dispose/取消し/設定保存/IDE再起動を確認します。既存2 unit testsは全製品の受入を代替しません。

## 受入NG修正のdevelop統合（#226）

元の受入NGを直すPRは、失敗した保証を修正版固定候補で再試験して合格し、その証拠を独立評価してからdevelopへ統合します。一般の新機能・無関係なpending/blocked Caseの全受入を前倒ししません。既存の正常なmerged PRは遡ってこのgateで再判定せず、元Case/失敗記録は保持します。変更した製品のmain promotionは引き続き全候補/全Caseを別に確認します。

開始時にdevelopの`type:bug` Issue本文へ `Acceptance repair: none`（受入NG修正ではない理由を併記）又は元NG記録のIssueコメント数値IDを記載します。新機能でも元NG修正を含む場合は後者が必要です。trusted baseの全親履歴で当該Issueへ紐づいたCaseのgpt/human failは、後のpending/pass・Case削除・fix_issue変更・revertでも元NG記録を必須とします。変更したCase JSONのblobを一度ずつ読み、履歴上の失敗Case ID・actor・canonical Case hashの組ごとに、修正Issueとの対応を持つ元記録を要求します。同じCase/actorでも内容や保証が異なる失敗版は別々に必要で、同じhashの再登場にはcommitごとの重複記録を要求しません。通常のstatus更新や全保証を満たした修正結果は許可し、無関係Issueに過去NGの再試験を要求しません。ラベルやnone宣言で回避できません。独立レビューは分類の正しさとIssue/QAに記録された元NGの漏れを確認します。機械検査が自然言語の全不具合を自動発見するものではありません。

元NGは修正Issueに一つのコメントとして保全します。本文を `<!-- acceptance-repair:v1 -->` と改行から開始し、その後へJSONオブジェクトを直接置きます（code fenceなし）。`schema: 1 / fix_issue / failures`を持ち、`failures`は `GAP-1` などの一意なIDから以下の値への対応です。記録の変更は旧記録を消さず新しいコメントへ移し、Issue宣言・修正結果hash・独立レビューを更新します。媒体・生ログ・私的pathは公開せず、固定公開要約と担当が保持する封印証拠への参照を使います。

- `case`: 元の `Issue番号:Case ID`。
- `source_candidate / source_case_sha256`: 失敗した固定40桁sourceと、その時点のCase全文のcanonical SHA-256。元sourceはdevelop baseの祖先で、そのCase JSONはregular fileでなければなりません。
- `original_result`: 元の `status: fail / head / artifact_sha256 / actor / observer / at / evidence / loaded_identity / reason`。元source/成果物・実条件・確認者・日時・固定証拠を保持します。未実施や関連unitの結果をnative失敗記録へ換えません。
- `guarantees`: 失敗した保証IDから目的・失敗条件への対応。複数NG・全失敗保証を列挙し、Case全体を未実施のまま再試験の合格にしません。

修正PRの `changes/issue-N.json` に `repair` オブジェクトを追加します。

```json
{
  "candidate": "<再試験する修正コードの固定40桁SHA>",
  "artifacts": {"app": "<修正版ZIPの64桁SHA-256>"},
  "failure_record_sha256": "<元NGコメントJSON全体のcanonical SHA-256>",
  "writer_session": "<実装担当session>",
  "results": {
    "GAP-1": {
      "observation": {"status": "pass", "head": "<candidate>", "artifact_sha256": "<修正版hash>", "actor": "gpt", "observer": "<実行担当session>", "at": "<実観察日時ISO8601>", "loaded_identity": "<実行成果物の識別>", "evidence": "<固定実行証拠>", "reason": "<元失敗保証の再現と解消>"},
      "coverage": {"<元の保証ID>": "<その保証の実行・結果への証拠参照>"},
      "method": "<有効目的契約に忠実な実施方法・nativeとの同等性・限界>",
      "contract_revision": "<現在のtrusted mainのresponsibility_revision block canonical SHA-256>"
    }
  }
}
```

`artifacts`の名前は元Caseの`artifact`に一致させます。既定は`app`で、#227の`27:RECONNECT-EMU`では`plugin`です。既存のcanonical hash関数（UTF-8、sort_keys、ensure_ascii=False、区切り`,`/`:`）を共用します。gateは現在のmain refをAPIで固定し、そのcommitの正式resolverで有効方法を読みます。candidate側の独自改訂は認めず、正式resolverに登録済みの旧計画との対応を用い、方法だけの改訂で無条件の製品再build/再試験を要求しません。責任改訂のない製品では`contract_revision`はJSON `null`のcanonical hashで、独立レビューが元Case契約を直接確認します。`results`は元NGの全IDと完全一致し、`coverage`も各NGの全保証IDと一致させます。観察は修正candidate/hashと一致し、元NG・candidate commitより古い日時を拒否します。元の失敗artifactは修正版として受理しません。

自己参照するtested HEADを作らないため、修正コードを先にcommitし、その固定candidateからbuild/実行してから結果metadataをcommitします。candidateは現baseより後、PR headより前にあり、`base..candidate`には受入JSON・promotion記録以外の実差分が必要です。base自体、受入metadataだけ、修正を打ち消した結果は拒否します。製品変更を一律要求せず、非製品原因の修正も独立レビューで実質を確認します。candidate後は当該Issueの受入JSONだけを変更できます。全commit edgeを調べ、symlink/削除/別ファイル/製品変更とその後のrevert・mergeを拒否します。base/製品が動いたら候補と証拠を再固定します。方法だけの改訂による既存事実の再評価と、製品NG修正後の新しい再試験を混同しません。

CIのAcceptance gateとcoordinatorは同じ`verify_pr`で上記の構造・出典・集合・候補履歴を検査します。参照文字列の存在は実行や合格内容を証明しません。独立sessionが元失敗・全保証の証拠内容、trusted有効契約、方法の忠実性と限界、実生成/ロードartifactを確認します。関連unit成功だけで元nativeNG全体を解消済みにせず、必要なnative接続/表示/保存を残します。忠実な同等production経路がその保証を十分に検出する場合は、理由と限界を記録して採用できます。

独立報告は通常のfixed HEAD/base・scope_complete・verdict等に加え、`repair_revision`（`[元NGコメントJSON, repair]`のcanonical hash）と、全失敗保証の実質評価を記した`repair_assessment`を持ちます。`session`はwriter/executorと異なる実際の独立sessionです。自己申告の別名を独立性の根拠にしません。coordinatorはこの一致と実担当経路を使い、欠落/古い評価/自己評価ではAgent review成功やmergeへ進みません。手動統合も同じ報告を照合してからAgent reviewを記録します。CI構造合格だけ、remote Code ReviewのCompletedだけ、writerのPASS宣言だけでは統合できません。merge直前には現HEAD/base・Issue/コメントfeedback・受入を読み戻し、変化したら旧評価を失効させます。tool権限・REAL保留・GUI lease・branch protectionは変更しません。

merge後・closure前も、develop修正PRの受入を当時の固定HEAD/baseと現在の元NGコメント・trusted mainから再計算し、同じ独立報告を照合します。後着編集に古い`state.acceptance`を使わず、変更で不一致になった結果・評価は失効させます。移管中の変更もclose/cleanup前に再照合します。

元NGコメントは編集・削除せず保持します。APIの`created_at`/`updated_at`が欠落・不正・不一致なら参照記録を拒否します。追記や訂正は新しい未編集コメントIDを作り、原記録へのリンクと全失敗/保証の対応、訂正理由をIssueに残して`Acceptance repair`を更新します。JSON schemaは変えず、独立レビューが旧記録・封印証拠との連続性と保証の欠落がないことを確認します。新IDへ移すだけで元の失敗を消せません。API時刻一致は構造検査であり、観察の真実性や同じ秒内の編集がなかったことを証明しないため、固定原記録と実証の独立照合を維持します。

失効対象の作成者はownerに限定しません。coordinatorのworker登録・実行に使うowner制限は別の権限境界として維持します。

main push時は既存Acceptance workflowが全ページの同repositoryのPR policyに準拠したopen develop修正PRを確認し、Agent reviewをpendingへ戻して現在HEADの既存Acceptance runを再実行します。current runがない・実行中・再実行不能ならpendingを保持し、担当が完了後に再試行します。CIと共通validatorの最終readbackもmain SHAを照合します。有効契約が変わったら現在契約で再試験記録と独立評価を更新します。mainの前進だけで有効契約・成果物・条件が不変なら、現在resolverによる再計算と同一性の確認を経て既存評価を再利用でき、全GUIの再実行は要求しません。

修正Issueの宣言変更/削除・参照コメントの編集/削除・type:bugのlabeled/unlabeledも、default branchのtrusted workflowから、そのIssueに紐づく同repositoryのPR policyに準拠したopen develop PRだけを失効させます。コメントイベントはIssue本文が実際に参照するIDだけを対象とし、未参照のマーカー付き本文も無視します。旧参照の変更・除去はIssue編集イベントの変更前/後の宣言比較で失効させ、コメントイベントの`changes.body.from`を旧Issue宣言とみなしません。全PRページを確認し、全対象のAgent reviewをpendingへ戻してから既存Acceptance runを再実行します。一つの再実行拒否/取得失敗で後続PRの失効を止めず、エラーは失敗として残します。PR一覧の通信失敗/HTTP5xxは同じページを最大3回（各requestの既存timeoutは30秒）取得し直し、401/403等の拒否は再試行しません。途中切断の`IncompleteRead`も同じ有限再取得へ含めます。一覧は配列を必須とし、不正JSON/文字コードや配列以外の応答は再試行せず未完了にします。有効な空配列は正常な終端として扱います。列挙できなければページ番号と不完全な範囲を失敗として残し、判明済みでpending書込みに成功した全対象の既存run復旧を試してから失敗を返します。未取得ページの未知PRが失効済みとは保証せず、担当PMが失敗runから有限の再列挙/readbackを行い、success/merge前に対象PR・main・受入の現在情報を正常取得して照合します。status書込み自体の失敗はpending成功と扱わず、統合を止めて担当がreadback/復旧します。独立レビューのbindingはIssueの`type:bug`の有無を含み、追加/除去で旧報告を再利用しません。ラベルの順序や無関係なpriority/status変更はbindingを変えません。type:bug以外のラベル変更・宣言に関係しない本文編集・通常コメント・PRコメント・新記録作成だけでは再実行せず、新IDへの宣言更新で再検証します。イベントは互いに取消さず、自己生成の報告ループ・pollingは追加しません。[イベント仕様](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#issue_comment)と[コメントmetadata](https://docs.github.com/en/rest/issues/comments#get-an-issue-comment)に従い、この非同期経路も直前live照合の代替にはしません。

mainはdevelop baseとは別のrefなので、GitHubのHEAD/strict-base保護だけでは最終readbackとmergeを原子的に結べません。main pushによるpending/re-runも非同期です。自動coordinatorと手動統合は直前のlive受入照合を必須とし、取得失敗・不一致を拒否しますが、サービスが提供しない跨refの原子性を保証したとは扱いません。CIのmainイベント経路はmain統合後、PR検証コードは通常のdevelop同期後に有効化されるため、同期前はPMの同じ運用gateを維持します。

## main promotion

1. main先行toolingをdevelopへ専用同期PRで取り込み、候補SHAを固定する。
2. mainにない候補の全commitをmerge済みdevelop PRへ一意に対応させ、各固定merge SHAのCase JSONを読む。
3. 同一候補・同一成果物の全Caseを確認し、promotion.jsonへ`schema: 1`、`base`（現在main SHA）、`candidate`、`changes`（commitとpr）、`artifact_sha256`、`results`を保存する。
4. resultsのキーは`Issue番号:Case ID`。passは`status / actor / observer / at / head / artifact_sha256 / evidence / loaded_identity / reason`を持つ。日時はtimezone付きISO8601、head/hashは候補と一致。loaded_identityに実際のCLI/Pluginのロード識別を書く。
5. 候補後に変更できるのはpromotion.jsonとpromotion IssueのCase JSONのみ。全範囲のgate・CI・独立レビューを通し、merge commitでmainへ統合する。

Plugin ZIP生成はプラグイン基盤の導入時に実buildへ接続します。現在は存在しないGradle/ZIP成果物を仮定せず、製品promotion前に再現できる固定成果物の作成・照合手順をそのIssueで定義します。main起点の限定変更も、trusted mainの`docs/verification/scopes/issue-N.json`による事前承認範囲と全Caseの観察が必要です。

### 実測GOPの境界試験（#56）

[GOP限定改訂](amendments/gop-boundaries.json)は、#13/#31/#41の`SYNC-WINDOW`と#27の`BUFFER-180`・`BUFFER-SHORT`・`SAVE-CONTINUE`（REAL/EMU）の9 Caseへ適用する。元PRの固定merge・全Case内容のSHA-256を照合し、#27はPR #33と#38の両出典を保持する。合計15出典の不足・追加・矛盾は拒否し、最後のPRを任意に採用しない。Case ID・artifact・実施方法・既存操作・期待結果・過去の状態は変更しない。

設計のI-frame間隔1秒はencoderへの試験開始値であり、実時間のGOPを保証しない。[Android仕様](https://developer.android.com/reference/android/media/MediaFormat#KEY_I_FRAME_INTERVAL)では、設定FPSからフレーム数へ換算するencoderの実間隔は実FPSによって変わり得る。指定値を実測値とせず、同じ固定ZIPの元PTS・隣接IDRから測定する。約1秒GOPは同一候補のJVM回帰として維持し、端末では実測した長いGOP・可変FPSを含め、IDR直後・中間・次IDR直前の切出しを追加確認する。JVM成功をIDE/端末の合格へ転記しない。実施方法と測定項目は改訂JSONの追加手順を正本とする。

最大限保存、切出し誤差≤1観測frame間隔＋時計誤差、mux/index丸め≤1ms、同期p95≤100ms・最大≤250ms・観測区間の中央値drift≤50ms・clock≤20ms、画質/負荷・容量/pin制限は維持する。欠落・時計不明・未確認末尾保持を明示し、独立した同期目的は現行precision方法で確認する。30分の実環境driftを主張する場合は対応する長時間実測が必要で、有限系列をその保証へ換えない。REAL保留をEMU結果で解消しない。

改訂はpromotionの固定base（trusted main）からだけ読む。通常のtooling PRで独立レビューしてmainへ導入し、専用同期PRでdevelopへ取り込んだ後に新candidateを固定する。candidateは改訂の`required_ancestor`を含み、同じ改訂JSONをregular fileとして持つ必要がある。候補側だけの変更、symlink、重複JSON key、不一致は拒否する。新candidateからZIPをbuild・実ロードし、全必要Caseを確認する。旧5dbの観察は履歴として残し、新候補のpassへ付け替えない。

改訂後の9 Caseの各passには、通常のcandidate/hash/実ロード/確認者/日時/証拠に加え、次を記録する。`gop_revision`は改訂JSON全体を`json.dumps(data, ensure_ascii=False, sort_keys=True, separators=(',', ':'))`で正規化したUTF-8のSHA-256。各`gop_evidence`値は、その候補で実行した確認の公開可能な証拠参照を記入する。私有媒体や生ログを公開しない。

```json
{
  "gop_revision": "改訂JSONの正規化SHA-256",
  "gop_evidence": {
    "one_second_regression": "同一sourceの約1秒GOP回帰commandと結果への参照",
    "after_idr": "実測IDR直後のcut位置・復号・誤差への参照",
    "mid_gop": "実測GOP中間のcut位置・復号・誤差への参照",
    "before_next_idr": "実測次IDR直前のcut位置・復号・誤差への参照",
    "vfr": "実測GOP時間/フレーム数・可変FPS条件への参照",
    "limits_and_quality": "既存保存量・画質/負荷・容量/pin条件の評価への参照"
  }
}
```

既存結果へこのhashを追記するだけでは新観察にならない。機械検査は識別・参照の存在を確認するもので、実行や品質の証明は証拠を読む独立レビューで行う。改訂hash・各証拠が不足した結果はgateと生成一覧の両方で合格にしない。

`verification.py --promotion ...`による一覧も同じresolverを使い、固定出典と元の前提、適用した前提・追加手順・改訂revision、観察側のrevisionと6種類の証拠参照を表示する。未登録の観察項目は未登録と表示する。履歴出典はtrusted改訂の固定merge/hashとcandidateの祖先関係から確認するため、初回promotion後も過去のCaseを選んで一覧化できる。渡すCase JSONは元の固定merge契約と一致させ、候補側の書換えで代用しない。promotion gateの必要集合は引き続き`base..candidate`の受入から決める。改訂だけではCase集合を減らせず、一覧のCase単位表示も全候補のpromotion gateを代替しない。

## 引継ぎ

### 初期版の段階受入（#64）

この節の固定30分/300event・9/18phase・GOP6証拠・一律human等は旧計画の履歴です。現行のCase範囲・出典/原結果を保持し、前提/実施方法/Agent依存は#223/#230のtrusted method_revisionを適用します。


利用者の方針に従い、初期版はエージェントが実行できる工程を完了し、人間の操作・確認と実機準備が必要な部分を[後続QA #65](https://github.com/shinma06/android-replay-buffer/issues/65)へ延期する。[固定計画](amendments/initial-agent.json)をtrusted mainの通常tooling PRでレビューし、develop同期後の候補へ適用する。#208の導入method改訂は、下記の登録直前計画を持つ不変候補にも限定適用できる。候補後の変更許可、main保護、独立レビュー、全commitの出典検査は変えない。

現在の計画は139 Case・208出典、105 CaseのAgent範囲と34 REAL Caseの延期を持つ。#219でPR218の実develop merge `06419bd17dddc00a1b57c086be4e86d6184e945f` の `216:SETTINGS-WIDTH-RECOVERY`（Case canonical `e53106b34733bd505bdd9740892674cbc2acf703ba204b06708d5768d9e9d8f3`）を既存ui-settingsへ1件追加し、required ancestorを同mergeへ進める。既存138 entry/207出典、原135/旧136計画、#182/208の方法・延期・元173契約と全失敗/未達を保持する。plan_extensionは累積4 keyと登録旧138計画hash `6d11e3cdee0b6189c743fb1a4c8ee3a4e582d72af7e9643bb001536dd4e0c79f`を持つ。追加1 key/group所属だけを除き、#201 ancestor/extensionへ戻した全文を独立固定hashへ照合する。その旧138計画から#208改訂前の固定13bd計画も復元し、両pinと原135hashを維持する。新候補は139計画のmain同期が必要で、旧138計画は保持事実の出典照合用の履歴に限る。候補自己申告hash・再hashした旧契約・candidate側だけの改訂では受理しない。

新Caseの全前提・全2手順・expected・再確認条件を保持し、同候補/標準ZIP/全JARの限定Settings製品class回帰と代表nativeの狭幅→広幅→元の狭幅、全文/最終文字/高さ・入力欄の収まり、編集/Apply/Cancel/reopen/元設定復元をQA27/175の同条件観察へ束ねる。新human義務/延期・GUI matrix・QAは追加しない。旧707の観察を新製品JARへ移植せず、製品/build/fixture/Case変更と途中revertを拒否するhistory gateは不変。main統合後の専用develop同期、新候補/標準ZIP固定、QA27/175/65・release10と元216への双方向移管はPMの後続作業で、計画登録はCase PASS・初期版完成ではない。

#201時点の計画は138 Case・207出典、104 CaseのAgent範囲と34 REAL Caseの延期を持つ。#201でPR199/200の実develop統合SHAへ接続し、`required_ancestor`を `4d278e65dec0c55e71d584ff24c7f6fa407586cb` とする。`197:SAVE-FAILURE-OWNERSHIP` と `198:LOCAL-OWNERSHIP-RECOVERY` の2 Case・2出典をrecoveryへ追加し、既存136 entry/205出典は変更しない。既存の`plan_extension`は#190の `186:CLOCK-BATCH-SAVE` と2追加keyを累積で列挙し、旧ancestor `5bf9e84a887171df5f0287b036e9ba288fbfae9f` へ戻して原135 entry/204出典のcanonical hashを照合する。`previous_plan_sha256`は原135計画のまま保持する。責任改訂では27:SAVE-FAIL-02-EMU/REALの保存attempt削除義務だけを保全へ置換し、失敗・取消し残留/容量増加・場所のbest effort伝達・既存取消しUI抑止を明示する。#198は旧取得rootを限定回収し、保存attempt残留を回収しない。新Caseの全前提・全2/3手順・expected・再確認条件と同ZIP製品class回帰を維持し、代表nativeの失敗場所表示→固定retry、起動回復/設定復元、通常取得→Save→継続取得→OFF/closeをQA27の同候補/同条件観察へ束ねる。全履歴/failed checks/未達/GOP・PTS・300event/30分・REAL34/人間延期/QA65は保持し、新QA・GUI全fault反復・human必須・延期・PASSを追加しない。#195等のmain運用差分を含めPMの専用develop同期後に新candidate/標準ZIPを固定する。製品class変更前の旧観察を新PASSへ転記せず、QA27/65とrelease10を継続する。計画の構造検証はGUI・初期版・main製品受入の成功ではない。

#190時点の計画は136 Case・205出典、102 Agent・REAL延期34で、PR189実統合 `542575c0a23a257be6bb37b028171426ea171efd` の `186:CLOCK-BATCH-SAVE` を追加した。その原entry・全5手順とClock/Store/Wire同ZIP回帰/代表native分担、4096履歴の限定JVM検証、QA74/27・71の同条件共有を維持する。この旧計画/責任revisionと旧観察は歴史上の記録で、後続の138/139計画へ一致扱いしない。

PR174反映時の計画は135 Case・204出典で、101 CaseのAgent範囲と34 REAL Caseの延期を持った。PR174の実develop統合SHA `5bf9e84a887171df5f0287b036e9ba288fbfae9f` を`required_ancestor`とし、#173の`PACKAGE-INPUT-GUIDANCE`の1 Case・1出典を追加した。既存134 entry・203出典と全agent_requirements/deferred_scope・人間指定・GOP・QA65、元#27/#121と旧c61の11件観察を保持する。新Caseの全前提・全3手順・expected・再確認条件を同じ新candidate/標準ZIP・全JAR hash・実ロードidentityで確認する。欄直下の入力条件・狭幅の折り返し・アクセシビリティ、代表正常値と代表不正package＋保持時間のatomic拒否、自動/手動・空package・Cancel/再open・設定復元と取得OFFを元契約どおり確認する。元#27 SETTINGS-INVALID-EMUの全条件を満たす同候補の観察だけを共有し、新native境界matrix・human必須・延期・自動PASSを追加しない。旧観察やJVM成功を新native受入へ転記しない。main計画統合後の別Issue/PRによるdevelop同期と新candidate/標準ZIP固定がQA27/175の先行条件であり、計画の構造検証はGUI・初期版・main製品受入の成功ではない。

PR145反映時の計画は134 Case・203出典で、100 CaseのAgent範囲と34 REAL Caseの延期を持った。PR145の実develop統合SHA `2baa4bef02d225137ada6cd1a019435abe74342d` を当時の`required_ancestor`とし、#143の`MIXED-VIDEO-LEVEL`の1 Case・1出典を追加した。既存133 entry・202出典と全agent_requirements/deferred_scope・人間指定・GOP・QA65を変更しない。元Caseの全前提・全3手順・expected・再確認条件を保持し、同じ新candidate/標準ZIP・全JAR hash・実ロードidentityでJVM回帰と実IDEの通常mixed Save/標準player観察を別証拠で確認する。全選択frame（decoder prefix含む）・元PTS/raw・1MP4/1track、全Playと通常/境界/中間/逆方向seek、文字・原寸/aspect・余白・色・窓/prefix非表示・可変duration、同候補Level5.1互換性を元契約どおり確認する。既存Case85の同候補工程・観察を対応する要件へ共有し、nativeで両Levelを狙い撃ちして生成する義務や新しいmixed失敗→再試行/取消し工程を追加しない。既存Caseの全残契約と失敗・未達を保持し、新延期・human必須・自動PASS・旧候補PASSの転記を行わない。JVM成功をnative受入やfull H.264/HRD準拠へ転用しない。mainの計画更新後に別Issue/PRでdevelopへ同期し、新candidate/標準ZIPを固定する。今回の構造検証はGUI・初期版・main製品受入の成功ではない。

PR136反映時の計画は133 Case・202出典で、99 CaseのAgent範囲と34 REAL Caseの延期を持った。PR136の実develop統合SHA `f5dd726a7282539f26d724f083e6e620b9366c45` を当時の`required_ancestor`とし、#135の`LOG-ONLY-README`の1 Case・1出典を追加した。既存132 entry・201出典と全agent_requirements/deferred_scope・人間指定・GOP・QA65は変更しない。元Caseの全前提・全3手順・expected・再確認条件、同production Writerの合成出力許容と実IDE/device Saveの区別を保持し、同じ新candidate/source・標準ZIP/JAR hash・実production Writerの生成bindingに結ぶ。動画あり/log-only各1のREADMEとsessionを標準editorで辿り、実MP4数1/0、条件付き説明、coverage.videoのstate/availableSeconds/reason、READMEhashとmanifest一致、文書非編集とowned終了・原保存物保全を確認する。新しい延期・human必須・自動PASSを追加せず、既存99dcの限定editor採用や旧候補PASSを新candidateへ転記しない。mainの計画更新後に別Issue/PRでdevelopへ同期し、新candidate/標準ZIPを固定する。今回の構造検証はGUI・初期版・main製品受入の成功ではない。

PR125反映時の計画は132 Case・201出典で、98 CaseのAgent範囲と34 REAL Caseの延期を持った。PR125の実develop統合SHA `57fec42cc108762b91e7619e029316904ade559d` を当時の`required_ancestor`とし、#121の`EMPTY-DESTINATION-GUIDANCE`の1 Case・1出典を追加した。既存131 entry・200出典と全agent_requirements/deferred_scope・人間指定・GOP・QA65は変更しない。元Caseの全前提・全3手順・expected・再確認条件を同じ新candidate/標準ZIP・実ロードで必須とし、取得OFFの近傍案内・空受理・非同期の古い結果排除・Apply/Cancel/reset/dispose・設定復元を確認する。新たな延期・human必須・自動PASSを追加せず、旧1dの観察/JVM成功をnative受入の代替にしない。#122は試験fixtureのみでGUI不要・Caseなし、新GUI Caseを追加しない。計画のmain統合後に別Issue/PRでdevelopへ同期し、新candidate/標準ZIPを固定する。計画の構造検証はGUI・初期版・main製品受入の成功ではない。

PR107反映時の計画は131 Case・200出典で、97 CaseのAgent範囲と34 REAL Caseの延期を持つ。PR107の実develop統合SHA `197aa0efca4486b76096e035fb98ba7e6d41d54a` を当時の`required_ancestor`とし、#106の`LOG-READER-PARTITION`・`LOG-READER-COMPATIBILITY`・`LOG-READER-RECOVERY-STOP`の3 Case・3出典を追加する。既存128 entry・197出典と全agent_requirements/deferred_scopeは変更しない。新3 Caseの全前提・操作・expected・再確認条件を保持し、同じ新candidate/標準ZIPのJVMと元契約が要求するnative観察を別証拠で確認する。新たな延期・human必須・自動PASSを追加せず、旧8d診断と旧候補の観察は履歴に保持する。固定100入力・300phaseの有限比較を正式300event/30分等の合格へ転用しない。

PR98反映時の計画は128 Case・197出典を持ち、既存196出典のPR/merge/path/Case全文hashを保持する。時計不確実区間の動画保存修正 #85 のPR86旧7手順・PR92の8手順・PR94の9手順を固定履歴として残し、PR98の実metadataで宣言されたVerificationにあるCase85の1出典だけを追加した。当時の`required_ancestor`はPR98の実統合SHA `34d14c90b3d44105eb18d09b8e2b86131fdc33d4`を指した。Case85と関連8 Caseの現在契約hashをそのtreeへ更新した。関連8 Caseは固定path/ID/hashをAgent requirementsから参照する。他119 entryは変更しなかった。当時の94 CaseのAgent範囲は必須で、34 REAL Caseは対応するEMU Caseの観察を残して延期する。混在40 Case・人間16 Case・GOP改訂9 key/15出典/6証拠とQA65を維持し、計画に列挙した部分だけを分ける。各sourceの履歴を消したり、古い緩い条件を選択したりしない。PR38の専用transport限定、共有ADB保護、同boot確認、画面OFFとdeep sleepの区別も維持する。新Caseや契約変更が加わった場合は計画の正式更新が必要である。

元のCase JSONとpending/blocked/failは書き換えない。`human: pending`だけでは人間工程を追加しない。標準player、GUI/可読性、全300イベント/30分の解析、同期・切出し精度、画質/負荷/容量/pin、失敗・復旧のAgent実行可能部分は維持する。最低601画像の人間による番号確認は初期段階から分離し、その前の解析を原Case全体の正式精度passとは呼ばない。製品failや単に難しい・時間がかかる工程は延期理由にしない。

`3:PLUGIN-LOAD`の通常導入は[固定ZIPのターミナル配置](../operations.md#固定plugin-zipのターミナル導入)とし、Settings → PluginsのInstall Plugin from Disk/版探索を必須にしない。自製品Settingsの表示/入力/Apply/Cancel/検証/アクセシビリティ/寿命は各元契約で維持する。初回自製品About/action、native導入、有効化、通常再起動、実PID/profile/source/versionロード識別、ZIPと全3JAR一致、取得OFFは実観察する。旧経路の原記録を保ち、再起動後のAbout再読取り/dialog終了画面確認だけを既存延期に残す。実体照合を画面確認のpassに置き換えず、別工程へ例外を拡張しない。

#208時点の`installation_revision`は直前138/207計画全文hash `13bd1e294519915d8c8a66be2770811a4d6b28465432061e7c2eae91a87fd8a3`を登録し、このfieldと追加3 overrideを除いた全文で照合する。原135hash/ancestor/全原entryと履歴は不変。当時の計画又は登録旧計画を持つ固定707等の候補だけを認め、[保持事実の現在評価](responsibility.md#trusted-main改訂と旧観察の再評価)では原観察/原判定/元plan、全有効条件への対応、不変source/全JAR/ZIP/build/fixture/条件と失敗帰属の証拠を独立レビューする。通常reuseの契約同一条件は緩めない。方法改訂だけで再build/再導入を要求せず、不足する事実だけ追加する。原Case JSON・旧fail/pending/blocked・707の既存packetは編集せず、方法変更を新GUI実行やCase PASSにしない。main統合/develop同期とQA27/65/release10の引継ぎはPMが別確認する。

`78:CLOCK-RECEIPT-NORMAL`は同ZIP製品classの限定JVMと実IDEの正常3event・全9phase行の確認を両方必須とし、延期やhuman必須を追加しない。3event確認を既存正式300event/30分・同期精度・GOP Caseの代替にせず、旧候補の結果を新buildへ転用しない。

`85:CLOCK-UNCERTAIN-VIDEO-PLAYBACK`はPR98実統合版の全前提・全9手順・expected・再確認条件を現在契約とし、PR86/92/94の旧source-only/mux方式を固定履歴として保持する。同じ新candidate/標準ZIPの製品classによる限定JVMと、実ロードidentityを確認したIDE・標準player観察を別証拠で束ね、双方を必須とする。N=300で通常3操作・両log各9phase行、および同取得で1回だけ通常再接続するPRE3/POST3操作・両log各18phase行を固定母数とし、事前のhost monotonicによる240秒の余裕と事後snapshot T・個別frameのsupported mappingによる全phaseの窓判定を分ける。動画のある成功保存は通常・再接続それぞれMP4ちょうど1件・1video track、動画のないlog-only保存は0件と理由明示とする。全対象を標準playerで通常durationの全Play/seekとして観察し、通常3/PRE3/POST3、gap前後・任意中間・逆方向seek・decoder preroll非表示・極短terminal境界を確認する。表示frame数を一律固定しないことをMP4個数1の例外にしない。未観察・欠落・unknown・窓外・時間不足を合格にせず、内部mappingがUI非公開なだけで保存を止めず、追加再接続・再録画を自動追加しない。延期やhuman必須を追加せず、旧候補のreview/PASSや短いCase85のPASSを全面解消・既存300event/30分・精密同期・OCR・画質/負荷/容量/pin・GOPの合格へ転用しない。

取得時にfreezeした正のdecodeRun/packetOrdinal、generation/session/config/寸法、単調な元PTS、完全byte順序とIDR file先頭を検査する。legacy0/欠番/overflow/矛盾・拒否packet・実VIDEO中断/容量lossからsource連続性を捏造せず、時計確度だけでは復号runを分割しない。clock scopes/復旧sample/両側anchorは説明用証拠として保持し、source再生許可や同期確定へ転用しない。元clock/閾値/gap/loss/null/MAX/個別unknown window/不足と共有retention/tailを維持する。観測PTS差によるVFR、PTS resetごとのmedia origin/config sample entry、prefix edit、既知gap空edit/不明actual duration nullとauthoring配置、terminal source_duration=nullとheld/tickを区別する。movie/media/元source/window対応を個別frameの時計確度で両logと照合し、動画全体秒を区間窓開始へ一律加算しない。表示保持や表現tickを確認済coverageへ加算せず、後続live/clock/別boot/任意fpsで未知durationを補完しない。旧候補のpartial/pending/FAIL/deferredや限定PASSを新候補へ転用しない。

同geometryは元VCL/configをremuxし、mixed geometryは現在契約の固定canvas・原寸/aspect/文字と色・全frame対応を検査する。元AnnexB config/全対象packet（必要decoder prefix含む）のraw sidecar・offset/size/hashと全画素/PTS/duration/headers/edit/indexを同ZIPで照合する。後半read/codec/disk/取消し/必要IDR不足で全対象を完成できない場合は成功フォルダを公開せずFAILEDと固定pinを保持し、後半drop・複数MP4fallback・非公開の部分成功にしない。device/app両logの共通record_id/UID/帰属/全9・18phaseと未知行、固定T/N/対象・設定履歴・原gap/clock・retry不変、atomic非上書き/UI不足表示を維持する。保存attemptの失敗・取消し残留は上記#201の保全契約に従い、取得・remote cleanupと区別する。全deferred_scopeと既存人間範囲を維持する。実装scopeは既存のコードgate・独立レビュー・双方向QA移管を確認して終了する。初期版/mainは同ZIPの全必要Agent受入と未観察・製品failの解消まで未完了とし、full GUI受入は既存延期範囲の完了まで未完了とする。

promotionには`stage: initial-agent`を明記し、`results`に全139 keyを置く。各値は以下の形とし、`stage_revision`には計画全体のcanonical SHA-256を入れる。

```json
{
  "status": "initial-pass",
  "stage_revision": "固定計画のcanonical SHA-256",
  "head": "新候補の40桁SHA",
  "artifact_sha256": "同じ成果物の64桁SHA-256",
  "followup_issue": 65,
  "initial_observation": {
    "status": "pass",
    "actor": "gpt",
    "observer": "実際の確認者",
    "at": "実際のtimezone付きISO8601日時",
    "head": "新候補の40桁SHA",
    "artifact_sha256": "同じ成果物の64桁SHA-256",
    "evidence": "初期範囲で実施した観察の公開可能な参照",
    "loaded_identity": "実ロードした同一候補の識別",
    "reason": "計画で維持した全条件の実観察"
  }
}
```

内部の観察には通常の同一候補・hash・確認者・日時・証拠を要求し、指定された`execution: computer_use`も維持する。GOP対象のAgent観察には従来のrevisionと6種類の証拠を追加する。REAL延期値は`status: deferred`とし、`initial_observation`を置かない。他の5キーは同じで、候補・計画・後続先の紐付けを記録するだけであり、REALの実観察を意味しない。未実施の確認者・時刻・ロード・GOP証拠を作らない。

全必須Agent範囲が合格すると`stage_complete: true`になるが、`gui_complete`と`full_acceptance_complete`はfalseのまま。一覧でも「初期版範囲合格」「延期・未実施」を原Case全体の合格と区別する。既存coordinatorの全GUI完了判定を変更せず、PMが初期版と後続QAの状態を別々に読み戻す。

初回main反映後も計画は残る。通常の完全受入では、`base..candidate`に現れなくなった延期Caseも固定出典から必要集合へ戻し、通常の全体passを要求する。`stage`を外すだけで延期を消せない。履歴の一覧も同じresolverで固定出典を読み、原契約・延期理由・担当・再開条件を保持する。初期段階のJSONへ過去の観察や計画hashを追記するだけでは新候補の実観察にならない。

人間による番号確認、またはその確認へ依存する16 Caseには、計画で`full_acceptance_actor: human`を指定する。完全受入の最終確認者を表し、Agentが実行できる測定やGUI操作を全て人間へ移す指定ではない。通常passの同一candidate/artifact・実確認者・日時・証拠に加え、`actor: human`と`human_evidence`を必須とする。`human_evidence`は同じ候補・成果物について、人間が行った番号確認またはその依存先の確認記録を参照する。同じ測定・確認を複数Caseで共有でき、16回の重複実行は不要。REAL準備保留だけのCaseや`PLUGIN-LOAD`の操作経路制限へhuman必須を追加しない。

通常の完全受入一覧は、指定ファイルに含まれない延期74 Caseも自動的に復元する。同名の現在Caseが変更されていても、固定23fの契約と全固定出典の各原契約を表示する。延期理由・担当・再開条件を保持し、合格表示にはgateと同じ人間証拠・実行経路・GOP検査を用いる。初期段階の「原Case未完了」表示や`initial_observation`を、完全受入の結果へ流用しない。

残試験を持つQAは[人間向け手順](human-qa.md)、固定mergeの全Case契約（前提・操作・期待・記録方法）、証拠、担当・再開条件へのリンクを持ちます。契約JSONを本文へ重複複写しません。元Issueの実際のsub-issueとしてMilestoneを継承し、双方向linkとProject表示を読み戻してから実装Issueをcloseします。Caseなしの変更は既存release追跡 #10 へmain反映を移管し、QAを新設しません。既存QAの整理は下記の終了条件で行い、自動削除しません。親・QA・Milestoneの完了を子PRから推測しません。

## 試験コストと完了の判断

[現在の製品責任](responsibility.md)を満たす最小の試験を計画します。安全性・データ保全・エラー復旧・同期/映像精度・画質/負荷/容量・指定された人間/REAL確認は省略しません。Caseを管理する数と操作の回数は同じではありません。

- **候補を凍結する**: 既知の製品修正と必要な運用同期を先にまとめ、実行前に候補/source・成果物・実ロード・計画revisionを固定します。試験中に無関係な変更を足して候補を更新しません。停止中は新候補の作成・導入を開始しません。
- **影響で再試験を選ぶ**: 修正箇所から呼出し元/先・共有状態・保存形式・build依存・Caseを照合し、まず失敗箇所と影響する境界を診断します。未変更の自動検査を理由なく反復せず、影響不明は関連範囲へ広げます。診断の選択と正式promotionの全Case充足を混同しません。
- **観察を共有する**: 同一候補・成果物・ロードと前提が一致し、各Caseの全必要条件を満たす観察は、証拠を参照して複数Caseへ使えます。一つの操作で満たすCaseを先に対応づけ、同じ操作や全文記録を繰り返しません。候補変更後の旧観察は履歴として保持し、新候補のpassへ付け替えません。下記の同一成果物・条件不変を実証できる場合だけ、明示的な`reused`として正式gateへ渡せます。不明・不一致なら再試験します。過去結果の自動失効削除も行いません。
- **追加確認を限定する**: 元契約・既知の不具合・変更影響・未解決の懸念に結びつく確認だけを追加します。単に確認可能だからという理由で全scene、全時計、追加再接続などを必須化しません。新たな品質条件が必要なら理由と契約変更を先にレビューします。
- **GUIの目的を明記する**: GUI担当への依頼には、試験の目的、trustedな有効条件の実接続/表示に必要な箇所、必要条件を満たす最小の実操作・観察、ファイル/自動解析で担う確認、利用する既存証拠、終了/中止条件を既存計画から短く示します。担当と独立レビュアーは、各操作が保証目的・必要な証拠/精度や未解決の懸念へ対応するか確認します。毎操作の承認や別台帳は追加しません。
- **GUI手段の必要性を分ける**: [手段の選択基準](responsibility.md#gui手段の必要性を先に評価する)に従い、検証目的が必要という理由だけで手動GUIを指定しません。既存証拠・API/IPC/CLI・実画像で同じ観測事実を得られる経路を先に評価し、具体的な未確認UI/OS/consumer保証へ最小のGUIを選びます。担当とレビュアーが選定理由を確認し、command成功だけを表示成功にしません。
- **consumerごとに共有する**: [動画とテキストの証拠評価](responsibility.md#動画とテキストのconsumer証拠を別々に評価する)に従い、playerとeditorの変更条件・残る自製品欠陥を割当と独立レビューで個別に確認します。新しい動画/本文値だけで両方のnative操作を追加せず、適格な既存実観察を保証ごとに共有します。全出力のFILE検査と実通知/open/copy等の製品glueは維持します。
- **内容照合と実操作を分ける**: 全行・UUID・時刻対応・hash・packet/画素等の正しさは妥当な既存解析へ集約し、標準editorで実際に開いて読めること、場面と両ログを辿れること、playerの代表open/短いPlay/前後seek・表示や実IDEの保存/復旧は目的に必要な実観察で確認します。現行groupで必要な代表実操作/表示を維持します。旧原文のGUI指定だけで全件照合をGUIへ戻しません。全件照合の母数を減らさず、母数だけを理由に全件GUI検索を追加しません。ソース読解やdecode結果を未実施のnative操作へ転記せず、合成fixtureに指定された故障を根拠なく実端末でも重複必須にしません。人間の操作確認という目的だけでactorをhuman必須へ変更せず、REAL固有/利用者保留と製品の精度条件を保持し、時間/回数/同runは保証に必要な理由で選びます。

| 完了の対象 | 終了条件と残条件の置き場所 |
|---|---|
| 実装Issue | コード・必要検査・独立レビュー・develop統合が完了し、残試験/反映を双方向移管してreadback |
| 個別QA | 担当する全試験の実証、または同じ未達を既存QAへ重複なく明示移管してreadback。main反映だけは #10 へ移管して管理終了可。移管終了は試験passではない |
| 初期版Agent段階 | 固定計画の全Agent条件を同じ候補で実観察。列挙済み延期は #65 に保全。個別QA完了から推測しない |
| main反映 | #10 で全候補commit/Case、固定build、promotion PRと実main履歴を照合。Agent段階とfull GUIの状態を別表示 |
| 完全GUI受入 | 人間/REALを含む元の全条件を満たす。初期版/main反映で自動完了しない |

既存QAを整理する際は、元の契約・本文/コメント・候補別証拠・failと修正依存・native関係を履歴に残します。移管先、担当、未達と再開条件を両端へ記録し、Projectも読み戻してから閉じます。旧passの付替え、製品不具合の管理上の解消、残試験を残した無根拠なcloseは禁止です。本文冒頭は現状と正本リンク、過去の進捗は履歴と区別します。

表示用一覧はJSONから生成します。

```bash
python3 scripts/workflow/verification.py docs/verification/changes/issue-12.json --output docs/verification/current.md
```

## 同一成果物の観察再利用（#161）

通常の結果は引き続き同一candidate/hashの実観察を要求します。#182による明示的な責任改訂の再評価は[限定手順](responsibility.md#trusted-main改訂と旧観察の再評価)へ分離し、以下の通常reuse条件を緩めません。文書・管理ルールのみの更新で候補SHAが進んだ場合、元の成功観察をそのまま保持する`reused`を例外として使えます。実際の再利用は製品・GUI試験の再開後、以下を確認してから行います。本ルールの導入やJSON構造検証だけで既存Caseを合格へ変えません。

- source candidateは現在candidateの祖先で、対象artifact SHA-256が一致すること。差分は`docs/`、`scripts/workflow/`、`.agents/skills/`、`.claude/skills/`、`.cursor/rules/`とrootの`AGENTS.md`・`CLAUDE.md`・`README.md`・`CONTRIBUTING.md`だけに限定します。途中でrevertされた変更も検査します。製品/runtime・build・設定・fixture・未知のpath、異なる成果物は再試験です。製品が生成するREADMEの実装はこの文書例外に含みません。
- #210はPR207の管理Python更新だけを固定登録します。trusted mainが実統合280c96cを含み、その親836da53と3 regular CI blobの前後を照合します。適用は観察source707を唯一の親とする最終squash候補に限定し、3 CIすべてのexact内容を要求します。ci.ymlは707の既存Java/Gradle/Plugin検査を保持したPython指定だけの固定blobへ照合します。同期branchや任意の別履歴、途中revert、未知CI、検査経路削除、不完全な証拠は拒否し、過去のmain履歴を除外する例外は設けません。実統合PR/全commitの出典は既存promotion gateで照合します。この内容照合だけではprotected merge・同artifact/ロード/環境・Case PASSを証明せず、以下の全条件と独立レビューが必要です。
- 元Caseが両candidateに存在し、契約が同じで、そのCase JSONの変更履歴がないこと。初期計画の該当entry（全出典・Agent条件・延期・人間指定）とhuman_scope・followup・responsibility_revisionも同じこと。GOPのrevision/6証拠、指定の実施経路、完全受入のhuman/human_evidenceは元観察に必要です。追加・変更Caseを旧結果で満たせません。
- 現在のロード実体、環境、全前提・試験条件・不具合履歴への適合を改めて確認し、`confirmation`に実確認者・日時・公開可能な証拠を記録すること。`equivalence_evidence`で同一artifactと製品/runtime/build入力不変、`environment_evidence`で環境・全前提・条件の継続を示します。単なる「同じはず」や不明な依存は不成立です。元観察又は現在candidate commitの日時より前の確認は拒否します。
- 元の`pass`または`initial-pass`だけを採用します。pending/blocked/fail/deferredや再利用の連鎖は拒否します。元candidate・日時・実施者・証拠を改変せず、独立レビューで原記録と同等性の証拠内容を確認します。文字列の存在検査は内容の実証を代替しません。確認は現在candidateでの製品再試験を意味しません。

各Caseのresultを次の形式にします（山括弧は実値へ置換）。`source_record`に、trusted main baseの祖先である元promotion記録の40桁commit SHAを指定します。その固定commitの`docs/verification/promotion.json`から元candidate/Case resultを読み、`source_result`全体との完全一致を検証します。過去に正式記録のない成功を新規作成してreuseすることはできません。raw/private証拠をGitへ載せる指示ではなく、元の正式結果は公開可能な要約/証拠参照だけを保持します。`source_result`には元の結果JSON全体を保存します。初期段階の場合も旧`stage_revision`を保持し、現在計画の必要Case・34 REAL延期・後続#65は元通り評価します。既存の延期値をreuseで包みません。

```json
{
  "status": "reused",
  "head": "<現在candidateの40桁SHA>",
  "artifact_sha256": "<同一成果物の64桁SHA-256>",
  "source_candidate": "<元candidateの40桁SHA>",
  "source_record": "<trusted mainの元promotion記録commitの40桁SHA>",
  "source_result": {"元結果JSON全体": "改変せず保存"},
  "confirmation": {
    "status": "pass",
    "head": "<現在candidateの40桁SHA>",
    "artifact_sha256": "<同一成果物の64桁SHA-256>",
    "actor": "gpt",
    "observer": "<実確認者>",
    "at": "<実確認日時 ISO8601>",
    "loaded_identity": "<現在の実ロード確認>",
    "reason": "<再利用を適用できる理由>",
    "evidence": "<現在の適合確認への参照>",
    "equivalence_evidence": "<成果物・製品依存・build入力不変の証拠>",
    "environment_evidence": "<環境・全前提・試験条件・不具合履歴確認の証拠>"
  }
}
```

Gateと一覧は共通の検証を使います。一覧には再利用可否、元candidate/観察、現在の適合確認を区別して表示します。全候補・全Caseのgate、独立レビュー、必要CIは引き続き必須です。再利用可能でも未解決の不具合・失敗checkを解消済みにしません。
