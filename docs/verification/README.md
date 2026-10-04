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

`verification.py --promotion ...`による一覧も同じresolverを使い、固定出典と元の前提、適用した前提・追加手順・改訂revisionを表示する。渡すCase JSONは元の固定merge契約と一致させ、候補側の書換えで代用しない。改訂だけではCase集合を減らせず、一覧のCase単位表示も全候補のpromotion gateを代替しない。

## 引継ぎ

全open QAは[人間向け手順](human-qa.md)への本文リンク、全Case、前提・操作・期待・記録方法・担当・再開条件、main反映追跡を持ちます。元Issueの実際のsub-issueとしてMilestoneを継承し、双方向linkとProject QA表示を読み戻してから実装Issueをcloseします。親・QA・Milestoneの完了を子PRから推測しません。

表示用一覧はJSONから生成します。

```bash
python3 scripts/workflow/verification.py docs/verification/changes/issue-12.json --output docs/verification/current.md
```
