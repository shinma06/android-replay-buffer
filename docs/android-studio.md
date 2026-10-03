# Android Studioプラグイン開発で共有する知見

開発対象はAndroid Studioプラグインです。[Kotlin/Gradle基盤](plugin-development.md)とロード確認用の情報表示actionを用意しています。録画・保存は保全したPython CLIで利用し、IDEとの接続は未実装です。以下の共通知見を接続設計から適用します。

参照元は [Cursor in Android Studio](https://github.com/shinma06/cursor-in-android-studio)。管理実装は [inventory](inventory.md) の固定版で比較しています。以下は継続的な調査入口で、参照先の最新branch/Issueを確認してから採用します。

| 共通知見 | このプロジェクトで確認すること | 参照入口 |
|---|---|---|
| IntelliJ Platform / Android Studio互換性 | 実際の対象IDE、JBR、Kotlin/Gradle targetを整合させる。別プラグインの固定版を無条件で採用しない | [build設定](https://github.com/shinma06/cursor-in-android-studio/blob/main/build.gradle.kts) |
| UIと非同期処理 | IDE API/EDT境界、長時間処理をbackgroundへ、dispose後の結果破棄、取消しと子processの終了を分ける | [現行source](https://github.com/shinma06/cursor-in-android-studio/tree/main/src) |
| 既存CLIとの接続 | 既存の構造化IPC・エラー・process所有を調査して再利用。ADB・scrcpy・ffmpegの実態をCLI側へ保つ | [このCLIの実装](../replay_buffer)、[CLI原型の設計（凍結）](cli-origin/design.md) |
| 設定・保存互換 | Plugin ID、保存キー、設定ファイル、出力形式を明示。参照元の名前/IDは転用しない | [アーキテクチャ](https://github.com/shinma06/cursor-in-android-studio/tree/develop/docs/architecture) |
| 日本語UI | 判断に必要な説明・エラーは自然な日本語。CLI commandやmodel/SDK ID等は翻訳しない | [開発指示](https://github.com/shinma06/cursor-in-android-studio/blob/develop/CLAUDE.md) |
| ZIPと検証buildの同一性 | source SHA・artifact SHA-256・ロードされたPluginを照合。同一ZIPを受入から配布へ使う | [ZIP手順](https://github.com/shinma06/cursor-in-android-studio/blob/develop/docs/development/plugin-zip-delivery.md) |
| IDE/端末の共有状態 | host-wide lease、専用fixture、Stop/再起動/cleanup、他プロジェクトのdaemonや端末を止めない | [GUI調整](https://github.com/shinma06/cursor-in-android-studio/blob/develop/docs/development/gui-coordination.md)、[本プロジェクトの運用](operations.md) |

Cursor固有のACP/print、セッションID、権限モード、Agent panel要件、モデル名、過去のGUI合格は移植しません。コードを再利用する場合は利用条件・呼出し・必要な互換性を確認し、採用理由と検証を当該Issue/PRに記録します。

## 作業に組み込む運用

Cursor in Android Studioを、同じIDE基盤の先行実装として活用します。IDE連携、UI、非同期処理/process、保存、SDK/build/配布、GUI検証、共通ハーネスを設計・変更するときに適用します。CLI固有の小さな修正や表記だけの変更では、関係のない参照元全体を読み直しません。

1. **開始時**: 上の参照表から該当領域を選び、参照元の現在のmain/develop、関連Issue/PR、実装とテストを必要な範囲で確認する。main採用済み、developのみ、設計のみ、実受入未完了を区別し、確認したcommit SHAの固定リンクを当該Issueへ残す。
2. **設計・実装時**: 解決済みの不具合、API境界、検証手順、既存の共通ツールを先に調べ、こちらのCLI/IDE要件に適合する部分を再利用する。採用・適合変更・非適用の判断と理由をIssue/PRに簡潔に記録する。前提が同じなら再発明せず、異なるなら差分を明示する。
3. **検証・レビュー時**: 参照元の再現条件・回帰Caseをこちらの製品境界へ適合し、自分のsource/buildで実行する。レビュアーは採用根拠・前提差・残受入を確認する。参照元のpassや完了Issueをこちらの合格へ転記しない。
4. **知見の更新時**: 長く使える判断だけを既存の正本文書へ追記し、作業経緯はIssue/PRへ置く。SDK更新、共通機構の変更、関連する新不具合で参照元を再確認する。無関係な作業ごとの全件調査や自動同期は行わない。

記録は既存Issue/PRの「Cursor知見の活用」欄へ、`参照元の固定SHA/Issue・PR → 採用/適合/非適用と理由 → こちらの検証Case/結果`を残します。適用外なら理由を一言で記し、新しい台帳や分類用の親Issueは作りません。新しい汎用的な問題を見つけた場合は、こちらのIssueに参照元への還元候補として残し、別repositoryの変更・担当への連絡はその対象の承認と所有手順に従います。

例えばPluginのprocess停止処理を追加するならCursor側の取消し/dispose/子process終了の実装と回帰試験を確認します。SDK更新なら互換表・実build・ロード識別の手順を確認します。CLIのlogcat文字列解析だけの修正なら、IDE実装との共通点がなければ適用外とし、そのCLIの呼出しとテストに集中します。
