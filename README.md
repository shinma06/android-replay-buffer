# Android Replay Buffer

Android操作直前の画面・logcat・時刻情報を保持し、不具合を見つけた時点で保存するAndroid Studioプラグインです。ToolWindowと同梱の`replay` / `replayd`が、同じprojectの設定・バッファ・保存処理を使います。動画とログを取得するモードと、動画を開始しないログ専用モードがあります。

このbranchの第2版候補と、受入・main反映済みの範囲は区別します。[プロジェクト情報](docs/project.md)と[#280](https://github.com/shinma06/android-replay-buffer/issues/280)が現在地です。初期版のAgent段階は完了済みですが、[QA #65](https://github.com/shinma06/android-replay-buffer/issues/65)のREAL・人間確認、元の失敗・未確認は引き継ぎます。新しいbuildやJVMテストの成功だけでIDE・端末受入済みとは扱いません。

## 利用する

対象はmacOS、[固定対応のAndroid Studio](docs/plugin-development.md#固定した開発環境)、projectのAndroid SDKとUSB接続したAndroid端末です。プラグインの配布ZIPには取得用server・時計測定・動画処理とCLIの入口が含まれます。利用者向けのPython、scrcpy、ffmpegの別途導入は不要です。

1. 配布ZIPのプラグインを導入し、Android Studioで対象projectを開きます。
2. Android Replay Bufferの設定で、保存先・保持時間・取得モード・対象アプリを確認します。
3. ToolWindowで取得をONにし、必要な場面で保存します。保存済みフォルダと、不足・失敗の説明を確認してください。

第2版候補の「スクリーンショットを保存」は取得OFFでも使えます。USB端末を1台接続し、選択アプリを端末の前面に表示して押すと、設定済みの保存先へ現在画面のPNGを保存します。保存完了通知から「保存場所を開く」「保存したファイルを削除」を操作できます。撮影中は画面を切り替えないでください。通知・キーボードも写り、保護領域は空白になる場合があります。対応条件・未受入範囲は[スクショ仕様](docs/design/ide-ui.md#第2版の独立スクリーンショット325)と[受入Case](docs/verification/changes/issue-325.json)を参照してください。

ターミナルから操作する場合は、同じZIPを展開したプラグインフォルダの`bin`をPATHへ追加します。

```bash
replay --project "$HOME/Projects/MyApp" daemon
replay --project "$HOME/Projects/MyApp" status
replay --project "$HOME/Projects/MyApp" save
replay --project "$HOME/Projects/MyApp" stop
```

`save`は保存完了後のフォルダを返します。`daemon`と`replayd`は取得をONにして終了し、IDEが取得を所有します。IDEを暗黙起動する機能はありません。旧Python CLIからの設定・常駐動作・保存形式・終了コードの移行と復旧は[同梱CLIの案内](docs/cli.md)を確認してください。

## 開発する

製品実装は`plugin/`へ統合しています。Pythonはrepositoryの管理・検査用です。[開発手順](docs/workflow.md)と[セットアップ](docs/setup/README.md)に従い、実在するIssueの専用branch/worktreeで作業します。製品変更はdevelop、固定した検証済み候補はmainへ統合します。

```bash
python3 scripts/bootstrap.py
python3 scripts/check.py
# JDK 27 / JAVA_HOME と Android SDK API 37.2 revision 1 / ANDROID_HOMEを設定する
python3 scripts/workflow/product_check.py
```

固定toolchain・配布物のbuildは[プラグイン開発](docs/plugin-development.md)、製品契約は[要件](docs/requirements.md)・[保存設計](docs/plugin-design.md)・[受入](docs/verification/README.md)を参照してください。原型のsourceと復元手順は[Git履歴](docs/cli-origin.md)に残しています。
