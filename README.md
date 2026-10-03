# Android Replay Buffer for macOS

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

現行製品は上記のPython CLIです。今後のAndroid Studioプラグイン開発に向け、[開発手順](docs/workflow.md)、[プロジェクト情報](docs/project.md)、[共通IDE知見](docs/android-studio.md)を入口にします。製品変更はdevelop、検証済み候補はmainへ統合します。

- [開発マップ](https://github.com/users/shinma06/projects/5) / [Issue](https://github.com/shinma06/android-replay-buffer/issues)
- [初回導入の検証・未実施項目](docs/validation.md) / [ハーネスの出典・対応表](docs/inventory.md)

管理ツールはPython 3.11以上を使用します。CLIのPython 3.9以上という条件は維持します。

```bash
python3 scripts/bootstrap.py
python3 scripts/check.py
python3 scripts/workflow/product_check.py
```
