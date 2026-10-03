# 取り込み元と適合範囲

確認日: 2026-10-03。導入元は[agent-harness-template e822318](https://github.com/shinma06/agent-harness-template/tree/e822318a6c0fa7175a89687b929197dfae879184)。共通管理engineは[既存移植の固定版 fbabc42](https://github.com/shinma06/graffix-ar/tree/fbabc42f6516332e04ce5dad31e7092973eed0a7)を再利用し、[Cursor main 33c51dc](https://github.com/shinma06/cursor-in-android-studio/tree/33c51dc01c1cc68ff1c035c8c3ce02654e613ce7)と比較しています。元環境のファイル/個人設定/実行状態は変更しません。

| 共通能力 | 導入先・適合 |
|---|---|
| AGENTS、CLAUDE、Cursor、start/finish Skills | templateの入口・所有・文脈規約をCLI/今後のIDE開発へ適合 |
| hooks・doctor/check・secret/path/link検査 | template＋既存移植の未追跡ファイル検査。製品sourceを保全 |
| Issue・Project・Milestone・実Relationship | 同じtype/priority/status、6 Views、QA分離、終了readback。導入先の新規ID |
| 変更影響 | CI/hooks/coordinator共通。Python/CLI/config/launchdへ適合、未知・mixed・rename/delete/mode・不完全履歴は安全側 |
| 二段階統合・Case・4 checks | trusted baseのAcceptance gate、metadata policy、固定HEAD/baseレビュー、main/develop保護 |
| PR coordinator・worker・QA・cleanup・監査 | 共通engineを再利用。別repoへのAPI操作を防ぐrepo定数とlockを変更。停止/所有/privacy・保護readbackの追加修正も保持 |
| GUI lease | Cursor側で実際に使うhost/user namespaceへ接続。独立した予約を作って同じIDE/端末を並行操作しない |
| private registry | opaque公開IDとprivateなpath/host。元の登録は移植しない |
| Android Studio知見 | [参照表](android-studio.md)から今後の製品Issueで適用 |

既存移植とCursor最新版のengine差分を確認し、既存移植で強化されたサーバー保護readback、Issueコメントによる承認失効、source消失時停止、private storage 0700を維持しています。管理コードの新規第三者依存はありません。

製品固有のPlugin ID・ACP/print・モデル・保存仕様・SDK/JDK固定・iOS buildは取り込みません。Plugin ZIP/実IDE受入/配布の実装は、存在するプラグインbuildへ接続する後続の製品作業です。認証、個人MCP/plugin設定、旧claim、review/GUI結果、scheduler/PAUSED heartbeatは複製しません。

機能が存在することと、この環境で実運用済みであることは別です。[検証記録](validation.md)と[導入Issue](https://github.com/shinma06/android-replay-buffer/issues/1)に実行結果を残します。将来の更新はこの固定版との差分を専用Issue/PRで確認します。
