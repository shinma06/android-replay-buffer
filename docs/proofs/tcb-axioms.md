# 5層 PDD 検証基盤と TCB 公理台帳

この文書は、Proof-Driven Development（PDD）の五層で何を機械的に検証し、何を検証できずに信頼しているか（TCB: Trusted Computing Base）を記録する。証明は本番コードそのもの、または本番状態遷移を忠実に線形化したモデルを対象とし、空疎な前提（`kani::assume(false)` など）は使わない。

## 1. 五層の概要と実行コマンド

| 層 | 道具 | 対象 | 実行コマンド | 実行頻度 |
|---|---|---|---|---|
| 1 | Kani（CBMC） | 純粋関数の全入力域（有界） | `cd core && cargo kani -p <crate> -Z stubbing`（必須集合: `arb-core` / `arb-os` / `arb-adb` / `arb-media` / `arbd`）、拡張集合は `--features kani-extended` を追加（`arb-core` / `arb-media` / `arbd`） | `.github/workflows/pdd-kani.yml`（毎週・手動・`workflow_call`・証明ファイルを変更した `develop` への push） |
| 2 | Stateright | 並行ライフサイクルの全インターリーブ | `cargo test --manifest-path core/Cargo.toml -p arbd --test pdd_stateright_models -- --nocapture` | 毎回の `cargo test`（3 OS） |
| 3 | Proptest / Miri | 実 I/O（SQLite・FS・MP4・ワイヤ）と未定義動作 | `cargo test --manifest-path core/Cargo.toml --workspace`、`cargo +nightly miri test -p arb-os --lib shm::`、`PROPTEST_CASES=8 cargo +nightly miri test -p arb-core --lib`、`PROPTEST_CASES=8 cargo +nightly miri test -p arb-media --lib mp4::` | Proptest は毎回、Miri は `pdd-kani.yml` |
| 4 | Konsist / 構造ガード | Kotlin と Rust のアーキテクチャ規則 | `cd ide-plugin && ./gradlew test --tests io.github.shinma06.arb.ArchitectureKonsistTest`、`python3 core/scripts/check-architecture.py` | 毎回（Gradle テスト、`core/scripts/verify.sh`、CI `os-parity`） |
| 5 | TCB 台帳 | 形式層が前提とする外部公理 | この文書 | 変更時にレビュー |

Proptest の試行回数は `PROPTEST_CASES` で上書きできる。重い SQLite / FS 系は既定 24〜64 件に抑えている。

## 2. 検証インベントリ

### 2.1 Layer 1: Kani

ハーネスは二つの集合に分かれる。**必須集合**は `pdd-kani.yml` の `kani` ジョブ（`cargo kani -p <crate> -Z stubbing`）で毎回すべて検証し、失敗・打ち切りはワークフローの失敗になる。必須集合のハーネスは、手元（Kani 0.68 / CBMC 6.11、Apple Silicon）で 1 本 60 秒以内に終わる。CI の Linux ランナーでは実行ごとのばらつきが大きく 60 秒を超えるものもある（§3.1）が、ハーネスごとの打ち切りはなく、ワークフローの制約はジョブのタイムアウト（180 分）だけである。**拡張集合**は手元でこの時間内に検証が終わらなかったハーネス（多くは 5〜15 分で打ち切り、またはメモリ不足）で、`#[cfg_attr(feature = "kani-extended", kani::proof, ...)]` で通常の `cargo kani` から外し、`--features kani-extended` を付けたときと、結果を記録するだけの `kani-extended` ジョブ（`continue-on-error`、ハーネスごと 15 分打ち切り）でのみ実行する。拡張集合の性質は**形式的には未証明**であり、括弧内の既存テストで確認しているにとどまる。

| ID | 必須集合 | 拡張集合（未証明） | ファイル |
|---|---|---|---|
| K1 | `k1_anchored_matches_oracle_and_swap_invariant`、`k1_bounded_matches_oracle_and_swap_invariant`、`k1_unanchored_always_uncertain`、`k1_anchored_window_expansion_never_demotes`、`k1_bounded_window_expansion_never_demotes`、`k1_frame_range_anchored_by_index_only`、`k1_frame_range_bounded_by_index_only`、`k1_frame_range_unanchored_never_main` | なし | `core/crates/arb-core/src/kani_proofs.rs` |
| K2 | `k2_select_records_equals_trim_with_empty_sidecar_n1`、スライス層 5 本: `k2_slice_select_records_partition_empty_sidecar_n2` / `_n3`、`k2_slice_prior_sidecar_none_main_two`、`k2_slice_prior_sidecar_one_consumes_budget_first`、`k2_slice_prior_sidecar_two_consumes_budget_first`（`partition_with` を固定長配列で検証。組み替えは下記） | `k2_select_records_equals_trim_with_empty_sidecar_n2` / `_n3`、`k2_prior_sidecar_none_main_two`、`k2_prior_sidecar_one_consumes_budget_first`、`k2_prior_sidecar_two_consumes_budget_first`（出力 `Vec` と `classify_record` まで通す元の形。分割の性質はスライス層で必須集合に移した。G4 でも確認） | 同上 |
| K3 | `k3_pc_sync_probe_boundary_math`、`k3_elapsed_same_boot`、スライス層 13 本: `k3_slice_map_monotonic_one_triple_no_panic_and_boot_tagged`、`k3_slice_map_monotonic_two_triples_no_panic_and_boot_tagged`、`k3_slice_map_monotonic_monotone_on_two_ingest_ordered_triples`、`k3_slice_map_logcat_wall_one_triple_no_panic_and_causal_ceiling`、`k3_slice_map_logcat_wall_two_triples_stable_epoch_causal_ceiling`、`k3_slice_map_logcat_wall_three_triples_stable_epoch_causal_ceiling`（壁時計の飛びなし）、`k3_slice_map_logcat_wall_two_triples_jump_between_read_{before_jump,inside_jump,after_jump}_wall_{forward,rollback}`、`k3_slice_map_logcat_wall_two_triples_jump_between_without_read` | `k3_map_device_one_triple_no_panic_and_boot_isolated`、`k3_map_device_two_triples_no_panic_and_boot_isolated`、`k3_map_device_monotonic_monotone_on_two_triples`、`k3_map_logcat_wall_no_panic_and_causal_ceiling`（`ClockTimeline` 全体経由）、`k3_slice_map_logcat_wall_three_triples_with_jumps_causal_ceiling`（`arb-core` の `time` 単体テストで確認） | 同上 |
| K4 | `k4_ring_eviction_plan_realistic_sizes_n1`（1 チャンクでの REQ-04 ピン保護・容量・空き容量下限・各停止判定の正当性。サイズは `u8`、上限は `u16` の範囲）、スライス層 5 本: `k4_slice_plan_n2_unpinned_unpinned` / `_pinned_unpinned` / `_unpinned_pinned` / `_pinned_pinned`（2 チャンク、ピンの組ごと。最古優先の順序と `chunk_id` による同時刻の決着、過剰退避なしを含む。サイズは `u8`、上限は `u16`、空き容量・下限・書き込みは `u64` 全域）、`k4_slice_plan_extreme_u64_n1`（`u64` 全域の 1 チャンクで同じ契約） | `k4_ring_eviction_plan_extreme_u64_n1`（`Vec`/`String` を通す元の形。CI の Linux で 68 秒）、`k4_ring_eviction_plan_realistic_sizes_n2` / `_n3` / `_n4`、`k4_ring_eviction_plan_extreme_u64_n2` / `_n3` / `_n4`、`k4_slice_plan_extreme_u64_n2`（`u64` 全域の 2 チャンク。手元 18 秒で CI の余裕が足りない）（3 チャンク以上は G2 で確認） | 同上 |
| K5 | `k5_every_table_edge_is_reachable_by_apply`、`k5_bundle_lifecycle`、`k5_save_job_apply_matches_transition_table_{pinned,draining,materializing,completed,discarded}_pins{0,1,2}`（15 本）、`k5_save_job_apply_matches_transition_table_failed_retryable_path_{empty,blank,real}_pins{0,1,2}`（9 本） | なし | 同上 |
| K6 | `k6_physical_one_pixel_never_jumps_to_bottom_right`、`k6_map_touch_to_frame_bounds_and_domain`、`k6_monotonic_{x,y}_integer_pixels_*`（6 本: 1080→460、2400→1024、1080→1080、1920→1920、3840→1920、2160→1080、整数座標）、`k6_monotonic_x_any_f64_pow2_1024_to_512`、`k6_monotonic_y_any_f64_pow2_2048_to_1024`、`k6_monotonic_x_any_f64_pow2_1_to_1024`（2 の冪寸法、有限 `f64` 全域）（以上 `arb-core`）、`k6_axis_project_range_and_endpoints_span_{0_32767,0_4095,0_1079,0_1080,m1000_1000}`、`k6_axis_project_monotonic_span_{同上}_display_pixels`（`arbd`、軸範囲は固定、値は任意の `i32`） | `k6_map_touch_to_frame_bounds_and_monotonic`、`k6_monotonic_x_integer_pixels_symbolic_dims_le_4096`、`k6_monotonic_x_any_f64_1080_to_460`、`k6_monotonic_y_any_f64_2400_to_1024`（`arb-core`）、`k6_axis_project_range_and_endpoints`、`k6_axis_project_monotonic`（`arbd`、記号的な `(min, max)`。`capture.rs` の `axis_calibration_is_per_device_and_preserves_uncalibrated_pixels` などで確認） | `core/crates/arb-core/src/kani_proofs.rs`、`core/crates/arbd/src/touch_calibration.rs` |
| K7 | `k7_empty_and_single_sample_strokes_are_taps`（`f64::hypot` を任意値スタブで過大近似）、`k7_touch_slop_finite_positive`、`k7_decimate_keeps_endpoints_and_cap_len{2..5}_cap{2..4}`（12 本）、`k7_decimate_passes_through_len3_cap0` / `_cap1`、`k7_decimate_passes_through_single_sample` | なし | `core/crates/arb-core/src/kani_proofs.rs` |
| K8 | `k8_required_buffer_bytes_exact_or_none`、`k8_required_buffer_bytes_rejects_u64_overflow`、`k8_required_buffer_bytes_covers_every_pixel`、`k8_align_up_exact`、`k8_compute_layout_slots_disjoint_and_aligned` | なし | `core/crates/arb-os/src/shm.rs` |
| K9 | `k9_checked_entry_count_bounds_allocation`、`k9_iter_boxes_children_stay_in_parent`、`k9_ticks_to_ns_exact_ts_1`、`k9_parse_stts_bounded`、`k9_parse_stts_rejects_truncated_header`、`k9_parse_stsc_strictly_increasing`、`k9_parse_stsc_rejects_empty_or_truncated`、`k9_parse_stsz_bounded_by_file`、`k9_parse_stsz_explicit_sizes_fit_box`、`k9_parse_stsz_rejects_truncated_header`（`arb-media/src/kani_proofs.rs`）、`k9_encode_chunk_offset_never_truncates`、`k9_stco_chosen_only_when_every_offset_fits`（`arb-media/src/mux_av.rs`）、`k9_frame_header_accepts_iff_valid_and_roundtrips`、`k9_frame_header_encode_decode_full_u64_len`（`arb-adb/src/wire.rs`） | `k9_ticks_to_ns_exact_ts_90000`、`k9_ticks_to_ns_exact_ts_u32_max`、`k9_ticks_to_ns_monotone_ts_1` / `_90000` / `_u32_max`、`k9_avcc_sample_to_annexb_bounded`（G3 と M2 で確認） | 各ファイル |
| K10 | `k10_normalization`（任意の i64 PTS 3 個を正規化し終端用の余地を保存）、`k10_frame_and_trim`（3 枚、各間隔・末尾長 1〜65536 ns、全時刻で床検索・単調性・包括 OUT 逆変換）、`k10_fraction`（1 枚、D が 4 または 7 ns の全整数時刻で端点・単調性・往復） | 長尺・1 枚・静止とバーストは G3 と JVM の共通ベクトル・仮想時計でも検証 | `arb-media/src/timeline.rs`、`arb-media/src/kani_proofs.rs` |
| K11 | `k11_g8_1_api_33_plus_is_never_api_below_min`、`k11_g8_2_unknown_api_is_never_api_below_min`、`k11_g8_4_api_28_to_32_degrades_to_video_and_logs`、`k11_g8_9_disabled_modes_ignore_every_outcome`、`k11_g8_6_fatal_audio_failure_is_sticky_until_restart`、`k11_output_compat_api_floor`、`k11_explicit_source_change_retries_audio`（PDD G8。API は `u32` 全域、モード・音声要求・ラッチ・`scrcpy-server` の音声結果 8 種はすべて記号値。G8-6 は再接続 4 回までの有界な監視モデルで、端末の起動は前進のみ・各回の前に停止→開始が入りうる。期待値はハーネス内の独立オラクルで、本番の `is_fatal` や `label_class` を使わない） | なし | `core/crates/arb-core/src/kani_proofs/k11_audio_policy.rs` |
| K12 | `k12_frame_and_gop_byte_budget_no_overflow_and_bounded`（`u16` 全域の寸法・全 4 画素形式・任意 GOP 長と予算で溢れなし、128 MiB での GOP ストリーミング判定が `len > budget / frame` と厳密一致）、`k12_forward_window_eviction_keeps_cached_bytes_within_budget`（任意 `usize` のフレーム 3 枚・任意の確保可能予算で `cached_bytes == 保持バイト ≤ max_bytes`、窓は連続で復号位置で終わり、返すのは常に `target`）、`k12_sequential_forward_decode_avoids_idr_restart_even_over_128mib`（最大 `u32::MAX` 枚の GOP で、冷えたシークは IDR から 1 回だけ、保持 3 枚までの任意の窓から `+1` 要求は再起動なしで 1 枚だけ復号。帰納法で `k` 歩は `k` 回の復号）、`k12_backward_or_cross_gop_seek_restarts_from_target_idr_and_bounds`（逆方向・別 GOP・別形式は対象 GOP の IDR から `IDR..=target` だけ復号、窓内の逆方向は復号 0）、`k12_filmstrip_cache_rejects_mismatched_content_digest`（`ARBFS002` でファイル長・フレーム数・SHA-256 指紋の 3 つが一致したときだけ受理、指紋だけの不一致と旧 `ARBFS001` は拒否） | なし | `core/crates/arb-media/src/playback.rs`、`core/crates/arb-media/src/kani_proofs.rs` |
| K13 | 実時間トリムの範囲と正長、ns 総尺保存、任意の相対時刻での表示フレーム同値、不正範囲の拒否、ティックの正長と総和 | VFR 3 枚の各間隔 1〜65536 ns と全有効区間。量子化は各 1〜256 ns の 3 枚、8 ns/tick で丸め・予約・表現不能の各分岐を検証する。MP4 の復号結果と timescale 切替は G3 とバーコード試験で検査する | `arb-media/src/timeline.rs`、`arb-media/src/kani_proofs.rs` |
| K14 | `k14_carry_in_none_without_coverage`（同じキーの 1 本の被覆区間が `[f_prev, W0]` を含まなければ必ず `None`、含めば必ず `f_prev` を選ぶ）、`k14_carry_in_selects_latest_valid_pre_window_frame`（選ぶのは `W0` より前で位置・時刻とも最も遅いフレームだけで、後続に窓前フレームがない）、`k14_carry_in_rejects_boot_session_or_dims_mismatch`（boot・セッション・寸法のキーが一致する被覆区間なしでは選ばず、最遅フレームが不一致なら古い一致フレームに戻らない）、`k14_coverage_duration_bounded_and_monotone`（`0 ≤ 被覆 ≤ 窓長`、区間の追加で減らない、1 区間は切り取り長に一致、窓全体を覆えば窓長）、`k14_ledger_beacon_extends_only_within_gap`（ビーコンだけでは開かず、途絶前のビーコンだけが区間を延ばし、遅れたビーコンは閉じる） | フレーム 3 枚（時刻は `i16` 全域、受信順で非減少）・被覆区間 2 本・キー 3 種・`W0` と窓端は `i16` 全域、途絶判定の幅は `u8` 全域。本番の `select_carry_in`・`coverage_duration_in_window`・`StreamCoverage` を直接実行する。保存経路の時刻軸の対応、MP4 の書き出し、端末のビーコン・切断の意味は保存の端から端までの試験（`capture_coverage_tests.rs`）と G3 で確認する | `arb-core/src/coverage.rs`、`arb-core/src/kani_proofs/k14_coverage.rs` |

K3〜K9 の一部は、完了しなかった元の形を次のように組み替えて必須集合に入れている。組み替えで定義域を狭めた分は、各項目に明記した範囲だけが機械検証済みである。

- K8: 二つの 64 ビット乗算器の同値性と `y * stride` の非線形境界を一度に解かせる形は 13 分経っても終わらなかった。`exact_or_none`（高さ 2^16 以下で `len == stride * rows`）、`rejects_u64_overflow`（溢れる唯一の形）、`covers_every_pixel`（全 `u32` 域での `row < rows` と `col < stride` の線形な事実）に分け、三つを合わせて 8K を含む高さ 2^16 以下の全フレームで元の性質を導く。高さ 2^16〜2^31 の厳密値は機械検証していない。
- K9 のパーサ: エラー文の `format!` が記号整数を十進文字列にする処理（64 ビット除算のループ）で CBMC が止まるため、`#[kani::stub(std::fmt::format, ...)]` で文字列生成だけを空文字に置き換える。`Ok`/`Err` の判定と境界検査はすべて本番コードのまま検証する。
- K9 の表パーサ（`parse_stts` / `parse_stsc` / `parse_stsz`）: 記号長スライスをやめ、固定長配列（stts 24 バイト、stsc 32 バイト、stsz は定数サイズ分岐 12 バイト・明示サイズ分岐 20 バイト）にした。エントリ数と各フィールドは記号値で、0〜2 エントリと末尾の余りバイトを含む。短すぎる入力の拒否は別ハーネス（7 / 11 バイト、stsc は 7・11・15 バイト）で検証する。長さによる受理条件は `checked_entry_count`（24 バイト以下の全長さ）で別に証明済み。各ハーネスの `kani::cover!` で `Ok` 経路が到達可能（SATISFIED）であることも確かめている。パーサの戻り値は `ManuallyDrop` で保持し、`MediaError` 内の `io::Error` の drop glue だけを解かせない（TCB-14）。
- K9 `ticks_to_ns`: `ts = 1` のオラクルを除算から乗算だけの商余り関係（符号なしは `q·ts ≤ n < (q+1)·ts`、符号付きは 0 方向切り捨て版、飽和は `n ≥ MAX·ts` / `n ≤ MIN·ts`）に置き換えた。積はすべて 2^127 未満なので `wrapping_*` は正確である。`ts = 90000` / `u32::MAX` と単調性 3 本は、本番側の 128 ビット除算器がボトルネックのまま（ソルバーを CaDiCaL / Kissat / MiniSat / Z3 に替えても、ticks を 24 ビットに制限しても 10 分で終わらない）。
- K3: `ClockTimeline` は `ingest_device_triple` を通さないと作れず、`BTreeMap<String, BootEpochState>`・`String`・`Vec<DiscontinuityEvent>` の展開で triple 1 本でも CBMC がメモリ不足になる。そこで `time.rs` から、`epochs.get(boot_id)` で boot epoch を選んだ後の本体を `map_monotonic_in_epoch` / `map_logcat_wall_in_epoch`（`pub(crate)`）として抽出し（挙動不変のリファクタ。`epoch.triples` → `triples`、`self.config` → `config`、`epoch.wall_jump_indices` → `wall_jump_indices` の置き換えのみ）、固定長の triple 配列（1〜3 本）と記号的な設定値で直接検証する。単調性と飛びあり経路は ingest の不変条件（同じ epoch 内で `monotonic_ns` と `boottime_ns` が非減少）を前提にし、`wall_jump_indices` は ingest が記録しうる全添字集合の上位集合を与える。飛びありの 2 triple は、読み取り時刻の位置（飛びの前・中・後・読み取りなし）× 壁時計オフセットの向き（前進・巻き戻し）で入力を分割した（求解時間のためだけの分割で、合わせて全入力を被覆する）。`ClockTimeline` 全体を通す元の 4 本と、3 triple・飛びありの 1 本は拡張集合に残す（TCB-15）。
- K4: チャンク ID を 1 バイト（`"a".."d"`）にし、オラクル側で記号長 `Vec` を再ソートせず、出力の順序を直接検査する。必須集合に入れたのは、現実的なサイズ（チャンクのバイト数と時間は `u8`、上限は `u16`）での 1 チャンクの安全性（REQ-04 ピン保護、容量、空き容量の下限、各停止判定の正当性）だけ。同じ性質を `u64` 全域で見る `extreme_u64_n1` は手元 24 秒だが CI の Linux で 68 秒かかり（§3.1 のとおり CI では他の昇格ハーネスにも 60 秒超の実行がある。これは保守的な判断である）、`(status, pin)` ごとの分割、退避可否での分割、MiniSat / Kissat への切り替えでも 1 本 17 秒（手元）を切れなかった（1 回の SAT 呼び出しが支配的）ため拡張集合に残す。最古優先の順序は 2 チャンク以上でのみ意味を持ち、n = 2 の安全性だけでも 150 秒で打ち切ったため拡張集合に残す。`alloc::slice::stable_sort` は挿入ソートでスタブ化する（TCB-13）。2 チャンクの順序は後述のスライス層で必須集合に入れた。
- K5: 6 状態 × 7 イベントの直積を一度に解かせる形は CBMC が 31 GB でメモリ不足になった。開始状態とピン数（0〜2）ごとに分け、`Retarget` を受理する唯一の状態 `FailedRetryable` はさらに再ターゲット先（空・空白・実パス）ごとに分ける。イベントは記号値 `k % 7` のまま 7 種すべてを検証し、各性質は含意形（`!ok || …`）で全経路の表明に到達させる。24 本を合わせると、ピン 2 個以下・パス 3 種での元の性質になる。
- K6 `map_touch_to_frame`: 境界側（Some になる条件と出力範囲）は記号的な寸法（2^14 以下、0 を含む）・全 4 形の表示ジオメトリ・NaN と ±∞ を含む任意の `f64` 座標で検証する。単調性は軸ごとに分け（他方の軸は `1 → 1` の `0.0` に固定）、非 2 冪の具体寸法 6 組では整数座標、2 の冪寸法 3 組では有限 `f64` 全域で検証する。非 2 冪の定数による記号 `f64` 除算は 10 分で終わらず、記号的な寸法も 5 分で終わらないため拡張集合に残す。`1 → 1` 寸法は `min(frame - 1)` で出力が常に 0 になり単調性の検査にならないため、必須集合に入れない。
- K6 `Axis::project`（`arbd`）: 記号的な `(min, max)` では除数が記号になり 128 ビット × 128 ビットの乗算器が残る。実在するタッチ軸の範囲 5 種（`0..=32767`、`0..=4095`、`0..=1079`、`0..=1080`、`-1000..=1000`）に固定し、値は任意の `i32` で検証する。範囲・端点は画素数 `1..=2^13` の記号値、単調性は画素数 720/1080/1440/2400/3120/4096 のいずれかで `v` と `v + 1` を比べる（全組は帰納法で従う）。`0..=1080` と `0..=1079` は `min == 0` のドライバ特例（`max == pixels` / `max == pixels - 1`）を通る。
- K7 `decimate`: `len` と `cap` が両方記号だと `u128` の歩幅除算で CBMC がメモリ不足になる。具体的な `(len, cap)` ごとに分け、元の記号的定義域 `len ∈ 1..=5`、`cap ∈ 2..=4` を全て被覆する（`len = 1` は `cap` 記号の単一サンプル版）。性質も強化した（出力長、各出力が無改変の入力で添字が厳密増加、先頭と末尾を保持、座標は任意の `f64`）。元の記号版は削除した。
- K2 スライス層: 元の K2 は `TimestampedRecord`（`String` と `TimeAlignment` を持つ）を記号的に作り、出力 `Vec` を 2 本組み立てるため、2 件以上で CBMC が約 31 GB でメモリ不足になった。`select_for_trim_with_truncated` の分割本体を `partition_with`（`pub(crate)`）として抽出し（挙動不変のリファクタ。`unanchored.len()` → `tally.unanchored_records`、`Vec::push` → `emit(lane, record)`、`classify_record(&record, frame_window)` → 引数の `classify` への置き換えのみ。`select_records` / `select_for_trim` / `select_for_trim_with_truncated` はすべてこれを通る）、`Copy` なレコードの固定長配列と記号的な所属（`InRange` / `OutOfRange` / 不確実 2 種）で直接検証する。期待値はハーネス内の独立した逐次参照で、全出力の順序・行き先と 4 つの計数を突き合わせ、保存則・上限・既存サイドカーの先行と非分類・入力順を表明する。`classify_record` 自体の正しさは K1 の担当で、`n1` の元の形は全経路を通したまま必須集合に残す。
- K4 スライス層: `RingEvictionPlanner::plan` の本体を `plan_refs`（`pub(crate)`、`&mut [&RingChunkMeta]` を受け取り、退避数と停止理由だけを返す）として抽出した。退避可能チャンクの抽出は `filter().collect()` から同じ順序を保つその場の前詰めに変わり、`plan` は参照を `Vec` に集めて先頭 `evict_count` 件の ID を複製するだけになった（判定順序、`u128` の合計、`(created_cont_ns, chunk_id)` の安定ソート、`pinned_bytes`、各停止理由の値は不変）。ハーネスは固定長のチャンク配列と参照配列を渡すため、`Vec<&RingChunkMeta>` も ID の `String` 複製も展開しない。2 チャンクはピンの組 4 通りに分け（`pinned_pinned` では退避可能なチャンクがないため、退避ループ内の表明は構造上到達せず、代わりに「何も退避しない」を表明する）、`u64` 全域は 1 チャンクを必須、2 チャンクを拡張集合とする。3 チャンク以上は必須集合にない。
- K12 `playback.rs`: `GopLruCache::fetch_forward` の窓操作を `reuses_forward_window`（窓を再利用できるか）と `advance_forward_window`（`next..=target` を順に復号し、古いフレームを捨てて予算内に保つ）、`fetch_frame` の全 GOP 判定を `full_gop_exceeds_budget`、`FilmstripCache::load_from_file` の検査を `from_bytes` として抽出した（挙動不変のリファクタ。窓は `FrameWindow` トレイトを通し、本番は `VecDeque<DecodedFrame>` そのもの。`frame.byte_len()` → `bytes()`、`frame.frame_index` → `index()` の置き換えのみ）。ハーネスは固定長配列の FIFO モデル `Window` と、`fetch_forward` の再利用・IDR 再起動の分岐を写した `ForwardModel` を使う（TCB-20）。逐次復号は「冷えたシーク」と「保持 3 枚までの任意の窓からの `+1` 歩」の基底・帰納段に分け、GOP 長は `u32::MAX` まで、フレームサイズと予算は記号値のまま検証する。予算は `1..=isize::MAX`（Rust の確保上限）に限る。`usize::MAX` では飽和した比較が発火せず計数が溢れうるが、本番の予算は 128 MiB 定数である。`filmstrip.bin` はエントリ 0 件・正しいチェックサムの 68 バイトに固定し、期待側のファイル長・フレーム数・指紋 32 バイトを記号値にする（エントリ解析部は到達しない）。

### 2.2 Layer 2: Stateright

| ID | モデル | 検査する性質 |
|---|---|---|
| S1 | IDE JVM 2 台・daemon・tray・CLI 常駐 daemon・単一インスタンスロック | ロック所有者は高々 1、保存中・クラッシュ処理中の強制停止なし、所有者以外は許可済みハンドオーバーなしに停止できない、全親の死亡と EOF 後に親結合子が残らない、ウィンドウ 1 枚を閉じても子は生存、tray 無効化で daemon は止まらない、CLI 常駐 daemon は CLI 終了後も生存。収束: 全親死亡後に子とロックが解放される／生存 IDE の supervisor が tray を再取得し、手動終了は自動復活しない |
| S2 | REQ-04 二相保存・単一ライタ・ディスク圧迫・クラッシュ／再起動／アップグレード・ゴミ箱 24h 猶予・リース・GC | `data_dir` ライタは高々 1、非終端ジョブが pin したチャンクは退避されない、Phase-1 後は `Committed` か回復可能、物理削除は管理下・非レガシー・`Trashed`・24h 経過・リース 0・ジョブ 0 のときのみ、レガシー schema 3/4 と非 ARB `.staging-*` は不変、クラッシュ回復後に ARB 所有 staging が残らない、再起動後の保存窓は新しい boot を選ぶ。到達性: ディスク回復後 `FailedRetryable` は `Completed` に到達できる |
| S2 GC | GC の公開パス置換・二段階 claim（`.purging-` → `.purged-`）・再検証・クラッシュ再開 | 外来実体は決して物理削除されない、照合失敗時は `Trashed` に戻り公開パスが空なら返却する。到達性: 事前照合と claim の間の置換が最終照合で止まる／元実体が削除されても公開パスの置換物は残る |
| S3 | 世代 0..=2・キャプチャ・ストレージ圧迫・シャットダウン・映像／音声（API 28–32 と 33+）・制御・古いワーカ・保存ドレイン水位・ロック順序 | 古い世代のイベントは現世代を変更しない、遅れた水位があるのにドレイン完了を報告しない（タイムアウト／ギャップ記録を除く）、`handover_gate` → `capture` のロック順序を逆転しない、ユーザーの再有効化を `AutoAttachTick` が消さない、`shutting_down` は `capture_enabled` を永続化しない、映像のみの EOF は再接続／ギャップに遷移する |
| S3 attach | 遅延アタッチ（`(serial, generation)` トークン）・端末切替・ストレージ圧迫・再有効化 | 選択中の端末・現世代のみ稼働する、再有効化状態は遅延アタッチ完了後も保持される。再起動後も明示した端末と録画意図の組を引き継ぐ（[S3 attach](../../core/crates/arbd/tests/pdd_stateright_capture_s3.rs)）。到達性: 別端末の古いアタッチが棄却される／圧迫回復後に再開できる |
| S4 | 2 接続 × 2 世代のビューポート開閉・書き出し完了・接続 EOF・ハンドオーバー | ビューポートまたは書き出し稼働中のハンドオーバーは拒否される、EOF と完了のドレインで必ずアイドルに収束する、1 要求あたりの公開は高々 1 回。到達性: EOF 後も書き出しが生存する／両世代で両クライアントが公開できる |
| S5 | クラッシュ自動保存の検出・永続 pin・再起動リプレイ・端末再起動（設計前提、TCB-16） | 同一 boot 内の同一クラッシュ確定は高々 1 回、永続 pin は再起動後に必ずリプレイされる。到達性: 別 boot の同一署名は 2 回保存される |
| S6 | Studio の再生時計、seek 世代、共有 RPC の再接続、編集タブが所有する viewport と SHM reader | 遅延応答が意図時刻と新しい再生を保つ。同一フレームの保持中に seek を追加しない。失敗時は有限回で理由を表示して停止し、再生再開時はループが起床する。再取得と破棄が競合しても資源を解放する。 |

S6 は `cargo test --manifest-path core/Cargo.toml -p arbd --test pdd_stateright_playback_s6 -- --nocapture` で再現できる。PTS を 0・3 ns、終端を 4 ns とした 2 枚の VFR 映像について、通常再生 5,157 状態、ループ 5,157 状態、単一フレーム区間ループ 4,334 状態を探索した（Apple Silicon の debug ビルドで合計 0.12 秒）。世代は実行中の要求が現行か旧世代かで区別する。`seekLock` の待ち行列では、世代照合を通れる最新の要求を保持する。復旧と解放の収束は RPC が返り、待機スレッドが実行されることを前提とする。SHM 所有数はセッションが保持する reader と開いたファイルチャネルを数える。JVM が `MappedByteBuffer` の OS マッピングを回収する時刻は検証範囲外。

[共有トレース](../specs/playback-s6-traces.json) の16本では、同じ操作と障害の順序から Stateright が発行する seek 列を再構成し、最終状態を Kotlin と照合する。`PddS6PlaybackTraceReplayTest` は本番の再生・表示状態、接続サービス、viewport セッションと SHM reader を実行する。時計と daemon 側の応答・接続 EOF はテストで制御する。3個の変異（到着フレーム時刻の書き戻し、古い失敗の受理、再取得中の解放漏れ）に対して反例を確認する。

### 2.3 Layer 3: Proptest / Miri

| ID | 対象 | ファイル |
|---|---|---|
| G1 | 実 SQLite/WAL `Catalog`（2 接続、1–32 操作列）と独立参照モデル、暦日・オフセット厳格拒否、ゴミ箱 24h 猶予 | `core/crates/arb-storage/tests/pdd_g1_catalog_props.rs` |
| G2 | 実 `RingStore` + 偽ディスク + 再起動でリセットする偽時計 + 各境界での障害注入 → 再オープン → retry/retarget、pin チャンク SHA-256 不変、レガシー schema 3/4 フィクスチャ不変 | `core/crates/arb-storage/tests/pdd_g2_ring_crash_props.rs` |
| G3 | 破損 MP4・深い入れ子・巨大テーブルで panic/OOM/スタック溢れなし、可変 GOP / VFR の mux → open → trim → share 往復（`edts`/`elst` なし、faststart）、30 秒の静止保持（最終サンプル長を明示した 1〜3 枚の MP4）の生成 → 再オープン → 保持区間内の任意のサブフレーム実時間トリム（`elst=0`、保持中の画を維持） | `core/crates/arb-media/tests/pdd_g3_media_props.rs` |
| G4 | 複数ストリーム（ログ・音声・タップ・巨大ストローク・部分重複・別 boot）の保存 → 全範囲トリム保存則と非アンカー予算 | `core/crates/arbd/tests/pdd_g4_multistream_props.rs` |
| G5 | `SaveJobMaterializeParams` の過去 JSON 互換（全追加フィールドの `#[serde(default)]` 検査）、リングチャンクの符号化往復と任意バイト非 panic | `core/crates/arbd/src/capture_pdd_g5_tests.rs`、`core/crates/arbd/tests/fixtures/pdd_g5_params_v2_minimal.json` |
| G6 | wire / scrcpy ストリームの任意分割と `Interrupted`/`TimedOut`/EOF 注入（連続解析と一致、完全レコード接頭辞 + 明示ギャップ） | `core/crates/arb-adb/tests/pdd_g6_stream_fault_props.rs` |
| G7 | SHM 奇数幅・全 4 形式・stride/padding の Rust/Kotlin パリティ（共通ゴールデン表 48 行）と短いプレーンの拒否 | `core/crates/arb-os/tests/pdd_g7_shm_parity_props.rs`、`ide-plugin/client-core/src/test/kotlin/io/github/shinma06/arb/shm/ShmLayoutParityTest.kt` |
| G8 | 音声ポリシー（Android 13 の「音声キャプチャ非対応」誤表示の再発防止）。G8-3: `build_server_args` は API 33〜`u32::MAX` で `audio=true audio_source=playback audio_dup=true`。明示選択の互換方式では API 30 以上で `audio_source=output` を渡し、`audio_dup` を省く。各方式の既知の下限未満と無効時は `audio=false`。G8-5: API 33 以上・不明での失敗結果は実行時の原因へ単射に対応し、`scrcpy-server` 標準エラーの末尾が `audio_reason` に入る（純関数と、`codec_id = 0 / 1`・即切断を返す模擬ソケットでの `arbd` 音声リーダー）。G8-6: 致命的な失敗の後は同じ `(serial, boot_id)` が `audio=false` に固定される。G8-7: Kotlin の網羅表（全 `AudioCause` × 状態 × API で「非対応」の表示 ⟺ `api_below_min`）。G8-8: 共有正本表 `docs/specs/audio-policy-table.json`（2 方式、86 行）を Rust と Kotlin の両方が読み、状態・原因・表示区分の一致を検査 | `core/crates/arb-core/src/audio_policy/tests.rs`、`core/crates/arb-adb/tests/pdd_g8_audio_args_props.rs`、`core/crates/arbd/src/live_adb.rs`（`g8_*`）、`core/crates/arbd/src/capture.rs`（`g8_*`）、`ide-plugin/client-core/src/test/kotlin/io/github/shinma06/arb/state/AudioPolicyTableTest.kt` |
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
| KON-6 | 音声の「非対応」表示（`音声キャプチャ非対応` / `音声: 非対応`）を書けるのは `AudioPolicyTable` だけ。`AudioPolicyTable.classOf` は `else` のない網羅 `when`（PDD G8-7） | 同上 |
| RS-1 | `arb-core` の通常依存 ⊆ {serde, serde_json, thiserror}、`arb-cli`/`arb-tray` はエンジン crate に依存しない、`arb-storage`/`arb-media`/`arb-adb` 間の横依存なし、`arb-core` 本番コードに FS・ネット・プロセス・スレッド・時計・OS cfg なし | `core/scripts/check-architecture.py` |
| RS-2 | OS `cfg` は `arb-os`、`arb-media/src/codec/`、`arb-tray/src/native_tray.rs` のみ | 同上 |
| RS-3 | `forbid(unsafe_code)`: `arb-core`/`arb-storage`/`arb-adb`/`arb-cli`/`arbd`。`deny(unsafe_code)`: `arb-media`/`arb-tray`（許可は `videotoolbox_backend.rs` と `native_tray.rs` のみ） | 同上 |
| RS-4 | 本番コードの直接 `fs::rename` は `arb-os/src/fs.rs` のみ | 同上 |
| RS-5 | ライブラリ 5 crate の非テストコードで `unwrap`/`expect` を clippy で禁止、`arbd` の全 RPC が GUI（`ArbdRpcClient`）/CLI/MCP から呼ばれ、クライアントが未実装 RPC を呼ばない | 同上 |

## 3. 実測結果

2026-11-25、macOS arm64（Apple Silicon、10 コア）で計測した。Kani 0.68.0 / CBMC 6.11.0 / CaDiCaL 3.0.0、Miri は nightly（2026-11-23）。CI の Linux ランナーでの所要時間は `pdd-kani.yml` の実行で確かめる。

### 3.1 Layer 1: Kani 必須集合（`cargo kani -p <crate> -Z stubbing`）

| crate | 結果 | ハーネスごとの検証時間 |
|---|---|---|
| `arb-core` | 79 / 79 成功（実時間 537.5 秒） | K1 8 本 0.3〜1.1 秒、K2 `n1` 23.3 秒、K3 `pc_sync` 4.5 秒・`elapsed` 0.9 秒・スライス層 13 本 1.1〜26.6 秒（最長は `jump_between_read_*` の 4 本）、K4 `realistic_sizes_n1` 22.3 秒、K5 `every_table_edge` 8.3 秒・`bundle_lifecycle` 0.1 秒・`apply_matches_transition_table` 24 本 4.4〜15.6 秒、K6 `physical_one_pixel` 1.1 秒・`bounds_and_domain` 1.2 秒・単調性 9 本 0.3〜4.9 秒、K7 `empty_and_single` 0.2 秒・`touch_slop` 0.2 秒・`decimate` 15 本 0.5〜3.7 秒 |
| `arb-os` | 5 / 5 成功（7.3 秒） | K8 各 0.1〜5.2 秒 |
| `arb-adb` | 2 / 2 成功（11.8 秒） | K9 wire 6.1 秒・3.4 秒 |
| `arb-media` | 12 / 12 成功（158.9 秒） | K9 `ticks_exact_ts_1` 24.3 秒（乗算オラクル。旧オラクルは 28.3 秒）、`iter_boxes` 23.4 秒、`parse_stts` 17.7 秒・`stts_rejects_truncated` 11.1 秒、`parse_stsc` 14.4 秒・`stsc_rejects` 12.9 秒、`parse_stsz_bounded_by_file` 17.0 秒・`explicit_sizes` 8.8 秒・`stsz_rejects_truncated` 7.3 秒、`checked_entry_count` 1.0 秒、mux_av 5.3 秒・1.6 秒 |
| `arbd` | 10 / 10 成功（48.5 秒） | K6 `axis_project_range_and_endpoints_span_*` 5 本 3.5〜6.1 秒、`axis_project_monotonic_span_*_display_pixels` 5 本 2.3〜7.9 秒 |

PDD-Next で K2・K4 のスライス層 10 本と K12 の 5 本を必須集合に加えた（2026-11-25、同じ macOS arm64、`develop` `6103996a` との統合後）。必須集合は 118 本から 128 本になった（`arb-core` 86 → 96、`arb-media` 15 → 20、`arbd` 10・`arb-os` 5・`arb-adb` 2 は不変）。`cargo kani -p arb-core -Z stubbing` は 96 / 96 成功（実時間 687.6 秒。最長は K3 `jump_between_read_after_jump_wall_rollback` の 28.3 秒）で、K2 スライス層は 1.7〜2.6 秒、K4 スライス層は `n2` の 4 本が 3.0〜8.6 秒、`extreme_u64_n1` が 4.0 秒。`cargo kani -p arb-media -Z stubbing` は 20 / 20 成功（実時間 237.2 秒）で、K12 は `frame_and_gop_byte_budget` 7.9 秒、`forward_window_eviction` 4.6 秒、`sequential_forward_decode` 19.6 秒、`backward_or_cross_gop_seek` 9.8 秒、`filmstrip_cache` 18.6 秒。`--features kani-extended --only-codegen` は `arb-core`・`arb-media` ともコンパイルできた。15 本の `kani::cover!` 41 件はすべて SATISFIED。ハーネス本体の性質の表明で UNREACHABLE になったのは、`k4_slice_plan_n2_pinned_pinned` の退避ループ内の表明（`REQ-04 pin protection`、`oldest-first order`、`no over-eviction` など。2 本ともピン留めで退避可能なチャンクがなく、このハーネスでは「何も退避しない」を表明する）だけで、同じ表明は他の `n2` 3 本で到達する。ほかに報告されるのは構造上の死にコード（要素 0 件の配列、`P = 0` での既存サイドカー側の分岐、`j == 0` で短絡する直前要素の参照、`n = 1` での `stable_sort_stub` のループ本体、K12 filmstrip のエントリ解析部と std の `fmt::Arguments::from_str` 内部）だけである。`kani::assume` は入力域の限定（予算 `1..=isize::MAX`、GOP の範囲が溢れない、GOP ID が互いに素なフレーム範囲を指す、窓の不変条件）だけで、`assume(false)` 相当はない。下の変異表の K2・K4・K12 の 9 件は手元で 1 件ずつ入れ、各 1 本のハーネスを `-Z concrete-playback --concrete-playback=print` 付きで実行した。`plan_refs` の古さ比較を逆にする変異だけは `pinned_unpinned`（退避可能 1 件で順序が効かない）では成功のまま残り、`unpinned_unpinned` で検出した。ほかの 8 件は表のハーネスで FAILED になり、具体的な反例の値も得た。各変異は確認後すぐに `git checkout` で戻し、差分がないことを確認した。

PDD G8 で K11 の 5 本を `arb-core` の必須集合に加えた（2026-11-25、同じ macOS arm64）。`cargo kani -p arb-core -Z stubbing` は 84 / 84 成功（実時間 528.7 秒。最長は K3 `jump_between_read_inside_jump_wall_forward` の 26.6 秒）。K11 は `g8_1` 0.04 秒、`g8_2` 0.04 秒、`g8_4` 0.07 秒、`g8_9` 0.05 秒、`g8_6` 0.23 秒で、すべて 60 秒の基準に十分収まる。K11 の性質の表明で UNREACHABLE になったものは 0 件（`g8_6` で報告される 1 件は std の `fmt::Arguments::from_str` 内部）。

互換方式の追加時に K11 全 7 本を再実行し、失敗 0、UNREACHABLE 0 を確認した。互換方式の下限を 30 から 31 に変更すると `k11_output_compat_api_floor` が失敗し、方式変更時の世代更新を無効にすると `k11_explicit_source_change_retries_audio` が失敗する。これらはホストの判定を検証する。OEM 端末での録音可否・消音・停止後の復帰は実機スモークで確認する。

K13 の macOS arm64 実測（2026-11-26、Kani 0.68 / CBMC 6.11）。arb-media 必須集合は既存 20 本と新設 5 本の計 25/25 が成功し、最長は 44.21 秒。範囲と正長 1.45 秒、総尺保存 4.09 秒、表示同値 4.59 秒、不正範囲 1.89 秒、量子化 6.42 秒。6 個の cover はすべて SATISFIED、ハーネス本体の性質表明で UNREACHABLE は 0 件。区間の証明では本番の `trim_intervals` が返す列を直接走査する。`Vec` の確保、MP4 の読み書き、コーデックは証明範囲の外にある。

K14 の macOS arm64 実測（2026-11-27、Kani 0.68 / CBMC 6.11）。`cargo kani -p arb-core -Z stubbing --harness k14_` は 5/5 成功（実時間 14.2 秒）。`none_without_coverage` 0.74 秒、`selects_latest_valid_pre_window_frame` 0.79 秒、`rejects_boot_session_or_dims_mismatch` 0.81 秒、`coverage_duration_bounded_and_monotone` 2.35 秒、`ledger_beacon_extends_only_within_gap` 4.00 秒。8 個の cover（引き継ぎの成立・拒否、後続フレームがある中間フレームの選択、古い一致フレームが存在する不一致、2 本目の区間による被覆増加、窓全体の被覆、ビーコンによる延長・途絶による終了）はすべて SATISFIED、K14 ファイル内の性質表明で UNREACHABLE は 0 件。必須集合は 138 本から 143 本になった（`arb-core` 96 → 101）。`arb-media` 必須集合 25/25 は本変更後も成功した。

K10 追加時の macOS 実測は `arb-media` 15/15 成功、正規化 0.42 秒・フレームとトリム 0.57 秒・比率 0.40 秒。K10 のハーネス本体の性質表明はすべて到達した。以下は K10・K11 追加前の比較記録。

必須集合は 28 本から 108 本になった（`arb-core` 16 → 79、`arb-media` 5 → 12、`arbd` 0 → 10、`arb-os` 5・`arb-adb` 2 は不変）。`arb-core` と `arb-media`・`arbd` は並行して実行したため、各値は単独実行より長めに出ている可能性がある。CI の Linux ランナー（`ubuntu-latest`）では手元のおよそ 1.5〜4 倍かかり、実行ごとのばらつきも大きい。統合前のブランチでの実行 45527036398 では `kani (arb-core)` ジョブが約 19 分（ハーネス最長は K4 `extreme_u64_n1` 68.1 秒、次いで K3 `jump_between_read_before_jump_wall_rollback` 53.9 秒、K4 `realistic_sizes_n1` 53.5 秒）。統合後の `develop`（`85bac571`）での実行 45536400328 では 108 本すべて成功し、`kani (arb-core)` ジョブは 21.7 分、`arb-media` 6.3 分、`arbd` 2.1 分で、タイムアウト 180 分に対して十分な余裕がある。ただしこの実行では K3 `jump_between_read_inside_jump_wall_forward` が 98.5 秒（手元 5.7〜26.6 秒）、K4 `realistic_sizes_n1` が 63.4 秒、K3 `jump_between_read_after_jump_wall_forward` が 60.5 秒と 60 秒を超え、以前から必須の K2 `n1` も 54.7 秒だった。昇格の基準は手元の 60 秒とし、CI の値は記録にとどめる。K4 `extreme_u64_n1` は、手元 24 秒で基準内だが、CI で 68 秒かかり 1 本 17 秒（手元）を切る分割も見つからなかったため、保守的に拡張集合へ戻した。

ハーネス本体の `assert` で UNREACHABLE になったものはない。例外は K1 の参照オラクルの `i128` 溢れ検査（構造上溢れない）と `variant == 2` 専用の表明（`k1_unanchored_always_uncertain` でのみ到達）、K2 `n1` の順序保持ループ（要素 1 件では到達せず、`n2`/`n3` は拡張集合）、K4 `n1` での `stable_sort_stub` のループ本体（長さ 1 以下のスライスでは回らない。TCB-13）だけで、今回昇格したハーネスの性質の表明には 0 件である。`kani::cover!` 4 件（stts の上限到達、stsc 2 エントリ、stsz 定数サイズ複数・明示サイズ 2 件）はすべて SATISFIED。`kani::assume` はハーネスの前提（入力域の分割、ingest の不変条件、性質の前件）にだけ使い、`assume(false)` 相当の空疎な前提はない。

昇格したハーネスが本番コードのバグを検出することを、1 行の変異を入れて確かめた（確認後すぐに元に戻し、差分が変異前と同一であることを確認）。

| 区分 | 変異（本番コード） | 検出したハーネス | 失敗した表明 |
|---|---|---|---|
| K2 | `selection.rs` `try_quarantine`: バイト上限 `next_bytes <= max_unanchored_bytes` → `<` | `k2_slice_prior_sidecar_one_consumes_budget_first` | `admitted record emitted`、`emitted in input order to its lane` |
| K2 | `selection.rs` `partition_with`: ingest 近接の上端 `ingest <= prox_hi` → `<` | `k2_slice_select_records_partition_empty_sidecar_n2` | `admitted record emitted`、`truncated count` |
| K3 | `time.rs` 飛びの中の読み取り: `min_boottime_ns: i64::MIN` → `before.boottime_ns` | `k3_slice_map_logcat_wall_two_triples_jump_between_read_inside_jump_wall_forward` | `read inside a jump: bounded by (i64::MIN, read]` |
| K4 | `state.rs` `plan`: 空き容量判定の `host_free_bytes.saturating_sub(incoming_bytes)` → `host_free_bytes` | `k4_ring_eviction_plan_realistic_sizes_n1` | `free.saturating_sub(incoming) >= min_free_disk_bytes` |
| K4 | `state.rs` `plan_refs`: 同時刻の決着 `a.chunk_id.cmp(&b.chunk_id)` → `b.chunk_id.cmp(&a.chunk_id)` | `k4_slice_plan_n2_unpinned_unpinned` | `oldest-first order`、`oldest-first prefix` |
| K4 | `state.rs` `plan_refs`: 古さの比較 `a.created_cont_ns.cmp(&b.created_cont_ns)` → `b.…cmp(&a.…)` | `k4_slice_plan_n2_unpinned_unpinned`（`pinned_unpinned` は退避可能 1 件で順序が効かず成功のまま） | `oldest-first order`、`oldest-first prefix` |
| K4 | `state.rs` `plan_refs`: 前詰めの条件 `is_evictable()` → `status == Closed`（ピンを無視） | `k4_slice_plan_n2_pinned_unpinned` | `REQ-04 pin protection`、`evicts evictable chunks only` |
| K5 | `state.rs` `Retarget`: `trim().is_empty()` → `is_empty()` | `k5_save_job_apply_matches_transition_table_failed_retryable_path_blank_pins1` | `empty retarget rejected` |
| K6 | `gesture.rs` `map_touch_to_frame`: `x / dw` → `(dw - x) / dw` | `k6_monotonic_x_any_f64_pow2_1024_to_512` | `monotonic non-decreasing` |
| K7 | `gesture.rs` `decimate`: `/ (cap - 1)` → `/ cap` | `k7_decimate_keeps_endpoints_and_cap_len5_cap3` | `last sample kept` |
| K9 | `mp4.rs` `parse_stsz`: 明示サイズの読み取り位置 `12 + i * 4` → `8 + i * 4` | `k9_parse_stsz_explicit_sizes_fit_box` | `sizes[i] == u32::from_be_bytes(...)` |
| K10 | 表示フレームの二分探索で `PTS <= t` を `PTS < t` に変更 | `k10_frame_and_trim` | PTS が 65536 ns の境界で前フレームを返し、区間被覆の表明が失敗（具体的反例を取得） |
| K11 | `audio_policy.rs` `api_is_below_audio_floor`: `level < MIN` → `level <= MIN` | `k11_g8_1_api_33_plus_is_never_api_below_min` | `API >= 33 is never api_below_min` |
| K11 | `audio_policy.rs` `api_is_below_audio_floor`: `level > 0 &&` を削除 | `k11_g8_2_unknown_api_is_never_api_below_min` | `unknown API is never api_below_min` |
| K11 | `audio_policy.rs` `plan_launch`: `if input.runtime_disabled` → `if input.runtime_disabled \|\| input.api.is_none()` | `k11_g8_2_unknown_api_is_never_api_below_min` | `unknown API asks for audio` |
| K11 | `audio_policy.rs` `plan_launch`: スティッキー判定を API 下限判定より前へ移動 | `k11_g8_4_api_28_to_32_degrades_to_video_and_logs` | `API 28..=32 is proactively off` |
| K11 | `audio_policy.rs` `evaluate`: `Disabled` の `video_expected` を `!logs_only` → `true` | `k11_g8_9_disabled_modes_ignore_every_outcome` | `video only without logs_only` |
| K11 | `audio_policy.rs` `is_fatal`: `Code1 \| LaunchFailedAudioOk` → `Code1` | `k11_g8_6_fatal_audio_failure_is_sticky_until_restart` | `a fatal boot never relaunches audio=true` |
| K12 | `playback.rs` `reuses_forward_window`: `target >= next` → `target > next` | `k12_sequential_forward_decode_avoids_idr_restart_even_over_128mib` | `+1 step keeps the cursor` |
| K12 | `playback.rs` `advance_forward_window`: 退避条件 `cached_bytes.saturating_add(frame.bytes()) > max_bytes` → `*cached_bytes > max_bytes` | `k12_forward_window_eviction_keeps_cached_bytes_within_budget` | `cached_bytes <= max_bytes`、`window is contiguous` |
| K12 | `playback.rs` `full_gop_exceeds_budget`: `estimate > max_bytes` → `>=` | `k12_frame_and_gop_byte_budget_no_overflow_and_bounded` | `exact 128 MiB decision` |
| K12 | `playback.rs` `from_bytes`: 指紋比較から `&source_digest != expected_source_digest` を削除 | `k12_filmstrip_cache_rejects_mismatched_content_digest` | `accepted exactly on ARBFS002 with a full fingerprint match` |
| K13 | OUT の二分探索で `p < out_time_ns` を `p <= out_time_ns` に変更 | `k13_time_range_trim_valid_bounds_and_positive_durations` | OUT が第 2 フレームの開始 48870 ns と一致すると、末尾に 0 ns のフレームを含む |
| K13 | 不正範囲判定で `in_time_ns >= out_time_ns` を `in_time_ns > out_time_ns` に変更 | `k13_time_range_trim_rejects_invalid_ranges` | IN = OUT = 70816 ns の空区間を受理する。両変異とも concrete playback で反例を取得し、復元済み |
| K14 | `coverage.rs` `select_carry_in`: `c.contains_span(f_prev.ts_ns, w0_ns)` → `c.start_ns <= f_prev.ts_ns`（`W0` までの被覆を見ない） | `k14_carry_in_none_without_coverage` | `uncovered span must never carry in` |
| K14 | `select_carry_in`: `rposition` → `position`（最も古い窓前フレームを選ぶ） | `k14_carry_in_selects_latest_valid_pre_window_frame` | `a later pre-window frame replaced it` |
| K14 | `select_carry_in`: キー一致 `c.key == f_prev.key &&` を削除 | `k14_carry_in_rejects_boot_session_or_dims_mismatch` | `selected frame must share the covering interval's key` |
| K14 | `coverage_duration_in_window`: `cursor = end` → `cursor = start`（重なりを二重計上） | `k14_coverage_duration_bounded_and_monotone` | `0 <= cov <= window_len` |
| K14 | `StreamCoverage::observe_alive`: 途絶判定 `<= self.max_gap_ns` を常に真にする | `k14_ledger_beacon_extends_only_within_gap` | `snap[0].end_ns == t0`。5 変異とも反例を取得し、元のファイルとバイト一致で復元済み |
| K6-Axis | `touch_calibration.rs` `Axis::project`: `value.min(last_pixel)` → `value.min(self.pixels)` | `k6_axis_project_range_and_endpoints_span_0_1080` | `0 <= p && p <= pixels - 1`、`max maps to last pixel` |

### 3.2 Layer 1: Kani 拡張集合（未証明）

次の 30 本は必須集合に入れていない（`--features kani-extended --only-codegen` で 3 crate ともコンパイルできることは確認済み）。元の 25 本のうち K5・K7 `decimate`・K9 表パーサ 3 本の計 5 本は性質ごと必須集合へ移し、K3・K4・K6 は定義域を絞った版を必須集合に入れたうえで元の形（または分割後の大きい側）を残した。原因は二つに分かれる。

- **SAT 段の時間**: 記号的な 64/128 ビットの乗除算。`ticks_to_ns`（定数 90000 / `u32::MAX` の 128 ビット除算器、単調性は 128 ビット積 2 つの突き合わせ）、`Axis::project`（記号的な除数 `max - min`）、K4 の `u64` 全域の 1 チャンク（CI の Linux で 68 秒）と 2 チャンク以上（`u64`/`u128` の和と記号的な時刻比較。n = 2 は 195 秒で成功、安全性だけでも 150 秒で打ち切り）、K6 の非 2 冪の定数・記号寸法による `f64` 除算（10 分超・5 分超）。
- **メモリ不足（OOM）**: 記号長のスライス・`Vec`・`BTreeMap` の展開。K2 の 2 件以上（SAT 段で約 31 GB）、K3 の `ClockTimeline` 全体（triple 1 本でも 30 GB 超）と 3 triple・飛びあり、`avcc_sample_to_annexb`（NAL スライスの読み戻しで 4 バイト入力でも 7 GB 超）。

- `arb-core`（22）: K2 `n2`・`n3`・`prior_sidecar` 3 本、K3 `map_device` 3 本・`map_logcat_wall`・`slice_map_logcat_wall_three_triples_with_jumps`、K4 `realistic_sizes_n2`〜`_n4`・`extreme_u64_n1`〜`_n4`・`slice_plan_extreme_u64_n2`、K6 `map_touch_to_frame_bounds_and_monotonic`・`monotonic_x_integer_pixels_symbolic_dims_le_4096`・`monotonic_x_any_f64_1080_to_460`・`monotonic_y_any_f64_2400_to_1024`
- `arb-media`（6）: K9 `avcc_sample_to_annexb`・`ticks_exact_ts_90000`・`ticks_exact_ts_u32_max`・`ticks_monotone` 3 本
- `arbd`（2）: K6 `axis_project_range_and_endpoints`・`axis_project_monotonic`（記号的な `(min, max)`）

`kani-extended (arb-media)` ジョブは、最初に走る `k9_avcc_sample_to_annexb_bounded` が CBMC のメモリを使い切った時点でランナーごと停止する（2026-11-25 の `workflow_dispatch` 実行で exit 143）。このジョブは記録用（`continue-on-error`）であり、同じジョブ内の `ticks_*` 5 本はその実行では結果が出ていない。

### 3.3 Layer 2〜4

| 層 | 結果 |
|---|---|
| Stateright | S1 一意状態 1,016,640（総 7,330,826、2.5 秒）、S2 74,988（587,713、0.4 秒）、S2 GC 56（125、1 ms）、S3 16,590（78,470、0.1 秒）、S3 attach 54（112、1 ms）、S4 405（1,277、4 ms）、S5 72（241、1 ms）。`always` 全性質に反例なし、`sometimes` 全性質に到達例あり。S2 の「レガシーと非 ARB staging は不変」と「物理削除は管理下・非レガシーのみ」は、GC 判定から形式（`!legacy`）を外す変異、場所（`managed`）を外す変異、staging 清掃から所有者印の確認を外す変異のいずれでも反例が出ることを確かめた |
| Proptest | G1〜G7 は `cargo test --workspace` で全件成功（既定試行回数） |
| Miri | M1 `arb-os shm::` 5 件成功・FS/mmap の 7 件は除外（109 秒）、M2 `arb-core` 94 件成功（`PROPTEST_CASES=8`、87 秒、2026-11-27。被覆台帳・引き継ぎ選択・表示開始の試験を含む）、M2 `arb-media mp4::` 2 件成功・FS の 2 件は除外（23 秒）。未定義動作・データ競合の報告なし |
| Konsist | `ArchitectureKonsistTest` 7 件成功（KON-1〜KON-6 と走査範囲の確認。KON-6 は PDD G8 で追加し、`ArbViewModels.kt` に「音声キャプチャ非対応」の文字列を書き戻すと失敗することを確認）。`buildPlugin` の配布 ZIP に `konsist` も JUnit などのテスト専用 JAR も含まれない |
| 構造ガード | `check-architecture.py` は RS-1〜RS-5 違反 0、RPC 41 件がクライアントと一致 |
| ライセンス | `check-licenses.py` は 351 パッケージで GPL/AGPL 違反 0 |

## 4. TCB 公理（TCB-01〜TCB-21）

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
| TCB-10 | Android Studio 2026.2 / JBR ランタイム ABI、`dispose()` ライフサイクル、リフレクション対象、Compose/Skiko 描画 | KON-1..5、S1 | `ide-plugin/src/test/kotlin/io/github/shinma06/arb/ArchitectureKonsistTest.kt` | IDE 上の実描画と実ライフサイクルは VM IDE 受け入れ（[develop 受け入れ](../verifications/VER-31-pre-develop-acceptance-e2e.md)、[main リリースゲート](../verifications/VER-34-main-release-gate.md)、[実機手順](../testing/work-device-smoke-checklist.md)） |
| TCB-11 | FHD 30fps の体感遅延基準（open ≤ 1s、seek 0.2–0.5s、trim は数秒以内）を対象開発機で満たす | なし（性能は形式層の対象外） | `core/crates/arb-media/tests/media_pipeline.rs` | 実機ハードウェアでの計測は Stage 3 |
| TCB-12 | 配布 ZIP のバイナリ / JAR / `THIRD_PARTY_NOTICES.md` の法的来歴（GPL/AGPL/LGPL 0 件、PDD ツールは開発専用） | RS-1、ライセンス検査 | `core/scripts/check-licenses.py`、`core/scripts/package_distribution.py --self-test` | Kani・Miri・Stateright・Konsist は dev/test 依存でのみ使い、配布物に含めない（ZIP 構造検査で確認） |
| TCB-13 | `alloc::slice::stable_sort`（`<[T]>::sort_by` の本体）は正しい安定ソートである。K4 ではこれを挿入ソートのスタブに置き換える | K4 | `core/crates/arb-core/src/state.rs::tests::ring_planner_evicts_oldest_unpinned_closed_and_stops_on_storage_pressure`、`core/crates/arb-storage/tests/pdd_g2_ring_crash_props.rs`（G2） | 必須集合のうち `k4_slice_plan_n2_unpinned_unpinned` だけが長さ 2 のスライスでスタブの比較・交換を実行する（他の K4 必須ハーネスでは長さ 1 以下で、std と同じく何もしない）。比較関数は本番の `(created_cont_ns, chunk_id)` のまま検証するが、std のソートと挿入スタブの同値性は機械検証していない |
| TCB-14 | 解析結果（`Vec<u32>` / `MediaError` 内の `io::Error`）の drop は安全に終わる。K9 の表パーサ系ハーネスは戻り値を `ManuallyDrop` で保持し、drop glue を実行しない | K9（mp4） | `cargo test -p arb-media`、Miri M2（`arb-media mp4::`） | 解放処理は std の実装を信頼する。パーサ本体と `Ok`/`Err` の判定は本番コードのまま検証している |
| TCB-15 | `ClockTimeline::map_device_monotonic` / `map_logcat_wall` は `epochs.get(boot_id)` で選んだ boot epoch の `triples` / `wall_jump_indices` をそのまま抽出関数に渡し、その epoch では `ingest_device_triple` が `monotonic_ns`・`boottime_ns` の非減少と `idx + 1 < len` の飛び添字を保つ | K3（スライス層） | `core/crates/arb-core/src/time.rs` の `time` 単体テスト | `BTreeMap` の参照と ingest 側の不変条件は機械検証していない（`ClockTimeline` 全体を通すハーネスは CBMC のメモリ不足で拡張集合） |
| TCB-16 | クラッシュ自動保存の重複排除は「boot 修飾付きクラッシュ ID が pin と一緒に永続化される」という前提に立つ。現在の `arbd` は pin と保存ジョブ（`save_jobs/<job>.json`・カタログ行）を永続化して再起動時に 1 回だけ再キューするが、クラッシュ ID は保存せず、繰り返し抑止はメモリ上の `CrashAutoSaveGate` のクールダウンだけである（再起動で空に戻る） | S5（`pdd_stateright_models.rs::s5_crash_watcher_model`） | `core/crates/arbd/src/capture.rs::tests::interrupted_crash_autosave_is_requeued_once_on_startup` | S5 は設計上の前提を検査するモデルで、`arbd` の実装を精緻化したものではない。デーモン再起動後に端末が同じ `am_crash` を再配信した場合の二重保存は未対策。永続クラッシュ ID の実装まで未解決 |
| TCB-17 | 端末側 `scrcpy-server` 5.0.1 の音声ソケットの意味: `codec_id = 0` は `AudioCaptureException`（`AudioPlaybackCapture` の隠し API リフレクションまたは `registerAudioPolicyStatic` の失敗。非致命で映像は続く）、`codec_id = 1` はそれ以外の例外（致命でサーバーごと終了）。API 33 以上で再生音キャプチャが成功するかは端末・OEM の `AudioPolicy` 実装と、同時に動く他のキャプチャ（Running Devices のミラーリングなど）に依存する | K11、G8 | `core/crates/arbd/src/live_adb.rs::tests::g8_5_runtime_audio_failures_on_api33_carry_cause_and_stderr`、`core/crates/arb-adb/tests/pdd_g8_audio_args_props.rs` | `codec_id` の意味は同梱の `scrcpy-server-v5.0.1` の dex（`AudioEncoder.encode` の catch 構造、`AudioPlaybackCapture.checkCompatibility` の `SDK_INT < 33`）を読んで確かめたもので、機械検証していない。物理 Android 13 端末・OEM ごとの成否は未検証（エミュレータ E2E は API 37 のみ。手順は [実機スモーク](../testing/work-device-smoke-checklist.md) の 9） |
| TCB-18 | viewport の ns 値が Rust・JSON・JVM 間で同じフレームと終端を表し、Studio が単調時計から得た共通のメディア時刻を描画・操作に使う | K10、G3、Kotlin 決定論的時計 | `pdd_g3_media_props.rs` と `PlaybackControllerTest` が同じ 11 秒・61 枚のベクトルを読み、`ArbUiComponentsTest.vfrTimelineLayersAndGesturesUseOnePixelCoordinate` が各層の画素を検査 | Kani の有界証明は JVM・JBR・Compose の実装を含まない。旧 daemon・10 万枚超の CFR フォールバックは中間 PTS を推定する。10 万枚超で最終サンプル長が 0 の MP4 では、daemon は平均間隔を最終保持長として `timeline_duration_ns` を返すが、Studio は末尾長が不明なため epoch からの推定に戻り、両者の終端は一致しない。実画面の描画周期と入力遅延は Studio 受け入れで確認する |
| TCB-19 | CLI/MCP の `save_outcome_failed` と IDE の `SaveOutcomeResponse.savedBundleId` が同じ wire 応答を保存成功と判定する | 共有契約表、KON-7 | `ui-feedback-contract-table.json` の save_rows を arb-cli、PddContractTablesTest、PddUiFeedbackContractTest で検査 | 表にある状態と ID の組合せを検証。実 IDE での通知描画は受け入れ対象 |
| TCB-20 | `std::collections::VecDeque` の `front` / `get` / `push_back` / `pop_front` は FIFO の意味どおりに振る舞い、`GopLruCache` では GOP 全体のキャッシュ（`entries`）と前進窓（`cursor`）が同時に存在しない（全 GOP 経路は `cursor` を、窓の再起動は `clear()` で `entries` を捨てる）。`filmstrip.bin` の指紋は元トラックの SHA-256 で、`sha2` が正しく計算する | K12 | `core/crates/arb-media/tests/media_pipeline.rs::gop_lru_cache_hits_on_backward_and_forward_step_and_evicts_bounded`、`core/crates/arb-media/tests/media_pipeline.rs::filmstrip_cache_writes_externally_and_reloads_without_touching_source` | K12 は窓を固定長配列の FIFO モデルで、`fetch_forward` の再利用・再起動の分岐を `ForwardModel` で写して検証し、`VecDeque` 本体・`fetch_frame` 全体・実デコーダ・ファイル I/O・SHA-256 の計算は通さない。`entries` と `cursor` の排他は手で読んで確かめたもので、機械検証していない |
| TCB-21 | MCP の公開入出力契約と stdio 応答が一致する | mcp-tools-table.json | 実デーモンと arb-cli の全 20 ツール、型・必須・列挙値、Resources・Prompts・Tasks の終了状態を stdio_contract で検査 | 合成データで検証。実端末と外部エージェントの操作は受け入れ対象 |

### S7 の Studio トリム状態機械

S7 はフレーム開始位置 0・2、終端 3、最小選択幅 1 の有限軸で、全ポインタ位置、再生ティック、ズーム、プレビューデコードの開始・遅延到着を全探索する。S7-1〜5 は再生位置と選択区間の独立、離した境界の保持、区間幅、プレビューの解放を検査する。S7-6 は再生位置の追従、離した境界の量子化、キャンセル時の解放漏れを個別に注入し、各反例の最終状態を検査する。

同じ遷移を 32 秒の VFR 軸（12〜22 秒は同一フレーム）で実行し、各ステップの時刻・描画座標・デコード結果の採否を [共有トレース](../specs/studio-trim-s7-traces.json) に固定する。Kotlin は本番の Studio コントローラ、選択リデューサ、描画座標変換で同じ操作を再生する。S7 の探索範囲は単一ポインタと直列デコーダで、実際のウィンドウイベントの配送、Skiko のラスタライズ、実機デコードの遅延は IDE 受け入れで確認する。実時間区間の永続化、再読込、再生ループ、映像・音声・ログ・入力の書き出しは実デーモンおよび Studio の統合試験で検証する。

端末選択では arbd の `select_live_target` と tray が `arb_core::device::selectable_online_serials` を直接使う。Rust 側の二重実装についての公理は不要になった。

## 5. Sara 互換性

この文書は `docs/` 配下にあるが、Sara のトレーサビリティ項目（front matter）を持たない説明文書であり、`sara check --strict` の検査対象項目を増減しない。

### 停止・再開時の同一性と証跡尺

ホストの連続時計と端末が報告する boot ID を同一性判定に使う。端末のフォールバック UUID が再接続ごとに変わった場合は、前回との同一性を証明できないため、別の保存範囲として扱う。ホスト起動を跨ぐ未退避リングにも同じ時間軸を仮定しない。静止画面で新しい VFR サンプルが来ない期間や、容量上限で回転したチャンクの映像は再構築できない。実機での上限・入力イベント・MP4 測定値の照合は実機受入で行う。
