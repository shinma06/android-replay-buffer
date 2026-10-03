# CLI原型の保全

原型はPython packageの版が `0.1.0` だった次のcommitです。リリース済み・実機試験済みという意味の版名ではありません。

- 固定source: [`e6fb021b144d4f0f6a1f7916e92449610b3ff80c`](https://github.com/shinma06/android-replay-buffer/tree/e6fb021b144d4f0f6a1f7916e92449610b3ff80c)
- 内容: 初期CLI実装とlogcat/scrcpy再試行・log-only保存
- [原README](cli-origin/README.md)、[CLI原型の設計（凍結）](cli-origin/design.md)、[全ファイルmanifest](cli-origin/manifest.json)

原型の実装、起動スクリプト、設定例、launchd、pyproject、テストは既存の実行方法を保つため元の場所に維持します。CLI設計書は現在のプラグイン設計と区別するため `docs/cli-origin/design.md` に移動し、原文を保持しています。プロジェクト共通のREADMEとgitignoreは開発基盤用に更新するため、原文を同じ `docs/cli-origin/` に別保存しています。manifestは元の名前・現在の保存先・SHA-256・実行権限を記録します。元commitの全21ファイルが対象です。

```bash
python3 scripts/workflow/product_check.py
```

このコマンドはファイル内容・削除・symlinkへの置換・実行権限の変化を検出し、その後にCLIの既存unit testsを実行します。Git履歴を省いたcheckoutでも保全確認が可能です。デバイス、daemon、ユーザー設定には触れません。

CLI原型は凍結した参考実装です。将来エンジンを変更する場合も、manifestを新しいhashで上書きして原型扱いにしません。別Issueで原型の完全な保存先と互換性方針を決めてから進めます。hashは誤変更の検知であり、悪意あるmanifest改変まで防ぐ署名ではありません。

## 原型だけを取り出す

履歴を持つcloneで、未使用の出力名を選んで実行します。

```bash
git archive --format=tar --prefix=android-replay-buffer-cli-origin/ \
  --output=../android-replay-buffer-cli-origin.tar \
  e6fb021b144d4f0f6a1f7916e92449610b3ff80c
```

生成したtarを別ディレクトリへ展開すれば、元の配置とREADMEでCLIを利用できます。現在のcheckoutを書き換える必要はありません。shallow cloneにcommitがなければ、元repositoryから履歴を取得するか上記固定sourceリンクからソースを取得します。

タグや別の保守branchに依存せず、固定commitと現在tree内の全原型ファイルの両方を保持します。main/developの履歴を書き換えず、原型の復元可能性を維持します。

## 保全と受入の違い

原READMEのMVPチェック欄は当時の記録です。今回のhash照合と既存2 testsは、動画の品質、USB再接続、保存失敗からの復旧を再検証するものではありません。録画機能を接続する段階で、このプロジェクトの固定build・専用端末・Caseで受入します。
