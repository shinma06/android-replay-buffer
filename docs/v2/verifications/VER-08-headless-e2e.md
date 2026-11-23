---
id: VER-08
type: verification
name: arbd・arb-cli・11シナリオ障害注入 E2E および実 API 37 ヘッドレスエミュレータ E2E（Gate G2 Headless MVP）検証結果
verifies:
  - SPEC-01
  - SPEC-02
  - SPEC-03
  - SPEC-04
  - REQ-01
  - REQ-02
  - REQ-03
  - REQ-04
  - REQ-05
  - REQ-06
  - REQ-07
  - REQ-08
  - REQ-09
  - REQ-10
  - REQ-11
  - REQ-12
  - REQ-13
  - REQ-14
---

# VER-08: `arbd` / `arb-cli` / ヘッドレス E2E（Gate G2 Headless MVP）検証結果

記録日: 2026-11-23（macOS arm64 / Rust workspace `core/` / Android Emulator `arb_api37_arm64` API 37）

## 1. 実装対象と構成

`arb-core` / `arb-os` / `arb-storage` / `arb-media` / `arb-adb` を統合し、単一ライターのローカルデーモン `arbd` とローカル IPC + SHM 経由の CLI クライアント `arb-cli`、および決定論的障害注入 E2E・実エミュレータ E2E を構築した。

| コンポーネント | 役割・主な実装内容 |
| --- | --- |
| `core/crates/arbd` | ライブラリ + バイナリ分離構成（`src/lib.rs`, `src/main.rs`）。`SingleInstanceLock`、起動時 `Catalog::recover_on_startup()`（死んだ PID リース回収・孤児 `.staging-*` 削除・中断ピン留めジョブの `FailedRetryable` 復元）、`LocalIpcListener` によるマルチクライアント NDJSON JSON-RPC 2.0 制御プレーン（`SPEC-03` 全メソッド）、`DeviceEventSource` → `ClockTimeline` → Zone-1 `RingStore` パイプライン、2 フェーズ保存（`PinJournal` + 境界ドレイン + `V2BundleWriter` + `retry`/`retarget`/`discard`）、SHM `ShmFrameRingWriter` + 外部 `FilmstripCache` + `GopLruCache` による低遅延スクラブ、`export.trim`（0 `elst`、ジョブリース保護）、`export.png`、ライブ ADB 並列リーダー（companion / scrcpy video / optional audio を独立スレッドで並列読取・自動再接続・終了時クリーンアップ保証）。 |
| `core/crates/arb-cli` | `arb-core` + `arb-os` のみに依存（`SPEC-01` クレート DAG 厳守）。`--data-dir` / `--cache-dir` / `--runtime-dir` / `--json` / `--auto-start` および `status`, `daemon start|stop`, `devices`, `capture start|stop`, `save`, `jobs list|retry|retarget|discard`, `bundles list|open|trash|restore|gc`, `inspect`, `trim`, `png`, `scrub-bench` を実装。 |
| `core/crates/arbd/tests/e2e_scenarios.rs` | 実 `arbd` IPC サーバー + `MockDeviceSource` + 実リポジトリ Legacy Schema 4 フィクスチャを用いた 11 シナリオ決定論的 E2E テスト（全 12 テストケース）。 |
| `core/scripts/e2e_emulator.sh` | `device-agent` ビルド → `arbd`/`arb-cli` ビルド → `arb_api37_arm64` ヘッドレス起動 → 実機相当ライブ録画・保存・検査・SHM スクラブベンチ・GOP 途中トリム・`ffprobe` / MP4 ボックス独立検査・PNG 出力検査・終了時完全クリーンアップ（`adb emu kill` + `adb devices` 空確認）を自動実行する E2E ランナー。 |

## 2. 決定論的 11 シナリオ障害注入 & Legacy 互換 E2E（`cargo test -p arbd --test e2e_scenarios`）

全 12 テスト PASS（実行時間 1.64 秒、全ケース実ローカル IPC ソケット経由で `arbd` と通信）。

| # | シナリオ | 検証結果 |
| ---: | --- | --- |
| 1 | 通常録画 → 保存 → 検査 → SHM スクラブ（`is_approximate: true → false` + GOP-LRU ヒット）→ GOP 途中トリム（`[17..=49]`）→ PNG 出力 | PASS。90 フレーム（3 キーフレーム）、WORM SHA-256 検証 OK、トリム後 33 フレーム・`edts=0`・`elst=0`・`faststart`・焼込みバーコード 17/30/49 完全一致、PNG バーコード 43 + `arb:frame_index=43` `tEXt` 一致、元バンドル改変 0 バイト。 |
| 2 | 遅延到着バースト（50 秒遅れ古いレコード） | PASS。30 秒ウィンドウから遅延フレームが完全除外（`frame_count=0`）、`unanchored` にも漏洩なし。 |
| 3 | クロック同期断 & 壁時計 30 秒巻き戻し → トリム時の `unanchored` 引継ぎ | PASS。`DeviceWallJump` と `ClockSyncStale` を記録し、曖昧ログを `unanchored` に隔離（`main` 混入 0 件）。GOP 途中トリム後も `unanchored` 側へそのまま引き継がれることを確認。 |
| 4 | 音声部分停止（`PartialAudioStop`） | PASS。音声に `StreamGap` を記録し `audio.status = partial` と誠実表示、映像・ログは途切れず保存。 |
| 5 | 切断 → 切断中保存 → 同一 `boot_id` 再接続 → ギャップ跨ぎ保存 | PASS。切断中も Zone-1 リングから保存成功（`ConnectionLost` 記録）。同一 `boot_id` 再接続後の保存では切断前・復帰後ログを双方含み、`DeviceReboot` に誤分類しないことを確認。 |
| 6 | 端末ディープスリープ（`DeviceDeepSleep`） | PASS。`DeviceSleep` 不連続（`delta_ns = 15_000_000_000`）を記録。 |
| 7 | 端末再起動（`boot_id` 変化ハード境界） | PASS。`DeviceReboot` を記録し、再起動前 `boot-A` のログが再起動後メインタイムラインに混入しないことを確認。 |
| 8 | 書き込み不可先への保存失敗 → ピン保持 → `save.retarget` 成功 | PASS。通常ファイルを保存先にして失敗させ、`FailedRetryable` かつリングピン保持を確認。正常ディレクトリへ `save.retarget` して WORM バンドル確定・ピン解放・SHA-256 検証に成功。 |
| 9 | 編集セッション + Viewer Lease 下での `bundle.trash` → GC 保護 → `bundle.restore` → リース解放 → `bundle.gc` 物理削除 | PASS。外部編集セッション保存、リース中は GC が `skipped_leased_bundle_ids` で物理削除を阻止、`restore` で `active` 復帰、再 `trash` + `bundle.close` 後に `bundle.gc` がディレクトリを完全削除。 |
| 10 | ディスク圧迫（空き容量 < 3 GiB 下限）→ `StoragePressureStop` で保存済・ピン留めデータ保護 | PASS。空き容量 1 GiB 低下で `storage_pressure_stopped = true` と `StoragePressureStop` 不連続を記録し録画停止。既存保存バンドルの整合性とピン留めジョブの `save.retarget` 復旧を確認。 |
| 11 | 実リポジトリ Legacy Schema 4 フィクスチャ（`replay-20240101-000000-manual-feedface`）の読取専用オープン・検査 | PASS。`format = legacy_schema4`・`is_legacy = true` でフレーム・ログ・タップを参照でき、`bundle.gc` も `skipped_unmanaged_or_legacy_bundle_ids` で保護。前後バイトスナップショット完全一致（改変 0）。 |

## 3. 実 API 37 ヘッドレス Android エミュレータ E2E（`./core/scripts/e2e_emulator.sh`）

実行環境: macOS arm64 / AVD `arb_api37_arm64`（`sdk_gphone64_arm64`, Android API 37, `-no-window -no-audio -gpu host -no-boot-anim -no-snapshot-save`）/ リリースビルド `arbd` & `arb-cli` / 実 `device-agent`（`arb-companion.dex.jar` + `replay-scrcpy-audio.dex.jar`）。

### 実行結果サマリ（全 9 ステップ PASS、終了コード 0）

| ステップ | 項目 | 実測値・検証内容 |
| ---: | --- | --- |
| 1 | `device-agent` ビルド | `ProtocolTest` 19/19 PASS。`arb-companion.dex.jar`（26,696 bytes）および `replay-scrcpy-audio.dex.jar`（735,101 bytes）を生成。 |
| 2 | Rust リリースビルド | `arbd` / `arb-cli` ビルド完了。 |
| 3 | エミュレータ起動 | `emulator-5554` が **9 秒** で `sys.boot_completed=1` 到達。 |
| 4 | `arbd` 自動起動・端末検出 | `arb-cli --auto-start status` 応答 7 ms、`device.list` で `emulator-5554 (state=device)` 検出。 |
| 5–6 | ライブ録画（約 8 秒）→ 保存 | 実 H.264 映像 **172 フレーム**（460×1024、3 キーフレーム）+ 実 logcat **7,098 件**（`completeness=verified_no_overflow`, `overflow_events=0`, 注入マーカー `ARB_E2E` **6/6 件** 取得）+ 実カーネル `boot_id` を Zone-1 リングに記録し、30 秒ウィンドウで WORM v2 バンドルを保存完了（`tail_may_be_incomplete=false`）。 |
| 7 | `arb-cli inspect` & `scrub-bench` | 実 IPC + SHM 経由の計測値（下表参照）。`scrub` で `shm_is_approximate=true`、`seek_exact` で `shm_is_approximate=false` かつ `shm_frame_index=86`、GOP-LRU ヒット 2 回を確認。 |
| 8 | GOP 途中スマートトリム + `ffprobe` 独立検証 | 非キーフレーム始点 `[30..=115]`（86 フレーム）を `videotoolbox` バックエンドでトリム（`reencoded_frames=30`, `stream_copied_frames=56`, `main_log_count=1717`）。`ffprobe` で `codec_name=h264`, `460x1024`, `nb_frames=86`, `nb_read_frames=86`、独自 MP4 ボックス走査で **`top_boxes=['ftyp', 'moov', 'mdat']`（faststart）・`edts=0`・`elst=0`** を確認。 |
| 9 | 厳密 PNG 出力検証 | フレーム 30 を `videotoolbox` で出力（460×1024, 71,641 bytes）。PNG シグネチャ・IHDR 寸法・`tEXt` 来歴チャンク（`arb:boottime_ns`, `arb:frame_index=30`, `arb:source_bundle_id`）を確認。 |
| 終了 | 完全クリーンアップ | `arbd` 停止、`/data/local/tmp/arb-*` 削除、`adb emu kill` 完了後、`adb devices` が **空** であることを自動確認。 |

### 実エミュレータ録画動画（460×1024, 172 フレーム）での `scrub-bench` 実測値

| 操作 | 実測レイテンシ | 詳細 |
| --- | ---: | --- |
| `media.filmstrip`（初回生成・外部キャッシュ書込） | **46.76 ms** | 3 キーフレームサムネイル（`origin=generated`、`<cache_dir>/<bundle_id>/filmstrip.bin`） |
| `media.scrub`（GOP 途中フレーム 86 要求） | **0.37 ms** | `shm_is_approximate=true`（最寄りキーフレーム 60 を即座に SHM 公開、SPEC-04 目安 < 16 ms 達成） |
| `media.seek_frame`（初回コールド GOP 1 復号 → フレーム 86） | **231.55 ms** | `shm_is_approximate=false`, `460×1024`（初回 VideoToolbox 復号 + 60→86 の 26 フレーム進行） |
| `media.seek_frame`（+1 コマ送り 86 → 87） | **1.34 ms** | GOP-LRU ヒット（`cache_hits=1`、SPEC-04 目安 < 33 ms 達成） |
| `media.seek_frame`（−1 コマ戻し 87 → 85） | **3.10 ms** | GOP-LRU ヒット（`cache_hits=2`、SPEC-04 目安 < 33 ms 達成） |

注記: ライブ ADB 録画部（`core/crates/arbd/src/live_adb.rs`）は companion・scrcpy video・optional audio の各トンネルを独立スレッドで並列読取しているため、`arb-adb::AdbSessionSource` の同期読取制約に影響されず実エミュレータで映像・ログ双方を同時取得できた。なおヘッドレスエミュレータは `-no-audio` 起動のため `--no-audio` を指定し、タッチ入力は `adb shell input tap/swipe` が InputManager 経由で `/dev/input/event*` を経由しないエミュレータ仕様のため `tap_main_count=0`（入力ストリームの正常動作・座標正規化は決定論的シナリオ 1・4 および [VER-04](VER-04-device-agent.md) で検証済）。

## 4. 品質ゲート（`./core/scripts/verify.sh`）

全 6 ゲート PASS（終了コード 0）:

1. `cargo fmt --all -- --check`: PASS
2. `cargo clippy --workspace --all-targets -- -D warnings`: PASS（警告 0 件）
3. `cargo test --workspace`: PASS（全ワークスペース 137 テスト合格: `arb-adb` 32 / `arb-cli` 2 / `arb-core` 35 / `arb-media` 13 / `arb-os` 25 / `arb-storage` 15 / `arb-tray` 1 / `arbd` ユニット 2 + 結合 E2E 12）
4. `python3 core/scripts/check-licenses.py`: PASS（**116 packages checked, 0 GPL/AGPL violations**、新規サードパーティ依存追加なし）
5. `sara check --strict`: PASS（26 items / 89 relationships: Requirement 14, Specification 4, Verification 8）
6. REQ → SPEC 被覆検査: OK（14/14 REQ items satisfied by downstream SPEC）

## 5. 未検証事項

- 実機 Android 端末（USB 物理接続、OEM 固有 `getevent` デバイスおよび実端末オーディオキャプチャ `--audio` 有効時）での連続録画・USB 抜き差し・端末再起動は本環境では未実施（決定論的モック E2E シナリオ 4・5・6・7 で代替検証済み）。
- Windows（Named Pipe / Win32 API）および Linux（UDS / OpenH264 フォールバック）でのネイティブ実行は本 macOS arm64 環境では未実施（CI `os-parity` で確認予定）。
- IDE プラグイン UI（Android Studio ツールウィンドウ / トレイ GUI）との結合は Phase 5（W8 以降）で検証予定。
- 実エミュレータ初回コールド `seek_exact`（26 フレーム進行）は 231.55 ms であり、30fps 1 秒 GOP 全展開（最悪 30 フレーム）を 150 ms 以内に収める追加最適化（ハードウェアデコードの先行プリフェッチや並列 BGRA 変換）は Phase 5 でのチューニング余地として記録する。
