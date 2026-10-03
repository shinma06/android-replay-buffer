# Android Studioプラグイン開発で共有する知見

現時点ではPython CLIが製品です。今回のハーネス導入はプラグインの完成やSDK採用を意味しません。CLIの動作を維持し、IDE側とCLI側の責務を設計する作業から開始します。

参照元は [Cursor in Android Studio](https://github.com/shinma06/cursor-in-android-studio)。管理実装は [inventory](inventory.md) の固定版で比較しています。以下は継続的な調査入口で、参照先の最新branch/Issueを確認してから採用します。

| 共通知見 | このプロジェクトで確認すること | 参照入口 |
|---|---|---|
| IntelliJ Platform / Android Studio互換性 | 実際の対象IDE、JBR、Kotlin/Gradle targetを整合させる。別プラグインの固定版を無条件で採用しない | [build設定](https://github.com/shinma06/cursor-in-android-studio/blob/main/build.gradle.kts) |
| UIと非同期処理 | IDE API/EDT境界、長時間処理をbackgroundへ、dispose後の結果破棄、取消しと子processの終了を分ける | [現行source](https://github.com/shinma06/cursor-in-android-studio/tree/main/src) |
| 既存CLIとの接続 | 既存の構造化IPC・エラー・process所有を調査して再利用。ADB・scrcpy・ffmpegの実態をCLI側へ保つ | [このCLIの実装](../replay_buffer)、[設計書](../設計書.md) |
| 設定・保存互換 | Plugin ID、保存キー、設定ファイル、出力形式を明示。参照元の名前/IDは転用しない | [アーキテクチャ](https://github.com/shinma06/cursor-in-android-studio/tree/develop/docs/architecture) |
| 日本語UI | 判断に必要な説明・エラーは自然な日本語。CLI commandやmodel/SDK ID等は翻訳しない | [開発指示](https://github.com/shinma06/cursor-in-android-studio/blob/develop/CLAUDE.md) |
| ZIPと検証buildの同一性 | source SHA・artifact SHA-256・ロードされたPluginを照合。同一ZIPを受入から配布へ使う | [ZIP手順](https://github.com/shinma06/cursor-in-android-studio/blob/develop/docs/development/plugin-zip-delivery.md) |
| IDE/端末の共有状態 | host-wide lease、専用fixture、Stop/再起動/cleanup、他プロジェクトのdaemonや端末を止めない | [GUI調整](https://github.com/shinma06/cursor-in-android-studio/blob/develop/docs/development/gui-coordination.md)、[本プロジェクトの運用](operations.md) |

Cursor固有のACP/print、セッションID、権限モード、Agent panel要件、モデル名、過去のGUI合格は移植しません。コードを再利用する場合は利用条件・呼出し・必要な互換性を確認し、採用理由と検証を当該Issue/PRに記録します。
