---
id: VER-06
type: verification
name: arb-media GPL 非依存メディア・ISO-BMFF・フレーム単位トリム・再生プリミティブ検証記録
verifies:
  - SPEC-03
  - REQ-05
  - REQ-08
  - REQ-11
  - REQ-12
---

# VER-06: `arb-media` GPL 非依存メディア・フレーム単位トリム・再生プリミティブ検証記録

記録日: 2026-11-23

## 1. 実装・検証範囲（`core/crates/arb-media`）

Spike S1 の全実測知見と `SPEC-03` に基づき、GPL/AGPL 非依存の純 Rust メディアエンジン `arb-media` を以下 4 モジュール構成で実装し、外部 `ffmpeg` に依存しない純 Rust 合成 H.264 バーコードフィクスチャ（`OpenH264` エンコード、90 フレーム / 3 GOP）を用いたエンドツーエンド統合テストにより検証した。

1. **`arb_media::mp4`（純 Rust ISO-BMFF パーサ・インデクサ・ゼロ `elst` `avc1` スプライスマクサ）**:
   - `Mp4VideoTrack::open` により `avc1`/`avc3` + `avcC`、`stts`、`ctts`、`stss`、`stsc`、`stsz`、`stco`/`co64` を解析し、全フレームの `IndexedFrame`（`frame_index`, `pts_ns`, `dts_ns`, `duration_ns`, `is_keyframe`, `gop_id`, `offset`, `size`）と `has_b_frames`（`ctts` / 非単調 PTS 検出）を厳密構築。
   - `write_single_stsd_avc1_mp4` および `mux_raw_h264_to_mp4` により、**単一 `stsd` `avc1` ＋ IDR 境界への 4 バイト長プレフィックス付きインバンド SPS/PPS 挿入**、faststart（`moov` 前置）、**`edts`/`elst` 0 個**の MP4 を生成。`inspect_mp4_atoms` による全ボックスツリー監査を実装。
2. **`arb_media::codec`（GPL 非依存 `VideoCodecBackend` 抽象）**:
   - `OpenH264Backend`（`openh264` / `BSD-2-Clause`、全 3 OS ベースライン）: Spike S1 で確認された 1 フレーム内部 DPB 遅延を `Flush::NoFlush` + `flush_remaining()` で厳密補正し、フレーム番号の 1 枚ズレを完全排除。
   - `VideoToolboxBackend`（`#[cfg(target_os = "macos")]`、Apple `VideoToolbox` / `CoreMedia` / `CoreVideo` / `CoreFoundation` 直接 C FFI、外部クレート 0、ゼロコピー `CVPixelBuffer` 復号→再エンコード、全 `unsafe` ブロックに単一操作・`// SAFETY:` 根拠を付与し C コールバック境界を `catch_unwind` で保護）。
   - `default_codec_backend()`: macOS では `VideoToolboxBackend` を優先し初期化失敗時は `OpenH264Backend` へフォールバック、Linux/Windows では `OpenH264Backend` を選択。
3. **`arb_media::trim`（整数 `frame_index` 単位トリム・PNG 書き出し）**:
   - `trim_video`: 整数 `frame_index` のみで範囲 `[in_frame_index..=out_frame_index]` を指定。IDR 開始時は無劣化ストリームコピー（`reencoded_frames == 0`）、GOP 途中開始時は先頭 GOP 残余のみ新規 IDR から再エンコードし後続 GOP を無劣化ストリームコピー（`edts=0, elst=0`）。`has_b_frames == true` のストリームは暗黙破損を防ぐため明示的 `MediaError::UnsupportedFeature` を返却。
   - `export_frame_png` / `export_frame_png_with_metadata`: 指定 `frame_index` を厳密復号し、許容的ライセンスの `png` クレートで RGB24 PNG および `tEXt` メタデータ（`arb:frame_index`, `arb:boottime_ns`, `arb:source_bundle_id`）を書き出し。
4. **`arb_media::playback`（外部フィルムストリップキャッシュ・GOP-LRU・スクラブ制御・再生時計）**:
   - `FilmstripCache`: 並列キーフレーム復号により `<cache_dir>/<bundle_id>/filmstrip.bin` へ外部保存・チェックサム検証・破損時自動再生成を行い、不変 WORM バンドルディレクトリへ一切書き込まないことを検証。
   - `GopLruCache`: GOP 数・バイト数上限付き LRU キャッシュにより `+1` / `-1` 隣接コマ送りの即時キャッシュヒットと上限超過時の退避を検証。
   - `ScrubController`: 最新優先世代キャンセル（`cancel_pending` / `settle_exact_if_latest`）および近似プレビュー（`is_approximate: true`）→ 厳密復号確定（`is_approximate: false`）遷移を検証。
   - `PresentationClock`: 外部音声クロック駆動（`AudioMaster`）と音声デバイス不在／途絶時の単調時計フォールバック（`MonotonicFallback`）の滑らかな切替、再生速度変更、および未校正既定値（`av_offset_ms = 0`, `AvOffsetStatus::Uncalibrated`）の誠実な保持を検証。

## 2. テスト項目とゲート実行結果（`./core/scripts/verify.sh`）

実行環境: macOS arm64 / `rustc 1.100.0` / `sara 0.12.3`

### 2.1 `arb-media` テスト内訳（計 13 件全合格）

- ユニットテスト（`src/lib.rs`）: 1 件（`smoke_crate_info_links_arb_core`）
- 統合テスト（`tests/media_pipeline.rs`）: 12 件
  1. `mux_parse_and_index_exact_frames_and_gops` — 生 H.264 マクス → faststart/単一 `avc1`/ゼロ `elst` 監査 → `frame_index`/`gop_id`/`is_keyframe` 検証
  2. `mid_gop_trim_openh264_exact_barcodes_and_zero_elst` — `OpenH264Backend` で `[17..=49]`（33f、再エンコード 13f + コピー 20f）、全 33 枚バーコード一致＋macOS `VideoToolboxBackend` クロス復号検証
  3. `mid_gop_trim_videotoolbox_exact_barcodes_and_zero_elst` — `VideoToolboxBackend` で `[17..=49]`（33f）、全 33 枚バーコード一致＋`OpenH264Backend` クロス復号検証
  4. `idr_start_trim_is_lossless_stream_copy_with_exact_barcodes` — `[30..=59]` IDR 開始トリムで `reencoded_frames == 0` かつ全 30 枚バーコード一致
  5. `single_gop_mid_gop_trim_and_bounds_validation` — 単一 GOP 内 `[10..=20]` トリム＋BGRA8 復号＋範囲外エラー（`InvalidFrameRange` / `FrameOutOfRange`）検証
  6. `export_frame_png_mid_gop_verifies_barcode_and_text_chunks` — GOP 途中フレーム 43 の PNG 書き出し → 再読込バーコード一致＋`tEXt` 3 種検証
  7. `gop_lru_cache_hits_on_backward_and_forward_step_and_evicts_bounded` — 前後コマ送りヒット（`cache_hits >= 3`）と `max_gops` 退避検証
  8. `filmstrip_cache_writes_externally_and_reloads_without_touching_source` — 外部キャッシュ書出・再読込・元バンドル不変・パストラバーサル拒否検証
  9. `corrupt_filmstrip_cache_is_safely_regenerated` — チェックサム破損検知（`CacheCorrupt`）と透過的再生成検証
  10. `scrub_controller_transitions_approximate_to_exact_and_honors_latest_wins` — `is_approximate: true -> false` 遷移と旧世代キャンセル検証
  11. `presentation_clock_audio_master_and_monotonic_fallback_with_honest_av_offset` — 音声クロック駆動／欠落フォールバック／倍速／停止／時刻→フレーム解決・未校正オフセット検証
  12. `b_frame_ctts_is_detected_and_explicitly_rejected_by_trim` — 合成 `ctts` B フレーム検出（`has_b_frames() == true`）とトリム時の明示的 `UnsupportedFeature` 拒否検証

### 2.2 全ゲート実行結果

| ゲート | 結果 | 詳細 |
| --- | --- | --- |
| `[1/6] cargo fmt --check` | PASS | ワークスペース全ファイル差分なし |
| `[2/6] cargo clippy --workspace --all-targets -- -D warnings` | PASS | BeautifulRust 厳格 lint（`undocumented_unsafe_blocks`, `multiple_unsafe_ops_per_block` 等）含め警告ゼロ |
| `[3/6] cargo test --workspace` | PASS | ワークスペース全テスト合格（`arb-media`: 13 件全合格） |
| `[4/6] license check` | PASS | 98 パッケージ検証、GPL/AGPL 違反 0 件（`openh264` = `BSD-2-Clause`、`png` = `MIT OR Apache-2.0`、`r-efi 6.0.0` は許容条項 `MIT` を選択） |
| `[5/6] sara check --strict` | PASS | トレーサビリティグラフ厳密検査合格 |
| `[6/6] REQ -> downstream SPEC coverage` | PASS | 14/14 REQ が下流 SPEC に充足済み |
| `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p arb-media` | PASS | 公開 API ドキュメント警告・リンク切れゼロ |

## 3. 明示的な未検証項目（Unverified）

- **Windows / Linux 上でのビルド・テスト実行**: 本環境は macOS arm64 のみのため、Windows/Linux での `OpenH264Backend`（`nasm` 有効ビルド含む）実行および Windows の `FileExt::seek_read` 経路は未実測（CI のマルチ OS マトリクスで検証予定）。
- **外部プレイヤー・他 OS での結合 MP4 再生互換**: 本テストでは `OpenH264Backend` と Apple `VideoToolboxBackend` の相互復号までを検証済み。Windows Media Player / Chrome / VLC / Android ExoPlayer での実機再生確認は別フェーズのマルチ OS 検証対象。
- **B フレーム（`ctts`）含むストリームの末尾 GOP 再エンコード**: 現状は `has_b_frames() == true` を確実に検知し `MediaError::UnsupportedFeature` で安全に拒否する設計であり、B フレーム出力端末向けの末尾 GOP 再エンコード対応は必要時に拡張予定。
- **実機音声出力デバイスとの接続**: `PresentationClock` は `AudioClockSource` トレイト経由のモッククロックで検証済みであり、実オーディオ出力ストリーム（OS オーディオ API）との結合は上位再生エンジン統合フェーズで検証予定。

## 4. R2 独立レビュー修正（S1〜S12）

`./core/scripts/verify.sh` 6 ゲート全合格（`arb-media` ライブラリ 10 件・統合 12 件）。回帰テストは `core/crates/arb-media/src/r2_regression_tests.rs` と `tests/media_pipeline.rs`。

| 指摘 | 修正 | 回帰テスト |
|---|---|---|
| S1 | VideoToolbox のコールバック状態を `Box::into_raw` 由来の `NonNull` で保持し、セッション無効化後に解放（エンコーダは `CompleteFrames` → `Invalidate` の順） | macOS で `default_codec_backend` を使う全デコードテスト（FFI のため Miri 未実行） |
| S2・S5 | デコード・mux の全サンプルを 4 バイト NAL 長に正規化し、`avcC` も 4 バイトを宣言 | `two_byte_nal_length_sources_decode_and_trim_to_consistent_4byte_tracks` |
| S3 | 色差面を `div_ceil(2)` で扱い、CoreVideo の面サイズを使用。変換は範囲外参照で panic しない。OpenH264 の奇数サイズエンコードはエラー | `odd_dimension_frames_convert_without_panicking`（19x13）、`odd_dimension_nv12_conversions_never_panic`（macOS 専用 API） |
| S4 | 最初の同期サンプルより前のフレームのトリム・デコードを拒否 | `trim_and_decode_refuse_frames_before_the_first_sync_sample` |
| S6 | 出力 = 入力を拒否し、一時ファイル + fsync + 置換なし公開で書き出す | `trim_and_png_export_refuse_to_overwrite_input_or_existing_output` |
| S7 | ストリームコピー部分を元ファイルから 1 サンプルずつ書き出す（サイズを先に確定） | 上記 S5 テスト・既存のバーコード一致テスト |
| S8 | サンプルテーブルの件数をボックス長と 1,000 万サンプル上限で事前検証 | `crafted_huge_sample_table_counts_return_err_without_allocating` |
| S9 | B フレームのトラックを `decode_frame`・`decode_gop`・PNG 出力・`GopLruCache` で一律に拒否 | `b_frame_ctts_is_detected_and_explicitly_rejected_by_trim` |
| S10 | GOP をデコードする前にメモリ量を見積もり、超える場合は 1 フレームだけデコードしてキャッシュしない | `gop_cache_never_decodes_a_gop_larger_than_max_bytes_into_memory` |
| S11 | `bundle_id` を `[A-Za-z0-9_-]{1,64}`（予約名を除く）に限定し、キャッシュ先がバンドル外であることを確認 | `filmstrip_bundle_id_is_whitelisted_and_cache_must_be_outside_bundle` |
| S12 | 再生中の位置を単調にし、seek 前の古いオーディオ位置を再同期まで無視 | `presentation_clock_is_monotonic_and_ignores_stale_pre_seek_audio` |

未検証: 実機・他 OS（Linux/Windows）での VideoToolbox 以外の実行、Miri による S1 の検証。
