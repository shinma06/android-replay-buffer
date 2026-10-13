# 開発への参加

[プロジェクト情報](docs/project.md)・[開発手順](docs/workflow.md)を読み、実在するIssueに紐づく専用branch/worktreeで作業します。

```bash
python3 scripts/bootstrap.py
python3 scripts/check.py
python3 scripts/workflow/product_check.py
# plugin変更はJDK 25を設定して実施
python3 scripts/workflow/plugin_check.py
```

PRには[テンプレート](.github/pull_request_template.md)のmetadata・Case JSON・実際の検証結果を記載し、[作業管理](docs/work-management.md)でProject/Milestone/実関係を確認します。Android Studioの実装は[共通知見](docs/android-studio.md)を入口にしてください。

プラグインの開始手順は [plugin開発](docs/plugin-development.md)、CLIの変更制約は [原型保全](docs/cli-origin.md) を参照してください。
