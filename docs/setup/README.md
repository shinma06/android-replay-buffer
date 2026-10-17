# 外部環境のセットアップ

リポジトリをcloneするだけでは、ログイン、MCP、Plugins、OS権限、GitHub保護、schedulerは移りません。必要な層だけを下の順序で設定します。

| 順番 | 設定するもの | 完了の確認 |
| --- | --- | --- |
| 1 | Git、管理/開発用Python 3.15以上、Bash、GitHub CLI | `git --version` / `python3 --version` / `gh --version` |
| 2 | [選んだ開発クライアント](clients.md) | ログイン、新規セッション、指示とSkillsの検出 |
| 3 | [MCPと認証](mcp.md) | server一覧、必要なread-only呼出し、権限範囲 |
| 4 | [Plugins・通知](plugins-and-hooks.md) | manifest/実行内容、重複、実イベントの確認 |
| 5 | [GitHub](github.md) | repository、CI、保護ルール、Issue/PR経路 |
| 6 | [GUI・リモート環境](gui-and-remote.md) | 必要な場合だけ、画面取得・対象識別・共通lease |
| 7 | [自動進行](automation.md) | 単発のdry-run、その後明示起動・停止・復旧 |

CIの起動範囲、時間の読み方、全量検証と切り戻しは[CIの運用](ci.md)を参照してください。

## runtimeと配置

Python標準ライブラリだけで共通ツールが動きます。Node/npmはContext7のstdio版やPonytail等、選んだ外部ツールが必要とする場合に導入します。製品のAndroid Studio SDK/JDK・実機の条件はdocs/project.mdへ記載し、ハーネスの依存と分けます。

技術スタックは依存関係に問題がなければ最新stableを使います。更新時に一次情報と実版を確認し、互換性の問題があれば最新stableに適合する公式・既存の方法を先に調べ、妥当な方法を採用します。実現できない場合だけ更新可能な最大のstableを選び、具体的な制約と再開条件をIssue/PRへ残します。pre-releaseは採用しません。製品の固定候補・SDK/build・対応版の契約は各変更の検証と統合手順を通して更新し、版の方針だけで既存受入を読み替えません。

主検査版は[プロジェクト情報](../project.md)、最新stableの確認は[Python公式](https://www.python.org/downloads/)を参照します。repositoryの管理/開発script（検査・hooks・QA/受入・GUI lease・coordinator）はPython 3.15以上を対応対象とし、主検査/CIはstable 3.15.0を使用します。旧3.11下限は[`tomllib`の追加版](https://docs.python.org/3/library/tomllib.html)が技術的根拠でしたが、最新stable方針と実検査版に合わせて対応範囲を更新します。配布CLIのPython 3.9以上と専用互換CIは維持し、JVM plugin利用者のruntime要件とも区別します。CIは既存の[setup-python](https://github.com/actions/setup-python/blob/v7.0.0/docs/advanced-usage.md#specifying-a-python-version)で確認したstableを指定します。

旧3.11〜3.14で管理作業を行う開発者は、既存managerで3.15を導入/選択し、`python3 --version`を確認してから下の検査を実行してください。別shell、GUIアプリ起動のCLI、既存の管理ジョブもそれぞれ実runtimeを確認します。登録済み処理の停止・移行・再開は既存の所有/PAUSED状態と[自動進行の起動条件](automation.md#起動条件)を保ちます。古いPythonで動くコードを強制拒否するguardは追加せず、3.15未満の管理互換性は対応・CI検証対象から外します。CLIだけを利用する環境にはこの移行を求めません。

プロジェクト検査では既存managerが提供する明示版を選び、OS Python・global defaultsを変更せずに実行します。Homebrewの3.15を検査で選ぶ例は次のとおりです。導入や既定版の選択を含むホスト環境整備はプロジェクトとは別の作業として管理します。

```bash
export PATH="$(brew --prefix python@3.15)/bin:$PATH"
python3 --version
python3 scripts/check.py
```

このPATHを選んだshellでbootstrap・commit/pushも実行し、hooksの`python3`を揃えます。Python内の検査起動は`sys.executable`で親と同じ版を使います。実runtimeと子processを照合してから検査結果を記録し、PATH変更はそのshellの終了で戻せます。

macOS/Linuxが共通CLIの対象です。GUI leaseは `fcntl` と同一OSユーザーのPOSIX権限を利用します。Windows nativeの動作は未確認です。WSLでコード検証する場合も、Windowsデスクトップ操作とは別ホスト扱いにせず、実際に共有する画面の操作者・予約を一本化してください。WSLのlockファイルがWindows側の操作を強制禁止するわけではありません。

新しい環境で自分のPATHを確認します。GUIアプリから起動するCLIは対話shellとPATHが異なる場合があります。既存機のNode絶対パス、app bundle内部の実行ファイル、socket、trusted code pathsは移植せず、正式なinstallerが再生成する値を使います。

## 秘密情報とセットアップ記録

設定全体や環境変数一覧をチャットへ貼らず、項目の有無・環境変数名・接続結果だけ記録します。認証は正規ログイン/secret storeを使い、外部サービスごとに導入先repositoryと必要操作へ権限を限定します。`gh auth status` 等を確認しても、tokenを表示するコマンドは使いません。

個人の `~/.codex` / `~/.claude` / `~/.cursor`、ブラウザープロファイル、credential storeを丸ごと同期しません。`.harness-local/`はGit管理外ですが暗号化保管庫ではありません。ログにもtokenを残さず、共有時に読み直してください。

[確認記録の形式](../validation.md)に、導入日・クライアント版・実model選択・検出・接続・未確認・解除担当を記録します。modelの選択値や料金プランをこのテンプレートで固定しません。
