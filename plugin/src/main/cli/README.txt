Android Replay Buffer — ターミナル入口

このbin/replayは、起動済みAndroid Studioのプラグインを操作するリモコンです。
GUI・CLI・replay mcpは同じ設定・記録バッファ・保存処理を使います。
このbinはIDEに導入済みのプラグインの中にあり、プラグイン更新時に一緒に更新されます。
正確な場所は設定画面（Tools > Android Replay Buffer）の「CLIの場所」からコピーできます。
配布ZIPを別に展開して使う必要はありません（更新後に版がずれ、接続を拒否されます）。
stdout/stderrはUTF-8です。Cロケールでも日本語を保持し、pipeやファイル出力も同じです。

replay --help
replay --project /absolute/project/path start
replay --project /absolute/project/path stop
replay --project /absolute/project/path save
replay --project /absolute/project/path status
replay --project /absolute/project/path screenshot
replay --project /absolute/project/path settings
replay --project /absolute/project/path settings set replay_seconds=60 auto_save_on_crash=true
replay --project /absolute/project/path settings set device=emulator-5556
replay --project /absolute/project/path mcp

空白のあるpathや値は引用符で囲んでください。対象が複数ある場合は--projectが必須です。
Windowsではreplay.cmdを使います（引数・出力・終了コードは同じです）。

使うAndroid Studioは次の順で決まります。
1. REPLAY_STUDIO_HOME（macOSはAndroid Studio.app/Contents）
2. プラグインを入れたStudioがprojectを開いたときに記録した導入先（~/.android-replay-buffer-cli/studio-home）
3. 標準配置（macOS: /Applications、Linux: /opt/android-studio、Windows: Program Files内のAndroidフォルダー）
標準以外の場所でも、一度そのStudioでprojectを開けばREPLAY_STUDIO_HOMEは不要です。

startは取得をONにして終了します。IDEを終了すると取得も終了します。
stopは対象projectの取得だけをOFFにし、未保存バッファ/失敗pinを破棄します。
保存済み成果物・保全partial・IDE・他projectは削除/終了しません。
saveのexit0/stdoutは完成フォルダです。受付だけでは成功になりません。
screenshotはGUIと同じPNGを保存し、パスだけをstdoutへ返します。
settingsはMCPのget_settings、settings setはupdate_settingsと同じ項目名・検証です。
複数の端末を接続したときはstatusのdevices[].serialから1台をdevice=<serial>で選びます（空は自動）。
未選択で2台以上なら取得せず選択を待ち、選んだ端末の切断中も別の端末へは切り替えません。
mcpは標準入出力のMCPサーバーです。IDE本体は起動しません。
失敗や結果不明はexit1/stderr、引数・設定値の不正はexit2です。通信断を自動retryしません。

全コマンド・終了コード:
https://github.com/shinma06/android-replay-buffer/blob/${sourceCommit}/docs/cli.md
