# エージェント向けMCPサーバー

同梱CLIの`replay mcp`は、[Model Context Protocol](https://modelcontextprotocol.io/specification/2025-11-25)のstdioサーバーです。Devin・Claude・その他のMCPクライアントが、起動済みAndroid Studioで開いているprojectの状態取得・取得ON/OFF・保存・スクリーンショット・設定の読み書きを、GUIや[CLI](cli.md)と同じ処理で操作できます。

- 入口は[CLI](cli.md#配置と最初の操作)と同じ、導入済みプラグインの`bin/replay`（Windowsでは`bin\replay.cmd`）です。正確なパスは設定画面の「CLIの場所」からコピーできます。引数・tool・結果はどのOSでも同じです。
- IDEのバージョンには依存しません。CLIと同じく、プラグインを導入したStudioのJBRで動き、IDE本体や独立した録画daemonは起動しません。対象projectを開いたStudioを先に起動してください。
- 追加のruntime・ライブラリは不要です。通信は改行区切りのJSON-RPC 2.0で、stdoutにはMCPのメッセージだけを書き、診断はstderrへ出します。
- 各tool呼び出しは既存CLIと同じ認証付きloopback接続の1要求です。接続先の確認・タイムアウト・保存の自動retryをしない規則は[接続と所有の境界](cli.md#接続と所有の境界)と同じです。

## 設定

`--project`を付けると、`project`引数を省略したtool呼び出しの対象になります。付けない場合、開いているprojectが1つだけなら自動で選び、複数あれば`project`引数が必須です（`list_projects`で確認できます）。空白のあるパスはJSONの文字列としてそのまま書きます。

Claude Code:

```bash
claude mcp add android-replay-buffer -- /path/to/plugins/android-replay-buffer/bin/replay --project /path/to/MyApp mcp
```

Claude Desktop・Cursorなど`mcpServers`形式の設定ファイル:

```json
{
  "mcpServers": {
    "android-replay-buffer": {
      "command": "/path/to/plugins/android-replay-buffer/bin/replay",
      "args": ["--project", "/path/to/MyApp", "mcp"]
    }
  }
}
```

Windowsでは`command`を`C:\\path\\to\\plugins\\android-replay-buffer\\bin\\replay.cmd`にします。Studioが標準の場所以外にあっても、そのStudioで一度projectを開けば導入先が記録されます（[Android Studioの場所](cli.md#android-studioの場所)）。別のStudioを使わせる場合だけ`"env": {"REPLAY_STUDIO_HOME": "..."}`を加えます。

Devin: 設定のMCP Marketplaceで「Add your own」からstdioサーバーを追加し、Commandに`bin/replay`の絶対パス、Argumentsに`--project`・project・`mcp`を指定します。Devin・Android Studio・端末が同じマシン（Devin VMなど）にある場合だけ使えます。接続はloopbackと利用者専用の接続情報に限られるため、別マシンのStudioへは接続できません。

その他のクライアントも、stdioで上記のcommand・argsを起動できれば同じ設定です。

## tool

| tool | 内容 | 対応するCLI |
| --- | --- | --- |
| `list_projects` | 接続できる開いているprojectと、同じ配布物か（`same_build`）を返します。 | なし |
| `get_status` | 取得状態をJSONで返します。変更はしません。 | `status` |
| `start_capture` | 取得をONにし、開始を確認して返します。 | `start` |
| `stop_capture` | 取得をOFFにします。未保存バッファは破棄します。保存済み成果物・IDE・他projectは変更しません。 | `stop` |
| `save_replay` | 保存し、完成したフォルダのパス（`directory`）と、最初に読む要約`README.md`のパス（`summary_path`）を返します。`logs_only: true`（既定false）ではLogcatツールバーのボタンと同じくログと`replay.json`・`README.md`だけを保存し（MP4なし、`coverage.video.state`は`EXCLUDED`）、動画の取得は止めません。 | `save` / `save --logs-only` |
| `take_screenshot` | GUIのスクリーンショットと同じ処理で、録画対象端末の今の画面をPNG保存し（対象アプリが前面である必要はありません）、パスを返します。`include_image`（既定true）で画像も返します（10 MiBまで）。 | `screenshot` |
| `get_settings` | 設定とON/OFF状態をJSONで返します。 | `settings` |
| `update_settings` | 指定した設定だけを変更し、変更後の設定を返します。 | `settings set KEY=VALUE...` |

全toolに任意の`project`（開いているprojectの絶対パス）があります。それ以外の未知の引数は拒否します。

`update_settings`の項目は`get_settings`と同じ名前です: `replay_seconds`（1〜900）、`output_directory`、`app_selection`（`AUTOMATIC` / `MANUAL`）、`manual_package`、`capture_mode`（`VIDEO_AND_LOGS` / `LOG_ONLY`）、`video_quality`（`STANDARD` / `LIGHTWEIGHT`）、`auto_save_on_crash`、`screenshot_uses_logcat_destination`、`screenshot_directory`、`show_touches`（取得中に端末画面へタップ位置の点を表示し、動画に映す）。設定画面の「適用」と同じ検証をし、保存先フォルダは事前に作成が必要です。取得モード・動画品質・`show_touches`は取得OFFで反映が終わってから変更します。取得のON/OFFは`start_capture` / `stop_capture`で行います。

## 結果と失敗

成功時は`content`のtextに結果を返します。protocol `2025-06-18`以降では、JSONの結果（`get_status` / `get_settings` / `update_settings` / `list_projects`）とパス（`save_replay`の`directory`と`summary_path`、`take_screenshot`の`path`）を`structuredContent`にも入れます。

失敗はtool結果の`isError: true`で返し、textの末尾にCLIと同じ終了コードを付けます。

- `exit code 2`: 引数・設定の誤り。内容を直して再実行できます。
- `exit code 1`: 操作の失敗または結果不明。保存は自動retryせず、`get_status`と保存先を確認してから判断してください。

未知のtool・不正な引数の型・JSONの誤りは、JSON-RPCのエラー（`-32602`、`-32601`、`-32700`など）で返します。

## 同時実行と終了

tool呼び出しは同時に4件まで実行し、さらに16件まで順番待ちで受け付けます。それを超えた要求は実行せず、JSON-RPCのエラー`-32000`（同時に実行できる操作の上限）を返します。

クライアントがstdinを閉じても、受け付けた要求（実行中と順番待ち）はすべて実行し、それぞれに1回だけ応答してから終了します。各要求は開始から自身のタイムアウト（CLIの1要求と同じ）までに結果か`exit code 1`（結果不明）を返します。保存や設定変更の要求が、応答のないまま打ち切られることはありません。

ただし実行中の要求が自身のタイムアウトを過ぎても終わらない異常時は、まだ開始していない要求を実行せず、`-32000`（終了のため実行していない）を返してから終了します。このエラーを受けた要求は実行されていないため、必要なら再実行できます。応答を受け取れなかった実行中の要求は結果不明として扱い、保存は`get_status`で確認してください。

## コマンドラインから

MCPの各toolは同じ処理のCLIコマンドを持ちます（上表）。設定の読み書きは`replay settings`と`replay settings set KEY=VALUE...`で、項目名・検証・終了コードは`get_settings` / `update_settings`と同じです。使い方は[CLI](cli.md#設定の読み書き)を参照してください。
