# Android Replay Buffer

昇格状況の以下の記載は2026-10-13のmain `f8ec628` 時点です。昇格後の実状態は[#87](https://github.com/shinma06/android-replay-buffer/issues/87)・[#10](https://github.com/shinma06/android-replay-buffer/issues/10)で読み戻してください。

開発対象はAndroid Studioプラグインです。[固定develop候補 `3a9af24`](https://github.com/shinma06/android-replay-buffer/tree/3a9af24231c3db339c0b3a82ba8aa8eb4cecb189/plugin/)に取得・保存コア、ToolWindow、設定を実装しています。初期Agent範囲105/105件は[QA #27で正式採用](https://github.com/shinma06/android-replay-buffer/issues/27#issuecomment-6136845141)済みですが、完全GUI受入と製品のmain昇格は未完了です。REAL34・人間確認は[QA #65](https://github.com/shinma06/android-replay-buffer/issues/65)、昇格は[#87](https://github.com/shinma06/android-replay-buffer/issues/87)で追跡します。現在の事実とチェックは[プロジェクト情報](docs/project.md)を参照してください。

以下は互換性を維持するmacOS向けPython CLI原型の利用手順です。プラグインの導入条件・機能・受入とは区別します。[凍結CLI文書](https://github.com/shinma06/android-replay-buffer/blob/3a9af24231c3db339c0b3a82ba8aa8eb4cecb189/docs/cli-origin/README.md)と[保全方針](https://github.com/shinma06/android-replay-buffer/blob/3a9af24231c3db339c0b3a82ba8aa8eb4cecb189/docs/cli-origin.md)を参照してください。移設後のローカル文書は `docs/cli-origin/` にあります。

Android 実機を USB 接続すると、操作なしで画面キャプチャと logcat 取得を開始し、直近 N 秒をリングバッファとして保持する開発ツール。

不具合発見時に `replay save` を実行すると、直前 N 秒の動画・logcat・タイムライン JSON を保存する。

## 前提

- macOS
- Python 3.9+
- [Android platform-tools](https://developer.android.com/tools/releases/platform-tools) (`adb`)
- [scrcpy](https://github.com/Genymobile/scrcpy) 2.4+
- [ffmpeg](https://ffmpeg.org/)

```bash
brew install scrcpy ffmpeg
```

## セットアップ

```bash
cd ~/Develop/android-replay-buffer

# bin を PATH に追加
export PATH="$HOME/Develop/android-replay-buffer/bin:$PATH"
```

設定（任意）:

```bash
mkdir -p ~/.config/replay-buffer
cp config.json.example ~/.config/replay-buffer/config.json
```

## 使い方

### 1. daemon 起動

```bash
replayd
```

出力例:

```text
Waiting for Android device...

Pixel connected
● Recording
Replay: 60 sec
Logs: logcat
```

Android を USB 接続すると自動で録画と logcat 取得が始まる。切断すると session を終了し、再接続で再開する。

### 2. Replay 保存

別ターミナルで:

```bash
replay save
```

`replayd` が未起動なら自動起動する。

保存先例:

```text
~/Replays/2026-09-17_11-42-18/
├── replay.mp4
├── logcat.txt
└── timeline.json
```

### その他コマンド

```bash
replay status   # 状態確認
replay stop     # daemon 停止
```

## 設定

`~/.config/replay-buffer/config.json`:

```json
{
  "replay_seconds": 60,
  "output_directory": "~/Replays",
  "segment_seconds": 5
}
```

YAML も利用可能（`config.yaml`）。

## Mac ログイン時の自動起動

```bash
sed "s|__REPLAY_BUFFER_ROOT__|$HOME/Develop/android-replay-buffer|g; s|__HOME__|$HOME|g" \
  launchd/com.selfregi.replayd.plist > ~/Library/LaunchAgents/com.selfregi.replayd.plist
launchctl load ~/Library/LaunchAgents/com.selfregi.replayd.plist
```

ログ: `~/.replay-buffer/replayd.log`

## アーキテクチャ

```text
Android ──USB──▶ ADB ──┬── scrcpy ──▶ ffmpeg segments ──▶ Video Buffer
                       └── logcat ───────────────────────▶ Log Buffer
                                      │
                                      ▼
                                 replay save
                                      │
                    replay.mp4 / logcat.txt / timeline.json
```

## MVP Definition of Done

- [x] `replayd` が Mac 上で常駐できる
- [x] USB 接続 ADB 端末を自動検出する
- [x] 接続時に画面キャプチャを自動開始する
- [x] 同時に logcat 取得を自動開始する
- [x] 直近 N 秒だけを保持する
- [x] N 秒を設定できる
- [x] `replay save` で直前 N 秒を保存できる
- [x] `replay.mp4` / `logcat.txt` / `timeline.json` を生成する
- [x] 切断しても daemon が継続する
- [x] 再接続で録画を再開する

## 開発ハーネス・Android Studioプラグイン開発

Python CLI原型を維持し、Android Studioプラグインを開発します。[開発手順](docs/workflow.md)、[プロジェクト情報](docs/project.md)、[仕様の正本と任意の外部知見](docs/android-studio.md)を入口にします。製品変更はdevelop、固定した検証済み候補はmainへ統合します。初期版完成後は [#206 stable基盤更新](https://github.com/shinma06/android-replay-buffer/issues/206) → [#103 CLI内包](https://github.com/shinma06/android-replay-buffer/issues/103) の順です。[過去管理欄の整備 #237](https://github.com/shinma06/android-replay-buffer/issues/237)は初期版後に行います。

- [開発マップ](https://github.com/users/shinma06/projects/5) / [Issue](https://github.com/shinma06/android-replay-buffer/issues)
- [初回導入の検証・未実施項目](docs/validation.md) / [ハーネスの出典・対応表](docs/inventory.md)

repositoryの検査・Git hooks・QA/受入・GUI lease・coordinatorなどの管理/開発ツールはPython 3.15以上を対応対象とし、主検査はstable 3.15.0を使用します。旧3.11〜3.14環境で管理作業を行う場合は[セットアップ手順](docs/setup/README.md#runtimeと配置)に従って3.15を選択してください。配布CLIの利用条件はPython 3.9以上のままです。

```bash
python3 scripts/bootstrap.py
python3 scripts/check.py
python3 scripts/workflow/product_check.py
```

プラグイン変更では、昇格後の `plugin/` と checker が存在するツリーで `python3 scripts/workflow/plugin_check.py` を実行します（JDK 25 / JAVA_HOMEが必要）。プラグインがまだないmainでbuild成功や安全なskipを主張しません。
