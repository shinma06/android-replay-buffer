# Android Replay Buffer

Android操作直前の画面とlogcatを保存し、IT（テスト）のエビデンス採取と、不意に発生したバグの調査を支援する **Android Studioプラグイン** を開発しています。

初期版候補として、ToolWindowからの取得・状態表示・直前区間の保存、常設設定、動画と両ログの取得コアを実装しています。現在は受入前の開発版です。固定ZIPでのIDE・Emulator・実機受入の結果は[初期版QA](https://github.com/shinma06/android-replay-buffer/issues/27)で管理し、実機受入は利用者の端末準備まで保留しています。既存のmacOS向けPython CLIも保全しています。

## 使う・開発する

- **製品要件を確認する**: [初期版の確定要件・開発順序・検討タスクとTODO](docs/requirements.md)。初期値180秒、1台での自動取得・1ボタン保存を目指します。macOSを最優先とし、IDE内の初期版完成直後にメニューバー連携へ着手します。
- **初期版候補を確認する**: [導入・設定・取得・保存・再試行の手順](docs/plugin-development.md#初期版候補の利用手順)。受入対象の固定ZIPを使い、開発版の制約と未検証項目を確認してください。
- **CLIを使う**: [オリジナルのセットアップ・コマンド説明](docs/cli-origin/README.md)。`bin/replayd` / `bin/replay`、設定ファイル、launchdの配置は従来どおりです。
- **プラグインを開発する**: [SDK/JDK・ビルド・IDE起動・次の実装順](docs/plugin-development.md)。Android Studioで `plugin/` をGradleプロジェクトとして開きます。
- **原型を復元する**: [CLI原型の保全方針と固定コミット](docs/cli-origin.md)。原型の全21ファイルをhashと実行権限で照合します。

```text
plugin/          Kotlin / IntelliJ Platformの独立Gradleプロジェクト
replay_buffer/   保全するCLI原型（Python 3.9+）
bin/             従来のCLI起動スクリプト
launchd/         従来のmacOS常駐設定例
tests/           CLI原型のテスト
docs/cli-origin/ 原README・CLI設計書・原gitignore・固定ファイルmanifest
scripts/         共通の管理・検証（Python 3.11+）
```

CLIを `legacy/` へ移動しないのは、既存のPATH、Python import、launchd設定を維持するためです。プラグインは `plugin/` で開発し、projectのAndroid SDKと同梱する録画用依存を使う[方式を採用](docs/design/timeline.md)しています。CLI原型のコード・設定・コマンドの互換性は保全します。

## 開発と検証

[プロジェクト情報](docs/project.md)と[開発手順](docs/workflow.md)に従い、Issue専用branch/worktreeからdevelopへPRを作成します。mainへは固定候補の受入後に反映します。

```bash
python3 scripts/bootstrap.py
python3 scripts/check.py
python3 scripts/workflow/product_check.py
# JAVA_HOMEにJDK 25を指定してから実行
python3 scripts/workflow/plugin_check.py
# cleanなcommit済みHEADで、差分に必要な確認をまとめて実行
python3 scripts/workflow/change_impact.py --base origin/develop --run-tests
```

CLI検証は原型の保全と既存2 tests、プラグイン検証はJVMテスト・コンパイル・標準ZIP生成・構造検査です。IDEロード・録画・実機動作の合格を意味しません。[初期版の受入Case](docs/verification/changes/issue-27.json)で実機とEmulatorの状態を分けて確認できます。

[開発マップ](https://github.com/users/shinma06/projects/5) / [Issue](https://github.com/shinma06/android-replay-buffer/issues) / [共通IDE知見](docs/android-studio.md)
