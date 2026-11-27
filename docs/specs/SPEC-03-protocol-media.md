---
id: SPEC-03
type: spec
name: 端末ワイヤ・ローカル IPC・映像とトリム
satisfies:
  - REQ-03
  - REQ-08
  - REQ-09
  - REQ-11
  - REQ-12
  - REQ-13
  - REQ-14
---

# SPEC-03 端末ワイヤ・ローカル IPC・映像とトリム

## 端末ワイヤとローカル IPC [実装済み]

端末コンパニオンの制御／ビーコン・logcat・入力は同じ ADB 転送接続上に多重化する。固定 20 byte の little-endian ヘッダ（magic `AR`、版、種別、ストリーム、長さ・連番等）の後にペイロードが続く。映像・音声は別の scrcpy ソケットで big-endian のヘッダを読む。音声だけ拒否されたときの分岐は [SPEC-07](SPEC-07-device-capture-audio-input.md)。[ワイヤのフィールド・バイト配置](../../device-agent/PROTOCOL.md)・[Rust ワイヤ実装](../../core/crates/arb-adb/src/wire.rs)

ホストの制御は同一ユーザーのローカル IPC 上の JSON-RPC 2.0、1 行 1 メッセージ。Unix はランタイムディレクトリの `arbd.sock`、Windows はランタイムディレクトリから導くハッシュ名の Named Pipe を使用し、所有者 SID を検査する。一つの要求に対応する応答は同じ ID、`jsonrpc="2.0"`、`result` または `error` の片方だけを持つ。Rust クライアントは `result: null` を受理し、通知・不正応答・期限切れ・伝送断で接続を破棄する。[エンドポイント](../../core/crates/arb-os/src/paths.rs)・[IPC クライアント](../../core/crates/arb-os/src/rpc.rs)

### RPC 一覧（同義名を含む 46 ワイヤ名）

| 領域 | ワイヤ名 |
| --- | --- |
| system | `system.shutdown_for_handover`、`system.hello`、`system.status`、`system.shutdown` |
| config | `config.get`、`config.update` |
| device | `device.list` |
| capture | `capture.start`、`capture.stop` |
| save | `save.trigger`、`save.logs`、`save.screenshot`、`save.list_jobs`、`save.retry`、`save.retarget`、`save.discard` |
| bundle | `bundle.manifest`、`bundle.list`、`bundle.open`、`bundle.lease_renew`、`bundle.close`、`bundle.trash`、`bundle.restore`、`bundle.gc`、`bundle.forget`、`bundle.query_logs` |
| timeline | `timeline.summary`、`timeline.overview`、`timeline.query_logs`、`timeline.query_taps`、`timeline.discontinuities`、`timeline.crashes`、`timeline.screens` |
| edit | `edit.get_session`、`edit.update_session` |
| export | `export.trim`、`export.png`（同義名 `export.frame_png`）、`export.share_mp4` |
| media | `media.open_viewport`、`media.filmstrip`、`media.scrub`、`media.seek_frame`、`media.close_viewport`、`media.decode_audio_pcm`、`media.inspect_frames` |

フレーム PNG の `output_path` が省略または空文字なら、`save.screenshot` と共通の保存処理が `output_dir`、`screenshot_dir_override`、`output_dir_override`、`<data_dir>/screenshots` の順に保存先を選び、一意名を付ける。応答の `output_path` は保存したファイルのパス。明示したパスに既存ファイルがあれば `-32002` を返す。

`system.hello` は `protocol_version`、`build_id`、`parent_bound`、`gc_dry_run`、`realtime_trim`、`standalone_share_trim` 等を返す。CLI／MCP の Rust クライアントは実時間引数を送る前に同じ接続で能力を確認する。旧デーモンが余分なバンドルを作りうるフレーム範囲指定等は単独共有能力も確認し、未対応なら `-32601` と更新案内を返す。終了引継ぎの拒否は成功応答内の `ok: false`。`system.status` の `recovery` は起動時の物理削除完了件数・失敗件数・失敗 ID を伝える。保存・カタログは [SPEC-02](SPEC-02-time-selection-bundle.md)、検索・MCP 側の既定値は [SPEC-06](SPEC-06-mcp-cli-contracts.md)。[ディスパッチ](../../core/crates/arbd/src/lib.rs)・[要求・応答の型](../../core/crates/arbd/src/protocol.rs)

ログの生行（`bundle.query_logs`／`timeline.query_logs`）と概要（`timeline.overview`）は同じ `LogCoordinates` で、同じ boot の主ログの ns 時刻から表示中の VFR フレームを引く。生行の `frame_index` と概要の代表行の `frame_index_hint` は同じ行なら一致する。先頭フレームの表示開始より前の行は `frame_index` を null とし、`offset_ms` は負の値になる（フレームを捏造しない）。`timestamp_utc` はログヘッダのミリ秒 UTC。検索・集約の件数とフィルタ、版別の MCP 投影は [SPEC-06](SPEC-06-mcp-cli-contracts.md)、保存尺・ログ完全性・ドレイン・GC は [SPEC-02](SPEC-02-time-selection-bundle.md)、タップ座標の取得・読取時校正は [SPEC-07](SPEC-07-device-capture-audio-input.md) を参照する。

### エラーと再送

| ワイヤ結果 | 呼び出し元の必須動作 |
| --- | --- |
| `-32602` 引数不正 | 必須値欠落・必須値の null・型違いを操作失敗として返す。型付き引数の欠落・必須値の null は `missing required field`、型違いは対象フィールドと期待型・実際の型を示す |
| `-32001` 対象なし、`-32002` 状態衝突 | 該当する操作の失敗を通知。接続を維持する |
| `-32003` ストレージ、`-32004` メディア、`-32005` ADB | 原因を表示し、RPC 業務エラーだけでデーモン切断にしない |
| `-32601` メソッドなし | ER-07。ワイヤ上は RPC エラーだが、IDE は古いデーモンとして切断表示・操作失敗通知、CLI は終了値 2、MCP は `DAEMON_UNAVAILABLE` |
| 転送失敗・応答不整合 | 使用中の接続を破棄する。処理が確定したか不明な更新を自動再送してはならない |

`system.hello`／`system.status`／`system.shutdown`／`config.get`／`save.list_jobs`／`save.discard`／`bundle.list`／`bundle.lease_renew`／`bundle.close`／`bundle.forget`／編集セッション取得・更新は既定 30 秒、通常の重い処理は 120 秒、`export.trim`／`export.share_mp4`／`media.decode_audio_pcm` は既定 600 秒。長時間系は `ARB_RPC_LONG_TIMEOUT_MS`、書き出し系は `ARB_RPC_EXPORT_TIMEOUT_MS` で調整する。MCP は安全な読み取りの転送断を 1 回だけ再試行し、タイムアウトした更新は再送しない。既に閉じた接続で一度も送れなかった要求は再接続できる。[コード対応](../../core/crates/arbd/src/error.rs)・[ER 判定表](ui-feedback-contract-table.json)

## viewport と SHM [実装済み]

制御メッセージで viewport を開き、ピクセルを 3 スロットの共有メモリリングで渡す。1 ライターはスロットのシーケンスを更新中（奇数）にし、メタデータと画素を書いて完了（偶数）を公開する。リーダーはコピー前後のシーケンスが同じ完了値のときだけ採用する。上書きが競合した読み取りでは再試行し、世代の異なるヘッダと画素を組み合わせてはならない。寸法・stride・形式とスロット容量を検査する。近似フレームには `is_approximate` が立ち、UI は近似を表示する。viewport はタブと接続に紐付き、接続 EOF で回収する。[SHM 実装](../../core/crates/arb-os/src/shm.rs)・[K8／M1／G7、TCB-05](../proofs/tcb-axioms.md)

## 再生時間軸とメディア出力 [実装済み]

- MP4 コンテナ、AAC-LC の M4A 多重化・時間範囲抽出は Rust 実装。H.264 は 3 OS で OpenH264 を利用でき、macOS では VideoToolbox の初期化に成功すれば採用する。キャプチャの AAC エンコードは端末側が担当する。[コーデック境界](../../core/crates/arb-media/src/codec/mod.rs)・[音声](../../core/crates/arb-media/src/audio.rs)
- フレーム指定は `[in_frame_index, out_frame_index]` の両端を含む。通常のトリムは非 IDR 始点の先頭 GOP だけ再エンコードし、以降はコピーする。単一 `avc1` と IDR 境界での SPS/PPS により異なるプロファイルを接続。`edts`／`elst` で前フレームを隠すことを禁止し、開始前フレームは 0 枚。faststart は `moov` をメディアデータより前に置く。焼き込みは画素を書き換えるため再エンコードを伴う。[トリム](../../core/crates/arb-media/src/trim.rs)・[VER-06](../verifications/VER-06-arb-media.md)
- K12 の GOP キャッシュ予算は 128 MiB。全 GOP が収まらないときは IDR から順に復号し、前進窓から古い画素を捨てる。連続した次フレームへの前進はデコーダーを再利用する。冷えた遠方シークの計算時間は IDR からの距離に依存する。単一フレームが予算を超える場合までプロセスのメモリ上限を約束しない。[再生キャッシュとフィルムストリップ](../../core/crates/arb-media/src/playback.rs)
- 外部フィルムストリップ `filmstrip.bin` の magic は `ARBFS002`。入力 MP4 の長さ、フレーム数、内容の SHA-256 等を照合する。同じ長さでも内容が異なる場合や旧形式は再生成する。ヒット時も元動画のハッシュ計算は必要。FHD30 の全体サムネイル 1 秒・正確なシーク 0.2〜0.5 秒は実機計測目標。[REQ-11](../requirements/REQ-11.md)
- メディア時刻 0 は先頭フレームの表示開始。引き継ぎフレーム（[SPEC-02](SPEC-02-time-selection-bundle.md)）では撮影時刻ではなく窓先頭 `W0` で、manifest の `video.carried_in` が示す。ログ座標、概要の `screens`、viewport の `frame_boottimes`、実時間トリムの時刻写像は、`BundleReader::display_frames` と `arb_core::bundle::frame_display_start_ns` の単一の原点を使い、保存済みの行は書き換えない。保存時の MP4 は `mux_raw_h264_to_mp4_with_last_duration` で最終サンプルに被覆の終端までの実時間長を書く。
- K10 の軸は先頭メディア時刻 0、各表示開始 `frame_pts_ns`、最終保持を含む排他的終端 `timeline_duration_ns`。時刻は直前の開始 PTS のフレームを指す。最終フレームは `last_frame_duration_ns` を含む。10 万枚超では全 PTS を転送せず中間を推定する。旧デーモンや末尾長不明の推定差は [既知制約 / TCB-18]。[実時間軸](../../core/crates/arb-media/src/timeline.rs)・[TCB 台帳](../proofs/tcb-axioms.md)

### 実時間トリムと四つのストリーム

`edit.update_session`、`export.trim`、`export.share_mp4` は元動画先頭からの整数 ns の対を受け付ける。成立条件は `0 ≤ in_time_ns < out_time_ns ≤ duration_ns`。片側だけ・空・逆順・総尺超過は `-32602`。両端があればフレーム指定に優先し、両端省略なら従来の両端を含むフレーム番号を使用する。カタログには時刻対と互換用フレーム番号を保存し、`edit.get_session` が返す。時刻対のない旧セッションは Studio がフレームの表示開始と OUT の次の開始（末尾なら総尺）から復元する。[範囲解決](../../core/crates/arbd/src/trim_range.rs)・[編集セッション](../../core/crates/arbd/src/lib.rs)

| ストリーム | 同じ半開区間 `[in_time_ns, out_time_ns)` をどう出力するか |
| --- | --- |
| 映像 | `arb-media::timeline::plan_time_range_trim` が表示区間との共通部分を列挙し、`trim_video_time_range` が媒体へ書く。VFR 静止中の 12〜22 秒から 15.5〜18 秒を選べば、焼き込みなしでは IDR 1 枚で 2.5 秒。先頭 GOP 以外はコピー可能。累積時刻を丸めて各サンプルを最低 1 ティックとし、必要なら出力 timescale を上げる。faststart、単一 avc1、編集リスト 0 個（`elst=0`）。レポートは要求区間と量子化後の実尺を区別する |
| 音声 | 選択の実時間を PCM サンプル境界へ写し、音声との重なりがあれば選択先頭・末尾の不足分を無音で補って AAC に再符号化する。準備用パケットの表示長は 0、音声ファイルと共有 MP4 の `iTunSMPB` に先頭・末尾パディングを記す |
| ログ | 各端の映像フレームの端末時刻を使い、スリープをまたいでも同じ boot の記録を選ぶ。同期した主行は IN を含み OUT を除く。出力の相対時刻原点は IN |
| 入力 | 同期した境界横断スワイプは時刻で軌跡を補間し両端を切る。焼き込みでは静止映像でも押下・軌跡・解放・フェードに合わせて保持時間を分割し、操作が見える区間だけ描画する |

K13 の 5 ハーネスは区間・時間保存・表示同値・不正入力・ティック量子化を扱う。実際に書いた動画・AAC・ログ・入力の照合は G3 と実デーモン試験を通す。[メディア計画](../../core/crates/arb-media/src/timeline.rs)・[AAC 切り出し](../../core/crates/arb-media/src/audio/realtime.rs)・[G3](../../core/crates/arb-media/tests/pdd_g3_media_props.rs)・[四ストリーム統合試験](../../core/crates/arbd/tests/t2b_mcp_share_audio_e2e.rs)

### バンドル出力と共有動画

`export.trim` は別の編集済みバンドルを作り、ログ・イベント・マニフェストも保存する。`include_share_mp4` を指定すれば共有動画を追加し、`burn_taps` でタップ／スワイプを描ける。`logcat.log` と `events.jsonl` は空でも作り、隔離ログがあれば `unanchored.log` を追加する。元の内容は不変。`trash_original` は成功後の論理退避である。

| 操作 | RPC と成果物 | カタログ |
| --- | --- | --- |
| Studio「選択範囲を書き出し」 | `export.trim` で映像・利用可能な音声・ログ・入力を持つ完全なバンドルを作る。「共有用 MP4」なら `include_share_mp4=true`、同じフォルダに `share.mp4`（焼き込み時は `share-taps.mp4`）を同梱 | 「トリム済み」に新規登録 |
| CLI／MCP の単独共有 | `export.share_mp4` の既定は `keep_bundle=false`。全体または選択区間を指定ファイルへ出力。パス省略時は指定ディレクトリまたは既定先へ出力し、既存動画との衝突は失敗 | 追加 0 件。`keep_bundle=true` の明示時だけ編集済みバンドルを保持・登録 |

[Studio の書き出し](../../ide-plugin/src/main/kotlin/io/github/shinma06/arb/ide/ArbBundleFileEditor.kt)・[バンドル生成](../../core/crates/arbd/src/media_engine.rs)・[単独動画の実装](../../core/crates/arbd/src/inspect_export.rs)

## 仕様 vs バグ

| 条件 | 正常仕様 | バグ |
| --- | --- | --- |
| 存在しない ID と未定義メソッド | 前者は対象なし、後者は ER-07 | 両方で IDE が切断を表示する |
| 静止画面の 5 秒保持 | 時計は進み同じ画を表示 | フレーム番号を等間隔に並べ時計とマーカーがずれる |
| 引き継ぎフレームのあるバンドル | メディア時刻 0 は `W0`。ログ・概要・トリム・再生が同じ原点 | 撮影時刻を原点にしてログ位置や `offset_ms` が引き継ぎ分ずれる |
| 非 IDR からフレーム指定出力 | 指定した先頭画像から再生 | 再生開始前のフレームがファイルに残る |
| 範囲指定の共有動画 | 明示的な保持指定がなければ MP4 1 本 | 単独共有で編集済みバンドルを登録する |

設計と検証は Trail #96・#97・#113・#114・#133／PR #538・#136／PR #540・#140／PR #543・#149／PR #552・#151／PR #554。K8〜K10・K12〜K14、S4・S6・S7、G3・G7、M1・M2、TCB-05／09／18／20。[TCB 台帳](../proofs/tcb-axioms.md)
