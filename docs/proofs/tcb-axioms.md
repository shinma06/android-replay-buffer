# 5層 PDD 検証基盤と TCB 公理台帳

この文書は、Proof-Driven Development（PDD）の五層で何を機械的に検証し、何を検証できずに信頼しているか（TCB: Trusted Computing Base）を記録する。証明は本番コードそのもの、または本番状態遷移を忠実に線形化したモデルを対象とし、空疎な前提（`kani::assume(false)` など）は使わない。

## 1. 五層の概要と実行コマンド

| 層 | 道具 | 対象 | 実行コマンド | 実行頻度 |
|---|---|---|---|---|
| 1 | Kani（CBMC） | 純粋関数の全入力域（有界） | `cd core && cargo kani -p <crate>`（`arb-core` / `arb-os` / `arb-adb` / `arb-media` / `arbd`） | `.github/workflows/pdd-kani.yml`（毎週・手動・`workflow_call`） |
| 2 | Stateright | 並行ライフサイクルの全インターリーブ | `cargo test --manifest-path core/Cargo.toml -p arbd --test pdd_stateright_models -- --nocapture` | 毎回の `cargo test`（3 OS） |
| 3 | Proptest / Miri | 実 I/O（SQLite・FS・MP4・ワイヤ）と未定義動作 | `cargo test --manifest-path core/Cargo.toml --workspace`、`cargo +nightly miri test -p arb-os --lib shm::`、`PROPTEST_CASES=8 cargo +nightly miri test -p arb-core --lib`、`cargo +nightly miri test -p arb-media --lib mp4::` | Proptest は毎回、Miri は `pdd-kani.yml` |
| 4 | Konsist / 構造ガード | Kotlin と Rust のアーキテクチャ規則 | `cd ide-plugin && ./gradlew test --tests io.github.shinma06.arb.ArchitectureKonsistTest`、`python3 core/scripts/check-architecture.py` | 毎回（Gradle テスト、`core/scripts/verify.sh`、CI `os-parity`） |
| 5 | TCB 台帳 | 形式層が前提とする外部公理 | この文書 | 変更時にレビュー |

Proptest の試行回数は `PROPTEST_CASES` で上書きできる。重い SQLite / FS 系は既定 24〜64 件に抑えている。

## 2. 検証インベントリ

### 2.1 Layer 1: Kani

| ID | ハーネス | ファイル |
|---|---|---|
| K1 | `k1_anchored_matches_oracle_and_swap_invariant`、`k1_bounded_matches_oracle_and_swap_invariant`、`k1_unanchored_always_uncertain`、`k1_anchored_window_expansion_never_demotes`、`k1_bounded_window_expansion_never_demotes`、`k1_frame_range_anchored_by_index_only`、`k1_frame_range_bounded_by_index_only`、`k1_frame_range_unanchored_never_main` | `core/crates/arb-core/src/kani_proofs.rs` |
| K2 | `k2_select_records_equals_trim_with_empty_sidecar_n1` / `_n2` / `_n3`、`k2_prior_sidecar_none_main_two`、`k2_prior_sidecar_one_consumes_budget_first`、`k2_prior_sidecar_two_consumes_budget_first` | 同上 |
| K3 | `k3_pc_sync_probe_boundary_math`、`k3_elapsed_same_boot`、`k3_map_device_monotonic_no_panic_and_boot_isolated`、`k3_map_device_monotonic_monotone_on_two_triples`、`k3_map_logcat_wall_no_panic_and_causal_ceiling` | 同上 |
| K4 | `k4_ring_eviction_plan_realistic_sizes`、`k4_ring_eviction_plan_extreme_u64` | 同上 |
| K5 | `k5_save_job_apply_matches_transition_table`、`k5_every_table_edge_is_reachable_by_apply`、`k5_bundle_lifecycle` | 同上 |
| K6 | `k6_map_touch_to_frame_bounds_and_monotonic`、`k6_physical_one_pixel_never_jumps_to_bottom_right`（`arb-core`）、`k6_axis_project_range_and_endpoints`、`k6_axis_project_monotonic`（`arbd`） | `core/crates/arb-core/src/kani_proofs.rs`、`core/crates/arbd/src/touch_calibration.rs` |
| K7 | `k7_empty_and_single_sample_strokes_are_taps`、`k7_decimate_keeps_endpoints_and_cap`、`k7_touch_slop_finite_positive` | `core/crates/arb-core/src/kani_proofs.rs` |
| K8 | `k8_required_buffer_bytes_covers_every_pixel`、`k8_align_up_exact`、`k8_compute_layout_slots_disjoint_and_aligned` | `core/crates/arb-os/src/shm.rs` |
| K9 | `k9_checked_entry_count_bounds_allocation`、`k9_iter_boxes_children_stay_in_parent`、`k9_parse_stts_bounded`、`k9_parse_stsc_strictly_increasing`、`k9_parse_stsz_bounded_by_file`、`k9_ticks_to_ns_saturating_and_monotone`、`k9_avcc_sample_to_annexb_bounded`（`arb-media/src/kani_proofs.rs`）、`k9_encode_chunk_offset_never_truncates`、`k9_stco_chosen_only_when_every_offset_fits`（`arb-media/src/mux_av.rs`）、`k9_frame_header_accepts_iff_valid_and_roundtrips`、`k9_frame_header_encode_decode_full_u64_len`（`arb-adb/src/wire.rs`） | 各ファイル |

ハーネスごとの結果と所要時間は「3. 実測結果」に記録する。

### 2.2 Layer 2: Stateright（`core/crates/arbd/tests/pdd_stateright_models.rs`）

| ID | モデル | 検査する性質 |
|---|---|---|
| S1 | IDE JVM 2 台・daemon・tray・CLI 常駐 daemon・単一インスタンスロック | ロック所有者は高々 1、保存中・クラッシュ処理中の強制停止なし、所有者以外は許可済みハンドオーバーなしに停止できない、全親の死亡と EOF 後に親結合子が残らない、ウィンドウ 1 枚を閉じても子は生存、tray 無効化で daemon は止まらない、CLI 常駐 daemon は CLI 終了後も生存。収束: 全親死亡後に子とロックが解放される／生存 IDE の supervisor が tray を再取得し、手動終了は自動復活しない |
| S2 | REQ-04 二相保存・単一ライタ・ディスク圧迫・クラッシュ／再起動／アップグレード・ゴミ箱 24h 猶予・リース・GC | `data_dir` ライタは高々 1、非終端ジョブが pin したチャンクは退避されない、Phase-1 後は `Committed` か回復可能、物理削除は管理下・非レガシー・`Trashed`・24h 経過・リース 0・ジョブ 0 のときのみ、レガシー schema 3/4 と非 ARB `.staging-*` は不変、クラッシュ回復後に ARB 所有 staging が残らない、再起動後の保存窓は新しい boot を選ぶ。到達性: ディスク回復後 `FailedRetryable` は `Completed` に到達できる |
| S3 | 世代 0..=2・キャプチャ・ストレージ圧迫・シャットダウン・映像／音声（API 28–32 と 33+）・制御・古いワーカ・保存ドレイン水位・ロック順序 | 古い世代のイベントは現世代を変更しない、遅れた水位があるのにドレイン完了を報告しない（タイムアウト／ギャップ記録を除く）、`handover_gate` → `capture` のロック順序を逆転しない、ユーザーの再有効化を `AutoAttachTick` が消さない、`shutting_down` は `capture_enabled` を永続化しない、映像のみの EOF は再接続／ギャップに遷移する |

### 2.3 Layer 3: Proptest / Miri

| ID | 対象 | ファイル |
|---|---|---|
| G1 | 実 SQLite/WAL `Catalog`（2 接続、1–32 操作列）と独立参照モデル、暦日・オフセット厳格拒否、ゴミ箱 24h 猶予 | `core/crates/arb-storage/tests/pdd_g1_catalog_props.rs` |
| G2 | 実 `RingStore` + 偽ディスク + 再起動でリセットする偽時計 + 各境界での障害注入 → 再オープン → retry/retarget、pin チャンク SHA-256 不変、レガシー schema 3/4 フィクスチャ不変 | `core/crates/arb-storage/tests/pdd_g2_ring_crash_props.rs` |
| G3 | 破損 MP4・深い入れ子・巨大テーブルで panic/OOM/スタック溢れなし、可変 GOP / VFR の mux → open → trim → share 往復（`edts`/`elst` なし、faststart） | `core/crates/arb-media/tests/pdd_g3_media_props.rs` |
| G4 | 複数ストリーム（ログ・音声・タップ・巨大ストローク・部分重複・別 boot）の保存 → 全範囲トリム保存則と非アンカー予算 | `core/crates/arbd/tests/pdd_g4_multistream_props.rs` |
| G5 | `SaveJobMaterializeParams` の過去 JSON 互換（全追加フィールドの `#[serde(default)]` 検査）、リングチャンクの符号化往復と任意バイト非 panic | `core/crates/arbd/src/capture_pdd_g5_tests.rs`、`core/crates/arbd/tests/fixtures/pdd_g5_params_v2_minimal.json` |
| G6 | wire / scrcpy ストリームの任意分割と `Interrupted`/`TimedOut`/EOF 注入（連続解析と一致、完全レコード接頭辞 + 明示ギャップ） | `core/crates/arb-adb/tests/pdd_g6_stream_fault_props.rs` |
| G7 | SHM 奇数幅・全 4 形式・stride/padding の Rust/Kotlin パリティ（共通ゴールデン表 48 行）と短いプレーンの拒否 | `core/crates/arb-os/tests/pdd_g7_shm_parity_props.rs`、`ide-plugin/client-core/src/test/kotlin/io/github/shinma06/arb/shm/ShmLayoutParityTest.kt` |
| M1 | ヒープ上の整列 `RingView`：長さ 0,1,7,8,9,15,16,17、末尾境界、1 ライタ + 2 リーダの多世代並行 | `core/crates/arb-os/src/shm.rs`（`m1_*`） |
| M2 | `arb-core` 全 lib テスト、`arb-media::mp4` の純粋パーサ（`miri_pure_tests`） | `core/crates/arb-core/src/`、`core/crates/arb-media/src/mp4.rs` |

Miri の隔離環境ではファイルシステムと mmap を使えないため、既存の FS/mmap テストは `cfg_attr(miri, ignore = ...)` で Miri 実行時のみ除外している（通常の `cargo test` では全 OS で実行する）。

### 2.4 Layer 4: 構造ガード

| ID | 規則 | 実装 |
|---|---|---|
| KON-1 | `..arb.ui..` は Files 書き込み・`FileOutputStream`・`FileWriter`・`ProcessBuilder`・`Runtime`・SQLite を直接使わない | `ide-plugin/src/test/kotlin/io/github/shinma06/arb/ArchitectureKonsistTest.kt` |
| KON-2 | 具象アクションは `ActionUpdateThread.BGT`、`*Content.kt` と `update` は RPC・プロセス・ブロッキング待ちを参照しない、子プロセス所有は `ArbdDaemonService`（アプリケーションレベル）と `ArbdProcessManager` のみ | 同上 |
| KON-3 | Swing ウィジェット（`JButton` など）と Swing UI DSL を本番コードで使わない | 同上 |
| KON-4 | OS 検出は `HostOs.kt` のみ、`HostOs.*` 分岐は `HostOs`/`ArbPaths`/`LocalIpcTransport`/`Win32NamedPipes` のみ | 同上 |
| KON-5 | `client-core` は IntelliJ/Jewel/Compose/Swing を import しない、`LegacyV1SettingsMigrator` の参照は `ArbStartupActivity` と自身のテストのみ | 同上 |
| RS-1 | `arb-core` の通常依存 ⊆ {serde, serde_json, thiserror}、`arb-cli`/`arb-tray` はエンジン crate に依存しない、`arb-storage`/`arb-media`/`arb-adb` 間の横依存なし、`arb-core` 本番コードに FS・ネット・プロセス・スレッド・時計・OS cfg なし | `core/scripts/check-architecture.py` |
| RS-2 | OS `cfg` は `arb-os`、`arb-media/src/codec/`、`arb-tray/src/native_tray.rs` のみ | 同上 |
| RS-3 | `forbid(unsafe_code)`: `arb-core`/`arb-storage`/`arb-adb`/`arb-cli`/`arbd`。`deny(unsafe_code)`: `arb-media`/`arb-tray`（許可は `videotoolbox_backend.rs` と `native_tray.rs` のみ） | 同上 |
| RS-4 | 本番コードの直接 `fs::rename` は `arb-os/src/fs.rs` のみ | 同上 |
| RS-5 | ライブラリ 5 crate の非テストコードで `unwrap`/`expect` を clippy で禁止、`arbd` の全 RPC が GUI（`ArbdRpcClient`）/CLI/MCP から呼ばれ、クライアントが未実装 RPC を呼ばない | 同上 |

## 3. 実測結果（Linux サンドボックス）

<!-- RESULTS -->

## 4. TCB 公理（TCB-01〜TCB-12）

各公理について、(a) 依存する形式層、(b) 公理を実環境で固定する自動テスト、(c) 未検証の境界を記す。

| ID | 公理 | (a) 依存する層 | (b) 固定する自動テスト | (c) 未検証の境界 |
|---|---|---|---|---|
| TCB-01 | 親の終了（`dispose` / `SIGKILL` / `TerminateProcess`）で stdin パイプの書き込み端が閉じ、子のブロッキング読み取りに EOF が届く | S1 | `core/crates/arbd/tests/parent_bound_lifecycle_e2e.rs::parent_pipe_eof_shuts_arbd_down_cleanly_and_frees_the_next_start`、`core/crates/arb-tray/tests/parent_bound_lifecycle.rs::parent_pipe_eof_quits_the_tray_and_releases_its_lock` | 実 IDE の強制終了と電源断は Stage 3（3 OS 実環境 / Devin ゲート）でのみ確認 |
| TCB-02 | stdin パイプ書き込みハンドルは他の IDE 子プロセスに継承されず、複数 IDE 間で tray を再取得できる | S1 | `core/crates/arb-tray/tests/parent_bound_lifecycle.rs::without_the_flag_stdin_eof_does_not_quit_a_standalone_tray` | 複数 IDE 同時起動の実機挙動は Stage 3 |
| TCB-03 | 同一ボリュームの非置換原子リネーム（`renamex_np`/`renameat2`/`MoveFileExW`）と `fsync_dir` の耐久性 | S2、G2 | `core/crates/arb-os/src/fs.rs::tests::fallback_rename_no_replace_falls_back_when_hard_link_is_unsupported`、`core/crates/arb-storage/tests/pdd_g2_ring_crash_props.rs` | 物理的な電源断・ディスクのキャッシュ偽装は未検証 |
| TCB-04 | SQLite WAL の ACID 耐久性、`SQLITE_OPEN_READ_ONLY` + `PRAGMA query_only=ON` によるレガシー読み取り専用不変性 | S2、G1、G2 | `core/crates/arb-storage/tests/storage_integration.rs::catalog_gc_protects_legacy_ro_and_active_job_targets_and_supports_retry_job`、`core/crates/arb-storage/tests/pdd_g1_catalog_props.rs` | SQLite 自体の正しさと電源断時の WAL 復旧は信頼 |
| TCB-05 | Rust `AtomicU64`（Release/Acquire）と JVM `VarHandle` の mmap 越し可視性と順序 | K8、M1、G7 | `core/crates/arb-os/src/shm.rs::tests::m1_one_writer_two_readers_multi_generation_is_race_free`、`ide-plugin/client-core/src/test/kotlin/io/github/shinma06/arb/shm/ShmFrameRingReaderTest.kt::seqlockPreventsTornFramesUnderConcurrentWriter` | プロセス間・言語間のメモリモデルは Miri の対象外。ARM（Apple Silicon）実機での弱順序は Stage 3 |
| TCB-06 | OS の同一ユーザーローカル IPC 認証（Unix `0700` + `SO_PEERCRED`/`LOCAL_PEERCRED`、Windows DACL + SID 一致） | S1（ハンドオーバー認可） | `core/crates/arb-os/src/ipc.rs::tests::uds_round_trip_multi_message_ndjson_and_permissions` | 他ユーザーからの実攻撃、Windows のポリシー差は未検証 |
| TCB-07 | ホストの連続／単調時計（`CLOCK_BOOTTIME`/`CLOCK_MONOTONIC_RAW`/`GetTickCount64`）がサスペンドと再起動を通じて仕様どおり振る舞う | K3、S2、G2 | `core/crates/arb-os/src/clock.rs::tests::host_clock_is_monotonic_over_ten_thousand_samples` | 実サスペンド・実再起動は Stage 3 |
| TCB-08 | Android ADB / `scrcpy-server` / `getevent` / `logcat` / OEM MediaCodec の API 28–37 での意味 | K9（wire）、S3、G6 | `core/crates/arb-adb/tests/pdd_g6_stream_fault_props.rs` | 物理 USB 端末と OEM 差は未検証（エミュレータのみ） |
| TCB-09 | VideoToolbox / OpenH264 FFI のメモリ安全性と外部プレイヤー（QuickTime / Chrome / Edge / VLC）での再生互換 | K9（mp4）、G3 | `core/crates/arb-media/tests/media_pipeline.rs::mux_parse_and_index_exact_frames_and_gops`、`core/crates/arb-media/tests/audio_pipeline.rs::round_trip_mux_and_open_verifies_exact_packets_asc_faststart_and_zero_elst` | FFI 内部は Miri/Kani の対象外。外部プレイヤー再生は Stage 3 |
| TCB-10 | Android Studio 2026.2 / JBR ランタイム ABI、`dispose()` ライフサイクル、リフレクション対象、Compose/Skiko 描画 | KON-1..5、S1 | `ide-plugin/src/test/kotlin/io/github/shinma06/arb/ArchitectureKonsistTest.kt` | IDE 上の実描画と実ライフサイクルは VM IDE 受け入れ（`docs/verification/vm-acceptance.md`） |
| TCB-11 | FHD 30fps の体感遅延基準（open ≤ 1s、seek 0.2–0.5s、trim は数秒以内）を対象開発機で満たす | なし（性能は形式層の対象外） | `core/crates/arb-media/tests/media_pipeline.rs` | 実機ハードウェアでの計測は Stage 3 |
| TCB-12 | 配布 ZIP のバイナリ / JAR / `THIRD_PARTY_NOTICES.md` の法的来歴（GPL/AGPL/LGPL 0 件、PDD ツールは開発専用） | RS-1、ライセンス検査 | `core/scripts/check-licenses.py`、`core/scripts/package_distribution.py --self-test` | Kani・Miri・Stateright・Konsist は dev/test 依存でのみ使い、配布物に含めない（ZIP 構造検査で確認） |

## 5. Sara 互換性

この文書は `docs/` 配下にあるが、Sara のトレーサビリティ項目（front matter）を持たない説明文書であり、`sara check --strict` の検査対象項目を増減しない。
