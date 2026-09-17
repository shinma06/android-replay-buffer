# Android Replay Buffer for macOS

## 1. Overview

Android実機開発時のデバッグ効率を上げるための、macOS向けローカル開発ツール。

Android実機をMacへUSB接続すると、自動的に以下を開始する。

* Android画面のキャプチャ
* 直近N秒のReplay Buffer保持
* `logcat` の継続取得
* 動画とログを共通タイムライン上で同期

開発者が不具合を発見したタイミングで保存操作を行うと、その直前N秒間の動画と対応するログを保存する。

プロダクトとして配布することよりも、日常のAndroid実機開発で「気軽に使えること」を最優先する。

---

# 2. Primary KPI

最重要KPIは以下。

> Android実機をMacにUSB接続すると、ユーザー操作なしで録画とログ取得が開始される。

特に重要なのは、録画開始操作を要求しないこと。

理想的な開発フロー：

```text
AndroidをUSB接続
        ↓
自動検出
        ↓
Replay Buffer開始
        +
logcat取得開始
        ↓
通常通り開発・操作
        ↓
不具合発生
        ↓
Save Replay
        ↓
直前N秒の動画＋ログを取得
```

---

# 3. Design Principles

## Zero Interaction

Android接続後の録画開始操作をなくす。

```text
USB Connect
    ↓
ADB Device Detection
    ↓
Recording
```

を自動化する。

## Lightweight

開発効率化ツールなので、大規模なGUIや複雑な設定システムは不要。

最初はCLI + daemonで成立させる。

## Mac as Host

Replay Bufferおよびログ管理はMac側で行う。

Android側は可能な限り既存のADB機能を利用し、専用AndroidアプリのインストールはMVPでは避ける。

## Timeline First

動画とログを別々の成果物として扱うのではなく、同じ時間軸上のデバッグ情報として扱う。

---

# 4. MVP Requirements

## Device Detection

Mac側でADBデバイスの接続状態を監視する。

Android実機がUSB接続され、ADBから利用可能になったら自動的にReplay Sessionを開始する。

切断された場合はSessionを終了する。

再接続された場合は自動的に新しいSessionを開始する。

---

## Screen Capture

Android画面をMac側へストリーミングする。

候補技術：

* ADB
* scrcpy
* H.264 stream
* ffmpeg

MVPでは既存ツールを積極的に利用し、独自Android screen capture implementationは避ける。

画面表示そのものは必須ではない。

目的はMac上でAndroid画面を見ることではなく、Replay Bufferを取得すること。

---

# 5. Replay Buffer

常に直近N秒間の画面を保持する。

例：

```text
Current Time
     ↓
──────────────────────────────
        Last 60 seconds
──────────────────────────────
```

Nは設定可能にする。

初期値候補：

```yaml
replay_seconds: 60
```

想定値：

```text
30 sec
60 sec
120 sec
```

MVPでは秒数以外の細かい録画設定をユーザーへ露出させる必要はない。

---

# 6. Buffer Implementation

可能であればAndroidから受信したH.264等の圧縮ストリームを、デコードせず保持する。

```text
Android
   │
   │ H.264
   ▼
Mac
   │
   ▼
Encoded Ring Buffer
```

これにより、

* CPU負荷
* GPU負荷
* Disk I/O

を抑える。

Replay保存時のみMP4等へmuxする。

ただしMVPでは実装速度を優先し、ffmpeg等によるsegment方式で十分。

例：

```text
segments/

000123.mp4
000124.mp4
000125.mp4
000126.mp4
```

古いsegmentを削除することで擬似Ring Bufferとして扱う方式でもよい。

---

# 7. Log Capture

画面キャプチャ開始と同時に、

```bash
adb logcat
```

を開始する。

ログについてもReplay Bufferと同じ時間範囲を保持する。

重要なのは、

> 動画とlogcatを同じタイムラインへ紐付ける

こと。

単純に動画と巨大なlogcatファイルを保存するだけにはしない。

---

# 8. Unified Timeline

内部では共通のmonotonic clockを基準として扱う。

概念：

```text
-60 sec                          0
  │                              │
  ├──────── Video ───────────────┤
  ├──────── logcat ──────────────┤
  │                              ▲
  │                           SAVE
```

Save操作を行った瞬間を、

```text
t = 0
```

として扱う。

Replay内の情報は、

```text
-60s ～ 0s
```

として表現できる。

---

# 9. Timeline Event Format

将来的なViewer等を考慮し、ログは構造化可能な形式にしておく。

例：

```json
{
  "replay_time_ms": -2580,
  "device_time_ms": 1789612938421,
  "source": "logcat",
  "level": "ERROR",
  "tag": "AndroidRuntime",
  "message": "FATAL EXCEPTION: main"
}
```

最低限保持したい情報：

```text
timestamp
replay relative timestamp
source
level
tag
message
```

---

# 10. Replay Save

ユーザーが問題を発見したら、

```bash
replay save
```

を実行する。

Save時点から直前N秒間を保存する。

例：

```text
replays/
└── 2026-09-17_11-42-18/
    ├── replay.mp4
    ├── logcat.txt
    └── timeline.json
```

---

# 11. Output

## replay.mp4

直前N秒間のAndroid画面。

## logcat.txt

同じ時間範囲のraw logcat。

人間が直接確認する用途。

## timeline.json

動画とログを関連付ける構造化データ。

将来的なViewerや解析ツールから利用する。

---

# 12. Example Timeline

例えばReplay動画のある地点でクラッシュが発生した場合：

```text
-00:00.608  D/API      POST /checkout
-00:00.316  D/Payment  startPayment()
-00:00.029  E/App      IllegalStateException
 00:00.000  [SAVE]
```

あるいはReplay開始を0とするViewerでは：

```text
00:36.812  D/API      POST /checkout
00:37.104  D/Payment  startPayment()
00:37.391  E/App      IllegalStateException
00:37.420  [VIDEO EVENT]
00:37.443  E/Android  FATAL EXCEPTION
```

内部データとUI表現は分離してよい。

---

# 13. Process Architecture

MVPではMac側に小さなdaemonを置く。

```text
                Android
                   │
                   │ USB
                   ▼
                  ADB
                   │
          ┌────────┴────────┐
          │                 │
          ▼                 ▼
    Screen Stream        logcat
          │                 │
          ▼                 ▼
    Video Buffer        Log Buffer
          │                 │
          └────────┬────────┘
                   │
                   ▼
              Timeline
                   │
                   ▼
              Save Replay
```

---

# 14. CLI

常駐プロセス：

```bash
replayd
```

起動例：

```text
$ replayd

Waiting for Android device...

Pixel connected
● Recording
Replay: 60 sec
Logs: logcat
```

ユーザー操作：

```bash
replay save
```

最低限これだけで成立させる。

---

# 15. Configuration

設定項目は極力少なくする。

MVPではReplay秒数だけでもよい。

例：

```yaml
replay_seconds: 60
```

必要になった場合のみ追加する。

候補：

```yaml
replay_seconds: 60
output_directory: ~/Replays
```

FPS、bitrate、codec等を最初からユーザー設定にする必要はない。

---

# 16. Automatic Startup

最終的にはMacログイン時に`replayd`を起動しておく。

```text
Mac Login
    ↓
replayd
    ↓
Waiting for Android...
```

その状態でAndroidを接続すると、

```text
Android Connected
        ↓
Replay automatically starts
```

となる。

macOSではLaunchAgent等を利用できる。

---

# 17. MVP Technology Candidates

Mac側：

```text
ADB
scrcpy
ffmpeg
shell / Python / Go / Rust etc.
```

まずは開発速度を優先する。

特に、

```text
adb
scrcpy
ffmpeg
```

を既存dependencyとして利用することで、screen capture部分を自作しない構成を優先する。

daemon本体については、長期運用するならGo/Rust等へ移行できるが、MVP段階では実装速度を優先してよい。

---

# 18. Non-Goals for MVP

以下はMVPでは不要。

* Android専用アプリ
* 高機能GUI
* 動画編集
* Cloud upload
* Team collaboration
* Account system
* Replay library UI
* 高度なcodec設定
* Android画面のremote control
* AIによるログ解析
* Crash reporting SaaS連携

目的はあくまで、

> 「今起きたバグ、もう一回再現しないと確認できない」

を減らすこと。

---

# 19. Possible Phase 2

MVPが日常的に使えることが確認できたら、macOS Menu Bar UIを追加する。

例：

```text
● Pixel / 60s
```

メニュー：

```text
Save Last 60 Seconds

Replay Length
✓ 60 sec
  30 sec
  120 sec

Open Replays

Device
Pixel
```

重要なのは、GUIを追加しても「録画開始」ボタンは基本的に作らないこと。

接続 = 録画開始というモデルを維持する。

---

# 20. Future Timeline Viewer

将来的には以下のようなViewerが考えられる。

```text
┌──────────────────────────────────────────────┐
│                 Android Video                │
│                                              │
│                                              │
├──────────────────────────────────────────────┤
│ ◀────────────── Timeline ─────────────────▶ │
│        ▲ API       ▲ Error      ▲ Crash      │
├──────────────────────────────────────────────┤
│ 12:41:31 D/API       POST /checkout          │
│ 12:41:32 D/Payment   startPayment()           │
│ 12:41:33 E/App       IllegalStateException    │
└──────────────────────────────────────────────┘
```

動画をseekすると、同じ時刻のlogcatへ移動する。

ただしこれはMVPの必須要件ではない。

`timeline.json`を最初から生成しておけば、後からViewerを追加できる。

---

# 21. MVP Definition of Done

以下を満たせば最初のバージョンは完成とする。

1. `replayd`がMac上で常駐できる
2. USB接続されたADB Android端末を自動検出する
3. 接続されたら画面キャプチャが自動開始する
4. 同時にlogcat取得が自動開始する
5. 直近N秒だけを保持する
6. N秒を設定できる
7. `replay save`で直前N秒を保存できる
8. `replay.mp4`が生成される
9. 同時間帯の`logcat.txt`が生成される
10. 動画とログを関連付ける`timeline.json`が生成される
11. Androidを切断してもdaemonが死なない
12. Androidを再接続すると自動的に録画が再開する

---

# 22. Core Product Principle

このツールで最も重要なのはReplay Bufferそのものではなく、

```text
Androidを接続する
```

という既存の開発行為だけで、

```text
「何か起きたら直前まで戻れる状態」
```

が自動的に作られること。

したがって、新しい操作を増やす機能よりも、

```text
接続検出の安定性
録画開始の速さ
Replayの確実な保存
動画とログの時間同期
常駐時の低負荷
```

を優先する。

最終的な理想UXは、

```text
Plug in → Forget about it → Bug happens → Save replay
```

である。
