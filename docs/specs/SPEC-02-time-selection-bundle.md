---
id: SPEC-02
type: spec
name: 実時間窓・証跡の選別・保存物の保護
satisfies:
  - REQ-01
  - REQ-02
  - REQ-03
  - REQ-04
  - REQ-07
  - REQ-08
  - REQ-10
  - REQ-12
---

# SPEC-02 実時間窓・証跡の選別・保存物の保護

## どの時刻を使うか [実装済み]

基準は同一 `boot_id` 内の端末起動後の経過時間（boottime、ns）。生の端末時刻、時計の種類と意味、PC 受信時刻は保存する。ホストの活動時計はスリープ中に止まり、連続時計は進む。壁時計は表示用。macOS は活動に `CLOCK_UPTIME_RAW`、連続に `CLOCK_MONOTONIC_RAW` を使う。PC と端末の同期プローブは往復時間の半分を切り上げた誤差半幅を与える。時間差を測れないときは確度を下げる（MUST）。

端末の boottime・monotonic・realtime の組で epoch を決める。boot の変化は再起動、boottime と monotonic の差の増加が既定 50 ms **超**なら端末スリープ、realtime と boottime の差の変化の絶対値が 50 ms **超**なら壁時計ジャンプ。時計の跳躍をまたいで一直線に補間してはならない。A/V とログの約 0.1 秒は実用目標。未校正・不確かなずれは表示する。[時刻変換](../../core/crates/arb-core/src/time.rs)・[REQ-12](../requirements/REQ-12.md)

## 保存窓と三値選別 [実装済み]

「直近 D 秒」はトリガー T で終わる閉区間 `[T−D, T]`。停止・スリープ中にも窓は進む。同じ serial **かつ**同じ boot_id で再開した場合に限り、保持予算と保存窓に残る過去の記録を引き継ぐ。停止期間は `stream_gap` として残す。別端末・再起動前の映像とログを現セッションの確定データとして接続してはならない。

| 選別入力 | 主タイムライン | 不確実な記録の扱い |
| --- | --- | --- |
| 同じ boot の `Anchored` | 共通分類器では中心 ± 誤差半幅が窓と交差すれば `InRange`。ライブ保存の確定ログ・映像にはさらに中心時刻が窓内という制限がある | 半幅だけが交差する窓外の確定ログや映像を出力に足さない |
| 同じ boot の `Bounded` | 区間全体が窓内なら採用、完全に外なら除外 | 部分交差は `UncertainTime` |
| `Unanchored` または boot 不一致 | 確定時刻として採用しない | 共通分類は不確実。受信時刻による近傍条件に従い、上限付きサイドカーに隔離する |
| 実時間トリム | バンドル先頭からの整数 ns の半開区間 `[IN, OUT)`。表示中の先頭・末尾フレームから同一 boot のデバイス時刻へ写し、閉区間の選別器には最後の対象 ns（OUT − 1）を渡す | 境界をまたぐ同期済みスワイプは端点を補間。主ログ・入力の `relative_time_ns` は選択先頭基準とし、元の時計情報と親の選択区間も保存する |
| フレーム指定トリム | IN・OUT とも含む。対応する映像フレームの採否はインデックスで決める | 既存の隔離記録を優先して引き継ぎ、上限超過を計上する |

隔離の既定は PC 受信窓の前後 2 秒、50,000 件、16 MiB。境界ドレインは有効なストリームの水位到達または既定 500 ms まで。`drain_timeout_ms` は整数 0〜5000 ms（両端を含む）。範囲外は RPC・MCP とも `-32602`。末尾の到達を裏付けられない場合は `tail_may_be_incomplete` を立てる。窓外の時計アンカーや IDR は時刻変換・復号に使えるが、表示フレームの余白は 0 枚。[選別・ドレイン](../../core/crates/arb-core/src/selection.rs)・[保存経路](../../core/crates/arbd/src/capture.rs)

### 要求尺と測定結果

| 結果 | 読み方 |
| --- | --- |
| `requested_duration_secs` | 利用者の要求秒数。実際の録画尺とは独立 |
| `actual_video_duration_ms` | MP4 の最終サンプルの表示時間まで含む再生尺。測定済みで映像なしなら 0 |
| `actual_log_duration_ms` | 同一 boot 内の確定ログの最小・最大時刻の差。0〜1 件なら 0。隔離ログと時計アンカーを含めない |
| `window_shortfall` | `video_shortfall`（映像保存時のみ）と `log_shortfall`（両モード）の論理和。各尺は独立に `actual_ms + 1000 < requested_secs × 1000` のとき true。ちょうど 1 秒不足したストリームのフラグは false |
| `video_frame_count`・`log_count`・`unanchored_log_count` | 映像枚数、隔離分を含むログ総数、そのうち隔離ログ数。未測定と確認済みの 0 を区別する |
| `truncated_unanchored_count`・`discontinuity_count`・`warnings` | 全ストリームの隔離切り捨て、不連続の数、短尺・音声なし・末尾未確認などの警告。空の警告だけでは完全取得を保証しない |

`warning_codes` は機械判定用。`video_shortfall`、`log_shortfall`、`window_shortfall`、`logcat_overflow`、`unanchored_truncated`、`audio_absent`、`stream_discontinuity`、`tail_unconfirmed` を該当時に返す。`warnings` は表示文。`logcat_completeness` は `status`、`dropped_count`、`overflow_events`、`unanchored_count`、`unanchored_truncated_count` を分ける。破損チャンク等で窓内のログに欠損の可能性があれば既知の 0 を撤回し `dropped_count=null` とする。端末の送信キューで失われたフレーム数も失われたログ行数へ換算できない。完全性を裏付けられなければ `unverified`、溢れを観測した場合の `overflow_detected` は件数不明でも保持する。[端末統計のワイヤ](../../device-agent/PROTOCOL.md)・[保存時の完全性判定](../../core/crates/arbd/src/capture.rs)

未発行や旧デーモンで測定値が省略され得る。発行後のカタログ登録に失敗して測定結果が返っても保存成功の判定は別に行う。一覧の `duration_ms` は映像があれば映像尺、なければ確定ログ尺。欠損時はカタログに残る集計を使う。新規保存の `created_at_utc` はミリ秒 UTC、RPC の `created_utc` は互換用の別名。`memo` は保存時のメモで未指定なら null。[保存結果の型](../../core/crates/arbd/src/protocol.rs)・[共有契約表](ui-feedback-contract-table.json)

## リング・二相保存・回復 [実装済み]

1. リングは既定 10 分・1.5 GiB。退避対象は古い未 pin の閉じたチャンク。空き容量が既定 3 GiB 未満のときは新規取得を安全停止する。確定バンドルや再試行用 pin を空き確保に流用してはならない（REQ-04）。
2. 保存時は対象チャンクの pin、ジョブ ID、出力先・保存窓・トリガーなど再構成に必要な情報を永続化する。`pinned` → `draining` → `materializing` と進む。
3. 同じファイルシステムのステージングに動画・ログ・読み取り専用の `timeline.sqlite` 等を書き、SHA-256 を含む `manifest.json` を確定して原子的に公開する。確定後の内容は WORM（一度書き込んだ内容を変更しない）。編集状態・リース・論理削除は外部の `catalog.sqlite` に保存する。
4. 正常完了は `completed`。`failed_retryable` は pin を維持し、`save.retry` で再試行、`save.retarget` で出力先を変えて再試行できる。明示破棄は `save.discard`。起動回復は残存 pin とジョブを照合し、所有を確認できた未確定作業領域を回収する。別のディレクトリを名前の類似だけで消してはならない。

[状態遷移](../../core/crates/arb-core/src/state.rs)・[リング](../../core/crates/arb-storage/src/ring.rs)・[バンドル公開](../../core/crates/arb-storage/src/bundle_io.rs)・[VER-05](../verifications/VER-05-arb-storage.md)

## ゴミ箱・物理削除・登録解除 [実装済み]

`bundle.trash` は論理退避、`bundle.restore` は復元。`bundle.gc` は最終退避から `gc_grace_secs`（既定 86,400 秒）以上経過した候補を再評価する。「ゴミ箱を空にする」の `ignore_grace: true` は猶予だけを省く。現存する実体の物理削除には管理域内・現行形式・退避済み・有効リースなし・対象ジョブなし・マニフェスト同一性の全条件が必要。削除前と退避名への移動後にも同一性を確認し、別物なら停止して復元可能な状態を保つ。起動回復の削除失敗は `system.status` の回復結果で識別できる。

| `disk_state` | 判定条件 | 許される操作 |
| --- | --- | --- |
| `present` | 登録先がディレクトリとして存在（リンク先がディレクトリの場合を含む） | 通常の閲覧・状態遷移。存在判定だけで物理削除を許可しない |
| `missing` | 親に到達でき対象が存在しない。親ごと移動されても、欠けた構成要素の上に到達可能な管理ディレクトリがあれば該当 | `bundle.forget` は有効リース・対象ジョブが 0 の行だけを解除。管理域外・旧形式も対象。ディスクは変更しない |
| `storage_unavailable` | 外付け媒体の切断等で親の可用性を確認できない、権限不足、通常ファイルへの置換など | 一時不在を恒久削除とみなさずカタログ行を保持 |

リースは所有 PID が終了するか、最後の更新から 600 秒以上経過すると失効する。削除前にマニフェスト ID・作成時刻・親 ID をカタログと照合する。公開パスが別の実体に置換された場合はその実体を保持する。

一覧は読み取り専用。ゴミ箱の `missing` は保護・猶予条件を満たせば GC がカタログ行だけを掃除する。`dry_run: true` は候補・スキップ理由・推定バイト・期限切れリースを返し、行・実体・リースを書き換えない。`preview.candidate_bundle_ids` と `counts` はその時点の予測で、実行は再判定する。CLI／MCP は同じ接続で `system.hello` の `gc_dry_run` を確認するまでプレビュー要求を送らない。[GC とディスク判定](../../core/crates/arb-storage/src/catalog.rs)

旧保存形式 schema 3 は `session.json`、schema 4 は `replay.json`（ログ単体を含む）から開く。閲覧・再生・新規トリム出力で元フォルダを移行・上書き・物理削除してはならない。元を退避する指定を旧形式に付けた書き出しは拒否する。[旧形式フィクスチャ](../../fixtures)・[REQ-07](../requirements/REQ-07.md)

## 仕様 vs バグ

| 条件 | 正常仕様 | バグ |
| --- | --- | --- |
| 同じ端末・同じ boot、停止前も窓・予算内 | 再開後の保存に停止前後と間のギャップが残る | 停止ボタンだけでリングを消す |
| 同名端末が再起動 | 再起動前後を分離 | 別 boot の映像を連結する |
| 出力先の容量不足 | 再試行待ちと pin を保持 | 失敗を成功と通知する、または古い WORM を消す |
| 退避直後の通常 GC | 既定 24 時間保護 | 直後に物理削除する |
| 管理域外のドライブが外れた | 一時利用不能として保持 | `--all-missing` で登録を消す |
| 保存窓の先頭に非 IDR がある | 復号準備用データだけ窓外から参照 | 保存映像を再生すると窓より前のフレームが現れる |

根拠は Trail #96・#112・#119・#132／PR #537・#136／PR #540・#151／PR #554。K1〜K5、S2・S3、G1・G2・G4・G5、TCB-03／04／07／13／15。[TCB 台帳](../proofs/tcb-axioms.md)
