---
id: VER-04
type: verification
name: Device Agent（arb-companion / scrcpy-server 再利用）API 37 エミュレータ実機検証結果
verifies:
  - SPEC-03
  - REQ-03
  - REQ-12
  - REQ-13
  - REQ-14
---

# VER-04: Device Agent（`arb-companion` / `scrcpy-server` 再利用）API 37 エミュレータ実機検証結果

記録日: 2026-11-22（macOS arm64 / Android SDK `android-37.0` + `build-tools/37.0.0/d8` / ローカル AVD `arb_api37_arm64`）

## 1. 成果物とビルド・ホスト単体テスト検証

- **ワイヤプロトコル仕様**: [PROTOCOL.md](../../device-agent/PROTOCOL.md)（20 byte Little-Endian 固定ヘッダ `magic="AR"`, `version=1`, `stream_id`, `msg_type`, `flags`, `reserved`, `seq: u32`, `payload_len: u64`、全 9 メッセージペイロード、および Apache-2.0 `scrcpy-server-v5.0.1` トンネルフレーミングを定義）。
- **軽量 DEX コンパニオン (`device-agent/companion/`)**: 純 Java（min API 24・外部実行時依存 0）。`device-agent/build.sh` により `arb-companion.dex.jar`（22,324 bytes）を決定論的に生成。
- **Apache-2.0 `scrcpy-server-v5.0.1` ＋ パッチ済み `AudioRecordReader` 再利用**: `device-agent/build.sh --with-scrcpy-audio` が `plugin/src/main/resources/replay/deps/scrcpy-server-v5.0.1` の SHA-256（`764eb6f79811d5211fe9df341120882ba9994c7a61b897d7bf3fb662e53bc536`）を照合し、初フレーム PTS 修正済み `AudioRecordReader` を `classes.dex` として結合した `replay-scrcpy-audio.dex.jar`（735,101 bytes）を生成（ライセンス表記: [NOTICE.md](../../device-agent/NOTICE.md)）。
- **純 Java ホスト単体テスト (`ProtocolTest`)**: 13/13 PASS（20 byte ヘッダ符号化・復号、異常 magic 拒否、ストリーム別単調連番、全 9 メッセージの往復検証、`logger_entry` 20/24/28 byte ヘッダ解析、`logcat -S` 統計・`Unverified`/`Verified`/`OverflowDetected` 判定、`getevent -lp`/`-lt` 解析）。

## 2. ローカル Android API 37 エミュレータ（`arb_api37_arm64`）ライブ実測結果

検証コマンド: `python3 device-agent/scripts/emulator_smoke.py --check-scrcpy --pings 20`（root なし・APK インストールなし・通常 ADB shell 経由の `app_process` 起動）。

| 検証項目 | 実測結果（`arb_api37_arm64`, API 37） | 判定 |
| --- | --- | --- |
| `boot_id` 読取可否（SELinux 確認） | `/proc/sys/kernel/random/boot_id` は通常 ADB shell から読取可能（`rc=0`, UUID `c729eaec-5428-4a38-b957-441109da33f7`、`boot_id_source = KernelProcBootId (0)`、`boot_count = 1`）。フォールバック用の `settings get global boot_count` および `/proc/stat btime` も読取可能を確認。 | PASS |
| `Hello` ハンドシェイク | `api=37`, `model=sdk_gphone64_arm64`, `manufacturer=Google`, `is_emulator=True`, `heartbeat_interval_ms=250` を接続直後に受信し、`boot_id` が端末直接読取と一致。 | PASS |
| `Heartbeat` 3 時計トリプル（250 ms 周期） | 24 回連続受信。全サンプルで `boottime_ns`・`monotonic_ns` の単調非減少と `boottime_ns >= monotonic_ns` を満たす。障害注入区間を含む最大間隔 `255.6 ms`。 | PASS |
| `Ping` / `Pong` 時刻同期 RTT（20 回連続） | 最小 `0.427 ms` / **中央値 `0.516 ms`** / **p95 `0.707 ms`** / 最大 `0.789 ms`。端末側 3 時計読取ブラケット（`boottime_after - boottime_before`）中央値 `0.4 µs`。`REQ-12` の同期目標（0.1 秒前後）に対し十分な余裕（p95 半幅誤差上限 `~0.35 ms`）。 | PASS |
| ライブ `logcat -b all -B -T 1` 読取 | 障害注入前に実エミュレータログ 104 件を `device_read_boottime_ns` 打刻付きで受信し、注入マーカー `ARB_SMOKE` の到達を確認。 | PASS |
| `logcat -b all -S` 取りこぼし監視（`REQ-03` 誠実判定） | 8 バッファ（`main`, `radio`, `events`, `system`, `crash`, `stats`, `security`, `kernel`）を解析成功。起動直後のベースラインは起動時からの既存プルーニング（`main:2023`）があるため誠実に `Unverified` を送出、続く 5 秒周期プローブでは `pruned_delta = 0`（`total_delta = 103`, `reader_delta = 104`）を実測し `Verified` を送出。 | PASS |
| ライブ障害隔離証明（`logcat` への `SIGKILL` 注入） | 稼働中の `logcat -b all -B -T 1`（PID `4821`）へ `kill -9` を送信。Stream 1 は `Error(process_exit:137)` ＋ `LogOverflowStats(OverflowDetected)` → `Restarting(restart_count=1, backoff_ms:500)` → `Starting(respawning)` → `Running(restarted)` へ自動復帰し、`FLAG_RECOVERED` 付きフレームおよび復帰後マーカー `marker2` を受信。その間も Stream 0 ビーコンは途切れなし（最大間隔 `255.6 ms`、kill 直後の `Ping`/`Pong` RTT `0.757 ms`）、Stream 2 Input も `Running` を維持。 | PASS |
| Stream 2 Input / Touch 検出 | `getevent -lp` によりエミュレータのマルチタッチ入力デバイス 11 系統（`virtio_input_multi_touch_*`）を検出し `Running (started:devices=11)` を報告。 | PASS |
| 正常終了・クリーンアップ | ホスト `Shutdown` 送信に対し `Shutdown(code=0, reason="ack:smoke_done")` を返し終了コード `0` で終了。`/data/local/tmp/arb-*` 削除、ADB forward 解除、および `adb emu kill` によるエミュレータ完全停止を確認。 | PASS |
| `scrcpy-server-v5.0.1` ＋ `AudioRecordReader` 映像ハンドシェイク（任意確認） | `replay-scrcpy-audio.dex.jar` を `app_process` で起動し、`h264`（`460x1024`）の Session メタデータ、32 byte の SPS/PPS コンフィグパケット、および 22,778 byte の先頭 IDR キーフレーム（`pts_us=488042148`）の受信を確認。 | PASS |

### 実測から得られた設計上の注意点

- **ゼロサスペンド環境での 3 時計読取順**: エミュレータ等サスペンド累積時間が 0 の環境では `CLOCK_BOOTTIME` と `CLOCK_MONOTONIC` が同一エポックとなるため、`Heartbeat` 内で `monotonic_ns` を `boottime_ns` の直前に読取る順序とすることで `boottime_ns >= monotonic_ns` を常に厳密に保つ。`Pong` では `[boottime_before_ns, boottime_after_ns]` で `monotonic_ns` を挟むため `boottime_after_ns >= monotonic_ns` が成立する。
- **`logcat -S` の誠実な限界**: `logcat -b all -S` はバッファ全体のリング上書き（`Total - Now`）は観測できるが、クライアントソケット単位のドロップカウンタを持たない。そのため `pruned_delta == 0` の区間のみ `Verified` とし、リング上書きが発生した通常区間は `Unverified`、プロセス終了や明示的ドロップ・容量超過時は `OverflowDetected` を送出する方針とした。

## 3. 明示的な未検証項目（Unverified — 実機スモークで今後確認）

- **OEM 実機 SoC での SELinux・端末固有挙動**: 実機（Pixel / Samsung / Xiaomi / Sony 等のベンダー ROM）における `/proc/sys/kernel/random/boot_id` の SELinux ポリシーによるフォールバック経路発火、`logcat -S` の権限・書式差分、および物理タッチパネルでの `getevent -lt` 実タップ座標ストリーム（API <= 28 のパイプバッファリング含む）は未検証（業務実機スモークテストで検証予定）。
- **実機スリープ（Doze / 画面消灯サスペンド）時の `boottime - monotonic` 乖離**: エミュレータでは実サスペンドを発生させていないため、実機サスペンド復帰後の `boottime_ns - monotonic_ns` 増分は未検証。
- **実機 `audio_source=playback audio_dup=true` 音声ストリームのエンドツーエンド A/V オフセット**: 本検証では `replay-scrcpy-audio.dex.jar` の映像ハンドシェイクまでを確認しており、API >= 33 実機での再生音声キャプチャと `AudioRecordReader` PTS 精度検証は未実施。
