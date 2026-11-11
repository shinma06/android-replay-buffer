# CLI（起動済みAndroid Studioのリモコン）

このページはCLIの契約です。CLIを変えるときは、このページ・`replay --help`・同梱の`bin/README.txt`・[MCPサーバー](mcp.md)を同時に更新し、GUI・CLI・MCPの動作を同じに保ちます。

`replay`は、起動済みAndroid Studioで開いているprojectのプラグインをターミナルから操作する入口です。GUI（ToolWindow・設定画面）、CLI、エージェント向けの`replay mcp`は、IDE内の同じ設定・記録バッファ・保存処理を使います。CLIが独自に録画・保存することはなく、IDEを暗黙起動することもありません。IDEを終了すると取得も終了します。

引数・出力・終了コードはLinux / macOS / Windowsで同じです。標準出力・標準エラーはUTF-8です。Cロケールでも日本語の説明・理由・保存先を保持し、pipやファイルへの出力も同じ文字コードを使います。

## 配置と最初の操作

1. プラグインをAndroid Studioへ導入し、対象projectを開きます。
2. CLIは導入済みプラグインのフォルダの`bin`にあります（Linux / macOSは`bin/replay`、Windowsは`bin\replay.cmd`）。正確な場所は設定画面（Settings > Tools > Android Replay Buffer）の「ターミナル・エージェントから操作」にある「CLIの場所」に表示され、「コピー」で貼り付けられます。この絶対パスで呼ぶか、`bin`をPATHへ追加します。
3. 配布ZIPを別の場所へ展開して使う必要はありません。展開した別コピーはプラグイン更新後にIDEの版とずれ、同じ配布物ではない接続として拒否されます。導入済みプラグインの`bin`はIDEと同時に更新されます。

```bash
# 「CLIの場所」をコピーして使う。空白のあるパスは引用符で囲む。
replay="/path/to/plugins/android-replay-buffer/bin/replay"
"$replay" --help
"$replay" --project "$HOME/Projects/MyApp" start
"$replay" --project "$HOME/Projects/MyApp" save
```

### Android Studioの場所

CLIは導入先StudioのJBRとKotlin runtimeで動きます（IDE本体は起動しません）。使うStudioは次の順で決まります。

1. 環境変数`REPLAY_STUDIO_HOME`（macOSは`Android Studio.app/Contents`、Linux / Windowsは導入フォルダ）
2. プラグインを入れたStudioがprojectを開いたときに記録した自身の導入先（使えるJBRとruntimeがある場合だけ）
3. 標準の場所（macOS: `/Applications/Android Studio.app/Contents`、Linux: `/opt/android-studio`、Windows: `%ProgramFiles%\Android\Android Studio`）

記録は利用者専用の`~/.android-replay-buffer-cli/studio-home`（Windowsは`%USERPROFILE%`配下）に、接続情報と同じ所有者限定の扱いで置きます。複数のStudioを使う場合は最後にprojectを開いたStudioが記録されます。標準以外の場所でも、一度そのStudioでprojectを開けば`REPLAY_STUDIO_HOME`は不要です。

## コマンド

```text
replay [--project PATH] start|stop|save|status|screenshot
replay [--project PATH] save --logs-only
replay [--project PATH] settings
replay [--project PATH] settings set KEY=VALUE...
replay [--project PATH] mcp
```

| コマンド | 動作 | 成功時のstdout | MCP tool |
|---|---|---|---|
| `start` | 取得をONにし、開始を確認して終了します。取得はIDEが続けます。 | `started` | `start_capture` |
| `stop` | 対象projectの取得だけをOFFにします。未保存バッファは破棄します。保存済み成果物・IDE・他projectは変更しません。 | `stopped` | `stop_capture` |
| `save` | GUIの保存と同じ処理で保存し、完成を待ちます。取得OFFなら取得を開始します。記録の蓄積前は保存できません。 | 完成したフォルダのパスだけ | `save_replay` |
| `save --logs-only` | Logcatツールバーの「直前N秒のログを保存」と同じ処理です。端末ログ・アプリログ・`session.json`だけを保存し、MP4は作りません。動画の取得中でも取得は止めず、`session.json`の`coverage.video.state`を`EXCLUDED`（意図的な除外。未要求の`NOT_REQUESTED`や取得失敗とは別）、`save_request`に`content: LOGS_ONLY`と要求元`origin`を記録します。取得OFFなら`save`と同じく取得を開始します。 | 完成したフォルダのパスだけ | `save_replay`（`logs_only: true`） |
| `status` | 設定を変えずに取得状態を返します。 | 状態JSON | `get_status` |
| `screenshot` | GUIと同じスクリーンショットを保存します。 | PNGのパスだけ | `take_screenshot` |
| `settings` | 現在の設定とON/OFFを返します。 | 設定JSON | `get_settings` |
| `settings set KEY=VALUE...` | 指定した項目だけを変更します。 | 変更後の設定JSON | `update_settings` |
| `mcp` | 標準入出力のMCPサーバーとして動きます。[MCPサーバー](mcp.md)を参照してください。 | MCPの応答 | — |

`--project`は開いているprojectのフォルダです。省略できるのは接続先が1つだけのときです。接続先が複数ある場合は、現在の作業フォルダや前面windowから推測せず失敗します。同じprojectを複数のIDEで開いている場合も、対象を一意にしてから実行します。

`save`の警告（記録の不足など）や失敗理由はstderrに出します。stdoutには成功時の結果だけを出します。

### 設定の読み書き

```bash
replay --project "$HOME/Projects/MyApp" settings
replay --project "$HOME/Projects/MyApp" settings set replay_seconds=60 auto_save_on_crash=true
replay --project "$HOME/Projects/MyApp" settings set output_directory="$HOME/Replays"
```

項目名は`settings`の出力・MCPと同じです: `replay_seconds`（1〜900）、`output_directory`、`app_selection`（`AUTOMATIC` / `MANUAL`）、`manual_package`、`capture_mode`（`VIDEO_AND_LOGS` / `LOG_ONLY`）、`video_quality`（`STANDARD` / `LIGHTWEIGHT`）、`auto_save_on_crash`、`screenshot_uses_logcat_destination`、`screenshot_directory`、`show_touches`。

- 1つの引数が`KEY=VALUE`の1項目です。値は最初の`=`より後ろ全体で、空白を含む値はシェルの引用符で囲みます。
- 整数・真偽値の項目は`60`・`true` / `false`を数値・真偽値として送ります。それ以外の書き方や未知・重複した項目は、MCPの`update_settings`と同じ検証で理由を表示して拒否します（終了コード2）。
- 設定画面の「適用」と同じ検証をします。保存先フォルダは事前に作成してください。取得モード・動画品質・`show_touches`は取得OFFで変更します。取得のON/OFFは`start` / `stop`で行います。

## 終了コード

| コード | 意味 |
|---|---|
| 0 | 完了。`save`は保存完了、`settings set`は変更の反映後 |
| 1 | 操作・接続の失敗、または結果不明（IDE/project未起動、通信切断、結果待ち120秒超過など）。保存は自動retryしません |
| 2 | 引数・設定値の不正。内容を直して再実行できます |

対象を確認できない場合は1を返します。結果不明のときは保存を取消した・失敗したとは断定しません。GUIの保存履歴・フォルダを照合してから次の操作を決めてください。

終了情報の保存先を安全に準備できないときは、`status` の `error` と `start` の標準エラーで対象を確認してください。権限やリンクを修正した後は、同じprojectを開いたまま `replay --project <project> start` を再実行できます。`status` は初期化を再試行しません。

同時Saveは同じ既存の単一writer/pin制約へ入ります。CLIごとの録画process・端末接続・バッファを生成しません。接続が切れても、既に受理した保存の結果はwriterの確定値で保持します。OFFや別の再試行で現在の表示が変わっても、待機中CLIへ別の要求・attemptの結果を返しません。

## 接続と所有の境界

プラグインはprojectごとにloopback `127.0.0.1`へ接続口を作り、利用者専用の`~/.android-replay-buffer-cli`へ接続情報を置きます。directoryは0700、ファイルは0600（Windowsは所有者だけのACL）で、現在利用者の所有・通常file・project・process開始時刻・同一plugin JAR・相互認証を確認します。共有/他利用者の情報やsymlinkは修復・追跡せず拒否します。同じOS利用者として動く任意codeの隔離までは提供しません。Studioの導入先の記録`studio-home`も同じdirectoryに同じ扱いで置き、接続情報としては使いません。

要求は1接続につき1件、4 workerまでです。入力/出力と設定サイズを制限し、認証を含む要求の読取り全体は5秒、操作結果の待機は120秒に制限します。未知の接続先へ切り替えず、保存の自動retryもしません。project終了で接続を閉じ、自分が公開した接続情報だけを除去します。異常終了の残情報はprocessの生存/開始時刻で除外し、他の情報や旧daemon stateを掃除しません。

方式選定では、[固定ApplicationStarter公開API](https://github.com/JetBrains/intellij-community/blob/8062c061ff33e22041d9f1844db5b8b8a6b5e4d7/platform/ide-core/src/com/intellij/openapi/application/ApplicationStarter.kt)と実Studioの同梱APIを確認しました。[IDEのcommand-line起動](https://www.jetbrains.com/help/idea/working-with-the-ide-features-from-command-line.html)だけでは、起動済み対象限定とstdout/stderrの返却を満たせないため、上記の専用接続を使います。配布への追加は[公式prepareSandbox方式](https://plugins.jetbrains.com/docs/intellij/tools-intellij-platform-gradle-plugin-recipes.html#bundle-additional-files-with-the-plugin)を使い、Pythonや別エンジンを同梱しません。

旧Python CLIの原型は[履歴](cli-origin.md)にあります。
