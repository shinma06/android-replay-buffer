# Android Studio開発の仕様と外部知見

開発・レビューは、このリポジトリの [製品要件](requirements.md)、設計、実装とテストを基準にします。検証と統合には [検証方針](verification/README.md) を使います。

| 対象 | 確認することと正本 |
|---|---|
| IDE・build互換性 | IDE/JBR、Kotlin/Gradle targetと依存を [開発手順](plugin-development.md) に合わせる。build成功と実IDEの動作は区別する |
| UI・非同期処理・process | [IDE UI設計](design/ide-ui.md) に従い、EDT境界、background処理、dispose後の結果破棄、取消しと所有processの停止、設定の一括反映を保つ |
| 取得・保存・互換性 | [保存設計](plugin-design.md)・[CLI契約](cli.md) に従い、対象・時刻・内容、欠落、不明範囲、容量、失敗時の保全、非上書き、保存キーと出力形式を保つ |
| 日本語UI・accessibility | 判断できる日本語の説明、可読性、keyboard操作を保つ。commandやSDK IDは翻訳しない |
| 実行環境 | 専用profile・fixtureを使い、他の作業のIDE・ADB・daemon・端末を停止したり変更したりしない |

公式仕様や先行実装は、必要な判断を補うときに参照します。[Cursor in Android Studio](https://github.com/shinma06/cursor-in-android-studio) も任意の参考先であり、巡回・同期・非適用理由の記入は不要です。採用した根拠は関連する設計やPRに記録し、他製品の動作確認をARBの結果に転記しません。過去の採用根拠は [IDE UI設計](design/ide-ui.md#cursor知見の採用範囲)、旧運用の記録は [保管先](legacy-ops/README.md) から参照できます。
