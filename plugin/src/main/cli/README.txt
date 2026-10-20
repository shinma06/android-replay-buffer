Android Replay Buffer — ターミナル入口

このbin/replayとbin/replaydは、同じ配布物を導入済みのAndroid Studioへ接続します。
起動済みprojectのGUIと同じ取得・バッファ・保存結果を使います。
stdout/stderrはUTF-8です。Cロケールでも日本語を保持し、pipeやファイル出力も同じです。

replay --help
replay --project /absolute/project/path daemon
replay --project /absolute/project/path status
replay --project /absolute/project/path save
replay --project /absolute/project/path stop

空白のあるpathは引用符で囲んでください。対象が複数ある場合は--projectが必須です。
このbinをPATHへ追加するか、入口の絶対pathを使います。
Studioが標準配置以外なら、そのContentsをREPLAY_STUDIO_HOMEへ指定してください。

daemon/replayd/--foregroundは取得ONの有限操作です。IDEを終了すると取得も終了します。
Saveのexit0/stdoutは完成フォルダです。受付だけでは成功になりません。
失敗や結果不明はexit1/stderr、引数不正はexit2です。通信断を自動retryしません。
stopは対象projectの取得だけをOFFにし、未保存バッファ/失敗pinを破棄します。
保存済み成果物・保全partial・IDE・他project・旧daemonは削除/終了しません。

旧設定は自動読込みしません。OFF時に--config FILEをdaemon/saveへ指定すると、
保持秒数/保存先だけを明示移行します。保存先フォルダは事前に作成してください。
旧ファイルは不変。非対応optionや設定は理由を表示します。
旧launchd/daemonと新しい取得を同時に動かさず、元の所有者が旧起動を停止してください。

command/config/output差分・結果待ち・保存形式・旧sourceの復旧手順:
https://github.com/shinma06/android-replay-buffer/blob/${sourceCommit}/docs/cli.md
