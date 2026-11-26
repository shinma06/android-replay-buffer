---
id: SPEC-03
type: spec
name: Device Agent・ローカル IPC・GPL 非依存メディアおよびフレーム単位トリム仕様
satisfies:
  - REQ-03
  - REQ-08
  - REQ-09
  - REQ-11
  - REQ-12
  - REQ-13
  - REQ-14
---

# SPEC-03: Device Agent・ローカル IPC・GPL 非依存メディアおよびフレーム単位トリム仕様

## 1. Device Agent と ADB ワイヤプロトコル（`arb-adb` / `device-agent`）

- **実績ある映像・音声取得の再利用**: `scrcpy-server-v5.0.1`（Apache 2.0）と初フレーム PTS 修正済み `AudioRecordReader` を中核として再利用し、`app_process` によりセッション単位で ADB 転送起動する（root・APK インストール・対象アプリ改修不要）。
- **キャプチャ推奨設定**: `video_codec=h264`, `video_codec_options=max-bframes:int=0,i-frame-interval:int=1`, `max_size=1920|1024`, `max_fps=30|15`（1 秒 GOP を明示要求しシーク・先頭 GOP 再エンコードを高速に保つ）。音声再生キャプチャ（`audio_source=playback audio_dup=true`）は API >= 33 対象とし、未校正オフセットを正直に記録する。
- **統合時計・ビーコン・logd コンパニオン**:
  - 20 byte 固定長 Little-Endian ヘッダのバイナリフレーミングで ADB forward 通信を行う。
  - `beacon`（250〜500 ms 周期）が `boot_id`, `boottime_ns`, `monotonic_ns`, `realtime_ns` と各ストリームの生存状態・連番を送信し、静止画面（`STATIC_HOLD`）と切断／欠損（`CONNECTION_LOST` / `STREAM_GAP`）を分離する。
  - `logcat` は端末読取時の `device_read_boottime_ns` を打刻し、`logcat -S` / `-g` 統計監視によりドロップを検知する。裏付け不可の端末では `completeness = "unverified"` を明示する。
  - `video` / `audio` / `logcat` / `input` / `beacon` は独立エラー境界で障害隔離する。
- 入力取得時は接続ごとにホストが `getevent -lp` のデバイス別・軸別の最小値と最大値を読み、絶対座標を物理画面のピクセル位置に変換する。Tap / Swipe の移動量判定、保存する軌跡、Studio・共有動画の表示は同じ座標を使う。軸情報や寸法を取得できない場合は従来の座標を使い、表示範囲外のマーカーは描画しない。
- **自動再接続**: ADB デバイストラッカーを常時監視し、切断検知後は指数バックオフ（200 ms → 500 ms → 1 s → 2 s）で数秒以内に Device Agent を自動再起動する。再接続時の `boot_id` 照合で `CONNECTION_RESTORED` と `DEVICE_REBOOT` を厳密区別する。

## 2. `arbd` ↔ クライアント ローカル IPC と共有メモリフレームリング

- **制御プレーン**: バージョン付き JSON-RPC 2.0 / NDJSON（Linux: `$XDG_RUNTIME_DIR/arb/arbd.sock`、macOS: `~/Library/Caches/arb/arbd.sock`、Windows: `\\.\pipe\arb-daemon-<sid>`）。`system.*`, `device.*`, `capture.*`, `save.*`, `bundle.*`, `timeline.*`, `edit.*`, `export.*`, `media.*` を提供する。
- **クライアントの応答検証と待ち時間**: `arb-os` の `RpcClient` は応答の `jsonrpc == "2.0"`・`id` が要求と一致すること・`method`（通知）を含まないこと・`result` と `error` のどちらか一方だけを持つことを検査する。`"result": null` は正常応答として受理し、欠落・不一致・両方ありは `InvalidResponse` として接続を破棄する。1 回の呼び出しの読み書きは合計の期限内に収め、IDE（`ArbdRpcClient`）・CLI・MCP・トレイで同じ 3 段階の期限を共有する。軽い制御メソッド（`system.hello` / `system.status` / `system.shutdown` / `config.get` / `save.list_jobs` / `save.discard` / `bundle.list` / `bundle.lease_renew` / `bundle.close` / `bundle.forget` / `edit.*`）は 30 秒、長尺再エンコードや全区間音声デコード（`export.trim` / `export.share_mp4` / `media.decode_audio_pcm`）は 600 秒（環境変数 `ARB_RPC_EXPORT_TIMEOUT_MS` で上書き可、未指定時は `ARB_RPC_LONG_TIMEOUT_MS` との大きい方）、それ以外のメソッド（保存・キャプチャ操作・`config.update`・`device.list`・`bundle.gc`・`timeline.*`・その他 `media.*` など）は 120 秒（環境変数 `ARB_RPC_LONG_TIMEOUT_MS` で上書き可、`wait_for_daemon` の `system.hello` は起動待ちの残り時間、最終確認だけ最低 1 秒）とする。期限切れ・不正応答・転送エラーでは接続を破棄し、以後の呼び出しは何も送らず `Closed` を返し、状態を変える RPC を同じ接続でも再接続でも自動再送しない（MCP は読み取り専用メソッドを転送断の 1 回だけ再送し、期限切れでは再送しない。`Closed` は未送信のため 1 回だけ再接続する）。次の呼び出しで再接続する。
- **保存失敗契約（CLI / MCP 共通）**: `save.trigger` / `save.logs`（および CLI の `save.retry` / `save.retarget`）は応答に `bundle_id` がない（`null`）または `job.state == "failed_retryable"` のとき失敗として扱う（CLI は終了コード 3、MCP の `arb_save` / `arb_save_logs` は `isError: true`）。MCP は daemon の応答にエラーコードを付加し、保存ジョブの情報を保持する（`job.job_id`・`job.last_error`・`job.pinned_chunk_ids` を含む）で、CLI の `save.retry` / `save.retarget` による回復に使える。`bundle_id` を持つ完了時だけ成功（`isError: false`）。
- `system.hello` は `parent_bound`（真偽値）を返す。`--exit-on-stdin-eof` で起動され、親の終了とともに終了する `arbd` だけが `true`。古い `arbd` は返さず、クライアントは `false` とみなす。
- `system.status` の `recovery` は起動時回復の要約として `completed_purging_bundles` に加え `failed_purging_bundles`（件数）と `failed_purging_bundle_ids`（失敗 ID 一覧、0 件時は省略可）を返し、GUI・CLI・MCP で起動時の削除失敗を同一に観測できる。
- **音声キャプチャ方式**　`config.get` / `config.update` の `audio_capture_mode` は既定値（旧設定での省略時も同じ）が `playback_dup`。API 33 以上で端末再生を維持し、`audio_source=playback audio_dup=true` を渡す。利用者が `output_compat` を選ぶと API 30 以上で `audio_source=output` のみを指定して端末出力を取得し、録音中は端末を消音する。API 不明時は選択した方式で試す。障害で方式を自動変更することはない。録画中の方式変更では現在の接続を終了し、同じ端末の新しい接続で音声を再試行する。接続単位の方式は `system.status.capture.audio_capture_mode` が返す。GUI は旧 daemon の省略値を既定方式として扱う。
- **音声の状態と原因（PDD G8）**: `system.status` の `capture` は `audio_state`（`unknown` / `available` / `disabled` / `unavailable`）に加え、機械可読の `audio_cause` と端末の `device_api_level` を返す。判定は `arb-core::audio_policy` だけが行い、`arb-adb` の `scrcpy-server` 引数と `arbd` の全経路が同じ判定を使う。`audio_cause` は `none` / `user_disabled` / `logs_only` / `api_below_min` / `device_capture_failed`（`codec_id = 0`、端末側で再生音キャプチャを開始できない）/ `encoder_fatal`（`codec_id = 1`、`scrcpy-server` ごと終了する致命エラー）/ `handshake_io` / `tunnel_connect_failed` / `launch_failed_audio_true` / `launch_failed_both` / `runtime_sticky_disabled` / `unknown`（新しい値を知らないクライアント用）。「非対応」と表示してよいのは `api_below_min`（既知の API が選択方式の下限未満）だけで、選択方式の下限以上や API 不明での失敗は必ず具体的な原因で返す。`audio_cause` を返さない旧 `arbd` の `unavailable` は、原因不明の実行時失敗として扱う。実行時失敗の `audio_reason` には `scrcpy-server` の標準エラー出力の末尾（最大 600 バイト、改行は ` | ` で連結）を含める。`encoder_fatal` と `launch_failed_audio_true` の後は、同じキャプチャ中の同じ `(serial, boot_id)` の再接続をすべて `audio=false` で起動し（`runtime_sticky_disabled`）、映像とログを止めない。音声方式変更、録画の停止→開始、別端末、端末の再起動（別 `boot_id`）では解除する。状態と原因の対応は共有正本表 `docs/specs/audio-policy-table.json` に定め、Rust（`arb-core`）と Kotlin（`client-core` の `AudioPolicyTable`）の両方のテストがこの表を読んで一致を検査する。GUI・CLI・MCP は同じ `system.status` を参照する。Tool Window は録画中の実状態と異なる音声設定を併記し、録画停止中は設定を表示する。切断中は接続待ちと表示する。Tool Window の音声バナーは短い理由をそのまま表示する。標準エラー出力や長い理由は要約し、該当時は音声方式の切替案内を添える。「詳細を表示」で受信した理由の全文を展開でき、「詳細をコピー」で原文をコピーできる。
- `system.shutdown_for_handover` は非 system RPC・クラッシュ自動保存と排他的に引き継ぐ。親付き daemon は、起動した IDE が子の環境変数 `ARB_HANDOVER_OWNER` に設定したランダム値と RPC 引数 `owner` の一致も要求する。処理中、開いている映像 viewport（Studio のタブ）がある場合、または所有確認に失敗した場合は `{"ok":false}`（タブを閉じると受理される）。受理時は次の保存の受付を閉じて通常終了し、`{"ok":true}` を返す。旧版の method not found では、所有者が停止するまで更新を延期する。
- ログのページ送りでは応答の `next_cursor` を次の要求の `cursor` に渡す。旧 `after_seq` は互換用。
- `bundle.list`（および `bundle.trash` / `bundle.restore`）は各 `BundleEntryView` に実ディスク状態 `disk_state`（`present` / `missing` / `storage_unavailable`、旧 `arbd` 省略時は `present` 扱い）を非破壊の純粋読取で付与する。`bundle.forget`（CLI: `bundles forget <ID>|--all-missing`、MCP: `arb_forget_bundle`）はディスクを一切触らず `missing` かつリース・保存ジョブ 0 件の行のみカタログから解除する。`bundle.gc`（CLI: `bundles gc [--now]`、MCP: `arb_gc_bundles`）は省略可能な `ignore_grace`（既定 `false`）を受け付け、`true` 指定時のみ `gc_grace_secs` を 0 として管理下 v2 の即時物理削除を行う（リース・保存ジョブ・管理域外・旧形式保護は維持）。
- **高帯域データプレーン**: ビューポート単位のトリプルバッファ共有メモリ（mmap）フレームリングで 1080p30 RGBA/BGRA を配信する。スロットヘッダには `generation`, `frame_index`, `is_approximate`, `width`, `height`, `stride`, `pixel_format`, `boottime_ns` を持ち、`is_approximate == true` の間は UI に近似プレビューバッジを必ず表示する。

### 保存結果・カタログ・ログ検索

| RPC | 引数と結果 |
| --- | --- |
| `capture.start` | `wait_connected` の既定は `false`。`true` では ADB 探索から接続確認まで `wait_timeout_ms`（既定 5000、最大 30000）待つ。端末選択待ちは即時に返る。応答の `serial`、`connected`、`wait_timed_out`、`audio_state`、`audio_cause` は今回の開始要求に対応する。期限に達した後も記録開始の意図は保持する。 |
| `save.trigger`、`save.logs`、`save.retry`、`save.retarget` | `requested_duration_secs` は要求値。`actual_video_duration_ms` は MP4 の再生尺、`actual_log_duration_ms` は同一 boot の確定ログの最大時刻と最小時刻の差。映像なしやログ 0～1 件の実尺は 0。`log_count` は隔離分を含む。`video_frame_count`、`unanchored_log_count`、`truncated_unanchored_count`（全ストリームの隔離上限超過など）、`discontinuity_count` を返す。映像と確定ログの尺を独立に評価し、要求より 1 秒を超えて短い場合に `video_shortfall`・`log_shortfall` を返す。ログのみの保存は映像を評価しない。`window_shortfall` は両者の論理和。`warning_codes` に不足したストリーム、logcat_overflow、unanchored_truncated などの安定コードを返し、`warnings` に表示文を返す。`logcat_completeness` は確認できた欠落件数（通常のリング追い出し、統計フレームを含む端末の送信キュー溢れ、保存窓のログに影響しうるホスト側の欠損など、行数が不明な場合は null）、溢れの回数、隔離件数・切り捨て件数を含む。カタログ登録失敗後も、発行済みなら測定値を返す。発行前や旧 daemon で未測定の値は省略できる。 |
| `bundle.list` | `bundle_kind` は `full`、`logs_only`、`trimmed`。`duration_ms` は映像があれば映像尺、それ以外は確定ログ尺。`frame_count`、`has_video`、`has_audio`、`log_count`、`crash_marker_count`、`created_at_utc`（旧クライアント向け RPC 別名は `created_utc`）、`memo`（未設定は null）を返す。欠損時はカタログの保存済み集計を使う。旧カタログで測定前に移動された記録の値は不明。 |
| `bundle.forget` | 登録パスを調べ、管理域内で親フォルダごと移された場合も、到達可能な管理ディレクトリ配下の欠損なら `missing` とする。管理域外で親の可用性が確認できない場合は `storage_unavailable` とする。 |
| `bundle.gc` | `dry_run` の既定は `false`。`true` は読み取り専用。応答の `preview.candidate_bundle_ids` が実行候補、`missing_bundle_ids` がそのうちカタログ行だけの候補。`legacy_bundle_ids`、`unmanaged_bundle_ids`、`expired_lease_ids`、`estimated_reclaim_bytes`（通常ファイルの論理サイズ合計）も `preview` 内に返す。従来の `skipped_*` と `failed` に加え `counts` は candidate / missing / legacy / unmanaged / leased / active_job / in_grace / expired_leases / failed 別の件数。実行時にはその時点で再判定する。CLI / MCP は同一の IPC 接続で `system.hello.gc_dry_run` を検査してからプレビューを要求する。旧デーモンが未定義の引数を読み飛ばして削除する事故を防ぐ。 |
| `bundle.query_logs`、`timeline.query_logs` | 同じ実装。`limit` は 1 以上、既定 10000、最大 100000。`cursor` は最後に返した `seq` の十進文字列で、旧 `after_seq` と排他。同じバンドル・条件で継続する。`total_matched` は全ページを通じた一致件数、`returned_count` は今回の件数。`has_more` と `next_cursor`（最終ページは null）を返す。`whole_word=true` は keyword と tag の前後が英数字・アンダースコア以外または文字列の端であることを要求する。各行の `timestamp_utc` はログヘッダのミリ秒精度 UTC、`offset_ms` は同一 boot の映像先頭（ログのみは epoch 開始）からの相対時刻、`frame_index` はその時点で表示されるフレーム。時刻を同期できない行は相対時刻・フレームとも null。 |

ログの `level` は大文字小文字を区別せず、V / D / I / W / E / F と VERBOSE / DEBUG / INFO / WARN / WARNING / ERROR / FATAL / ASSERT を受け付ける。不正な値、必須フィールドの欠落（null を含む）、型不一致は -32602。データが見つからない場合は -32001、状態衝突は -32002、ストレージ障害は -32003、メディア障害は -32004、ADB 障害は -32005。業務エラー後は接続を維持する。

## 3. GPL 非依存メディア処理とフレーム単位トリム（`arb-media`）

- **純 Rust ISO-BMFF コンテナ層（スパイク S1 確定）**:
  - 異種プロファイル結合（例: OpenH264 Constrained Baseline 先頭 GOP ＋ ストリームコピー High/CABAC 後続 GOP）における全デコーダ互換のため、**単一 `stsd` `avc1` ＋ IDR 境界でのインバンド長さプレフィックス付き SPS/PPS 挿入** を採用する（二重 `stsd` は VideoToolbox で失敗、`avc3` は Apple `AVAssetReader` で失敗することを S1 で確認済み）。
  - `edts` / `elst`（Edit List）に一切依存せず、開始前フレームを符号化データ自体から完全除去（0 枚）し、faststart 配置（`moov` 前置）で出力する。
- **`VideoCodecBackend` 抽象とバックエンド構成（スパイク S1 確定）**:
  - **ユニバーサル 3 OS ベースライン**: `openh264`（`BSD-2-Clause`、1 フレーム内部 DPB 遅延は `flush_remaining()` / 1 パケット先読みで補正）。
  - **macOS デフォルト HW バックエンド**: `VideoToolbox` / `CoreMedia` / `CoreVideo` 直接 C FFI（外部クレート 0、ゼロコピー `CVPixelBuffer` 復号→再エンコード、障害時は OpenH264 へ自動フォールバック）。
  - Windows / Linux x86_64 向け HW バックエンド（Media Foundation / VA-API）は OpenH264（`nasm` 有効）実測後に必要時のみ追加する。
- **体感性能設計**:
  - **外部フィルムストリップキャッシュ**（`cache/<bundle-id>/filmstrip.bin`）は MP4 全体の SHA-256 を照合する。形式が古い場合や内容が変わった場合はキーフレームのサムネイルを再生成する。全体のハッシュはストリーミングで計算し、ファイル全体をメモリに載せないが、キャッシュ命中時も MP4 全体の読み取りが要る（およそ 0.5 GB/s 以上の実測。数百 MB の保存物で最大 1 秒前後、ストレージの読み取り速度に依存）。並列復号によるオープンの目標は 1 秒以内で、大きな保存物では実機計測で確認する。
  - Studio のタイムライン・マーカー・時刻表示はフレーム 0 からのメディア時間を共有する。`media.open_viewport` の `frame_pts_ns` が各フレームの開始、`timeline_duration_ns` が最終フレームの保持を含む排他的終端を示す。10 万枚を超える場合は総尺と最終保持長から中間位置を等間隔に推定する。旧 daemon では利用可能な PTS・末尾長・epoch の順に推定に用いる。
  - 再生中のプレイヘッドは 0.25・0.5・1・2 倍の時計に従い、静止中も進む。時間シークと一時停止は保持区間内の時刻を保存する。コマ送りは隣の PTS に移る。トリムのフレーム番号は両端を含み、右ハンドルは OUT の次の開始時刻（最終フレームなら総尺）に置く。端末時刻は boot ごとのアンカーで合わせ、対応が特定できない行は「未同期」と表示する。
  - **GOP-LRU 復号済みフレームキャッシュ**＋最新優先キャンセルで任意シーク 0.2〜0.5 秒以内、前後 1 フレームコマ送りの遅延ゼロを達成。
  - **映像の実時間トリム**。`arb-media::trim::trim_video_time_range` は半開区間 `[in_time_ns, out_time_ns)` と各表示区間の共通部分を出力する。先頭が GOP の途中なら先頭 GOP のみを再エンコードする。累積時刻を最寄りのティックに丸め、各サンプルに最低 1 ティックを割り当てる。複数枚に必要なティック数が不足する場合は出力の timescale を上げる。単一フレームの静止区間 12〜22 秒から 15.5〜18 秒を選ぶと、長さ 2.5 秒の IDR 1 枚になる。入力区間と実出力尺をレポートに記録し、出力は faststart・単一 avc1・編集リストなしとする。
  - **フレーム単位トリム**: `[in_frame_index, out_frame_index]` 契約で先頭が非キーフレーム時のみ先頭 GOP 残余を新規 IDR から再エンコードし、後続 GOP は無劣化ストリームコピー。関連トラックは `arb-core::selection` の同一 3 値選別エンジンで抽出し元バンドルを一切変更しない。


### Studio の連続再生と接続復旧

- 表示用時計は単調時計で進み、静止区間は次の PTS まで同じフレームを表示する。`media.open_viewport` の任意フィールド `last_frame_duration_ns` は MP4 の最終サンプル長を返す。旧デーモンでは直前のフレーム間隔を使用する。
- 再生タブごとに専用 IPC 接続と SHM viewport を保持する。切断または viewport 失効時は一度だけ開き直して同じ位置をシークする。復旧後も選択範囲と速度を保持する。再生継続が困難なときはフレーム番号と理由を表示する。
- 解像度と GOP 長から復号画像がキャッシュ上限を超える場合は、先頭 IDR から順次復号し、上限内の直近フレームを再利用する。

## MCP stdio と CLI の調査契約

`arb-cli mcp` は 2024-11-05、2025-03-26、2025-06-18、2025-11-25 をネゴシエートし、未知版には 2025-11-25 を提示する。2025-06-18 以降で JSON-RPC バッチを受け取った場合は -32600 を返す。公開名は `android-replay-buffer`。各行は独立した JSON-RPC メッセージで、応答・進捗・ログ通知は同じ stdout のロックを通る。[MCP 2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25)

- 20 ツールの入力、出力、注釈、実行形式の正本は [MCP ツール契約表](mcp-tools-table.json)。入力は同じ表で検証する。`structuredContent` と JSON テキストの値は等しい。エラーは `isError`、`error_code`、`rpc_code`、`message` を持つ。再試行可能な保存失敗では元のジョブ情報を保持する。
- 2025-11-25 の書き出しは元バンドルを保持し、スクリーンショットは既存ファイルを保護する。`trash_original: true` / `overwrite: true` は入力エラーになる。元データの整理には `arb_trash_bundle` を使う。以前の3版は既存スクリプトの指定を受理し、この2ツールに `destructiveHint: true` を付ける。
- `arb_save_and_inspect` は保存、概要、画面遷移、クラッシュ、最後の ERROR ログを共有 RPC で取得する。`memo` はその調査結果に添える。保存後の照会が失敗した場合は `save` に保存済みの結果を残す。保存結果の計測不能値は `null`。
- `arb_query_logs` と `arb-cli logs <ID>` は既定 100 件。上限 100000 件、`cursor` と `after_seq` は排他。CLI は `--limit`、`--cursor`、`--whole-word` を使う。`gc --dry-run` と `capture start --wait-connected --wait-timeout-ms 5000` も同じ RPC を使う。プレビューでは同じ IPC 接続の `system.hello.gc_dry_run` を確認し、未対応のデーモンには削除要求を送らない。
- `arb://status`、`arb://bundles` と各バンドルの `manifest`、`screens`、`crashes`、`logs`、`frame/{frame_index}` を提供する。ログのリソースは最初の 100 件。フレームはメタデータだけを既定とし、PNG のデコードは `resources/read` または `include_images: true` の時点で行う。パスを直接指定した外部バンドルには、その stdio セッション内で使える参照 ID を割り当てる。旧世代にはリンクをテキストとして通知する。
- プロンプトは `investigate_crash`、`triage_recent_bug`、`safe_cleanup_review`。バンドル ID はカタログから前方一致で最大 100 件を補完する。整理前にはプレビューと利用者確認を案内する。
- `2025-11-25` の Tasks は任意。保存・複合調査・MP4/トリム・録画開始をバックグラウンド実行できる。MP4 とトリムの公開名は既存の `arb_export_trim` を使い、明示形式の呼び出し名 `arb_export_share_mp4` と `arb_trim_bundle` も受け付ける。`tasks/result` の待機中も同じ stdio で照会・キャンセルできる。キャンセル後は新しい RPC を送信しない。デーモンが既に受理した変更は完了し得る。保持はセッション内 64 件、既定 TTL は 60000 ms、最大 3600000 ms。TTL 到達後の終了済みタスクを回収し、EOF でセッションを破棄する。[MCP Tasks](https://modelcontextprotocol.io/specification/2025-11-25/basic/utilities/tasks)

### タップとログの観測範囲

入力トラックは getevent に届いたハードウェア・仮想タッチデバイスのイベントを記録する。実機での指操作や、対応するタッチデバイスへの sendevent が対象となる。adb shell input tap など Android Framework 経由の注入はこの経路を通らない場合があり、タップ件数が 0 でも操作がなかったとは判定できない。[AOSP getevent](https://source.android.com/docs/core/interaction/input/getevent)

タップの座標単位は端末の画面ピクセル。端末ごとの evdev 軸を校正し、宣言された軸の外にある入力は最寄りの画面端に制限する。この校正より前に保存したバンドルに範囲外の座標が残っている場合も、記録済みの画面寸法を使って読み出し時だけ画面端に制限する。元の画面内位置を特定できない入力を表すため、エミュレータの `emu event mouse` には表示範囲内の画面座標を渡す。概要、タップクロップ、Studio と焼き込みはこの画面座標を使用する。

ログのフレーム番号は同一ブートのナノ秒時刻から半開区間で求める。UTC はミリ秒まで表示する。MCP のログ検索は録画窓内を既定とし、次のページは同じ条件で next_cursor を cursor に渡す。概要と保存後の調査は優先度・タグ・本文が一致するエラーをまとめ、最新出現順で表示対象を選ぶ。各代表行には出現数 repeat_count と最初・最後のオフセットがある。total_error_logs はフィルタ一致の生行総数、unique_error_logs はグループ総数、error_logs_truncated は上限によりグループを省いたかを示す。arb_query_logs は生行を返す。

MCP 2025-11-25 の arb_screenshot は既定で保存パスとメタデータを返す。PNG のインライン返却には include_image=true を指定する。旧プロトコルではインライン返却が既定。最新のスクリーンショット・トリムは overwrite・trash_original を受け付けない。保存時間の正本は duration_secs、対象の正本は bundle_id。drain_timeout_ms の有効範囲は 0～5000 で、境界を超えると RPC と MCP のどちらも -32602 を返す。

Task 非対応のツールに task を付けた場合と taskId が欠落・型違い・未知または期限切れの場合は、それぞれの理由を付けて -32602 を返す。進捗トークンのある呼び出しの各段階は notifications/progress で通知する。失敗はエラーログ、トークンのない詳細な段階ログは debug 設定で通知する。

MCP 2025-11-25 の概要と保存結果は同型の `logcat_completeness` を返す。RPC の概要では以前からの状態名も維持し、詳細を `logcat_completeness_detail` に返す。状態・停止ツールの `audio_reason` は先頭の要約を最大 256 文字で返す。IDE は RPC から詳細を取得する。

共有 MP4 出力は指定ファイル（省略時は dest_dir または既定の出力先）に動画を保存する。keep_bundle を指定した場合だけ併せて編集済みバンドルを登録する。export.trim の編集済みバンドル出力は従来どおりである。
