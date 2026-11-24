---
id: VER-07
type: verification
name: arb-adb（ワイヤプロトコル・scrcpy パーサ・ADB 自動再接続・決定論的障害注入ハーネス）検証結果
verifies:
  - SPEC-03
  - SPEC-04
  - REQ-03
  - REQ-12
  - REQ-13
  - REQ-14
---

# VER-07: `arb-adb` 検証結果

記録日: 2026-11-22（macOS arm64 / Rust workspace `core/`）

## 1. 対象

`core/crates/arb-adb` の 4 モジュール。

| モジュール | 内容 |
| --- | --- |
| `wire` | `device-agent/PROTOCOL.md` の 20 byte Little-Endian 固定ヘッダ（`magic="AR"`, `version=1`, `stream_id`, `msg_type`, `flags`, `reserved`, `seq: u32`, `payload_len: u64`）と全 9 メッセージ（`Hello` / `Heartbeat` / `Ping` / `Pong` / `StreamState` / `LogRecord` / `LogOverflowStats` / `TouchEvent` / `Shutdown`）の符号化・復号。`max_payload_len`（既定 4 MiB）超過はペイロード確保前に拒否し、インクリメンタル deframer は 1 byte 単位の分割入力にも対応。 |
| `scrcpy` | `scrcpy-server-v5.0.1` の映像トンネル（dummy byte `0x00`、codec `h264`、Session メタデータ、`flags/PTS` + 長さ `1..16 MiB` + Annex-B）と音声トンネル（codec `aac` / `0` 無効 / `1` エラー、長さ `1..64 KiB`）の Big-Endian パーサ。API < 33 の音声不可を `AudioUnavailableApiLevel` として明示し、映像・ログ・入力は継続。 |
| `adb` | `ARB_ADB_PATH` → `PATH` → `${ANDROID_HOME:-$HOME/Library/Android/sdk}/platform-tools/adb` の順で `adb` を解決。`devices -l`、セッショントークン付き `/data/local/tmp/arb-<token>-*.jar` への push、`forward tcp:0 localabstract:...`、`forward --remove`、終了時のリモート削除。`adb kill-server` は一切呼ばない。`ReconnectPolicy`（`200ms → 500ms → 1s → 2s` 上限）と `SessionSupervisor`（同一 `boot_id` の `ConnectionRestored` / 異なる `boot_id` の `DeviceReboot`、`StaticHold` / `StreamGap` の区別）。 |
| `mock` | 統一出力 `CaptureEvent`、ライブ `AdbSessionSource` と決定論的 `MockDeviceSource` が共に実装する `DeviceEventSource` トレイト、`FaultScenario` による 9 種の障害シナリオ。 |

## 2. ワイヤ互換性（Rust ⇔ Java）

`device-agent/companion/src/.../PayloadCodec.java`・`WireFrame.java`・`Protocol.java`（読取専用）と Rust 実装をフィールド単位で突合した。

- ヘッダ: オフセット・型・エンディアン・4 MiB 上限が一致。`ProtocolTest.java` と同じベクタ（`stream_id=1`, `msg_type=0x10`, `flags=FLAG_RECOVERED`, `seq=42`, 5 byte ペイロード → 25 byte）でバイト一致を確認。
- ペイロード: 全 9 メッセージのフィールド順・幅・符号（`str16` = `u16` 長 + UTF-8、`LogRecord` 末尾 `u32` 長 + 生バイト、`Heartbeat` の 20 byte ストリーム健全性エントリ + 予約 `u32`、`LogOverflowStats` の予約 `u8`）が一致。
- 列挙値: `stream_id`（0–4）、`msg_type`（`0x01`–`0x05`, `0x10`, `0x11`, `0x20`, `0x30`）、ストリーム状態（0–4）、`boot_id_source`（0–2）、completeness（0–2）、shutdown code（0–2）が一致。
- 差異: Java は 65535 byte を超える文字列を黙って切り詰めるが、Rust 送信側は `WireError` を返す（より厳格。受信互換性には影響しない）。

## 3. テスト結果（`cargo test -p arb-adb`）

合計 31 件 PASS（ユニット 26 + 結合 5）。

| 区分 | 件数 | 主な内容 |
| --- | ---: | --- |
| `wire` | 5 | Java ベクタとのビット一致、全メッセージ往復、切り詰めフレーム・過大ペイロード・不正バージョン拒否、1 byte 分割 deframe |
| `scrcpy` | 3 | Session/config/IDR/P フレーム、音声 AAC・無効・エラー codec、API レベル判定、過大・0 長・不正 dummy byte 拒否 |
| `adb` | 7 | `adb` 解決優先順位、`kill-server` 非実行、バックオフ系列、`boot_id` による復帰/再起動/未検証分類、`StaticHold` / `StreamGap` 区別 ほか |
| `mock` | 10 | 9 シナリオ個別（下表）＋ シナリオ一覧の網羅性・決定論性 |
| 結合 (`tests/`) | 5 | `PROTOCOL.md` ゴールデンバイト列 2 件、敵対的入力拒否（危険なセッショントークンでは `adb` を起動しない）、`DeviceEventSource` の多態利用、`arb-core` 選択との連携（50 秒遅延パケットの除外） |
| smoke | 1 | `arb-core` スキーマ v2 リンク |

### 障害注入シナリオ（`MockDeviceSource`、実機・エミュレータ不要）

| # | シナリオ | 検証した出力 |
| ---: | --- | --- |
| 1 | `NormalFhd30` | 1 秒 GOP 映像 + 音声 + ログ + タップ + 正常ハートビート、不連続 0 件 |
| 2 | `StaticScreenVsStall` | 12 秒無フレームかつビーコン `Running` → `StaticHold`、続くビーコン/映像 `Error` → `StreamGap` |
| 3 | `DelayedArrivalBurst` | 端末ネイティブ `monotonic` PTS を保持したまま PC 取込時刻で 50 秒遅延 |
| 4 | `ClockSyncOutageAndWallRollback` | 同期プローブ断 + logcat の `DeviceWallJump` 巻き戻しを誠実に劣化表示 |
| 5 | `PartialAudioStop` | 音声のみ `Error` で停止、映像・ログ・タップは継続 |
| 6 | `DisconnectAndReconnect` | 同一 `boot_id` で `ConnectionRestored`、再起動扱いにしない |
| 7 | `DeviceDeepSleep` | `Δ(boottime − monotonic) = 15 s` を検出 |
| 8 | `DeviceReboot` | `boot-A` → `boot-B` を 1 回だけ `DeviceReboot` として検出 |
| 9 | `LogcatOverflowAndUnverified` | `Unverified` ベースラインと `OverflowDetected` の両方 + 復帰フラグ |

## 4. 品質ゲート

`./core/scripts/verify.sh` の 6 ゲート（`cargo fmt --check`、`clippy --all-targets -D warnings`、`cargo test --workspace`、GPL/AGPL ライセンス検査、`sara check --strict`、REQ→SPEC 被覆）がすべて終了コード 0。依存は `arb-core`、`serde`、`serde_json`、`thiserror`（MIT/Apache-2.0）のみ。

## 5. 未検証事項

- ライブ `AdbSessionSource` の実機 USB 抜き差し・エミュレータ再起動での自動復帰時間は未計測（本作業はホスト単体・決定論的ハーネスの範囲。ADB コマンドは差し替え可能な `AdbCommandRunner` で検証）。ライブ Device Agent 側の実測は [VER-04](VER-04-device-agent.md) を参照。
- Linux / Windows でのテスト実行は本記録では未実施（CI `os-parity` で確認予定）。

## 6. R2 独立レビュー修正（M1・M2・S20・S21）

`r2-phase3-review.md` の指摘に対する修正と回帰テスト。`./core/scripts/verify.sh` 6 ゲート全合格（`arb-adb` ライブラリ 35 件・統合 6 件）。

| 指摘 | 修正 | 回帰テスト |
|---|---|---|
| M1 | `AdbSessionSource` が companion・video・audio をそれぞれ専用スレッドで読み、1 本の有界チャネルへ送る。`pc_cont_ns` は読み取り完了時に各スレッドで打刻 | `live_loopback_tcp_reads_streams_concurrently_and_reports_transport_loss`（実 localhost TCP。companion を開いたまま Heartbeat と H.264 フレームが交互に届くことを確認） |
| M2 | companion の EOF / ワイヤエラーで `on_disconnect` と `ConnectionLost`、video/audio の EOF / エラーで `StreamGap`（Heartbeat では `Running` に戻らない）。周期的な liveness 評価を追加 | 同上（無音で開いたままの video は `StaticHold`、video ソケット切断は `StreamGap`、companion 切断は `ConnectionLost`）、`live_source_after_shutdown_treats_transport_ends_as_graceful` |
| S20 | `boot_id` は同じ `boot_id_source` 同士のときだけ比較。`ContinuityUnverified` ではタイムラインの `DeviceReboot` を抑止（ライブ・モック両方） | `boot_id_source_change_is_continuity_unverified_not_device_reboot`、`fallback_random_uuid_reconnect_suppresses_timeline_device_reboot` |
| S21 | `SystemAdbRunner` にタイムアウト（期限切れで kill + wait）、部分デプロイ時の push 済み jar 削除、ポート解析失敗時の forward 削除、リモート `sh` 引数の厳格検証（`try_*_launch_command`） | `run_command_with_timeout_kills_wedged_subprocess`、`partial_deploy_failure_removes_already_pushed_remote_jars`、`unparsable_forward_output_removes_orphan_forward`、`remote_shell_arguments_are_strictly_validated` |

未検証: 実機 USB・エミュレータでのソケット切断時間と再接続（ホスト単体のループバック TCP で検証）。
