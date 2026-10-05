# 受入・固定候補・QA

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

## main promotion

1. main先行toolingをdevelopへ専用同期PRで取り込み、候補SHAを固定する。
2. mainにない候補の全commitをmerge済みdevelop PRへ一意に対応させ、各固定merge SHAのCase JSONを読む。
3. 同一候補・同一成果物の全Caseを確認し、promotion.jsonへ`schema: 1`、`base`（現在main SHA）、`candidate`、`changes`（commitとpr）、`artifact_sha256`、`results`を保存する。
4. resultsのキーは`Issue番号:Case ID`。passは`status / actor / observer / at / head / artifact_sha256 / evidence / loaded_identity / reason`を持つ。日時はtimezone付きISO8601、head/hashは候補と一致。loaded_identityに実際のCLI/Pluginのロード識別を書く。
5. 候補後に変更できるのはpromotion.jsonとpromotion IssueのCase JSONのみ。全範囲のgate・CI・独立レビューを通し、merge commitでmainへ統合する。

Plugin ZIPは [開発手順](../plugin-development.md) の標準buildPluginで生成します。cleanな固定source SHAをversionに含め、ZIPのSHA-256と実ロードversionを照合します。構造検査の成功だけでIDE受入やAPI互換性をpassにしません。main起点の限定変更も、trusted mainの`docs/verification/scopes/issue-N.json`による事前承認範囲と全Caseの観察が必要です。

### 実測GOPの境界試験（#56）

[GOP限定改訂](amendments/gop-boundaries.json)は、#13/#31/#41の`SYNC-WINDOW`と#27の`BUFFER-180`・`BUFFER-SHORT`・`SAVE-CONTINUE`（REAL/EMU）の9 Caseへ適用する。元PRの固定merge・全Case内容のSHA-256を照合し、#27はPR #33と#38の両出典を保持する。合計15出典の不足・追加・矛盾は拒否し、最後のPRを任意に採用しない。Case ID・artifact・実施方法・既存操作・期待結果・過去の状態は変更しない。

設計のI-frame間隔1秒はencoderへの試験開始値であり、実時間のGOPを保証しない。[Android仕様](https://developer.android.com/reference/android/media/MediaFormat#KEY_I_FRAME_INTERVAL)では、設定FPSからフレーム数へ換算するencoderの実間隔は実FPSによって変わり得る。指定値を実測値とせず、同じ固定ZIPの元PTS・隣接IDRから測定する。約1秒GOPは同一候補のJVM回帰として維持し、端末では実測した長いGOP・可変FPSを含め、IDR直後・中間・次IDR直前の切出しを追加確認する。JVM成功をIDE/端末の合格へ転記しない。実施方法と測定項目は改訂JSONの追加手順を正本とする。

最大限保存、切出し誤差≤1観測frame間隔＋時計誤差、mux/index丸め≤1ms、同期p95≤100ms・最大≤250ms・30分drift≤50ms・clock≤20ms、画質/負荷・容量/pin制限は維持する。欠落・時計不明・未確認末尾保持を明示し、正式300イベント/30分と必要な人間確認は別の必須Caseで実施する。REAL保留をEMU結果で解消しない。

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

利用者の方針に従い、初期版はエージェントが実行できる工程を完了し、人間の操作・確認と実機準備が必要な部分を[後続QA #65](https://github.com/shinma06/android-replay-buffer/issues/65)へ延期する。[固定計画](amendments/initial-agent.json)をtrusted mainの通常tooling PRでレビューし、developへ同期した後の新候補だけに適用する。候補後の変更許可、main保護、独立レビュー、全commitの出典検査は変えない。

計画は既存126 Case・192出典のentryとPR/merge/path/Case全文hashを変更せず、時計支持範囲修正 #78 / PR80 の1 Case・1出典を追加した127 Case・193出典を保持する。`required_ancestor`は修正の実統合SHAを指し、元124 Caseの現在契約hashは固定23fから不変である。93 CaseのAgent範囲は必須で、34 REAL Caseは対応するEMU Caseの観察を残して延期する。混在Caseでは計画に列挙した部分だけを分ける。各sourceの履歴を消したり、古い緩い条件を選択したりしない。PR38の専用transport限定、共有ADB保護、同boot確認、画面OFFとdeep sleepの区別も維持する。新Caseや契約変更が加わった場合は計画の正式更新が必要である。

元のCase JSONとpending/blocked/failは書き換えない。`human: pending`だけでは人間工程を追加しない。標準player、GUI/可読性、全300イベント/30分の解析、同期・切出し精度、画質/負荷/容量/pin、失敗・復旧のAgent実行可能部分は維持する。最低601画像の人間による番号確認は初期段階から分離し、その前の解析を原Case全体の正式精度passとは呼ばない。製品failや単に難しい・時間がかかる工程は延期理由にしない。

`3:PLUGIN-LOAD`は現在の操作経路で再起動後のSettings/About別windowを読めなかったため、計画に記載した再読取りとdialog終了確認だけを延期する。新候補のnative導入、初回UI、通常再起動、実ロード識別、ZIPと全JARの一致、actionの存在、取得OFFは実観察する。実体照合を延期した画面確認のpassに置き換えない。別の工程へ自動的に例外を拡張しない。

`78:CLOCK-RECEIPT-NORMAL`は同ZIP製品classの限定JVMと実IDEの正常3event・全9phase行の確認を両方必須とし、延期やhuman必須を追加しない。3event確認を既存正式300event/30分・同期精度・GOP Caseの代替にせず、旧候補の結果を新buildへ転用しない。

promotionには`stage: initial-agent`を明記し、`results`に全127 keyを置く。各値は以下の形とし、`stage_revision`には計画全体のcanonical SHA-256を入れる。

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

全open QAは[人間向け手順](human-qa.md)への本文リンク、全Case、前提・操作・期待・記録方法・担当・再開条件、main反映追跡を持ちます。元Issueの実際のsub-issueとしてMilestoneを継承し、双方向linkとProject QA表示を読み戻してから実装Issueをcloseします。親・QA・Milestoneの完了を子PRから推測しません。

表示用一覧はJSONから生成します。

```bash
python3 scripts/workflow/verification.py docs/verification/changes/issue-12.json --output docs/verification/current.md
```
