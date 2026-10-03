# コンテキストの置き方

| 内容 | 正本・形式 | 読込条件 |
| --- | --- | --- |
| 常時制約と入口 | AGENTS.md、英語 | セッション開始 |
| Claude入口 | CLAUDE.md → AGENTS.md | Claude読込。独立規約を作らない |
| 作業手順 | `.agents/skills/*/SKILL.md`、英語 | 該当作業時。Claude経路はsymlink |
| Cursor入口 | `.cursor/rules/harness.mdc` | projectで常時適用 |
| プロジェクト事実 | project.md、共有docs、日本語 | 判断に必要な範囲 |
| 作業の進行・受入 | Issue/PR | 開始、再開、レビュー、終了 |
| 証拠 | SHA/版/観察者付きの記録 | 該当Case |
| 個人設定・認証・一時状態 | Git管理外 | 初期化・復旧 |

セッション経緯は当該Issueへ残します。文章だけで内部推論保存や自動compactionを保証しません。ファイルの存在、実読込、実行成功は別です。リンク検査は存在だけ、意味・優先順位・client検出はレビューとsmoke testで確認します。

制約を移す際は適用条件、承認範囲、所有権、完了条件、互換性、原証拠を保ちます。個人の専門領域・モデル固有制限は全利用者へ自動適用せず、必要な導入先だけで設定します。

## 文書の配置と命名

開発文書は `docs/` に置き、ファイル名は英語の小文字、複数語はハイフン区切りを基本とします。`README.md`・`AGENTS.md`・`CONTRIBUTING.md` などの定番名とclientが指定する名前は維持します。本文は日本語です。

凍結したCLI原型の文書は `docs/cli-origin/` に置きます。[CLI原型の設計（凍結）](cli-origin/design.md)は原文を保持し、参照名で過去の設計と明示します。現在のプラグイン設計は、実際に設計を書く段階で `docs/plugin-design.md` に作成します。[プラグイン開発手順](plugin-development.md)とは役割を分け、空の設計書は先に作りません。
