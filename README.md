# Android Replay Buffer

Android操作直前の画面・logcat・時刻情報を保持し、不具合を見つけた時点で保存するAndroid Studioプラグインです。ToolWindowと同梱の`replay`が、同じprojectの設定・バッファ・保存処理を使います。動画とログを取得するモードと、動画を開始しないログ専用モードがあります。

このbranchの第2版候補と、受入・main反映済みの範囲は区別します。[プロジェクト情報](docs/project.md)と[#280](https://github.com/shinma06/android-replay-buffer/issues/280)が現在地です。初期版のAgent段階は完了済みですが、[QA #65](https://github.com/shinma06/android-replay-buffer/issues/65)のREAL・人間確認、元の失敗・未確認は引き継ぎます。新しいbuildやJVMテストの成功だけでIDE・端末受入済みとは扱いません。

## 利用する

対象はmacOS、[固定対応のAndroid Studio](docs/plugin-development.md#固定した開発環境)、projectのAndroid SDKとUSB接続したAndroid端末です。プラグインの配布ZIPには取得用server・時計測定・動画処理とCLIの入口が含まれます。利用者向けのPython、scrcpy、ffmpegの別途導入は不要です。

1. 配布ZIPのプラグインを導入し、Android Studioで対象projectを開きます。
2. Android Replay Bufferの設定で、保存先・保持時間・取得モード・対象アプリを確認します。
3. ToolWindowで取得をONにし、必要な場面で保存します。保存済みフォルダと、不足・失敗の説明を確認してください。

第2版候補の「スクリーンショットを保存」は取得OFFでも使えます。取得中は録画対象の端末、OFFでは接続済みの1台で選択アプリを前面に表示して押すと、設定済みフォルダの直下へ現在画面のPNGを1枚保存します。保存先の既定はAndroid StudioのLogcatスクリーンショット設定へ追従します。設定で手動のフォルダにも切り替えられ、動画・ログと同じ場所も指定できます。Logcat設定を取得できない場合や保存先に不備がある場合は理由を表示し、別の場所へ切り替えません。保存完了通知から「保存場所を開く」「保存したファイルを削除」を操作できます。撮影中は画面を切り替えないでください。通知・キーボードも写り、保護領域は空白になる場合があります。対応条件・未受入範囲は[スクショ仕様](docs/design/ide-ui.md#第2版の独立スクリーンショット325)と[受入Case](docs/verification/changes/issue-325.json)を参照してください。

ターミナルやエージェントからは、導入済みプラグインの`bin/replay`（Windowsは`bin\replay.cmd`）で同じ設定・取得・保存を操作できます。正確な場所は設定画面の「CLIの場所」からコピーし、絶対パスで呼ぶか`bin`をPATHへ追加します。

```bash
replay --project "$HOME/Projects/MyApp" start
replay --project "$HOME/Projects/MyApp" status
replay --project "$HOME/Projects/MyApp" save
replay --project "$HOME/Projects/MyApp" stop
replay --project "$HOME/Projects/MyApp" settings set replay_seconds=60
```

`save`は保存完了後のフォルダを返します。`start`は取得をONにして終了し、IDEが取得を続けます。IDEを暗黙起動する機能はありません。全コマンドと終了コードは[CLI](docs/cli.md)を確認してください。エージェント向けは[MCPサーバー](docs/mcp.md)です。

## 開発する

製品実装は`plugin/`にあります。JDK 27（`JAVA_HOME`）とAndroid SDKの`android-37.2`（`ANDROID_HOME`）を用意して、次を実行します。

```bash
python3 scripts/workflow/plugin_check.py
```

開発はDevinが担当します。進め方は[AGENTS.md](AGENTS.md)、旧運用の保管と戻し方は[docs/legacy-ops](docs/legacy-ops/README.md)を参照してください。製品契約は[要件](docs/requirements.md)・[保存設計](docs/plugin-design.md)・[受入](docs/verification/README.md)、固定toolchainは[プラグイン開発](docs/plugin-development.md)にあります。
