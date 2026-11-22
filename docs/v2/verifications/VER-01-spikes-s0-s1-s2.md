---
id: VER-01
type: verification
name: 初期スパイク S0 / S1 / S2 実測結果と未検証項目
verifies:
  - SPEC-01
  - SPEC-02
  - SPEC-03
  - SPEC-04
  - REQ-05
  - REQ-07
  - REQ-08
  - REQ-11
  - REQ-14
---

# VER-01: 初期スパイク S0 / S1 / S2 実測結果と未検証項目

記録日: 2026-11-22（macOS 27.3.0 / Apple M5 arm64 / `rustc 1.100.0` / `clippy 0.1.100`）

## 1. Spike S0: 既存リポジトリ読取専用監査（`develop` @ `4acbc01`、`main` @ `6c84530` / `v0.2.0`）

- **旧保存フォーマット確定**:
  - Schema 4（`develop`）: `replay-<yyyyMMdd-HHmmss>-<manual|crash>[-logs]-<8hex>`、`replay.json`（`schema: 4`）、`video.mp4`、`video-frames.jsonl`、`logcat-device.jsonl`、`logcat-app.jsonl`、任意 `touches.jsonl`。具体フィクスチャ `plugin/src/test/resources/replay-trim-parity/replay-20240101-000000-manual-feedface/` を確認。
  - Schema 3（`main` / `v0.2.0`）: `replay-<UTC-ISO>-<UUID>`、`session.json`、`video-001.mp4`、`frames.jsonl`、`logcat-device.jsonl`、`logcat-app.jsonl`。
- **端末キャプチャ基盤確認**: `scrcpy-server-v5.0.1`（Apache 2.0）＋ `AudioRecordReader.java` パッチ、`app_process` 起動、API >= 33 での `audio_source=playback audio_dup=true`（API 36 エミュレータ実測中央値 ~97 ms 遅延）。
- **Entire / CI 構成確認**: `.entire/settings.json` が専用リモート `shinma06/entire-checkpoints` を指定（`origin` へはプッシュしない運用）。

## 2. Spike S1: GPL 非依存 Rust H.264 復号・先頭 GOP トリム実測（macOS arm64）

テスト素材: 1920x1080 30 fps 30 s（900 フレーム、High L4.1 `bf=0`、バーコード焼き込み）、N = 7 回中央値（p95）。

| 計測項目 | 合格目標 | OpenH264 0.9.10 (BSD-2-Clause) | VideoToolbox 直接 FFI | 判定 (C1 / C2) |
| --- | ---: | ---: | ---: | --- |
| 1s GOP フィルムストリップ（マルチスレッド） | <= 1000 ms | 84.48 ms (86.15 ms) | 37.41 ms (42.02 ms) | PASS / PASS |
| 1s GOP 中間シーク `f=47` + 1080p RGB24 | <= 200–500 ms | 96.26 ms (96.35 ms) | 47.44 ms (52.96 ms) | PASS / PASS |
| 1s GOP 最悪シーク `f=59` + 1080p RGB24 | <= 200–500 ms | 150.51 ms (152.75 ms) | 83.06 ms (84.98 ms) | PASS / PASS |
| 1s GOP トリム `[47..=647]` (601f) | <= 数秒 | 561.30 ms (562.76 ms) | 132.03 ms (138.30 ms) | PASS / PASS |
| 5s GOP 最悪シーク `f=149` + 1080p RGB24 | <= 200–500 ms | 712.59 ms (715.14 ms) | 332.10 ms (333.78 ms) | MISS / PASS |
| 5s GOP トリム `[47..=647]` (601f) | <= 数秒 | 3,921.74 ms (3,936.64 ms) | 522.79 ms (525.42 ms) | BORDERLINE / PASS |
| GOP-LRU コマ送り `+1` / `-1` | 体感遅延なし | 4.65 ms / < 0.01 ms | 1.47 ms / < 0.01 ms | PASS / PASS |

- **ゼロ `elst`・開始前フレーム 0 枚証明**: 出力 4 本すべて `edts=0, elst=0, avc1=1, avcC=1`、`pkt[0]` = `pts=0 (K__)`、601/601 全フレームのバーコード一致を `ffmpeg` SW `h264`、`ffmpeg -hwaccel videotoolbox`、Apple `AVFoundation` `AVAssetReader` の 3 系統で検証 PASS。
- **重大発見と解決**: 異種プロファイル結合において単一 `stsd` `avc1` ＋ IDR 境界インバンド SPS/PPS が全 3 デコーダを通過（二重 `stsd` は VideoToolbox 失敗、`avc3` は `AVAssetReader` 失敗）。OpenH264 の 1 フレーム内部 DPB 遅延は `flush_remaining()` で解消。
- **依存ライセンス監査**: 全 20 クレートおよび `otool -L` 動的リンク先ともに GPL/AGPL ゼロ（UEFI 条件付き `r-efi` は `MIT OR Apache-2.0` 条項を選択）。

## 3. Spike S2: macOS 二重時計・ローカルエミュレータ・BeautifulRust / sara 実測

- **macOS 二重時計（darwin/arm64）**:
  - `CLOCK_UPTIME_RAW`（平均 32.58 ns、分解能 42 ns）と `CLOCK_MONOTONIC_RAW`（平均 16.16 ns、分解能 42 ns）が `mach_absolute_time` / `mach_continuous_time`（timebase 125/3 = 24 MHz）と一致。
  - ブート以降の累積スリープ `CLOCK_MONOTONIC_RAW - CLOCK_UPTIME_RAW = 5,433.569 s`（1.51 h）を確認。
  - `CLOCK_MONOTONIC` は `-4.365 s` の周波数調整乖離と 1000 ns 粒度のため使用不可と確定。
- **ローカル Android SDK / AVD**: Hypervisor.Framework OS X 27.3 有効、`system-images;android-37.0;google_apis;arm64-v8a` 導入済み、AVD `arb_api37_arm64`（`medium_phone`, `1080x2400`）を作成・確認済み。
- **品質ツール**: `BeautifulRust` lints が `clippy 0.1.100` で 30/30 期待通り発火。`sara 0.12.3` を `--no-default-features --locked` でローカル導入し、カスタム 3-type `model.yaml` による厳密検査を確認済み。

## 4. 明示的な未検証項目（Unverified — 今後フェーズで検証）

- **Windows x86_64 / Linux x86_64 実測**: `openh264`（`nasm` 有効）の速度計測、および OS HW コーデック（Media Foundation / VA-API）要否判断は未実施。
- **クロスプラットフォームプレイヤー互換**: 単一 `stsd` `avc1` ＋ インバンド SPS/PPS 結合 MP4 の Windows Media Player / Chrome / VLC / Android ExoPlayer での再生検証は未実施（Devin マルチ OS 検証で実施予定）。
- **実機物理スリープ計測**: macOS 実機での制御されたフタ閉じスリープ差分、および Windows / Linux 二重時計の実測は未実施。
- **Android 実機キャプチャと B フレーム**: 実機 FHD30 長時間録画、実機 A/V 同期誤差、および B フレーム（`ctts`）を出力する OEM エンコーダ実機検証は未実施。
