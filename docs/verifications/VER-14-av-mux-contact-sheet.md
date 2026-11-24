---
id: VER-14
type: verification
name: arb-media 純 Rust H.264+AAC-LC share.mp4 単一ファイルマクサ・AI コンタクトシート／タップクロップ PNG エンジン検証記録
verifies:
  - SPEC-03
  - REQ-05
  - REQ-08
  - REQ-12
---

# VER-14: `arb-media` 純 Rust H.264 + AAC-LC `share.mp4` マクサと AI コンタクトシート／タップクロップ PNG エンジン検証記録

記録日: 2026-11-23

## 1. 実装範囲

新規クレート依存ゼロ（既存の `png` のみ再利用）・GPL/AGPL 非依存。`core/crates/arb-media` の公開 API を一切破壊せず、`mp4.rs` の内部ヘルパ（`build_avc1_entry` 等）を `pub(crate)` で共有した。

### 1.1 `mux_av.rs` — `mux_share_mp4`

```rust
pub fn mux_share_mp4(
    video_mp4: impl AsRef<Path>,
    audio_m4a: Option<&Path>,
    audio_lead_in_offset_ns: i64,
    output_mp4: impl AsRef<Path>,
) -> Result<AvMuxReport, MediaError>
```

- 既存の境界検査付きパーサ `Mp4VideoTrack::open`（`mp4.rs`）と `Mp4AudioTrack::open`（`audio.rs`）をそのまま再利用（R2-S8 のメモリ上限を維持）。出力は `output.rs` の `write_atomically(..., Publish::NoReplace)`（兄弟一時ファイル → `fsync` → 非置換公開）と `ensure_output_is_not_input`（シンボリックリンク対応）で書き出し、入力ファイルは決して上書きしない。
- 出力は faststart（`ftyp` → `moov` → `mdat`）、`ftyp` ブランド `isom`/`iso2`/`avc1`/`mp41`、映像 `trak`（`track_ID = 1`, 単一 `stsd` `avc1`）と音声 `trak`（`track_ID = 2`, `mp4a`/`esds`）。**`edts` / `elst` は厳密に 0 個**。
- 映像サンプルは bit-for-bit ストリームコピー（IDR の in-band SPS/PPS 保持。in-band が無い先頭／キーフレームのみ `avcC` から補完し、4 バイト NAL 長へ正規化）。B フレーム入力は拒否。
- `mdat` は約 1 秒単位のチャンクで映像／音声をインターリーブ（`stsc` はラン長圧縮、`stco`/`co64` はチャンクごと）。
- `audio_lead_in_offset_ns`（`audio.m4a` 先頭サンプルの映像タイムライン上の位置）の扱い（`edts`/`elst` なし）:
  - `offset < 0`（音声が先行）: 終端が映像時刻 `0` 以前の先頭 AAC パケットをスキップし、負の PTS/DTS を出さない。最初に残るパケットが 0 をまたぐ 1 パケット未満の差は `AvMuxReport::audio_residual_offset_ns`（負値）で報告。
  - `offset > 0`（音声が遅れて開始）: `elst` 遅延も無音挿入もせず音声は 0 から開始し、要求オフセット全量を `audio_residual_offset_ns`（正値＝音声が理想より早く再生）として報告。映像終端以降に始まる場合は範囲内パケット 0 件として映像のみにフォールバック。
  - 映像終端以降に開始するパケットは破棄（`audio_packets_dropped_trailing`）。
- `audio_m4a == None` または範囲内パケット 0 件 → クリーンな映像単独 faststart ゼロ `elst` MP4 を出力し `audio_included: false` を正直に報告。
- `AvMuxReport` は寸法・フレーム数・キーフレーム数・映像／音声時間・スキップ／破棄パケット数・要求オフセット・`audio_start_ns`・`audio_residual_offset_ns`・`edts`/`elst` 個数（常に 0）・`faststart`・出力バイト数を返す。

### 1.2 `contact_sheet.rs` — AI コンタクトシート／タップクロップ

- `render_contact_sheet_png(frames: &[ContactSheetTile], opts: &ContactSheetOptions) -> Result<Vec<u8>, MediaError>`: 最大 16 タイル。整数のみのボックスフィルタで縮小（既定 `max_tile_width = 280`、`max_tile_height = 560`、4 列、ガター 4 px、拡大はしない）、暗色タイルセル内に縦横比維持で中央配置。各タイル上端に内蔵 5×7 ビットマップフォント（外部フォント不要）で `#372  00:12.4` 形式のラベルを焼き込み、`tap_point` があればシアンのリング＋十字（黒ハロー付き）を描画。既存の `png` クレートで RGB8 PNG にエンコード。整数演算のみのため 3 OS でバイト単位に同一。
- `render_tap_crop_png(frame_rgba, width, height, tap_x, tap_y, opts) -> Result<TapCropResult, MediaError>`: 解像度相対クロップ（既定 `width_fraction = 0.28`, `height_fraction = 0.12`、最小 64 px、フレーム内にクランプ）を等倍で切り出し、タップ画素 `(tap_x - crop_x, tap_y - crop_y)` に 1 px 十字＋小リングを描画（タップ画素自体は元の色のまま）。`TapCropResult { png_bytes, crop_x, crop_y, crop_width, crop_height, tap_rel_x, tap_rel_y }`。隅・辺のタップでもパニック／範囲外アクセスなし。フレーム外タップは `InvalidArgument`。
- `select_contact_sheet_indices(start_frame, end_frame, max_tiles) -> Vec<u64>`: `max_tiles >= 2` かつ `end > start` で両端を必ず含み、内部を等間隔に配分（最大 16、ソート済み・重複なし、`u128` 演算でオーバーフローなし）。

## 2. 純 Rust テスト内訳（`ffmpeg` 非依存）

`cargo test -p arb-media`: unit 16 + `audio_pipeline` 4 + `contact_sheet` 15 + `media_pipeline` 12 + `mux_av` 8 = **55 件全合格**（手動検証用 `#[ignore]` ドライバ 2 件を除く）。

`tests/mux_av.rs`（8 件）:
1. `mux_h264_and_aac_into_faststart_zero_elst_share_mp4` — トップレベル順 `ftyp, moov, mdat`、`track_ID` = [1, 2]、`avc1`/`stsd` 各 1、`mp4a`/`esds` 各 1、`edts = elst = 0`、ブランド、映像全 60 サンプルの bit-for-bit 一致、音声 94 パケットの bit-for-bit 一致（映像終端以降の 6 パケットを破棄）、映像／音声のインターリーブ、レポート値
2. `video_only_when_audio_is_none_is_clean_single_track` — `trak` 1 個・`mp4a` 0 個・`audio_included = false`
3. `negative_lead_in_skips_leading_packets_and_reports_residual` — 先頭 4 パケットをスキップ、全 PTS ≥ 0、残差 −14.67 ms（1 パケット未満）、先頭ペイロードがソース #4 と一致
4. `positive_lead_in_is_reported_without_elst` — 残差 +500 ms を報告、`elst = 0`
5. `audio_entirely_outside_video_range_falls_back_to_video_only` — 範囲外音声は映像のみにフォールバック
6. `refuses_to_replace_outputs_or_overwrite_inputs_and_never_touches_them` — 既存出力・入力と同一出力・`.` 経由エイリアスを拒否、入力無変更、一時ファイル残存ゼロ
7. `refuses_symlink_to_input_as_output`（`cfg(unix)`: シンボリックリンク生成のみ OS 依存）
8. `malformed_inputs_return_errors_and_leave_no_output`

`tests/contact_sheet.rs`（15 件）: `select_contact_sheet_indices` の 0/1/2/16/1000 フレーム・逆転範囲・`u64::MAX`・上限 16 キャップ、1/4/16 タイルの PNG デコード往復（寸法、ラベル領域の白画素 > 0、タイル画素の一致）、ボックスフィルタ平均、RGB 入力、シアンマーカー（有無・隅でのクリップ）、決定性、不正入力拒否、タップクロップの中央／`(0,0)`／`(w-1,h-1)`／辺での境界クランプと十字・リング画素検証、極小フレーム、RGB 入力、不正分率拒否。

## 3. ローカル `ffprobe` / `ffmpeg` 実測結果（macOS arm64）

映像は `OpenH264` 実エンコード（128×96, 30 fps, 60 フレーム = 2.0 s, GOP 30）、音声は `ffmpeg` が生成した実 AAC-LC 48 kHz ステレオ（`ffmpeg -f lavfi -i sine=frequency=440:sample_rate=48000:duration=2.3 -ac 2 -c:a aac -profile:a aac_low -b:a 128k real_audio.m4a`）を `Mp4AudioTrack::open` 経由でマクスした。`#[ignore]` ドライバ `emit_share_mp4_for_external_probe`（`ARB_VER14_OUT_DIR` / `ARB_VER14_AUDIO`）で `share.mp4`（オフセット 0）、`share_neg.mp4`（−100 ms）、`share_pos.mp4`（+250 ms）、`share_video_only.mp4` を生成。

```text
=== ffprobe share.mp4 ===
[stream 0] codec_name=h264 profile=Constrained Baseline codec_type=video codec_tag_string=avc1
           width=128 height=96 pix_fmt=yuv420p time_base=1/90000 start_pts=0 start_time=0.000000
           duration_ts=180000 duration=2.000000 nb_frames=60
[stream 1] codec_name=aac profile=LC codec_type=audio codec_tag_string=mp4a
           sample_rate=48000 channels=2 channel_layout=stereo time_base=1/48000 start_pts=0 start_time=0.000000
           duration_ts=96256 duration=2.005333 nb_frames=94
[format]   nb_streams=2 format_name=mov,mp4,m4a,3gp,3g2,mj2 start_time=0.000000 duration=2.005333
           probe_score=100 major_brand=isom compatible_brands=isomiso2avc1mp41
ffmpeg -v error -i share.mp4 -f null -  => exit 0 (0 errors)
edts count = 0, elst count = 0, moov offset (36) < mdat offset (1726)  => faststart
```

| ファイル | streams | probe_score | start_pts（映像/音声） | `edts`/`elst` | `ffmpeg -v error -f null -` |
| --- | --- | --- | --- | --- | --- |
| `share.mp4` | h264 + aac LC | 100 | 0 / 0 | 0 / 0 | 0 errors |
| `share_neg.mp4`（−100 ms: 先頭 4 パケットスキップ、残差 −14.67 ms） | h264 + aac LC | 100 | 0 / 0 | 0 / 0 | 0 errors |
| `share_pos.mp4`（+250 ms: 残差 +250 ms を報告） | h264 + aac LC | 100 | 0 / 0 | 0 / 0 | 0 errors |
| `share_video_only.mp4` | h264 のみ | 100 | 0 | 0 / 0 | 0 errors |

`ffprobe -show_entries packet=pts,dts` で `share.mp4` に負の PTS/DTS が 0 件であることも確認した。

コンタクトシート（16 タイル、1028×2340、4×4）とタップクロップ（151×144）を目視確認した（ラベル `#120  00:03.9` 等が鮮明、シアンの十字＋リングがタップ位置に表示）。生成は `#[ignore]` ドライバ `emit_contact_sheet_samples_for_visual_inspection`。

## 4. 全ゲート実行結果

`./core/scripts/verify.sh` と `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` の結果は PR 本文および Trail に記録する（ローカル実行: fmt / clippy `-D warnings` / test / license / `sara check --strict` / REQ→SPEC 被覆）。

## 5. 明示的な未検証項目（Unverified）

- **Linux / Windows 上での実行**: 本検証は macOS arm64 で実施。両 OS は CI の `test` / `os-parity` で確認する（整数のみの描画・縮小のため PNG 出力は 3 OS でバイト同一の想定）。
- **QuickTime / ブラウザ / 各 SNS での再生**: `ffprobe`/`ffmpeg` と `Mp4VideoTrack`/`Mp4AudioTrack` による再読込のみ。実プレイヤーでの A/V 同期の体感確認は未実施。
- **正の `audio_lead_in_offset_ns` の完全補正**: `edts`/`elst` 禁止と無音挿入なしの方針により、音声が遅れて開始するケースは残差として報告するのみで、再生側のずれは解消しない。
- **実機 Android キャプチャ（`scrcpy` 音声／映像）由来の入力**: 合成映像＋`ffmpeg` 生成 AAC で検証。実機結合は統合タスクで検証予定。
- **HE-AAC / マルチブロック ADTS / B フレーム映像**: 暗黙破損を避けるため既存パーサ同様に明示的に拒否。
