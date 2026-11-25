---
id: VER-16
type: verification
name: T-2B AI ネイティブ `arb-cli mcp` サーバ・`share.mp4` 書き出し・AAC→PCM 音声デコード RPC・AI 検査 RPC 検証記録
verifies:
  - SPEC-01
  - SPEC-03
  - REQ-04
  - REQ-05
  - REQ-07
  - REQ-08
  - REQ-12
---

# VER-16: T-2B `arb-cli mcp` / `export.share_mp4` / `media.decode_audio_pcm` / AI 検査 RPC 検証記録

記録日: 2026-11-23

## 1. 実装範囲

### 1.1 `arb-media::audio_decode` — 純 Rust AAC-LC → PCM WAV

- `decode_audio_m4a_to_wav(audio_m4a, output_wav) -> Result<AudioDecodeReport, MediaError>`。AAC アクセスユニットの取り出しは既存の境界検査付き `Mp4AudioTrack::open`（R2-S8）をそのまま使い、デコードは Apache-2.0・依存ゼロの純 Rust `rusty_aac` で行う（GPL/AGPL 依存なし）。
- 出力は標準 44 バイト RIFF/WAVE の 16-bit signed LE PCM。`output.rs` の `write_atomically`（兄弟一時ファイル → `fsync` → 非置換公開）で書き、入力ファイルの上書きと既存出力の置換は拒否する。デコード後 PCM は 512 MiB、RIFF の `u32` 上限で打ち切り（寸法増幅の防止）。
- `inspect_wav_header` で T-3 の `WavFilePcmSource` が読む同一ヘッダを検証できる。

### 1.2 `arbd` RPC

| RPC | 要点 |
| --- | --- |
| `media.decode_audio_pcm` | `{bundle_id}` → `cache_dir/audio-pcm/<bundle_id>.wav` に原子的にキャッシュ。音声なしは `available: false` をエラーなしで返す。`{available, wav_path, sample_rate_hz, channels, duration_ns, av_offset_ms}` |
| `export.share_mp4` | `{bundle_id, in_frame_index?, out_frame_index?, output_path?, trash_original?}`。範囲指定時は mid-GOP トリムで不変 WORM の新バンドルを作成してから `mux_share_mp4`。全体指定は `video.mp4` + 任意の `audio.m4a` を直接マクス。`edts`/`elst` は 0 個。 |
| `timeline.overview` | バンドル概要・画面遷移・タップ・クラッシュ・不連続・`E`/`F` ログ上位（最大 50）・未アンカー件数を 1 回で返す。 |
| `media.inspect_frames` | `contact_sheet`（最大 16 タイル）/ `single` / `tap_crop`。PNG を `cache_dir/inspect/` に書き、正確なメタデータを返す。 |

**安全不変条件（REQ-04 / REQ-07）**: `trash_original: true` は新しい出力バンドルと `share.mp4` が `fsync`・検証・確定した後にのみ、元バンドルを**非破壊ゴミ箱**へ移動する。エラー時はゴミ箱に入れない。`Legacy (RO)` バンドルでは `CONFLICT` で拒否し、元のファイルは無変更のまま残す。永久削除は行わない。

### 1.3 `arb-cli`

- `arb-cli share-mp4 <bundle-id-or-path> [--in-frame N --out-frame M] [--out FILE.mp4] [--dest DIR] [--trash-original]`（`--in-frame`/`--out-frame` は両方必須）。
- `arb-cli mcp`: stdin/stdout 改行区切りの JSON-RPC 2.0 MCP サーバ（`initialize` / `notifications/initialized` / `ping` / `tools/list` / `tools/call`、プロトコル `2025-03-26` / `2024-11-05` 互換）。**stdout は JSON-RPC フレームのみ**、診断は stderr のみ。`arbd` を自動接続／起動する。

### 1.4 MCP ツール（17 名 = 14 機能群）

`arb_status`, `arb_devices`, `arb_capture_start`, `arb_capture_stop`, `arb_config_get`, `arb_config_update`, `arb_save`, `arb_screenshot`（MCP `image/png` ブロック付き）, `arb_save_logs`, `arb_list_bundles`, `arb_trash_bundle`, `arb_restore_bundle`, `arb_timeline_overview`, `arb_query_logs`, `arb_inspect_frames`（MCP `image/png` ブロック + `text` メタデータ）, `arb_export_trim`（`share_mp4` / `bundle`、非破壊 `trash_original`）, `arb_export_frame_png`。

## 2. テスト内訳（macOS arm64）

- `arb-media`: unit 20 + `audio_decode` 2 + `audio_pipeline` 4 + `contact_sheet` 15 + `media_pipeline` 12 + `mux_av` 9。`audio_decode` は実 AAC-LC `audio.m4a` を 48 kHz ステレオ s16le WAV へ往復し、RIFF ヘッダ・非無音エネルギー・非置換出力・一時ファイル掃除を検証。
- `arbd`: unit 23 + `e2e_scenarios` 22 + `t1_save_crash_config_e2e` 15 + **`t2b_mcp_share_audio_e2e` 3**。実 `arbd` に IPC 接続し、(1) `export.share_mp4` の全体／トリム／`trash_original` の非破壊動作と Legacy RO 拒否、(2) `media.decode_audio_pcm` のキャッシュと音声なしクリーン応答、(3) `timeline.overview`・`media.inspect_frames`（`contact_sheet`/`single`/`tap_crop` の PNG 署名検証）・`timeline.query_logs` フィルタ、を検証。
- `arb-cli`: unit 15 + **`mcp_stdio_integration` 1**。`arb-cli mcp` を実プロセスで起動し `initialize` → `tools/list`（全ツールとスキーマ）→ `tools/call`（`arb_status`/`arb_list_bundles`/`arb_timeline_overview`/`arb_inspect_frames` の `image/png` ブロック/`arb_export_trim` `share_mp4`/`arb_screenshot`）を実行、stdout が厳密な JSON-RPC のみであることと `arb-cli share-mp4` を確認。

## 3. ゲート結果

`./core/scripts/verify.sh`（fmt / clippy `-D warnings` / test / `check-licenses.py` = **0 GPL/AGPL 違反** / `sara check --strict` / REQ→SPEC 被覆）と `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` を実行し通過。追加依存は `rusty_aac 2.1.5`（Apache-2.0）のみ。

## 4. 明示的な未検証項目（Unverified）

- **Linux / Windows 実行**: 本検証は macOS arm64。両 OS は CI の `test` / `os-parity` で確認する。
- **実機 Android の AAC 音声由来入力**: `ffmpeg` 生成 AAC-LC による検証。実機結合は統合タスクで確認する。
- **HE-AAC / マルチブロック ADTS**: 暗黙破損を避けるため既存パーサ同様に明示的に拒否。
- **実際の MCP クライアント（Claude Desktop 等）との接続**: プロトコル準拠の自前 stdio クライアントによる検証のみ。

## 5. 追補: 保存失敗契約と RPC クライアントの堅牢化（H-1）

- `mcp_stdio_integration::mcp_reports_retryable_saves_with_recoverable_job_and_pins`: `arb_save` / `arb_save_logs` の `failed_retryable`（`bundle_id: null`）が `isError: true` で、`job_id`・`last_error`・`pinned_chunk_ids` を保持することと、`save.retry` 後の再保存が `isError: false` になることを確認。
- `arb-os` `rpc` の単体試験: `id` 不一致・欠落・通知混入・`result`/`error` 両欠落または両方ありの拒否と接続破棄、`"result": null` の受理、無応答・応答の細切れ送信での期限切れ、状態変更 RPC を再送しないこと、保存系を含む quick 以外のメソッドが長い期限を使うこと、閉じた接続への呼び出しが `Closed` を返すこと。
- `arb-tray`: アイコンとツールチップの更新が互いを止めないこと、未知の job state を警告付きで表示し再試行・破棄を出さないこと、メニュー再構築失敗後の再同期、期限切れ後の次回更新での再接続（再送なし）。
- `verify_release_bundle.py --self-test`: `capture_enabled` が `False` 以外（`True` / `None` / `0` / 欠落）の変異を検出する。
- 未検証: Windows 名前付きパイプ上の期限切れ動作は CI の `os-parity (windows-latest)` で確認する。
