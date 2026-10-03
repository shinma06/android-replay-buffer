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

## 引継ぎ

全open QAは[人間向け手順](human-qa.md)への本文リンク、全Case、前提・操作・期待・記録方法・担当・再開条件、main反映追跡を持ちます。元Issueの実際のsub-issueとしてMilestoneを継承し、双方向linkとProject QA表示を読み戻してから実装Issueをcloseします。親・QA・Milestoneの完了を子PRから推測しません。

表示用一覧はJSONから生成します。

```bash
python3 scripts/workflow/verification.py docs/verification/changes/issue-12.json --output docs/verification/current.md
```
