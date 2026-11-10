# VMでのIDE・エミュレータ受入

`develop` → `main` のリリース前に、Devin VM上のAndroid StudioとAndroid Emulatorで、配布ZIPが実際に動くことを確かめます。人の実機確認は不要です。テストの合格はこの受入の代わりになりません。

## 対象

- 配布物: `develop` 先頭のCIが作ったプラグインZIP（`gh run download` で取得）
- IDE: `plugin/build.gradle.kts` の `androidStudio(...)` と同じAndroid Studio
- 端末: Android Emulator（headless）
- project: Android Studioの新規project（Empty Activity）。Gradle syncが通り、アプリを端末へinstallしておく

## 環境の準備

1. エミュレータを `-no-window` で起動し、`adb devices` で `device` になるまで待つ
2. `Xvfb :99 -screen 0 1920x1080x24` を起動し、`DISPLAY=:99` でStudioを起動する。既定の画面ではOpenGLの初期化に失敗することがある
3. 日本語のUIを読むため、`fonts-noto-cjk` を入れてからStudioを起動する
4. ZIPをStudioのplugin置き場（Linuxでは `~/.local/share/Google/AndroidStudio<版>/`）へ展開する
5. Maven Centralが429を返す場合は、VMだけの `~/.gradle/init.d/` のinit scriptでGoogleのミラーへ向ける。リポジトリには入れない
6. 保存先はprojectの `.idea/workspace.xml` の `ReplaySettingsStore` に `destination` として入れる。Studioを止めている間だけ編集する
7. メモリが足りないため、Gradle syncのあとはGradle daemonを止める

## 確認する項目

| 項目 | 確認方法 | 合格の条件 |
|---|---|---|
| 読み込み | `idea.log` | `Loaded custom plugins` に配布ZIPの版が出る |
| ToolWindow | Find Actionで「Android Replay Buffer」を開く | 状態と操作ボタンが日本語で表示される |
| 取得ON・端末 | `bin/replay --project <dir> daemon` のあと `status` | `capture_state` が `CAPTURING`、`device_serial` がエミュレータ |
| 保存（CLI） | `bin/replay --project <dir> save` | 終了コード0。出力フォルダにMP4・`logcat-device.jsonl`・`logcat-app.jsonl`・`session.json`・`README.txt` |
| 保存（GUI） | ToolWindowの「直前180秒を保存」 | 新しい保存フォルダが同じ構成でできる |
| 動画 | `ffprobe` | H.264で、長さが0より大きい |
| スクリーンショット | ToolWindowの「スクリーンショット」 | 保存先にPNGができる |

`bin/replay` には `REPLAY_STUDIO_HOME` にStudioの導入先を指定します。取得を始めた直後の保存は、時計の対応が未確定で「記録の不足・未確認」になることがあります。数十秒取得してから保存し、`session.json` の `coverage` が動画・両ログとも `CAPTURING` であることを確かめます。

## 結果の扱い

- 録画・logcat・スクリーンショットは公開しない。報告には項目ごとの合否、版、件数・形式だけを書く
- この受入で確かめていないもの（USB接続の実機、macOS・WindowsのIDE）は、報告に未確認として書く
