---
id: VER-05
type: verification
name: arb-storage 3ゾーンストレージ（Live Ring・WORM v2 / 旧形式 I/O・SQLite WAL カタログ/リース/GC）検証記録
verifies:
  - SPEC-02
  - REQ-01
  - REQ-02
  - REQ-04
  - REQ-07
  - REQ-10
---

# VER-05: `arb-storage` 3ゾーンストレージ検証記録

記録日: 2026-11-23（macOS arm64 / `rustc 1.100.0` / `cargo 1.100.0`）

## 1. 実装モジュールと仕様対応（`core/crates/arb-storage`）

`arb-core` および `arb-os` 上に、SPEC-02 に準拠した 3 ゾーンストレージ層を実装した。

| モジュール | ゾーン | 主な公開 API / 責務 | 対象仕様 / 要件 |
| --- | --- | --- | --- |
| `ring` | Zone 1 | `RingStore`, `PinJournal`, `DiskSpaceProvider` / `OsDiskSpaceProvider`, `RingChunkKind`, `RingChunkDescriptor`。`<data_dir>/ring/` 追記専用不変チャンク書込、`<data_dir>/save_jobs/<job-id>.json` 永続 Pin、`RingEvictionPlanner` による容量・時間 Eviction、空き容量下限割れ時の `StoragePressureStop`（Pin 済みチャンク・確定バンドル無傷保護）。 | SPEC-02, REQ-04, REQ-10 |
| `bundle_io` | Zone 2 | `V2BundleWriter`, `V2BundleReader`, `LegacyBundleReader`, `BundleReader` トレイト, `open_bundle`。`AtomicStagingDir` による `.staging-<job-id>/` 原子コミット、SPEC-02 準拠 `timeline.sqlite`（`frames` / `discontinuities` / `logs` / `taps` / `clock_anchors` + `boottime_ns` / `bucket` / `gop_id` インデックス、ロールバックジャーナル `DELETE` モード）、`manifest.json`（SHA-256 + `ManifestV2::validate()`）、`SQLITE_OPEN_READ_ONLY` 並列読取、および Schema 4（ログ単体 `-logs` 含む）・Schema 3 の完全読取専用アダプタ。 | SPEC-02, REQ-01, REQ-02, REQ-04, REQ-07, REQ-10 |
| `catalog` | Zone 3 | `Catalog`, `BundleEntry`, `LeaseRecord`, `CatalogSaveJob`, `EditSession`, `GcReport`, `RecoveryReport`。`rusqlite`（`bundled`）WAL モード（`journal_mode = WAL`, `foreign_keys = ON`, `busy_timeout = 5000`）による `bundles` / `leases` / `save_jobs` / `edit_sessions` 管理、論理退避（`trash_bundle` / `restore_bundle`）、リース保護付き遅延物理 GC（`gc_trashed_bundles`、管理外パス・`legacy_ro` は絶対非削除）、起動時クラッシュ回復（`recover_on_startup`：孤立 `.staging-*` 削除、死活 PID 判定 `arb_os::fs::is_pid_alive` による孤立リース回収、中断ジョブの `failed_retryable` 復旧）。 | SPEC-02, REQ-04, REQ-10 |
| `error` | 共通 | `StorageError`（`#[non_exhaustive]`, `thiserror::Error`, `Send + Sync + 'static`）。 | SPEC-02 |

### 依存クレートとライセンス境界（GPL/AGPL ゼロ）

- `rusqlite = { version = "0.35", features = ["bundled"] }`（`MIT`、同梱 SQLite は Public Domain）
- `sha2 = "0.10"`（`MIT OR Apache-2.0`）
- `serde = { version = "1", features = ["derive"] }` / `serde_json = "1"`（`MIT OR Apache-2.0`）
- `thiserror = "2"`（`MIT OR Apache-2.0`）
- dev-dependency: `tempfile = "3"`（`MIT OR Apache-2.0`）
- `core/scripts/check-licenses.py`: 全 95 パッケージ検査 PASS（GPL / AGPL 違反 0 件、`r-efi 6.0.0` は許容条項 `MIT` を選択）。

## 2. テスト・障害注入検証結果（`arb-storage` 計 14 テスト全合格）

### 2.1 単体テスト（4 件）

- `tests::smoke_crate_info_links_domain_and_os`
- `ring::tests::append_close_evict_and_pin_protection_work_deterministically`
- `bundle_io::tests::v2_bundle_write_read_roundtrip_and_tamper_detection`（`manifest.json` + 全ファイル SHA-256 改ざん検知含む）
- `catalog::tests::edit_sessions_and_save_job_lifecycle_persist_in_wal_catalog`

### 2.2 統合・障害注入・実フィクスチャテスト（10 件）

1. **二相保存失敗回復（`fault_two_phase_save_failure_preserves_pins_and_succeeds_after_retarget`）**:
   - 書込不可出力先での `V2BundleWriter::write` 失敗時に `.staging-*` と不完全ディレクトリを残さず、`FailedRetryable` 状態で Pin 済みチャンクが大量新規リング書込（Eviction 多数発生）を耐えて 100% 保持され、`retarget_job` 後の再実体化・SHA-256 検証が完全成功することを確認。
2. **ステージング途中失敗・再起動 Pin 持続・時間予算 Eviction（`fault_mid_staging_failure_cleans_staging_and_ring_reopen_preserves_pins_and_duration_eviction`）**:
   - ステージング途中での欠損メディアファイル I/O エラーで `.staging-*` が自動削除されること、および `RingStore` 再オープン後に閉鎖チャンクと永続 `PinJournal` が復元され `max_ring_duration_ns` 超過時に未 Pin 古チャンクのみ Evict されることを確認。
3. **同時編集・退避・復元・GC 安全性（`fault_concurrent_edit_trash_restore_and_lease_guarded_gc_safety`）**:
   - 不変バンドル外への `EditSession` 保存、Viewer リース取得中の `trash_bundle` でもファイル不変・読取継続、`gc_trashed_bundles` がリース中バンドルおよび管理外外部バンドルを削除拒否、`restore_bundle` での復元、再退避＋リース解放後の管理下バンドルのみ物理削除を確認。
4. **`legacy_ro` / アクティブジョブ GC 保護・`retry_job`（`catalog_gc_protects_legacy_ro_and_active_job_targets_and_supports_retry_job`）**:
   - 管理ルート内であっても `legacy_ro` バンドルおよびアクティブ保存ジョブ対象パスは GC されないこと、`retry_job` が Pin を保持したまま `Materializing` へ復帰することを確認。
5. **孤立リース・クラッシュ回復（`fault_orphan_lease_staging_and_interrupted_jobs_recover_on_startup`）**:
   - 終了回収済み子プロセス PID の孤立リース、稼働中 PID の生存リース、孤立 `.staging-crash`、および中断した `pinned` / `draining` / `materializing` 各保存ジョブを仕込み、`recover_on_startup()` により死 PID リース回収・生存リース維持・`.staging-crash` 削除・全 3 ジョブの Pin 保持付き `FailedRetryable` 復旧を確認。
6. **ディスク逼迫保護（`fault_disk_pressure_stops_new_ring_writes_while_protecting_pins_and_saved_bundles`）**:
   - 疑似空き容量低下（`< 3 GiB`）で即座に `StorageError::StoragePressureStop`（および `DiscontinuityKind::StoragePressureStop`）で新規リング書込を停止し、既存確定 v2 バンドルの SHA-256 と Pin 済み・既存リングチャンクが 100% 無傷であることを確認。
7. **4 スレッド並列読取（`concurrent_four_thread_v2_bundle_readers_succeed_with_zero_write_locks`）**:
   - 4 スレッド同時オープン＋共有 `Arc<V2BundleReader>` で `verify_integrity` / `frames` / `discontinuities` / `query_logs` / `query_taps` / `clock_anchors` を並列実行しエラー 0 件、かつバンドル内に `-wal` / `-shm` サイドカーが一切生成されないことを確認。
8. **旧形式読取専用互換（実 Schema 4・合成 Schema 3・ログ単体 Schema 4 の 3 テスト）**:
   - 実リポジトリフィクスチャ `plugin/src/test/resources/replay-trim-parity/replay-20240101-000000-manual-feedface/`（24 フレーム、セグメント間 1 ギャップ、5 デバイスログ・3 アプリログ、2 タッチ、SHA-256 検証合格）および合成 Schema 3（`session.json`, `video-001.mp4`, `frames.jsonl`）・ログ単体 Schema 4（`LOGS_ONLY`, `video.mp4` なし）を `LegacyBundleReader` / `open_bundle` で読取り、全テスト前後でディレクトリ・全ファイルのサイズおよび mtime が一切変更されないことを確認。

## 3. 品質ゲート結果（`./core/scripts/verify.sh`）

| ゲート | 結果 | 詳細 |
| --- | --- | --- |
| `[1/6] cargo fmt --check` | PASS | ワークスペース全ファイル差分なし |
| `[2/6] cargo clippy --workspace --all-targets -- -D warnings` | PASS | BeautifulRust 厳格 lint 含め警告ゼロ |
| `[3/6] cargo test --workspace` | PASS | `arb-storage` 14 件（単体 4 + 統合/障害注入 10）含むワークスペース全テスト合格 |
| `[4/6] license check` | PASS | 95 パッケージ検査、GPL/AGPL 違反 0 件 |
| `[5/6] sara check --strict` | PASS | 全トレーサビリティ・リンク検証合格 |
| `[6/6] REQ -> downstream SPEC coverage` | PASS | 14/14 REQ 充足 |
| `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p arb-storage` | PASS | 公開 API ドキュメント警告ゼロ |

## 4. 明示的な未検証項目（Unverified）

- **Linux / Windows 実機ファイルシステム実行**: 本環境は macOS arm64 であり、Linux (`statvfs` / ext4) および Windows (`GetDiskFreeSpaceExW` / NTFS における AV 一時ロック下のリトライリネーム) での実機実行は CI ランナー（`ubuntu-latest` / `windows-latest`）で検証する。
- **`arbd` / `arb-media` との E2E 実機キャプチャ統合**: 実 MP4 マルチプレクス出力と端末連続取得からの Zone 1→2→3 連結は後続ワークストリーム（W5 以降）で検証する。

## 5. R2 独立レビュー修正（S13〜S19）

`./core/scripts/verify.sh` 6 ゲート全合格（`arb-storage` ライブラリ 8 件・統合 16 件）。

| 指摘 | 修正 | 回帰テスト |
|---|---|---|
| S13 | `Catalog::reconcile_pins`（`recover_on_startup_with_pins`）で完了・破棄・不明ジョブのジャーナルを解放し、ジャーナルを失った進行中ジョブを再ピン | `r2_s13_pin_journals_and_catalog_jobs_are_reconciled_on_startup` |
| S14 | 古い `.staging-<job>` を削除し、同じ `bundle_id` の健全なバンドルが既にあれば既存マニフェストを返す（冪等な再試行） | `r2_s14_retry_after_post_rename_crash_is_idempotent_and_stale_staging_is_removed` |
| S15 | `timeline.sqlite`（と Present の映像・音声）が `manifest.files` に載っていることを必須にし、未記載のメディアは返さない | `r2_s15_v2_integrity_requires_timeline_and_present_media_in_manifest` |
| S16 | 旧形式マニフェストの全ファイル名を検証し、正規化後のパスがバンドル内にあることを確認 | `r2_s16_legacy_manifest_paths_cannot_escape_the_bundle_folder`（`../../secret.jsonl`・`/etc/passwd` など） |
| S17 | 書き込み失敗時は既知の長さまで切り詰めてチャンクを封印。close はメタデータ確定後にだけハンドルを解放。起動時に孤立チャンクを確定・孤立メタデータを削除 | `failed_append_seals_the_chunk_instead_of_leaving_a_zombie`、`close_chunk_keeps_the_handle_until_metadata_is_durable`、`reopen_finalizes_orphan_chunks_and_drops_orphan_metadata` |
| S18 | `StoragePressureStop` は逼迫期間ごとに 1 件。`take_discontinuities()` を追加 | `storage_pressure_stop_is_recorded_once_per_episode` |
| S19 | GC はバンドル単位の失敗を `GcReport.failed` に記録して続行、`Purging` を再試行、時刻を数値で比較 | `r2_s19_gc_survives_failures_retries_purging_and_compares_time_numerically` |

未検証: Windows の AV ロック・共有違反の実発生（通常ファイルをバンドルパスに置いて削除失敗を再現）、macOS 以外での実行（CI `os-parity` で確認予定）。
