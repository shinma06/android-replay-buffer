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

## Phase 4 統合補足（R2 厳格 API の `arbd` への配線）

- 起動時リカバリは `Catalog::recover_on_startup_with_pins(&PinJournal)` を使用（S13）。解放・再作成したピンジャーナル件数を `system.status.recovery.released_pin_journals` / `repinned_jobs` に表示。
- `bundle.gc` 応答に `failed`（`bundle_id` / `error`）を追加し、パージ失敗（例: Windows の共有違反）を隠さず返す（S19）。
- `CapturePipeline` が `RingStore::take_discontinuities()` を書き込みごとに排出（メモリ有界化）し、`StoragePressureStop` をライブ表示と以降の保存バンドルの discontinuities に統合（S18）。シナリオ 12 で検証。
- R2 の `verify_integrity`（S15）は音声 `Present` に `audio.m4a` を要求する。`arbd` は AAC を `audio.m4a` へ多重化していないため、音声ストリームは `Present` ではなく `Partial` と誠実に報告する（トリム時も同様）。

## 6. R3 独立レビュー指摘修正（M1–M9 / S1–S18）検証結果

`r3-phase4-review.md` の全 Must fix（M1–M9）および全 Should fix（S1–S18）を `arb-storage::catalog`・`arbd`・`arb-cli`・`core/scripts/e2e_emulator.sh` へ反映し、各指摘に対する決定論的回帰テストを追加した。

| 項目 | 主な修正内容 | 回帰テスト / 検証 |
| --- | --- | --- |
| **M1 / S8a** | `Catalog::register_bundle_idempotent` 追加。`phase2_materialize` / `export_trim` で `CommittedParentSyncFailed` を公開済み（`warning` 付）として扱い破棄・孤立を防止。コミット後のカタログ登録／完了エラーも `?` で `Materializing` に残留させず `FailedRetryable` へ遷移し冪等リトライ可能化。 | `r3_m1_register_bundle_idempotent_tolerates_retry_and_rejects_path_mismatch`, `r3_m1_parent_sync_failure_publishes_and_catalogs_with_warning`, `r3_m1_crash_and_catalog_error_after_register_retry_idempotently` |
| **M2** | 保存ウィンドウ重複チャンク＋直近先行キーフレーム／設定／ハートビートのみを選択的ピン留め。リングチャンクをバイナリ連結形式（`ARBRC02\n`＋生ペイロード、旧 JSON 後方互換）へ圧縮。Phase-2 読取・多重化・書込をキャプチャロック `C` 解放後に実行。 | `r3_m2_compact_ring_chunk_is_binary_and_round_trips`, `r3_m2_legacy_json_ring_chunks_still_decode`, `r3_m2_selective_window_pinning_leaves_older_chunks_evictable` |
| **M3** | `mux_selected_video` の `Partial` フォールバックを `MediaError::InvalidContainer(_)`（SPS/PPS 無しスタブ）に限定し、`MediaError::Io` 等の障害は伝播してピン保持 `FailedRetryable` に遷移。`TempFileGuard` で一時 MP4 を確実に削除。 | `r3_m3_mux_io_error_propagates_instead_of_partial_without_video`, `r3_m3_stub_stream_without_parameter_sets_falls_back_to_partial` |
| **M4 / S12** | `companion_child` を `wait_for_companion_ready` 呼出前に `SessionCleanupGuard` へ格納し、`stop_flag` をポーリング。`capture.start` は終了済みハンドルを不在扱いとし、`capture.stop` は `L` ロック解放後にスレッドを `join`。 | `r3_m4_companion_child_owned_by_guard_before_handshake_wait`, `r3_s12_companion_ready_wait_honours_stop_flag` |
| **M5 / M6 / S13 / S14** | `live_adb.rs` の死活・再接続判定を `SessionSupervisor`（`LiveTracker`）へ統一：映像接続失敗・EOF 時は `StreamGap`（無音消失や `StaticHold` 誤分類を禁止）、静止画のみ `StaticHold`、ビーコンタイムアウトで切断再接続、`DeviceReboot` / `ContinuityUnverified` の重複・誤断定抑止。`PingState` に 2 秒タイムアウト追加、連続再接続失敗の `ConnectionLost` 集約および `last_discontinuities` 上限化。 | `r3_m5_live_video_loss_emits_stream_gap_never_static_hold`, `r3_m5_silent_but_alive_video_emits_one_static_hold_and_beacon_timeout_detected`, `r3_m6_continuity_unverified_never_emits_device_reboot`, `r3_m6_real_reboot_reported_exactly_once`, `r3_s13_expired_pending_ping_recovers_sync_probes`, `r3_s14_repeated_reconnect_failures_coalesce_into_one_connection_lost` |
| **M7** | クライアント接続単位で SHM ビューポート ID を追跡し EOF／エラー時に自動クローズ。全体上限 `MAX_VIEWPORTS = 8`（超過時 `Conflict`）。起動時 `SingleInstanceLock` 取得後に残存 `*.shm` を一掃。 | `r3_m7_client_disconnect_closes_viewports_cap_and_startup_shm_sweep` |
| **M8 / C3 / S7** | `arb-cli` で全パス引数およびパス形式の `bundle-id-or-path` を CLI カレントディレクトリ基準で絶対化（`std::path::absolute`）。`arbd` 側で相対パスおよび `ring/`・`cache_dir`・既存 WORM バンドル内への書込を `create_dir_all` 前に `InvalidParams` 拒否。`export.png` に `ExportJob` リース取得を追加。 | `r3_m8_absolutizes_relative_cli_paths_and_keeps_bundle_ids`, `r3_m8_s7_daemon_rejects_relative_and_forbidden_paths_and_bundle_png_mutation` |
| **M9 / S18** | `core/scripts/e2e_emulator.sh` のプレフライト異常終了時に `CLEANED_UP=1` で既存エミュレータ保護、通常クリーンアップもスクリプト起動 `EMU_SERIAL`/`EMU_PID` のみ停止。`arbd.lock` PID 抽出を `tr -dc '0-9'` へ修正、`INT`/`TERM`/`EXIT` トラップ分離で終了コード保持、失敗時に `emulator.log`/`arbd.log` を保全。 | `bash -n core/scripts/e2e_emulator.sh` PASS（実エミュレータ起動は未実施） |
| **S1 / S2 / S3 / S4 / S5** | `T_trigger` 確定と Phase-1 ピンをドレイン待機前に実行し、アクティブ切断時は `tail_may_be_incomplete=true` を返却（S1）。未アンカー映像フレームを映像 `StreamGap` と `unanchored_summary.truncated_count` に記録（S2）。`PinRollbackGuard` と原子的 `write_job_params`（tmp+fsync+rename+dir fsync）（S3）。`save.retarget` のカタログ状態検証をピンジャーナル変更前に実施（S4）。リング書込 I/O エラー時に `StreamGap` 記録および `ring_write_errors` 計上（S5）。 | `r3_s1_s2_disconnected_save_tail_flag_and_unanchored_video_recorded`, `r3_s3_pending_save_drop_rolls_back_pin_journal`, `r3_s4_retarget_non_retryable_job_leaves_pin_journal_untouched`, `r3_s5_ring_write_io_error_records_stream_gap_instead_of_silent_loss` |
| **S6 / S8b / S8c / S9 / S10 / S11 / S15** | `Catalog::gc_trashed_bundles` で死んだ PID リースを回収、`bundle.open` は `holder_pid` 必須＋生存確認、`arb-cli bundles close <LEASE_ID>` 追加（S6）。トリム未一致フレームに偽装 `Anchored` を使わず `Unanchored` を設定（S8b）し、末尾フレーム包含時は `tail_may_be_incomplete` を伝播（S8c）。`export.trim`/`export.png` をメディアロック `M` 外で実行（S9）。4 MiB 超過応答を接続切断せず `-32010` 構造化エラー返却＋`timeline.query_*` デフォルト上限適用（S10）。`arbd` シグナルハンドラ（SIGINT/SIGTERM/SIGHUP・Win32 Console）とシャットダウン後の非 `system.*` 拒否・待機クライアント速やか終了（S11）。ロック毒化時の安全なシャットダウン遷移（S15）。 | `r3_s6_gc_reaps_dead_pid_leases_and_purges_unblocked_bundle`, `r3_s6_parses_bundles_close_and_open_holder_pid`, `r3_s8b_trimmed_unmatched_frame_uses_unanchored_never_fabricated_anchor`, `r3_s6_s8c_s10_s11_leases_trim_tail_oversized_response_and_shutdown_gate` |
| **S16 / S17** | `--auto-start` 起動 `arbd` を独立プロセスグループ化（Unix `process_group(0)` / Windows `DETACHED_PROCESS \| CREATE_NEW_PROCESS_GROUP`）、`stderr` を `<runtime_dir>/arbd.log` へ出力、`try_wait()` で早期終了をログ末尾付き即時報告、`ARB_AUTO_START_TIMEOUT_MS` 対応、`system.hello` で `protocol_version == 1` 検証（S16）。`save` / `jobs retry` / `jobs retarget` の `bundle_id == null` を終了コード `3` とし、`--json` 時は構造化 JSON エラーおよび識別可能な終了コード（1/2/3/4/5）を返却（S17）。 | `r3_s16_auto_start_timeout_parses_env_override_and_defaults`, `r3_s16_verify_hello_rejects_protocol_version_mismatch`, `r3_s16_wait_for_daemon_reports_early_exit_with_log_tail`, `r3_s17_maps_rpc_errors_and_failed_save_to_distinct_exit_codes_and_json_errors` |

### R3-Fix 品質ゲート実行結果

- `./core/scripts/verify.sh`: 全 6 ゲート PASS（終了コード 0）
  1. `cargo fmt --all --check`: PASS
  2. `cargo clippy --workspace --all-targets -- -D warnings`: PASS（警告 0 件）
  3. `cargo test --workspace`: PASS（**196 passed**, 0 failed, 1 ignored ヘルパープロセス: `arb-adb` 41 / `arb-cli` 8 / `arb-core` 35 / `arb-media` 22 / `arb-os` 25 / `arb-storage` 26 / `arb-tray` 1 / `arbd` ユニット 18 + E2E 20）
  4. `python3 core/scripts/check-licenses.py`: PASS（116 packages checked, 0 GPL/AGPL violations、`Cargo.toml`/`Cargo.lock` 変更なし）
  5. `sara check --strict`: PASS（26 items / 89 relationships）
  6. REQ → SPEC 被覆検査: OK（14/14 REQ items satisfied）
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`: PASS（警告 0 件）
- `bash -n core/scripts/e2e_emulator.sh`: PASS
- 未検証（本 R3-Fix 実行範囲外）: 実ヘッドレスエミュレータ再実行、Windows/Linux ネイティブ実行、実機 USB 接続。

