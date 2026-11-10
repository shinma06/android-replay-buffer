# 旧運用の保管と戻し方

2026-11-07に、Codex・Claude・Cursorの複数エージェント運用（Omil、受入ゲート、Project管理）から、Devin × Entireの運用へ切り替えました。旧運用のファイルはタグに残しています。

| タグ | 内容 |
|---|---|
| `legacy-ops-develop` | 切り替え直前の`develop` |
| `legacy-ops-main` | 切り替え直前の`main` |

## 戻し方

1. 切り替えPRをrevertする。
2. GitHubのrulesetを、このフォルダの`develop-ruleset.json`と`main-ruleset.json`の内容に戻す（必須チェック: `test`、`PR policy`、`Agent review`、`Acceptance gate`）。
3. Devinのblueprintのmaintenanceは、`scripts/bootstrap.py`があれば自動で実行します（旧`.githooks`の設定）。
