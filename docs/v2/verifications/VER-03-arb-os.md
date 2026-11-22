---
id: VER-03
type: verification
name: arb-os クロスプラットフォーム OS 抽象層（二重時計・原子 FS・IPC・SHM・パス）検証記録
verifies:
  - SPEC-01
  - SPEC-02
  - SPEC-03
  - REQ-01
  - REQ-04
  - REQ-05
  - REQ-08
  - REQ-09
  - REQ-10
  - REQ-11
---

# VER-03: `arb-os` クロスプラットフォーム OS 抽象層検証記録

記録日: 2026-11-22（macOS 27.3.0 / Apple Silicon arm64 / `rustc 1.100.0` / `cargo 1.100.0` / `clippy 0.1.100`）

## 1. 実装モジュールと仕様対応

`core/crates/arb-os` において、3 OS 差分を単一クレート境界に閉じ込めた 6 モジュールを実装した。

| モジュール | 主な公開 API | 対象仕様 / 要件 |
| --- | --- | --- |
| `error` | `OsError`（`#[non_exhaustive]`, `thiserror::Error`, `Send + Sync + 'static`） | SPEC-01, REQ-08 |
| `clock` | `HostClockSample`, `sample_host_clock`, `sleep_delta_ns`, `detect_pc_sleep`, `DEFAULT_PC_SLEEP_THRESHOLD_NS` | SPEC-02, REQ-01, REQ-08 |
| `fs` | `DiskSpaceInfo`, `query_disk_space`, `fsync_file`, `fsync_dir`, `AtomicStagingDir`, `SingleInstanceLock`, `is_pid_alive` | SPEC-01, SPEC-02, REQ-04, REQ-08, REQ-09, REQ-10 |
| `ipc` | `LocalIpcListener`, `LocalIpcStream`, `write_frame_line`, `read_frame_line`, `DEFAULT_MAX_IPC_LINE_BYTES` | SPEC-01, SPEC-03, REQ-08, REQ-09 |
| `shm` | `PixelFormat`, `FrameDescriptor`, `FrameSlotMeta`, `ShmFrameRingWriter`, `ShmFrameRingReader`, `SHM_SLOT_COUNT` | SPEC-03, REQ-08, REQ-11 |
| `paths` | `ArbPaths::resolve`, `ArbPaths::resolve_with_env`, `lock_file`, `ipc_endpoint`, `shm_dir` | SPEC-01, SPEC-03, REQ-08 |

### 依存クレートとライセンス境界（REQ-05 / SPEC-01）

- `libc = "0.2"`（Unix 限定、`MIT OR Apache-2.0`）
- `thiserror = "2"`（`MIT OR Apache-2.0`）
- `memmap2 = "0.9"`（`MIT OR Apache-2.0`）
- `windows-sys = "0.59"`（Windows 限定、`MIT OR Apache-2.0`）
- `core/scripts/check-licenses.py`: 全 26 パッケージ検査 PASS（GPL / AGPL 違反 0 件）。

## 2. macOS arm64 実測テスト結果（21/21 PASS）

`./core/scripts/verify.sh` により以下全 21 件の `arb-os` 単体テストおよびワークスペース全体（28 テスト）が PASS した。

### 2.1 `clock`（二重時計・スリープ検出）

- `clock::tests::host_clock_is_monotonic_over_ten_thousand_samples`: macOS にて `clock_gettime_nsec_np(CLOCK_UPTIME_RAW)` と `clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW)`（S2 実測で `-4.365 s` 乖離した `CLOCK_MONOTONIC` は不使用）および `CLOCK_REALTIME` を 10,000 回連続サンプリングし、逆行 0 回を確認。
- `clock::tests::active_and_continuous_clocks_agree_across_100ms_sleep`: 100 ms の `thread::sleep` 前後で `active_mono_ns` / `continuous_mono_ns` の差分乖離が `< 5 ms` 以内かつ `detect_pc_sleep` が誤検知しないことを確認。
- `clock::tests::detect_pc_sleep_identifies_synthetic_sleep_deltas`: 10 ms（閾値未満）・50 ms（境界一致）・2.5 s（スリープ検出）・負ジッタの合成サンプルで `sleep_delta_ns` / `detect_pc_sleep` の厳密判定を確認。

### 2.2 `fs`（原子ステージング・空き容量・単一ロック・PID 生存）

- `fs::tests::atomic_staging_commit_moves_files_and_writes_marker`: `.staging-<id>` へのファイル書出・`fsync_file`・`.committed` マーカー書込・`fsync_dir`・`renamex_np(RENAME_EXCL)` 原子リネーム・親ディレクトリ `fsync_dir` を確認。
- `fs::tests::uncommitted_staging_dir_is_cleaned_up_on_drop`: 未コミット `Drop` で `.staging-<id>` が確実に削除され、破損バンドルが残らないことを確認。
- `fs::tests::atomic_staging_never_overwrites_existing_destination`: 作成時およびコミット直前競合（既存空ディレクトリ含む）のいずれでも既存出力先を上書きせず `OsError::AlreadyExists` を返すことを確認。
- `fs::tests::staging_rejects_path_traversal_and_invalid_ids`: `../` や絶対パス、空 `staging_id` の拒否を確認。
- `fs::tests::retry_helper_handles_transient_errors_and_commit_marker_recovery`: Windows 一時ロック向け指数バックオフ再試行と `.committed` マーカーによる復帰ロジックを決定論的に検証。
- `fs::tests::query_disk_space_returns_nonzero_capacity_for_temp_dir`: `statvfs` による `available_bytes <= total_bytes`（正値）取得を確認。
- `fs::tests::single_instance_lock_detects_contention_and_releases_on_drop`: `flock(LOCK_EX | LOCK_NB)` による排他競合検知・PID 記録・`Drop` 解放後の再取得を確認。
- `fs::tests::pid_liveness_distinguishes_current_process_from_exited_child`: 自プロセス PID 生存、PID 0 非生存、終了回収済み子プロセス PID 非生存を確認。

### 2.3 `ipc`（ローカル IPC・NDJSON 長さ制限フレーミング）

- `ipc::tests::uds_round_trip_multi_message_ndjson_and_permissions`: 親ディレクトリ `0700`・ソケット `0600` 権限の作成、および複数 JSON-RPC NDJSON メッセージの双方向ラウンドトリップを確認。
- `ipc::tests::oversized_ndjson_line_is_rejected_on_read_and_write`: 上限超過行の送受信拒否、改行なし過大ストリームの非肥大拒否、埋め込み改行拒否、CRLF 除去、正常 EOF（`Ok(None)`）を確認。
- `ipc::tests::stale_socket_is_recovered_but_active_socket_and_regular_file_are_protected`: 接続拒否を確認した孤立ソケットのみ自動回収し、稼働中ソケットおよび通常ファイルは絶対に削除しないことを確認。

### 2.4 `shm`（トリプルバッファ共有メモリフレームリング）

- `shm::tests::round_trip_1080p_bgra_triple_buffer_and_approximate_transitions`: 1920x1080 BGRA（8,294,400 bytes / スロット、約 8.3 MB/slot）で 3 スロット周回（5 フレーム連続）の完全ピクセル一致、単調増加 `generation`、正確な `frame_index` / `boottime_ns`、および `is_approximate` バッジ遷移（`true -> false -> true -> false`）を確認。
- `shm::tests::concurrent_writer_and_reader_never_observe_torn_frames`: 並列書込スレッド（200 フレーム連続発行）と読取スレッド間で `Release`/`Acquire` シーケンスロックによりテアリング 0 件・`generation` 非減少を確認。
- `shm::tests::shm_validates_pixel_formats_and_rejects_oversized_frames`: `Nv12` バッファサイズ算出、狭小ストライド拒否、スロット容量超過拒否を確認。

### 2.5 `paths`（標準 OS ディレクトリ解決）

- `paths::tests::env_overrides_take_precedence_over_os_defaults`: `ARB_DATA_DIR` / `ARB_CACHE_DIR` / `ARB_RUNTIME_DIR` によるハーメティック上書きを確認。
- `paths::tests::macos_defaults_match_spec_paths`: macOS デフォルト（`~/Library/Application Support/arb`、`~/Library/Caches/arb`、`arbd.sock`）および空環境変数フォールバックを確認。
- `paths::tests::live_resolve_succeeds_in_host_environment`: 実ホスト環境での `ArbPaths::resolve()` 成功を確認。

## 3. 品質ゲート結果（`./core/scripts/verify.sh`）

- `[1/6] cargo fmt --check`: PASS
- `[2/6] cargo clippy --all-targets -- -D warnings`: PASS（BeautifulRust 厳格 lint 0 警告、`undocumented_unsafe_blocks` / `multiple_unsafe_ops_per_block` / `as_conversions` / `max-fn-params-bools = 0` 全通過）
- `[3/6] cargo test`: PASS（`arb-os` 21 テスト含む全ワークスペース PASS）
- `[4/6] license check`: PASS（26 クレート、GPL/AGPL 違反 0 件）
- `[5/6] sara check --strict`: PASS
- `[6/6] REQ -> downstream SPEC coverage check`: PASS（14/14 REQ 充足）

## 4. 明示的な未検証項目（Unverified — Linux / Windows CI にて実行予定）

- **Linux 実行検証**: `CLOCK_MONOTONIC` / `CLOCK_BOOTTIME`、XDG デフォルトパス、`statvfs` / `flock` / UDS の Linux ランナー実機実行は本環境（macOS arm64）では未実施（`libc` シンボル・型整合はソース照合済み、`ubuntu-latest` CI で実行予定）。
- **Windows 実行検証**: `QueryUnbiasedInterruptTime` / `QueryInterruptTime` / `GetSystemTimePreciseAsFileTime`、`GetDiskFreeSpaceExW`、`LockFileEx`、`OpenProcess` / `GetExitCodeProcess`、Named Pipe（`CreateNamedPipeW` / `ConnectNamedPipe` / `CreateFileW`）および `%LOCALAPPDATA%\arb` の実機ビルド・実行は本環境では未実施（`windows-sys 0.59.0` の機能フラグと全関数シグネチャはレジストリ定義と照合済み、`windows-latest` CI で実行予定）。
- **実機物理スリープ差分検証**: macOS / Linux / Windows の実機フタ閉じ・サスペンド復帰を挟んだ `detect_pc_sleep` 実測は未実施。
