# 旧運用の保管先

2026-11-07に複数エージェント運用からDevin × Entireへ切り替えました。旧運用（Omil、PM、受入ゲート、lease、handoff等）の文書・台帳・rulesetは履歴であり、現在の作業規約ではありません。現在は [AGENTS.md](../../AGENTS.md) と [検証方針](../verification/README.md) を使います。

## 切り替え時の原本

| タグと固定commit | 内容 |
|---|---|
| [legacy-ops-develop](https://github.com/shinma06/android-replay-buffer/tree/2e576ce3c724dc95ccf0688b8a1bf7cd92d774a8) | 切り替え直前のdevelop。旧operations、実行規約、検証台帳、fixture、スクリプト等 |
| [legacy-ops-main](https://github.com/shinma06/android-replay-buffer/tree/fb3c417a27079a503ab28db380b4042dec994a3f) | 切り替え直前のmain。developにだけあった追加記録は含まない |

このフォルダの `develop-ruleset.json` と `main-ruleset.json` も当時の設定の控えです。現在の必須checksや承認条件へ適用しません。

## 検証資料を削除する直前の原本

旧 `docs/verification/` の正確な復元元は、削除前の固定commit [38e18c9ef256a51ca6efb2e273f3cb644f5a7e79](https://github.com/shinma06/android-replay-buffer/tree/38e18c9ef256a51ca6efb2e273f3cb644f5a7e79/docs/verification) です。`changes/` 161件、`amendments/` 3件、`promotion.json`、旧方針と補助文書を含みます。リンク先から当時の関連文書・ソースも参照できます。

削除・置換対象171ファイルのうち166ファイルは `legacy-ops-develop` と内容が同一ですが、次の5文書は切り替え後に改訂されているため、タグだけでは削除直前の内容を復元できません。

| 文書 | 削除直前の固定原本 |
|---|---|
| 旧検証方針 | [README.md](https://github.com/shinma06/android-replay-buffer/blob/38e18c9ef256a51ca6efb2e273f3cb644f5a7e79/docs/verification/README.md) |
| 診断ログ入力の履歴 | [diagnostic-log-input.md](https://github.com/shinma06/android-replay-buffer/blob/38e18c9ef256a51ca6efb2e273f3cb644f5a7e79/docs/verification/diagnostic-log-input.md) |
| 初期版の検証計画 | [initial-version.md](https://github.com/shinma06/android-replay-buffer/blob/38e18c9ef256a51ca6efb2e273f3cb644f5a7e79/docs/verification/initial-version.md) |
| 性能比較 | [performance.md](https://github.com/shinma06/android-replay-buffer/blob/38e18c9ef256a51ca6efb2e273f3cb644f5a7e79/docs/verification/performance.md) |
| 製品責任・旧受入条件 | [responsibility.md](https://github.com/shinma06/android-replay-buffer/blob/38e18c9ef256a51ca6efb2e273f3cb644f5a7e79/docs/verification/responsibility.md) |

`vm-acceptance.md` はタグ当時には存在せず、現行の [リリース時のVM受入](../verification/vm-acceptance.md) として残しています。

## 現在のcheckoutを変えずに読む

履歴を持つcloneで、例えば次を実行します。

```bash
git show 38e18c9ef256a51ca6efb2e273f3cb644f5a7e79:docs/verification/README.md
git show 2e576ce3c724dc95ccf0688b8a1bf7cd92d774a8:docs/operations.md
```

履歴のないcloneでは上記の固定GitHubリンクから参照できます。過去の結果は当時の記録であり、現在のテストや動作確認の結果ではありません。
