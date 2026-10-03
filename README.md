# Android Replay Buffer

Android操作直前の画面・logcat・timelineを保存し、不具合の再現情報を取り逃さないための **Android Studioプラグイン** を開発しています。

現在はプラグインのビルド・ロード確認用メニューまで用意した段階です。録画・保存を利用するには、保全しているmacOS向けPython CLIを使用してください。プラグインからCLIへの接続はまだありません。

## 使う・開発する

- **CLIを使う**: [オリジナルのセットアップ・コマンド説明](docs/cli-origin/README.md)。`bin/replayd` / `bin/replay`、設定ファイル、launchdの配置は従来どおりです。
- **プラグインを開発する**: [SDK/JDK・ビルド・IDE起動・次の実装順](docs/plugin-development.md)。Android Studioで `plugin/` をGradleプロジェクトとして開きます。
- **原型を復元する**: [CLI原型の保全方針と固定コミット](docs/cli-origin.md)。原型の全21ファイルをhashと実行権限で照合します。

```text
plugin/          Kotlin / IntelliJ Platformの独立Gradleプロジェクト
replay_buffer/   保全するCLI原型（Python 3.9+）
bin/             従来のCLI起動スクリプト
launchd/         従来のmacOS常駐設定例
tests/           CLI原型のテスト
docs/cli-origin/ 原README・原gitignore・固定ファイルmanifest
scripts/         共通の管理・検証（Python 3.11+）
```

CLIを `legacy/` へ移動しないのは、既存のPATH、Python import、launchd設定を維持するためです。プラグイン開発は `plugin/` で進め、録画エンジンとの接続方式を決めるまではCLI原型を変更しません。

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

CLI検証は原型の保全と既存2 tests、プラグイン検証はコンパイル・標準ZIP生成・構造検査です。IDEロード・録画・実機動作の合格を意味しません。[受入Case](docs/verification/changes/issue-3.json)で未実施項目を確認できます。

[開発マップ](https://github.com/users/shinma06/projects/5) / [Issue](https://github.com/shinma06/android-replay-buffer/issues) / [共通IDE知見](docs/android-studio.md)
