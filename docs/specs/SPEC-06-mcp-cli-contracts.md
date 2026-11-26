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

公開ツールの `inputSchema`、成功型・エラー型から組み立てる `outputSchema`、`ToolAnnotations` は同じツール表に従う。`tools/call` の本体は `structuredContent` と後方互換の JSON テキストで同じ値を伝える。追加の画像・リンクは別の content ブロック。`isError`、`error_code`、`rpc_code`、`message` を用いて操作失敗を報告する。引数検証やメソッド未定義のプロトコル応答と、実行済みツールの失敗を区別する。ヒント（読み取り専用・破壊性・冪等性・外部作用）は各ツールの表の値を使う。[正本 JSON](mcp-tools-table.json)・[版別スキーマ生成](../../core/crates/arb-cli/src/mcp/contract.rs)・[MCP Tools 原仕様](https://modelcontextprotocol.io/specification/2025-11-25/server/tools)

| 用途 | `tools/list` の公開名（20 件） |
| --- | --- |
| 状態・端末・設定 | `arb_status`、`arb_devices`、`arb_config_get`、`arb_config_update` |
| キャプチャ制御 | `arb_capture_start`、`arb_capture_stop` |
| 保存・調査開始 | `arb_save`、`arb_save_logs`、`arb_screenshot`、`arb_save_and_inspect` |
| ライブラリ | `arb_list_bundles`、`arb_trash_bundle`、`arb_restore_bundle`、`arb_gc_bundles`、`arb_forget_bundle` |
| 保存物の調査 | `arb_timeline_overview`、`arb_query_logs`、`arb_inspect_frames` |
| 書き出し | `arb_export_trim`、`arb_export_frame_png` |

`2025-11-25` では `arb_export_trim` の `trash_original: true`、`arb_screenshot` の `overwrite: true` を許可しない。以前の 3 版はこれらを受理し破壊性ヒントを立てる。`arb_export_trim` は `share_mp4` と `bundle` の形式を区別する。明示形式の呼び出し別名は公開 20 件に加算しない。[stdio 実装](../../core/crates/arb-cli/src/mcp.rs)

## 証跡を保存し調査する

1. `arb_capture_start` の `wait_connected` は既定 false。true なら `wait_timeout_ms`（既定 5,000、最大 30,000 ms）まで今回の接続を待つ。複数候補からの選択待ちは速やかに返る。タイムアウト後も開始意図は保持する。`connected` と `wait_timed_out` で結果を読む。
2. `arb_save_and_inspect` は保存後に概要・画面遷移・クラッシュ・最新の ERROR ログを調べる。保存に成功した後の照会失敗でも保存結果を保持する。基準版の `memo` はこの複合結果に添える。`failed_retryable` の場合はジョブ情報を保持する。CLI の `jobs` 系コマンドから再試行・出力先変更ができる。
3. `arb_query_logs` と CLI の `logs` の既定 `limit` は **100**、内部の `bundle.query_logs`／`timeline.query_logs` の既定は **10,000**。各々 1〜100,000。`total_matched` は条件全体、`returned_count` はこのページ。通常ページは `has_more` と `next_cursor` を返す。次回の `cursor` にそのまま渡す。古い `after_seq` と同時指定してはならない。末尾抽出で古い一致を落とした場合は `has_more` が真でも前方カーソルはない。
4. `whole_word` は keyword／tag の両端を単語境界（英数字・アンダースコア以外、または文字列端）に制限する。`level` は大文字小文字を区別せず V/D/I/W/E/F、VERBOSE/DEBUG/INFO/WARN/WARNING/ERROR/FATAL/ASSERT。未定義値は拒否。旧 2 版の MCP だけは A を F として受理する。
5. 各行の `timestamp_utc` はログヘッダ由来、`offset_ms` は同じ boot の映像先頭（なければ epoch 開始）からの差、`frame_index` はその時刻の表示フレーム。同期できない行の相対時刻・フレームは null。映像がない場合もフレームを捏造してはならない。公開引数の秒・ms・ns はそれぞれのフィールド名に従い、フレーム番号は 0 始まりである。

[検索・出力](../../core/crates/arbd/src/inspect_export.rs)・[CLI の入口](../../core/crates/arb-cli/src/main.rs)・[保存尺の定義](SPEC-02-time-selection-bundle.md)

### Resources・Prompts・Tasks

| 機能 | 必須の挙動 |
| --- | --- |
| Resources 2 件 | `arb://status` と `arb://bundles` は JSON |
| Templates 5 件 | `arb://bundles/{bundle_id}/` 以下の `manifest`、`screens`、`crashes`、`logs`（先頭 100 件）、`frame/{frame_index}`（PNG） |
| 画像 | `arb_inspect_frames` の `include_images` は既定 false。メタデータと `resource_link` を返し、必要な画像だけ `resources/read` で取得。明示 true はインライン画像。2025-06-18 より古い版には URI をテキストで渡す |
| Prompts | `investigate_crash`、`triage_recent_bug`、`safe_cleanup_review`。`completion/complete` は参照・引数を検証し、バンドル ID の前方一致を最大 100 件返す |
| 進捗・ログ | 進捗トークンがあれば `notifications/progress` はそのトークンと増加する値を返す。`logging/setLevel` は RFC 5424 の debug／info／notice／warning／error／critical／alert／emergency を受け付ける。しきい値以上を `notifications/message` で送る |
| Tasks | 2025-11-25 の任意機能。開始・保存・保存して調査・トリム／共有出力が対応。`tasks/get`、`tasks/list`、`tasks/result`、`tasks/cancel`。セッションあたり上限 64、保持期限は作成時から既定 60 秒、指定時 1〜3,600,000 ms。実行中は期限だけで消さない |
| 待機・キャンセル | 結果待ちとキャンセルを同じ stdio で処理できる。キャンセル後の新たな処理を止める。既にデーモンが受理した変更は完了し得る。終了済みの再キャンセル・未知／失効 ID は `-32602` |

[リソースとプロンプト](../../core/crates/arb-cli/src/mcp/resources.rs)・[タスク管理](../../core/crates/arb-cli/src/mcp/tasks.rs)・[MCP Tasks 原仕様](https://modelcontextprotocol.io/specification/2025-11-25/basic/utilities/tasks)

## CLI との一致・仕様 vs バグ

| 操作 | 正常仕様 | バグ |
| --- | --- | --- |
| 同じ条件で MCP と CLI のログを検索 | 同じ 100 件の既定と継続条件。CLI は `--limit`／`--cursor`／`--whole-word` を提供 | 同一 seq をページ境界で落とす、不正な level を無条件扱いする |
| GC プレビュー | `arb_gc_bundles(dry_run: true)` と `bundles gc --dry-run` は能力確認後に候補と保護理由を返す | 旧デーモンが未知引数を無視して削除し得る状態で要求を送る |
| 保存の再試行待ち | CLI は終了値 3、MCP は `isError: true` とジョブ情報 | JSON-RPC の通信成功だけで保存完了とする |
| 端末が見つからず待機期限切れ | 開始意図と未接続を別々に通知 | 開始成功だけを根拠にフレーム取得済みとする |
| Tasks のキャンセル | 以後の段階を停止、発行済み成果物は残り得る | 自動ロールバック済みと断定する |

[既知差分 / PR #557] 共有動画の副生成物、ログ既定バケットと VFR 対応、UTC 精度、保存不足の区分・警告コード、メモの永続化、概要の絞り込み、操作後の一覧集計は Trail #154 の統合後に wire 表と再照合する。

根拠は Trail #101・#133／PR #538、#151／PR #554、#153／PR #556、TCB-19／21。[実 stdio 契約試験](../../core/crates/arb-cli/tests/stdio_contract)・[エラー・保存結果表](ui-feedback-contract-table.json)
