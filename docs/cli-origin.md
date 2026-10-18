# 退役したPython CLIの履歴と復旧

[#103](https://github.com/shinma06/android-replay-buffer/issues/103)でCLIをプラグインの取得core・保存writerへ内包しました。現行の入口・互換範囲・明示移行は[CLI案内](cli.md)を参照してください。Python実装・独立daemon・旧JSON IPC・launchd・設定例・Python packageと原型専用tests/checker・原文コピーは現行treeから退役し、第二の配布製品として維持しません。管理用Python scriptと過去の検証記録は別です。

原型の完全な復旧元は、Python package版が`0.1.0`だった固定commit [`e6fb021b144d4f0f6a1f7916e92449610b3ff80c`](https://github.com/shinma06/android-replay-buffer/tree/e6fb021b144d4f0f6a1f7916e92449610b3ff80c)です。版名はrelease・実機受入済みを意味しません。[原README](https://github.com/shinma06/android-replay-buffer/blob/e6fb021b144d4f0f6a1f7916e92449610b3ff80c/README.md)・[原設計](https://github.com/shinma06/android-replay-buffer/blob/e6fb021b144d4f0f6a1f7916e92449610b3ff80c/docs/design.md)・実装・tests・設定例・起動scriptの全21ファイルと実行権限をGit履歴に保持します。既存のユーザー設定、録画、一時領域、launchd登録を移行処理が削除することはありません。

## 原型だけを取り出す

履歴を持つcloneで、未使用の出力名を選んで実行します。

```bash
git archive --format=tar --prefix=android-replay-buffer-cli-origin/ \
  --output=../android-replay-buffer-cli-origin.tar \
  e6fb021b144d4f0f6a1f7916e92449610b3ff80c
```

tarを別ディレクトリへ展開し、原READMEの条件で利用します。現在のcheckoutを書き換える必要はありません。shallow cloneにcommitがなければ元repositoryから履歴を取得するか、上記固定sourceからソースを取得します。main/developの履歴を書き換えず、可変tagや別保守branchにも依存しません。Git履歴を含まないPlugin ZIPだけでは旧sourceを復旧できないため、移行前にこのcommitのarchiveを保管できます。

## 復旧確認と製品受入

#103の退役前確認では、元commitの全21ファイルを別ディレクトリへ抽出し、内容・実行権限を照合して旧CLIのhelpを実行しました。元treeの保全checker・compile/import・既存2 testsの結果もIssueに保持します。これらは原型の復旧可能性を確認するもので、旧版の実機録画や新版のIDE受入を表しません。旧観察・FAIL・未実行条件は変更せず、新版は同一候補のCaseで受け入れます。
