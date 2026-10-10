# 人間向けQAの進め方

[QA一覧](https://github.com/users/shinma06/projects/5)から対象Issueを開き、元Issue/PR、Case JSON、候補SHA・成果物hash・担当・未実施項目を確認します。Caseの前提や期待が不足する場合は観察を始めず、担当が補います。

1. GUI leaseと他のIDE/端末/daemon利用者を確認し、専用fixtureと対象端末を用意する。既存ログ/録画を上書きしない。
2. 実際に動くCLI/Pluginの候補SHA・版・成果物hashを記録する。未実装のPluginを検証済みとしない。
3. Caseごとの前提→操作→期待を順に実行する。録画とlogcatに認証情報や個人情報が入らないfixtureを使う。
4. pass/pending/blocked/fail、観察者、timezone付き時刻、実観察、証拠の保存場所をCase JSONへ記録する。生ログや動画は公開せず必要な範囲に限定する。
5. 製品failは専用修正Issueへ引き継ぎ、新候補で再確認する。自分のprocessを停止して予約を解放する。

試験完了・初期版段階完了・main反映は別です。[完了と再試験の規則](README.md#試験コストと完了の判断)に従い、QAが担当する残試験を完了し、main反映だけが残る場合は既存release追跡 #10 への双方向移管を読み戻してQAを閉じます。Project・Milestone・関係・残資源も読み戻します。

[初期版の段階受入](README.md#初期版の段階受入64)では、人間・実機が必要な列挙済み部分を[後続QA #65](https://github.com/shinma06/android-replay-buffer/issues/65)へ未実施のまま残します。初期版のAgent範囲の合格を、元Case全体や人間のpassへ転記しません。human欄がpendingという理由だけで新たな必須確認も追加しません。利用者が後続へ着手するまで人間操作や実機準備を再依頼せず、実施可能なAgent確認を進めます。初期版の完了・main反映と、後続QAの完了・Milestoneは別々に確認します。
