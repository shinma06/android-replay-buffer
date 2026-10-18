# 開発への参加

[プロジェクト情報](docs/project.md)・[開発手順](docs/workflow.md)を読み、実在するIssueに紐づく専用branch/worktreeで作業します。

```bash
python3 scripts/bootstrap.py
python3 scripts/check.py
# 製品検査はJDK 27 / ANDROID_HOME（SDK API 37.2 revision 1）を設定して実施
python3 scripts/workflow/product_check.py
```

PRには[テンプレート](.github/pull_request_template.md)のmetadata・Case JSON・実際の検証結果を記載し、[作業管理](docs/work-management.md)でProject/Milestone/実関係を確認します。Android Studioの実装は[製品要件](docs/requirements.md)と[有効な製品責任](docs/verification/responsibility.md)を基準にし、[外部知見](docs/android-studio.md)は必要な場合に任意で利用します。

プラグインの開始手順は [plugin開発](docs/plugin-development.md)、CLIの操作・設定・形式は [移行契約](docs/cli.md) を参照してください。
