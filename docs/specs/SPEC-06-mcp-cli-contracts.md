---
id: SPEC-06
type: spec
name: MCP stdio と CLI の公開契約
satisfies:
  - REQ-01
  - REQ-03
  - REQ-04
  - REQ-08
  - REQ-09
  - REQ-13
---

# SPEC-06 MCP stdio と CLI の公開契約

## バージョン交渉と結果 [実装済み]

`arb-cli mcp` は stdin／stdout の改行区切り JSON-RPC 2.0 サーバー。`initialize` で指定された `2024-11-05`、`2025-03-26`、`2025-06-18`、`2025-11-25` を返す。省略・未知版には `2025-11-25` を提示する。`2025-06-18` 以降のバッチ要求は `-32600`。応答と通知は stdout 上で行を混ぜてはならない。

`tools/list` は `initialize` で交渉した版に従い、全 20 ツールを 1 応答で返す（`nextCursor` なし）。1 応答は 4 版とも 87,000 バイト未満を維持する。各ツールの入出力スキーマは自己完結した JSON Schema Draft 2020-12 で、参照先はそのスキーマ内にある。[4 版の実 stdio 検査](../../core/scripts/check-mcp-trim-schema.py)

公開ツールの `inputSchema`、成功型・エラー型から組み立てる `outputSchema`、`ToolAnnotations` は同じツール表に従う。版による既定値・受理範囲の違いは [`version.rs`](../../core/crates/arb-cli/src/mcp/version.rs) の能力述語だけが決め、ほかのファイルは版文字列を持たない。デーモン RPC の形は IDE・CLI と共通のまま保ち、MCP だけの整形（軽量行、画面の参照、任意のローカル時刻、続きの呼び出し）は [投影層](../../core/crates/arb-cli/src/mcp/project) の純関数が行う。投影は繰り返しを省くか派生項目を足すだけで、証跡の行を落とさない。`tools/call` の本体は全版で JSON テキストに入り、2025-06-18 以降は同じ値を `structuredContent` にも載せる。追加の画像・リンクは別の content ブロック。`isError`、`error_code`、`rpc_code`、`message` を用いて操作失敗を報告する。CLI の構造化エラーと MCP は `kind`・`hint`・`retryable` の判定を共有し、二重退避 `ALREADY_TRASHED` と二重復元 `ALREADY_ACTIVE`、映像トラックのない入力 `NO_VIDEO` を識別する。CLI の JSON と MCP のエラー本体はデーモンの `error.data.details` を任意の `details` オブジェクトとして保持する。引数検証やメソッド未定義のプロトコル応答と、実行済みツールの失敗を区別する。ヒント（読み取り専用・破壊性・冪等性・外部作用）は各ツールの表の値を使う。[正本 JSON](mcp-tools-table.json)・[版別スキーマ生成](../../core/crates/arb-cli/src/mcp/catalog.rs)・[MCP Tools 原仕様](https://modelcontextprotocol.io/specification/2025-11-25/server/tools)

対象を特定できない旧デーモンの `-32001` は `NOT_FOUND` と読む。現行デーモンはバンドル・保存ジョブ・リース・ビューポートにそれぞれ `BUNDLE_NOT_FOUND`・`JOB_NOT_FOUND`・`LEASE_NOT_FOUND`・`VIEWPORT_NOT_FOUND` を付ける。対象端末なしは `DEVICE_NOT_FOUND`、端末用 JAR の欠落は `INSTALLATION_INCOMPLETE`、保存に必要なパラメータ・チャンク・ジャーナルの欠落は `SAVE_FAILED` とし、いずれも従来の数値コード `-32001` を維持する。このうち再試行可能なのは端末未接続だけで、IDE は接続を維持する。

`arb_inspect_frames` は映像のないバンドルと 0 フレームの映像をメタデータ・描画とも `NO_VIDEO`（`-32602`）で返す。`tap_crop` の `tap_index` が概要タップの範囲外なら `INVALID_PARAMS`（`-32602`）とし、`details` に `field`、指定値 `got`、件数 `overview_tap_count`、上限 `allowed_max`（0 件は null）、診断文 `cause` を入れる。`hint` は件数に応じて有効範囲、または `tap_index` を外して映像ピクセル座標 `tap_x`・`tap_y` を指定する方法を案内する。

| 用途 | `tools/list` の公開名（20 件） |
| --- | --- |
| 状態・端末・設定 | `arb_status`、`arb_devices`、`arb_config_get`、`arb_config_update` |
| キャプチャ制御 | `arb_capture_start`、`arb_capture_stop` |
| 保存・調査開始 | `arb_save`、`arb_save_logs`、`arb_screenshot`、`arb_save_and_inspect` |
| ライブラリ | `arb_list_bundles`、`arb_trash_bundle`、`arb_restore_bundle`、`arb_gc_bundles`、`arb_forget_bundle` |
| 保存物の調査 | `arb_timeline_overview`、`arb_query_logs`、`arb_inspect_frames` |
| 書き出し | `arb_export_trim`、`arb_export_frame_png`（4 版とも `output_path` か自動命名の `output_dir` のどちらか一方が必須。どちらも既存ファイルを上書きせず、`overwrite` 引数はない。明示パスの衝突は `-32002`） |

`2025-11-25` の `arb_export_trim` は `trash_original`、`arb_screenshot` は `overwrite` の指定自体を拒否する。以前の 3 版はこれらを受理し破壊性ヒントを立てる。保存秒数の正本は `duration_secs`、バンドル指定は `bundle_id`。`arb_export_trim` の既定形式 `share_mp4` は `export.share_mp4` に渡し、`keep_bundle=false` が既定。`format=bundle` は `export.trim`。呼び出し別名 `arb_export_share_mp4` と `arb_trim_bundle` は列挙数に含めない。CLI の `--in-time-ns`／`--out-time-ns` と MCP の `in_time_ns`／`out_time_ns` は同じ実時間の対で、フレーム指定との優先順位・端点・成果物は [SPEC-03](SPEC-03-protocol-media.md) に従う。[stdio 実装](../../core/crates/arb-cli/src/mcp.rs)

バンドル指定の別名と正本は値が同じなら 1 つの指定として受理する。他の別名の併用、異なるバンドル指定、排他引数の同時指定は `INVALID_PARAMS`（-32602）となる。保存 3 ツールは duration_secs（window_secs、duration_sec）、note（memo）、dest_dir、drain_timeout_ms を共通に受理する。調査・書き出し 5 ツールは bundle_id（bundle_id_or_path）を使う。cursor と after_seq、スクリーンショットと PNG 書き出しの output_path と output_dir は各組で片方だけ指定する（PNG 書き出しはどちらか一方が必須で、両方は details.conflicts、どちらもなければ details.one_of_required）。検証失敗の details には衝突引数名、許容範囲と入力値、列挙候補、型または必須キーを入れ、message と hint で修正方法を示す。

管理域（データディレクトリ配下の bundles）外への保存（dest_dir、output_dir_override）は unmanaged で arb_gc_bundles の物理削除対象外です。実体を手動で移動・削除し、元の保存先が missing になったら arb_forget_bundle で登録を解除します。

## 証跡を保存し調査する

1. `arb_capture_start` の `wait_connected` は既定 false。true なら `wait_timeout_ms`（既定 5,000、最大 30,000 ms）まで未列挙・オフラインの対象を再探索し、今回の取得世代の接続成立を待つ。期限切れは started=false、connected=false、wait_timed_out=true。複数候補からの選択待ちは速やかに返る。タイムアウト後も開始意図は保持する。`connected` と `wait_timed_out` で結果を読む。
2. `arb_save_and_inspect` は保存後に概要・画面遷移・クラッシュ・エラーを調べる。エラーは 4 版とも `arb_timeline_overview` と同じ `timeline.overview` の集約を `error_logs`（`logs`、`total_matched`、`unique_count`、`returned_count`、`has_more`）で返し、`package`／`tag`／`exclude_tags` と `max_errors`（概要の `max_error_logs` と同じ。1〜200、既定 20）を同じ意味・型・検証で受ける。同じバンドルと条件なら、代表行・件数・`noise_candidates`・`screens`・`crashes` は概要と一致する。`has_more` で、より多くのグループを返せるときだけ `continuation` に `arb_timeline_overview` の呼び出し（`bundle_id`、`max_error_logs`、渡した条件）を入れ、それ以外は null。上限 200 グループを返し終えた残りは `raw_query` のページ送りで読む。`raw_query` には生の E・F 行を先頭から読む `arb_query_logs` の呼び出し（`bundle_id`、`min_level: E`、`bucket: main`、渡した `package`／`tag`／`exclude_tags`）を常に入れる。`arb_query_logs` は概要と同じ規則で除外するので、`raw_query` の行はグループの元になった行と一致する。continuation は上限を広げて先頭から再取得する。`summary` は `timeline.summary` の応答をそのまま返す。保存に成功した後の照会失敗でも保存結果を保持する。`memo` を保存 RPC の `note` に渡して永続化し、複合結果にも添える。`failed_retryable` の場合はジョブ情報を保持する。CLI の `jobs` 系コマンドから再試行・出力先変更ができる。
3. `arb_query_logs` と CLI の `logs` は既定で主タイムライン（`bucket=main`）を検索する。既定 `limit` は **100**、内部の `bundle.query_logs`／`timeline.query_logs` は既定で `bucket=all`、`limit` は **10,000**。各々 1〜100,000。`total_matched` は条件全体、`returned_count` はこのページ。通常ページは `has_more` と `next_cursor` を返す。次回の `cursor` にそのまま渡す。古い `after_seq` と同時指定してはならない。末尾抽出で古い一致を落とした場合は `has_more` が真でも前方カーソルはない。`fields` は `compact`（`seq`、`timestamp_utc`、`offset_ms`、`frame_index`、`level`、`tag`、`message`、`package_name`）か `full`（デーモンの全項目）。既定は 2025-06-18 以降が `compact`、旧 2 版が `full`。compact は全行の `boot_id` が同じなら最上位の `boot_id` に 1 回だけ出し、混在時は最上位を null にして各行に残す。行・順序・件数・カーソルは `fields` で変わらない。100 行の実測は本文 25.8 KB（full は 64.5 KB）、テキストと `structuredContent` の二重送信を含む応答全体で 54 KB（full は 135 KB）。
4. `whole_word` は keyword／tag の両端を単語境界（英数字・アンダースコア以外、または文字列端）に制限する。`level` は大文字小文字を区別せず V/D/I/W/E/F、VERBOSE/DEBUG/INFO/WARN/WARNING/ERROR/FATAL/ASSERT。完全一致で検索する。排他の `min_level`（CLI は `--min-level`）は V/D/I/W/E/F の大文字 1 字を受理し、指定以上の行を選ぶ。CLI と MCP は同じ接続の `system.hello.log_min_level` が true のときに限り下限付き検索を送る。未対応なら -32601 と arbd の更新案内を返す。`exclude_tags`（CLI は `--exclude-tag` を繰り返す）は `timeline.overview` と同じく、指定したタグと完全一致（大小無視）する行を除く。省略時と空配列は除外しない。1 件以上の除外は `system.hello.log_exclude_tags` が true の接続だけに送り、未対応なら同じく -32601 を返す。空配列は旧 arbd にもそのまま送る。`start_ns`／`end_ns` は端末 boottime の ns で、バンドルの `epochs` と同じ軸（バンドル先頭からの相対値ではない）。未定義値は拒否。旧 2 版の MCP だけは A を F として受理する。
5. 各行の `timestamp_utc` はログヘッダ由来のミリ秒 UTC、`offset_ms` は同じ boot の映像先頭（なければ epoch 開始）からの差、`frame_index` はその時刻の表示フレーム。同期できない行の相対時刻・フレームは null。映像がない場合もフレームを捏造してはならない。画面区間は `start_ns`／`end_ns` に加えて、ログ行と同じその boot の原点（先頭フレームの表示開始、映像がなければ epoch 開始）からの `start_offset_ms`／`end_offset_ms` を返す。複数 boot のバンドルで区間の boot を先頭フレームから決められなければ null。フレーム調査の各フレームは、`arb_export_trim` の `in_time_ns`／`out_time_ns` にそのまま渡せるメディア時刻 `offset_ns` と `offset_ms` を返す（`timestamp_ms` は互換の同値）。`time_zone` は既定 `utc` で、応答はホストのタイムゾーンに依存しない。`host` のときだけ、数値オフセット付き RFC 3339 のローカル時刻（オフセットは分単位。秒単位の歴史的オフセットは 0 方向に切り捨て、同じ瞬間を表す）をログ行・代表行の `timestamp_local`、一覧の `created_local`、スクリーンショットの `captured_at_local` に添える（`arb_query_logs`、`arb_timeline_overview`、`arb_save_and_inspect`、`arb_list_bundles`、`arb_screenshot`）。公開引数の秒・ms・ns はそれぞれのフィールド名に従い、フレーム番号は 0 始まりである。
6. `arb_timeline_overview` のエラーは `package`／`tag`／`exclude_tags` を適用した E/F 主ログを優先度・タグ・本文で集約する。最新出現を代表とし、グループを最新側から上限まで選ぶ。`repeat_count`、`first_offset_ms`、`last_offset_ms` を返す。`total_error_logs` は生行、`unique_error_logs` はグループの総数、`error_logs_truncated` は省略グループの有無。全版の保存後調査もこの集約を使い、生行が必要なら `arb_query_logs` で読む。`noise_candidates` は除外の候補を示すだけで何も隠さない。条件適用後・表示上限前の E 行が 5 件以上で、10 件以上または本文の種類数の 2 倍以上あるタグを、件数の降順・タグの昇順で最大 5 件返す（`tag`、`total_count`、`unique_messages`、最頻の `sample_message`）。主ログに F 行があるタグとクラッシュ元の行のタグは含めず、`exclude_tags` で除いたタグも出さない。代表行・件数・クラッシュ・ログ検索の結果は推薦の有無で変わらない。除外は呼び出し側が `exclude_tags` で明示し、端末ごとの暗黙の既定除外は持たない（#574）。
7. 新規記録は一覧の `created_at_utc` もミリ秒 UTC。退避・復元の応答行も一覧と同じ集計を持つ。最新 MCP では保存結果・概要に同型の `logcat_completeness` を返す。保存結果の警告は `warning_details`（共通型 `WarningDetail`。必須の `code`・`message` と任意のオブジェクト `details`）が正本で、`warning_codes`・`warnings` は同じ順序・同じ長さの射影。旧デーモンの応答で `warning_details` がなければ MCP が同じ位置の組から補う。`log_coverage_ms`・`video_coverage_ms`・`carried_in_frame`・`static_hold_ms`・`info_codes` は [SPEC-02](SPEC-02-time-selection-bundle.md) の被覆と引き継ぎをそのまま返す。概要 RPC は状態名に加え `logcat_completeness_detail` を返す。状態・停止ツールの `audio_reason` は先頭の要約を最大 256 文字にする。`drain_timeout_ms` の有効範囲は 0〜5000 ms。

[検索・出力](../../core/crates/arbd/src/inspect_export.rs)・[CLI の入口](../../core/crates/arb-cli/src/main.rs)・[保存尺の定義](SPEC-02-time-selection-bundle.md)

開始・停止は変更した設定を `config_changed` の各キーに [変更前, 変更後] で返す。変更なしは空オブジェクト。開始の `serial_source` は引数・設定・自動選択をそれぞれ `argument`・`config`・`auto` とする。状態と録画対象の意味は [SPEC-07](SPEC-07-device-capture-audio-input.md) に従う。旧デーモンではこれらの追加項目を省略できる。

### Resources・Prompts・Tasks

| 機能 | 必須の挙動 |
| --- | --- |
| Resources 2 件 | `arb://status` と `arb://bundles` は JSON |
| Templates 5 件 | `arb://bundles/{bundle_id}/` 以下の `manifest`、`screens`、`crashes`、`logs`（先頭 100 件）、`frame/{frame_index}`（PNG） |
| スクリーンショット | `arb_screenshot` は `2025-06-18` と `2025-11-25` で `include_image=false` が既定（保存パスとメタデータ）。この 2 版は本文に `path`・`file_uri`・`resource_links` を返し、同じ参照を resource_link ブロックにも載せる。旧 2 版は `file_uri` を持たない。true なら PNG を同梱。旧 2 版はインライン返却が既定。保存先の衝突時は `overwrite` を受理する版（2025-06-18 まで）に限り overwrite も案内し、全版で別の output_path を案内する |
| 画像 | `arb_inspect_frames` の `include_images` は既定 false。メタデータと `resource_link` を返し、必要な画像だけ `resources/read` で取得。本文は true／false で同じ形（`screens` に画面を 1 回ずつ並べ、各フレームは `screen_index` で参照）。true は描画した PNG を content の image ブロックに足し、キャッシュの `png_path`・`png_bytes_len` とクロップ情報を返す。取得していない項目は null。スクリーンショットは保存した PNG の file URI を全版の resource_links に載せる。同一セッションの直近 256 件は resources/read で取得でき、保存時と SHA-256 が異なる場合は RESOURCE_NOT_FOUND を返す。対象端末がオフラインのときは DEVICE_NOT_FOUND で再接続を案内する。保存系 3 ツールとフレーム調査は全版の本文先頭で `resource_links`（uri、name、mimeType の配列）を返す。2025-06-18 以降は同じ参照を resource_link ブロックにも載せる |
| Prompts | `investigate_crash`、`triage_recent_bug`、`safe_cleanup_review`。`completion/complete` は参照・引数を検証し、バンドル ID の前方一致を最大 100 件返す |
| 進捗・ログ | 進捗トークンがあれば `notifications/progress` はそのトークンと増加する値を返す。`logging/setLevel` は RFC 5424 の debug／info／notice／warning／error／critical／alert／emergency を受け付ける。しきい値以上を `notifications/message` で送る。失敗は error、トークンのない詳細な段階通知は debug |
| Tasks | 2025-11-25 の任意機能。開始・保存・保存して調査・トリム／共有出力が対応。`tasks/get`、`tasks/list`、`tasks/result`、`tasks/cancel`。セッションあたり上限 64、保持期限は作成時から既定 60 秒、指定時 1〜3,600,000 ms。実行中は期限だけで消さない |
| 待機・キャンセル | 結果待ちとキャンセルを同じ stdio で処理できる。キャンセル後の新たな処理を止める。既にデーモンが受理した変更は完了し得る。Task 非対応ツールへの `task` 指定は `-32602`。`tasks/get`・`tasks/result`・`tasks/cancel` の `taskId` は欠落／文字列以外／未知または失効を別々の理由付き `-32602` で返す。終了済みタスクの再キャンセルも `-32602` |

[リソースとプロンプト](../../core/crates/arb-cli/src/mcp/resources.rs)・[タスク管理](../../core/crates/arb-cli/src/mcp/tasks.rs)・[MCP Tasks 原仕様](https://modelcontextprotocol.io/specification/2025-11-25/basic/utilities/tasks)

## CLI との一致・仕様 vs バグ

| 操作 | 正常仕様 | バグ |
| --- | --- | --- |
| 同じ条件で MCP と CLI のログを検索 | 同じ 100 件の既定と継続条件。CLI は `--limit`／`--cursor`／`--whole-word` を提供 | 同一 seq をページ境界で落とす、不正な level を無条件扱いする |
| PNG の自動命名 | `arb_export_frame_png(output_dir)` と `png --out-dir` は同じ保存先選択で一意名を付ける | 既存ファイルを置き換える、両方の指定を黙って片方に寄せる |
| GC プレビュー | `arb_gc_bundles(dry_run: true)` と `bundles gc --dry-run` は能力確認後に候補と保護理由を返す | 旧デーモンが未知引数を無視して削除し得る状態で要求を送る |
| 保存の再試行待ち | CLI は終了値 3、MCP は `isError: true` とジョブ情報 | JSON-RPC の通信成功だけで保存完了とする |
| 端末が見つからず待機期限切れ | 開始意図と未接続を別々に通知 | 開始成功だけを根拠にフレーム取得済みとする |
| Tasks のキャンセル | 以後の段階を停止、発行済み成果物は残り得る | 自動ロールバック済みと断定する |

根拠は Trail #101・#133／PR #538、#151／PR #554、#153／PR #556、TCB-19／21。[実 stdio 契約試験](../../core/crates/arb-cli/tests/stdio_contract)・[エラー・保存結果表](ui-feedback-contract-table.json)
