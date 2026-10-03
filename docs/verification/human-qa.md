# 人間向けQAの進め方

[QA一覧](https://github.com/users/shinma06/projects/5)から対象Issueを開き、元Issue/PR、Case JSON、候補SHA・成果物hash・担当・未実施項目を確認します。Caseの前提や期待が不足する場合は観察を始めず、担当が補います。

1. GUI leaseと他のIDE/端末/daemon利用者を確認し、専用fixtureと対象端末を用意する。既存ログ/録画を上書きしない。
2. 実際に動くCLI/Pluginの候補SHA・版・成果物hashを記録する。未実装のPluginを検証済みとしない。
3. Caseごとの前提→操作→期待を順に実行する。録画とlogcatに認証情報や個人情報が入らないfixtureを使う。
4. pass/pending/blocked/fail、観察者、timezone付き時刻、実観察、証拠の保存場所をCase JSONへ記録する。生ログや動画は公開せず必要な範囲に限定する。
5. 製品failは専用修正Issueへ引き継ぎ、新候補で再確認する。自分のprocessを停止して予約を解放する。

GUI passとmain反映は別です。同じbuildの全必要Caseとpromotion結果を確認してからQAを閉じ、Project・Milestone・関係・残資源を読み戻します。
