# プラグイン同梱CLI

`replay` / `replayd` は、起動済みAndroid Studioで開いているprojectの取得・保存を操作します。GUIと同じ設定・バッファ・保存処理を使います。IDEを終了すると取得も終了します。独立した録画daemonやPython実行環境の導入は不要です。

これは#103の候補の利用契約です。固定ZIPでのIDE・端末受入結果は[Case](verification/changes/issue-103.json)とIssue/QAに記録し、実装やJVMテストだけで合格とは扱いません。対応環境は[固定した開発環境](plugin-development.md#固定した開発環境)のmacOS / Android Studioです。

## 配置と最初の操作

1. 同じ配布ZIPのプラグインを導入し、Android Studioで対象projectを開きます。設定画面で保存先、保持時間、取得モード、対象アプリを確認します。
2. 配布ZIPを任意の場所へ展開します。展開したプラグインフォルダの`bin/replay`と`bin/replayd`が入口です。この`bin`をPATHへ追加するか、絶対パスで呼びます。旧Python CLIより先に解決されることを`command -v replay`で確認してください。
3. Studioが標準の`/Applications/Android Studio.app`以外にある場合は、その`Contents`を`REPLAY_STUDIO_HOME`へ指定します。入口はそのStudioのJBRとKotlin runtimeを使います。IDE本体は起動しません。

```bash
# 展開先に合わせて指定する。空白のあるパスは引用符で囲む。
export PATH="$HOME/Tools/android-replay-buffer/bin:$PATH"
replay --help
replay --project "$HOME/Projects/MyApp" daemon
replay --project "$HOME/Projects/MyApp" status
replay --project "$HOME/Projects/MyApp" save
replay --project "$HOME/Projects/MyApp" stop
```

`--project`は開いているprojectのフォルダです。省略できるのは接続先が1つだけのときです。接続先が複数ある場合は、現在の作業フォルダや前面windowから推測せず失敗します。同じprojectを複数のIDEで開いている場合も、対象を一意にしてから実行します。新旧JARが異なる場合は、同じ配布物の入口と通常再起動したIDEを使います。

## 旧操作との対応

命名は既存操作との対応が分かりやすいため維持します。旧原型の公開操作は`save/status/stop/daemon`と`replayd`であり、`start`はありません。

| 旧操作・条件 | 統合後の操作・移行 |
|---|---|
| `replayd` / `replay daemon` | 対象projectの取得をONにして終了する。同じGUIのONと同じ処理。記録開始前や端末未接続なら待機状態になる |
| `replay daemon --foreground` / `replayd --foreground` | 互換aliasとして受理し、有限のON操作になったことをstderrへ案内する。ターミナルを占有する常駐監視はしない |
| `replayd --daemon` | 独立daemonの廃止を説明して終了コード2。起動済みIDEで通常の`replayd`を使う |
| `replay save` | 同じGUIバッファを保存する。OFFならGUIと同じ経路でONにする。まだ保存可能なデータがなければ理由付きで失敗し、準備できてから明示的に再実行する |
| 保存の成功 | writerが検証・公開を完了したフォルダだけをstdoutへ出し、終了コード0。受付だけでは成功にしない。一部欠落はstderrへ案内し、成果物のREADMEで詳細を確認する |
| 保存失敗・保存中・失敗対象の保全中 | 終了コード1、理由はstderr。既存GUIで同じ固定対象の再試行・保存先変更・破棄を選ぶ。CLIから追加のretry操作は提供しない |
| `replay status` | 同じprojectの状態をJSONで返す。`running`は取得ON、`capture_state`は待機/取得/復旧等、`save_state`は保存状態、`last_save`は直近のrequest/attemptと状態・結果。`last_save.directory`は完成時だけ返し、失敗試行の保全先は`retained_path`で区別する。ONを取得成功と解釈しない。取得状態の照会成功は0で返し、取得の障害は`error`で確認する。旧装飾テキストや障害時の終了コードを解析するスクリプトは移行が必要 |
| `replay stop` | 対象projectの取得だけをOFFにする。未保存バッファ・失敗pinの寿命と保存中取消しはGUIと同じ。IDE、他project、旧daemon、保存済み/保全された保存試行は終了・削除しない |
| IDE/project未起動・終了中・対象不明 | IDEを暗黙起動せず終了コード1。旧stopの未起動時0とは異なり、対象の確認不能を成功扱いしない |
| ログイン時launchd起動 | 新しいlaunchd jobは提供しない。IDE/projectの有効状態の復元を使う。旧jobは所有者が停止し、旧CLIとプラグインの同時取得を避ける |
| 通信切断・結果待ち120秒超過 | 終了コード1、結果不明と案内する。保存を取消した・失敗したとは断定しない。GUIの保存履歴/フォルダを照合してから次操作を決め、自動再試行しない |
| 引数不正・不正設定 | 終了コード2。接続・実操作失敗は1。設定ファイルを読めない場合は1 |

同時Saveは同じ既存の単一writer/pin制約へ入ります。CLIごとの録画process・端末接続・バッファを生成しません。接続が切れても、既に受理した保存の結果はwriterの確定値で保持します。OFFや別の再試行で現在の表示が変わっても、待機中CLIへ別の要求・attemptの結果を返しません。

## 設定移行

新CLIはGUIに保存した設定を使い、旧設定の既定場所を自動探索しません。GUIの初期値は180秒・保存先未指定です。旧原型の60秒・`~/Replays`を使う場合は、取得OFFで既存ファイルを明示します。保存先フォルダは事前に作成してください。移行後に取得をONにするため、開始前に対象projectと設定内容を確認します。

```bash
replay --project "$HOME/Projects/MyApp" stop
mkdir -p "$HOME/Replays"
replay --project "$HOME/Projects/MyApp" --config "$HOME/.config/replay-buffer/config.json" daemon
```

JSON又は拡張子`.yaml` / `.yml`の旧簡易YAML（平坦な`key: value`）を、UTF-8・64 KiB以内の通常ファイルで受け付けます。未知項目、重複項目、入れ子、不正値は拒否します。旧ファイルの内容は書き換えません。`--config`は`daemon`と`save`だけで指定でき、取得中は一式を拒否します。`status/stop`は設定を変更しません。

| 旧設定 | 移行内容 |
|---|---|
| `replay_seconds` | GUIの保持時間。明示ファイル内で省略時は旧既定60秒。プラグインが対応する1〜900秒に制限する |
| `output_directory` | GUIの保存先。省略/nullは旧既定`~/Replays`、`~`は現在利用者、相対パスはCLIを呼んだ作業フォルダ基準。他利用者の`~name`は絶対パスへ変更する |
| `segment_seconds` | 正の整数であることを確認し、使用しない旨を表示。動画区切りは既存plugin方式を使う |
| `state_directory` | 移行しない旨を表示。旧state/pid/socketは変更せず、新CLIも参照しない |
| `adb_path` | 省略又は既定`adb`のみ受理。個別path指定は理由付きで拒否し、projectのAndroid SDK設定へ移行する |
| `scrcpy_path` / `ffmpeg_path` | 移行しない旨を表示。プラグイン同梱server/JVM処理を使う |
| 対象アプリ・取得モード・自動保存 | 旧ファイルにない項目。現在のGUI設定を維持する |

反映中や終了と競合した場合は再開せず理由を返します。設定の保存後にAndroid modelの再解決が失敗・取消しになった場合は「保存したが反映を確認できない」と案内するため、GUIの保存設定と稼働状態を確認します。新しいGUI操作を古いCLI設定で上書きして復旧しません。

## 保存形式と復旧

旧原型の`replay.mp4 / logcat.txt / timeline.json`から、GUIと同じ`video-001.mp4 / logcat-device.jsonl / logcat-app.jsonl / frames.jsonl / session.json / README.txt`へ移行します。動画なしの理由、元ログ、frame/PTS・窓・時計・欠落は既存の[保存契約](plugin-design.md)と[同期契約](design/timeline.md)に従います。ログ専用は動画を生成しません。旧形式を期待する利用者側ツールは新形式へ対応が必要です。既存の保存物を変換・上書きする処理はありません。

原型のsourceはGitの固定commit `e6fb021b144d4f0f6a1f7916e92449610b3ff80c`から復元できます。取り出し先を新しい別フォルダとし、現在のcheckoutや保存物に重ねません。

```bash
git archive --format=tar --prefix=android-replay-buffer-cli-origin/ \
  --output=../android-replay-buffer-cli-origin.tar \
  e6fb021b144d4f0f6a1f7916e92449610b3ff80c
```

復旧時はプラグインの取得をOFFにして終了を確認し、旧sourceのREADMEと元の設定・launchd所有情報で必要な環境を復元します。旧設定・旧成果物・旧観察はそのまま保持します。旧Python runtimeと外部ツールは原型を復元して使う場合だけ必要です。プラグインの取得と旧daemonの併用を自動検出・終了する機能はなく、所有不明のprocessをCLIから停止しません。

## 接続と所有の境界

プラグインはprojectごとにloopback `127.0.0.1`へ接続口を作り、利用者専用の`~/.android-replay-buffer-cli`へ接続情報を置きます。directoryは0700、ファイルは0600で、現在利用者の所有・通常file・project・process開始時刻・同一plugin JAR・相互認証を確認します。共有/他利用者の情報やsymlinkは修復・追跡せず拒否します。同じOS利用者として動く任意codeの隔離までは提供しません。

要求は1接続につき1件、4 workerまでです。入力/出力と設定サイズを制限し、認証を含む要求の読取り全体は5秒、操作結果の待機は120秒に制限します。未知の接続先へ切り替えず、保存の自動retryもしません。project終了で接続を閉じ、自分が公開した接続情報だけを除去します。異常終了の残情報はprocessの生存/開始時刻で除外し、他の情報や旧daemon stateを掃除しません。

方式選定では、[固定ApplicationStarter公開API](https://github.com/JetBrains/intellij-community/blob/8062c061ff33e22041d9f1844db5b8b8a6b5e4d7/platform/ide-core/src/com/intellij/openapi/application/ApplicationStarter.kt)と実Studioの同梱APIを確認しました。[IDEのcommand-line起動](https://www.jetbrains.com/help/idea/working-with-the-ide-features-from-command-line.html)だけでは、起動済み対象限定とstdout/stderrの返却を満たせないため、上記の専用接続を使います。配布への追加は[公式prepareSandbox方式](https://plugins.jetbrains.com/docs/intellij/tools-intellij-platform-gradle-plugin-recipes.html#bundle-additional-files-with-the-plugin)を使い、Pythonや別エンジンを同梱しません。
