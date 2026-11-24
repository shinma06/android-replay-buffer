---
id: VER-28
type: verification
name: キャプチャ常駐、保存窓、メディアとデーモンの回帰検証
verifies:
  - SPEC-01
  - SPEC-02
  - SPEC-03
  - SPEC-04
---

# キャプチャ常駐と保存・再生の回帰検証

2026-11-24。macOS arm64 で、模擬 ADB、独立したデーモンプロセス、一時バンドル、OpenH264 で符号化した映像を使用した。

| 指摘 | 修正内容 | 回帰テストで確認する結果 |
|---|---|---|
| H1 | 初回の pin で世代、端末情報、UID マップ、表示寸法を固定。世代が変わったら追加 pin を終了する。 | `pending_save_keeps_original_device_across_switch` が、B の初回イベント前の保存と、A の保存中に B を開始する場合を検査する。 |
| H2 | 窓の前の IDR から復号し、窓の先頭を IDR として再符号化する。先頭参照を得られない場合は映像を部分取得として記録する。 | `save_between_idrs_decodes_window_endpoints` が窓内の枚数と、復号された先頭・末尾の輝度を照合する。前の参照フレームを欠落させる試験では部分取得を確認する。 |
| H3 | IDR 時点で有効な SPS/PPS を保持し、変更毎に mux セグメントを分ける。 | `save_separate_configs_decodes_both_dimensions` が別設定パケットを伴う実 H.264 を保存し、前半と後半の解像度、画素を照合する。 |
| H4 | OpenH264 の単一フレーム seek では目的の画像だけを出力バッファに複製する。 | `sparse_seek_only_materializes_target_yuv` が最後の画像を GOP 全体の復号と照合し、複製対象が 1 枚であることを計数する。 |
| H5 | atom のヘッダーを順に読み、mdat は seek、moov は上限付きで読む。共有 MP4 の検査は一時ファイルで完了してから公開する。 | `sparse_large_mdat_reads_only_headers` の 4 GiB 超の疎ファイルでは読取りが 24 バイト。`failed_verification_allows_retry_at_same_path` が検査失敗後の同名再試行を確認する。 |
| H9 | 未登録の旧形式フォルダーも出力先の祖先検査に含める。 | `legacy_ancestors_protect_screenshot_destinations` が schema 3 の模擬バンドルと schema 4 の実フィクスチャのコピーへのディレクトリ指定・ファイル指定を拒否し、全ファイルのハッシュを照合する。 |
| H10 | manifest、タイムライン、メディア、ログ、digest 対象を正規化したバンドルルート内に制限する。 | `current_bundle_rejects_external_media_and_timeline` が別のバンドルへのファイルリンクを境界エラーとして拒否する。 |
| M3 | viewport 登録簿のロックを短くし、復号は viewport ごとに直列化する。 | `stalled_thumbnail_decode_allows_status_and_another_viewport_seek` が A のサムネイル生成を停止したまま status と B の seek を完了する。 |
| M4 | 正常な shutdown を確定してから最終応答を書き込む。 | `shutdown_flag_precedes_client_read_of_final_reply` がクライアントの読取りを保留して終了フラグを確認する。 |
| M5 | 世代・起動 epoch・トリガー時刻を通知に含め、クラッシュ検出中に対象の窓を pin してから worker に渡す。 | `t1_crash_autosave_debounce_and_timeline_crashes_screens` が保存窓より先の heartbeat を投入し、2 番目のバンドルにトリガーを含む 3 件のクラッシュを確認する。 |
| M6 | PID、serial、token、JAR、転送先を記録し、停止済み PID の記録だけを照合して回収する。 | `stale_forward_cleanup_matches_serial_port_and_socket` と模擬 ADB のプロセス試験で、別ツールの転送および生存 PID の記録を保持する。 |
| M7 | 元の音声ファイルを逐次ハッシュし、SHA-256 で PCM キャッシュを識別する。 | `t2b_decode_audio_pcm_decodes_real_aac_caches_and_handles_no_audio_cleanly` が同じ bundle ID を持つ二つの音声の出力先と再生時間を区別する。 |
| M8 | 画面区間の終了と最近傍フレームは boot ID ごとに計算し、ホストの記録順で返す。 | `screen_spans_preserve_boot_order_and_epoch_frame_bounds` が再起動で小さくなった端末時刻をまたいで検証する。 |
| M9 | 起動時に実行ファイルの SHA-256 を計算して build_id に含める。 | `t6c_logs_only_capture_foreground_app_logs_and_hello_build_id` が hello と独立に計算した digest を照合する。 |
| M10 | UID が 1000 と確認できた記録だけに、クラッシュ検出の副作用を許可する。 | M5 のテストが UID 不明の同形式イベントを追加し、検出件数と自動保存件数を確認する。 |
| M13 | 開始予約を短いロック区間で取得し、ADB 探索後に予約が有効なら開始する。 | `live_supervisor_waits_selects_reconnects_and_cancels_slow_discovery` が探索を保留したまま stop を完了し、古い開始要求を破棄する。 |
| L1 | JSON-RPC バージョンと dispatch 成功を確認してから shutdown を処理する。 | `invalid_shutdown_keeps_connection_alive` が無効なバージョンのエラー後に同じ接続で hello を成功させる。 |
| H8 | フレーム順の実 PTS を先頭時刻基準の ns で返す。10 万枚を超えた場合は配列を省略する。 | `scenario_01_normal_capture_save_inspect_shm_scrub_mid_gop_trim_png` と `viewport_timestamps_preserve_static_holds_and_bound_wire_size` が RPC 応答、静止区間、枚数上限を検査する。 |
| 常駐キャプチャ | 利用者の有効設定を永続化し、起動時と切断後に再探索する。指定端末を優先し、未指定かつ複数接続なら選択を待つ。 | `armed_capture_survives_restart_and_explicit_stop` と M13 の試験が起動時設定、端末なし、複数、指定端末不在・復帰、再接続、明示停止を検証する。 |

## 実行範囲

- `./core/scripts/verify.sh` の 6 ゲートに合格。Rust テストは成功 411 件、既存の ignored 3 件。Clippy 警告 0 件。ライセンス検査 330 パッケージ中 GPL/AGPL 違反 0 件。
- `sara check --strict` は VER-28 を含む 46 項目・239 関係で成功。要求 14 件すべてに対応仕様がある。
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` が成功。
- JDK 27 の `:client-core:test` は 107 件成功、スキップ 0 件。

USB 実機での抜き差し、Android Studio の操作、Windows の実機、4 GiB 超の実映像の書出し速度とメモリ消費は今回の手元試験の対象外。模擬 ADB の試験では探索・再接続・所有資源の照合を再現した。
