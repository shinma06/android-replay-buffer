---
id: VER-12
type: verification
name: arb-media 純 Rust ISO-BMFF AAC-LC audio.m4a マクサ・時刻範囲トリム検証記録
verifies:
  - SPEC-03
  - REQ-05
  - REQ-08
  - REQ-12
---

# VER-12: `arb-media` 純 Rust ISO-BMFF AAC-LC `audio.m4a` マクサ・時刻範囲トリム検証記録

記録日: 2026-11-23

## 1. 実装・検証範囲（`core/crates/arb-media/src/audio.rs`）

Gate G2 で識別された音声永続化ギャップを解消するため、新規クレート依存ゼロ・GPL/AGPL 非依存の純 Rust ISO-BMFF AAC-LC `audio.m4a` エンジンを `core/crates/arb-media/src/audio.rs` として実装し、`arb_media` クレートルートから再エクスポートした（既存の `arb-media` 公開 API は一切破壊せず、`mp4.rs` の内部 ISO-BMFF ボックス構築・境界検査ヘルパを `pub(crate)` で共有し `inspect_mp4_atoms` の `mp4a`→`esds` 走査も拡張）。

1. **`AudioSpecificConfig` パーサ／ビルダー & `mp4a` + `esds` 構築**:
   - `parse_audio_specific_config`: 2 バイト以上の ISO/IEC 14496-3 `AudioSpecificConfig`（標準 4-bit サンプリング周波数テーブル `0..=12` および明示 24-bit 周波数 `index == 15`、1024/960 `frameLengthFlag`）を解析し、AAC-LC（`audio_object_type == 2`）および妥当なサンプルレート・チャンネル構成を検証。
   - `build_aac_lc_audio_specific_config(sample_rate_hz, channels) -> Result<[u8; 2], MediaError>`: 例として `(48_000, 2) -> [0x11, 0x90]`、`(44_100, 2) -> [0x12, 0x10]` を生成。
   - `build_mp4a_sample_entry`: `AudioSampleEntry`（`mp4a`）と `esds` `FullBox`（`ES_Descriptor` `0x03` → `DecoderConfigDescriptor` `0x04` (`objectTypeIndication=0x40`, `streamType=0x15`) → `DecoderSpecificInfo` `0x05` (`AudioSpecificConfig`) ＋ `SLConfigDescriptor` `0x06` (`0x02`)）を生成。パーサ側は 1 バイト最小長および `ffmpeg`/QuickTime 形式の 4 バイトパディング長（`0x80 0x80 0x80 <len>`）の双方を受理。
2. **純 Rust faststart `audio.m4a` マクサ（`mux_aac_to_m4a`）**:
   - 入力 `AacMuxInput { audio_specific_config, sample_rate_hz, channels, packets: Vec<AacPacket { pts_us, data }> }` を受け取り、`ftyp` (`M4A `) ＋ `moov` ＋ `mdat` の faststart 配置で出力。
   - `mdhd`/`mvhd` タイムスケール = `sample_rate_hz`、`tkhd` 音量 `0x0100`、`hdlr` = `soun`、`smhd`、`stts`（1024 サンプル／AU 基準＋安全上限付き `pts_us` 差分算出）、`stsz`、`stsc`、`stco`/`co64` を構築し、**`edts`/`elst` は厳密に 0 個**。
   - 生 AAC-LC アクセスユニットをそのまま格納しつつ、7 バイト（CRC なし）および 9 バイト（CRC あり）の ADTS ヘッダが混在する場合は 12-bit 同期語・層 0・13-bit フレーム長完全一致を確認のうえ安全に剥離（`strip_adts_header`）。
   - `output::write_atomically(..., Publish::NoReplace)` により兄弟一時ファイル書出 → `fsync` → 非置換公開を行い、既存ファイルを決して上書きしない（R2 S6 準拠）。
3. **純 Rust `Mp4AudioTrack` インデクサ & 時刻範囲トリム（`trim_audio_m4a`）**:
   - `Mp4AudioTrack::open`: `mp4a`/`esds` の `AudioSpecificConfig`、`mdhd` タイムスケール、`stsz`/`stts`/`stsc`/`stco`/`co64` を R2 S8 準拠の事前サイズ・上限検証（`MAX_MOOV_BOX_BYTES`, `MAX_TRACK_SAMPLES`, `checked_entry_count`, `stsz` 先行による `stts` 展開上限）付きで解析し、`IndexedAudioPacket { index, pts_ns, duration_ns, offset, size, pts_ticks, duration_ticks }` とロックフリー位置指定読取（`read_packet_into`）を提供。
   - `trim_audio_m4a(input_m4a, output_m4a, start_ns, end_ns) -> Result<AudioTrimReport, MediaError>`: `output == input`（エイリアス含む）および既存出力先への上書きを拒否し、`[start_ns, end_ns)` と交差する全 AAC アクセスユニットを単一再利用バッファで無劣化ストリームコピーして `pts = 0` 基点・**ゼロ `edts`/`elst`** の faststart `audio.m4a` として原子的書き出し。

## 2. 純 Rust テスト内訳（`ffmpeg` 非依存）

- ユニットテスト（`core/crates/arb-media/src/audio.rs`、6 件全合格）:
  1. `build_and_parse_standard_audio_specific_configs` — `[0x11, 0x90]` / `[0x12, 0x10]` および全 13 標準サンプルレート × 7 チャンネル構成の往復検証
  2. `parse_audio_specific_config_handles_explicit_rate_short_frame_and_rejects_invalid` — 明示 24-bit レート・960 サンプル短フレーム解析、不正長・非 AAC-LC・予約インデックス・PCE の拒否検証
  3. `esds_round_trips_both_compact_and_four_byte_padded_lengths` — 最小長および 4 バイトパディング長 `esds` の往復検証と 5 バイト超過長拒否
  4. `mp4a_builder_rejects_mismatched_asc` — `mp4a` 引数と `AudioSpecificConfig` 不一致の拒否検証
  5. `adts_stripper_strips_valid_7_and_9_byte_headers_and_preserves_raw_packets` — 7/9 バイト ADTS 剥離と偶然 `0xFF 0xF1` で始まる生 AAC の誤剥離防止検証
  6. `packet_duration_ticks_handles_48khz_rounding_and_bounds` — 48 kHz (`21_333` / `21_334` µs → `1024` ticks) 丸め・逆転・過大デルタ上限クランプ検証
- 統合テスト（`core/crates/arb-media/tests/audio_pipeline.rs`、4 件全合格）:
  1. `round_trip_mux_and_open_verifies_exact_packets_asc_faststart_and_zero_elst` — 100 パケット（うち 50 パケット ADTS 付き）の `mux_aac_to_m4a` → `inspect_mp4_atoms`（faststart、`M4A `、`mp4a`+`esds`+`smhd`、`edts=0, elst=0`）→ `Mp4AudioTrack::open` で全 100 パケットのペイロード完全一致と既存ファイル上書き拒否を検証
  2. `trim_audio_m4a_selects_exact_subset_rebases_to_zero_and_has_zero_elst` — 100 パケット（約 2.13 秒）に対する境界一致トリム `[20..65)`（45 パケット）と任意時刻トリム `[0.5s, 1.5s)`（48 パケット）で、ペイロード完全一致・`pts_ns == 0` 起点リベース・faststart・`edts=0, elst=0` を検証
  3. `trim_audio_m4a_refuses_overwrite_input_alias_and_invalid_ranges` — `output == input`、`.` 経由エイリアス、既存出力先、逆転範囲、交差 0 件範囲の拒否と入力無変更・一時ファイル残存ゼロを検証
  4. `crafted_oom_and_malformed_audio_boxes_return_err_without_allocating` — 細工された過大 `stts`/`stsz`（定数長・可変長）/`stsc`/`stco` 件数および `stts` `u32::MAX` リピート爆弾に対して OOM を起こさず安全にエラーまたは `stsz` 上限クランプすることを検証

## 3. ローカル `ffprobe` / `ffmpeg` 互換性実測結果（macOS arm64）

実 AAC-LC 48 kHz ステレオ 440 Hz 正弦波（2.134 秒、102 パケット）を ADTS ストリームとして生成し、`arb_media::audio::mux_aac_to_m4a`（全 102 パケットの ADTS 剥離＋`audio.m4a` マクス）および `trim_audio_m4a(..., 500_000_000, 1_500_000_000)`（48 パケット・1.024 秒）を適用した出力、さらに `ffmpeg` 自身が出力した 5 バイト ASC（`[11, 90, 56, e5, 00]`）付き M4A を `Mp4AudioTrack::open` → `trim_audio_m4a` した出力を `ffprobe` および `ffmpeg -v error -i <file> -f null -` で検証した（全 3 ファイルでデコードエラー 0 件、`probe_score=100`）。

```text
MUXED: packets=102 adts_stripped=102 duration_ns=2176000000 faststart=true edts=0 elst=0
TRIMMED: in=102 out=48 duration_ns=1024000000 faststart=true edts=0 elst=0
FFMPEG_REF_OPENED: packets=102 sr=48000 ch=2 asc=[11, 90, 56, e5, 00]
FFMPEG_REF_TRIMMED: in=102 out=48 duration_ns=1024000000

=== ffprobe arb_muxed.m4a ===
codec_name=aac
profile=LC
codec_type=audio
codec_tag_string=mp4a
mime_codec_string=mp4a.40.2
sample_rate=48000
channels=2
channel_layout=stereo
initial_padding=0
time_base=1/48000
start_pts=0
start_time=0.000000
duration_ts=104448
duration=2.176000
bit_rate=126694
nb_frames=102
extradata_size=2
TAG:language=und
TAG:handler_name=SoundHandler
format_name=mov,mp4,m4a,3gp,3g2,mj2
probe_score=100
TAG:major_brand=M4A 
TAG:compatible_brands=M4A mp42isom
ffmpeg -v error -i arb_muxed.m4a -f null - => exit 0 (0 errors)

=== ffprobe arb_trimmed.m4a ===
codec_name=aac
profile=LC
codec_type=audio
codec_tag_string=mp4a
mime_codec_string=mp4a.40.2
sample_rate=48000
channels=2
channel_layout=stereo
initial_padding=0
time_base=1/48000
start_pts=0
start_time=0.000000
duration_ts=49152
duration=1.024000
bit_rate=128351
nb_frames=48
extradata_size=2
probe_score=100
TAG:major_brand=M4A 
TAG:compatible_brands=M4A mp42isom
ffmpeg -v error -i arb_trimmed.m4a -f null - => exit 0 (0 errors)
```

## 4. 全ゲート実行結果（`./core/scripts/verify.sh`）

| ゲート | 結果 | 詳細 |
| --- | --- | --- |
| `[1/6] cargo fmt --check` | PASS | ワークスペース全ファイル差分なし |
| `[2/6] cargo clippy --workspace --all-targets -- -D warnings` | PASS | BeautifulRust 厳格 lint（`as_conversions`, `unwrap_used`, `missing_docs` 等）含め警告ゼロ |
| `[3/6] cargo test --workspace` | PASS | ワークスペース全テスト合格（`arb-media`: unit 16 件＋`audio_pipeline` 4 件＋`media_pipeline` 12 件 = 計 32 件全合格） |
| `[4/6] license check` | PASS | 新規依存 0 件、GPL/AGPL 違反 0 件 |
| `[5/6] sara check --strict` | PASS | `VER-12` 含むトレーサビリティグラフ厳密検査合格 |
| `[6/6] REQ -> downstream SPEC coverage` | PASS | 14/14 REQ が下流 SPEC に充足済み |
| `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p arb-media` | PASS | 公開 API ドキュメント警告・リンク切れゼロ |

## 5. 明示的な未検証項目（Unverified）

- **Android 実機 `scrcpy-server-v5.0.1` 音声トンネルとの結合実行**: `arb-media::audio` 単体および `ffmpeg` 実 AAC-LC ストリームとの相互運用は検証済みだが、`arbd` 経由のライブ音声キャプチャ保存・トリム結合は Phase 5 統合タスクで検証予定。
- **Linux / Windows 上でのビルド・テスト実行**: 本検証は macOS arm64 環境で実施したため、Linux/Windows の実行確認（Windows 側 `FileExt::seek_read` 経路含む）は CI マルチ OS マトリクスで検証予定。
- **HE-AAC（SBR/PS, AOT 5/29）およびマルチブロック ADTS**: 現スコープは Android `scrcpy` の AAC-LC（`object_type == 2`、1024/960 サンプル単一ブロック）を対象としており、非 AAC-LC ストリームは暗黙破損を避けるため明示的に拒否する。
