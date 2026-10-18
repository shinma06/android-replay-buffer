# Android Studio開発の仕様と任意の外部知見

ARBの開発・レビュー・受入は、本repositoryの要件、有効な責任/受入契約、実装とテストを基準にします。Cursor in Android Studioを含む他repositoryの常時・条件付き参照は要求しません。参照しなかった理由や「非適用」の記入も不要です。この方針は通常の検証・独立レビューを経てmainへ統合された時点で有効となり、未統合のPRや草案だけで現行規約を置き換えません。

## このプロジェクトで守る仕様と保証

開発対象はAndroid Studioプラグインです。実装済み・受入済み・未確認を[プロジェクト情報](project.md)と対象Issue/QAの固定証拠から区別します。同梱CLIは同じプラグインを操作し、旧原型からの差異と復旧は[移行契約](cli.md)に従います。旧source・受入観察・失敗はGitと元の記録に保持します。

| 対象 | 保持する保証と正本 |
|---|---|
| IDE・build互換性 | 実際のIDE/JBR、Kotlin/Gradle targetと依存を[開発手順](plugin-development.md)で整合させる。別製品の固定版やbuild成功を本製品のIDE受入にしない |
| UI・非同期処理・process | [IDE UI設計](design/ide-ui.md)と実装/テストでAPI/EDT境界、background処理、dispose後の結果破棄、取消しと子processの実停止・所有、入力/設定のatomic反映を確認する |
| 取得・保存・互換性 | [製品要件](requirements.md)・[保存設計](plugin-design.md)・[CLI移行契約](cli.md)に従い、対象/時刻/内容、loss/unknown、容量/pin、失敗時保全・atomic非上書き、Plugin ID/保存キー/出力形式と文書化したCLI移行契約を維持する |
| 日本語UI・accessibility | [製品要件](requirements.md)と[IDE UI設計](design/ide-ui.md)に従い、判断できる日本語の説明/エラー、可読性・keyboard操作を保つ。commandやSDK ID等は翻訳しない |
| 検証・配布の同一性 | [受入](verification/README.md)・[製品責任](verification/responsibility.md)に従い、固定source/ZIP hash/実ロード、全有効Case、原観察・失敗、独立レビューと正式な証拠再利用条件を保つ。外部製品の合格を転記しない |
| IDE・端末・実行環境の共有状態 | [運用](operations.md)・[実行規約](execution-policy.md)に従い、共有host/user lease、所有fixture、停止/復旧/cleanup、実tool権限を守る。他projectのdaemon/端末/作業へ影響させない |

## 作業に組み込む運用

開始時は上の正本、変更する実装の呼出し元/先、テストと対象Issueの不足を確認します。外部の一般知見・公式仕様・先行実装は、その不足を補う場合に任意で利用できます。特定repositoryの巡回、SDK更新時のCursor再調査、全件調査や自動同期は義務にしません。API/互換性の判断に必要な一次情報の確認は引き続き行います。

外部知見を採用する場合だけ、既存Issue/PRへ `固定source/版・関連Issue/PR → 採用/適合理由と前提差 → ARBの検証/残条件` を残します。main採用済み、developのみ、設計のみ、未検証を区別し、利用条件と関連する実装・テストを確認します。他repositoryの製品制約・ID・認証・個人設定・実行状態や受入結果は移植しません。還元の候補はARBのIssueへ残せますが、他repositoryの変更・担当への連絡にはその対象の承認と所有手順が必要です。

レビューと受入ではARBの有効契約・固定証拠・実検証を確認します。外部参照がないことを指摘やgate失敗にしません。旧依頼文・template・引継ぎ・チェックリストにあるCursor参照義務は本方針へ読み替え、次の担当割当前に現行入口と整合します。人間の明示したscope、所有・権限・品質条件や有効なCaseをこの読み替えで免除しません。

## 過去の採用根拠を保持する

[導入inventory](inventory.md)、[IDE UIの固定sourceと採否](design/ide-ui.md#cursor知見の採用範囲)、[初期版受入の知見適合](verification/initial-version.md#cursor知見の適合)、元Issue/PRは当時の判断・原観察/失敗の履歴として保持します。参照義務の終了は採用済み保証の削除、過去の適用確認の未実施化、旧FAILの解消や新たなPASSを意味しません。

[Cursor in Android Studio](https://github.com/shinma06/cursor-in-android-studio)も任意の参考先の一つです。共有GUI leaseの既存namespace、Cursorクライアント用の入口、旧branch形式の読取互換性は別の実運用契約であり、本変更では維持します。
