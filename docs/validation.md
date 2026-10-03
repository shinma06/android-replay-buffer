# 導入検証

確認日: 2026-10-03。結果は[導入Issue #1](https://github.com/shinma06/android-replay-buffer/issues/1)と関連PRが正本です。過去の別プロジェクトの成功を転記しません。

| 対象 | 確認方法・状態 |
|---|---|
| 管理ハーネス | pass。`python3 scripts/check.py`、162 tests。固定HEADとCIはPRに記録 |
| 既存CLI | pass。`python3 scripts/workflow/product_check.py`、既存2 tests。製品source/tests/設定/設計書の差分なし |
| 変更影響・push | `python3 scripts/workflow/change_impact.py --base origin/main --run-tests`。cleanなcommitに対して実行 |
| hooks/entrypoints | pass。bootstrap/doctorで設定・存在を確認。各clientの新規session実読込とは区別 |
| GitHub CI/独立review/統合/保護 | 導入PRとIssueに固定HEAD/base・実checks・effective rules・mergeのreadbackを記録。準備だけでは完了としない |
| Project | pass。Project #5の登録・In Progress/P1・Milestone #1・Standaloneとnative関係なし・6 Viewsをreadback |
| coordinator | trusted mainへ統合後read-only scanで実接続を確認。review/fix/merge workerの通し試験は別の許可された経路で実施 |
| GUI/録画/実機 | not-required（製品挙動の変更なし）。実機録画やIDEの合格は未観察 |
| Plugin build/ZIP | 未実装。この導入の成果ではない |
| 認証/MCP/plugins/remote | 本人の既存設定を利用し、内容を複製しない。doctorの存在確認は実接続・権限の証明ではない |
| 定期実行 | not-required。登録やPAUSED解除はしない |

管理テストは隔離Git/process/JSON fixtureを使います。実機録画・全CLI仕様・secret scannerの完全性・client自動読込は保証しません。未実施項目を成功として閉じず、次の操作をIssueに残します。

この実行環境のPATHではadb/scrcpy/ffmpegを検出できませんでした。管理/CLI unit testsに不要なので導入せず、実機試験前の前提として残します。
