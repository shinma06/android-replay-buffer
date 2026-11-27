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

隔離の既定は PC 受信窓の前後 2 秒、50,000 件、16 MiB。境界ドレインは有効なストリームの水位到達または既定 500 ms まで。`drain_timeout_ms` は整数 0〜5000 ms（両端を含む）。範囲外は RPC・MCP とも `-32602`。末尾の到達を裏付けられない場合は `tail_may_be_incomplete` を立てる。窓より前の時刻は表示しない。窓外の時計アンカーや IDR は時刻変換・復号だけに使う。例外は次節の引き継ぎフレームで、窓先頭 `W0` から表示が始まるので、この原則を守る。[選別・ドレイン](../../core/crates/arb-core/src/selection.rs)・[保存経路](../../core/crates/arbd/src/capture.rs)

管理域（データディレクトリ配下の bundles）外への保存（dest_dir、output_dir_override）は unmanaged で arb_gc_bundles の物理削除対象外です。実体を手動で移動・削除し、元の保存先が missing になったら arb_forget_bundle で登録を解除します。

### ストリーム被覆台帳と引き継ぎフレーム

フレームやログ行の出現幅（最後−最初）は、ストリームが窓を被覆した時間ではない。静止画面は映像フレームを送らず、静かなアプリはログ行を書かない。`arbd` は端末ごとに映像と logcat の連続被覆区間を [`arb-core::coverage`](../../core/crates/arb-core/src/coverage.rs) で記録する。時間軸はホストの受信時刻（`PcContinuousMono`）。

| ストリーム | 区間を開く・延ばす | 閉じる |
| --- | --- | --- |
| 映像 | フレームで開始・延長。端末ビーコンは直前の証拠から `beacon_timeout`（既定 1.5 秒）以内に届く間だけ、開いている区間を静止として延長する。ビーコンだけでは開かない | 映像の `stream_gap`、`ConnectionLost`、`Shutdown`、記録容量不足、boot の変化、`VideoSession` の再生成・解像度変更、`Hello`、停止、新しい取得世代、ビーコン途絶 |
| logcat | ログ行、または logcat が `Running` と報告するビーコンで開始・延長 | logcat の `stream_gap`・停止・エラー・再起動、上と同じ接続断・boot 変化・停止・世代交代、ビーコン途絶 |

古い世代の通知は新しい世代の被覆を延ばさない。`logcat_overflow`、`dropped_count`、`tail_may_be_incomplete` は被覆とは別に完全性として報告する。保存時は pin と同時に、窓に交わる被覆区間をジョブパラメータへ永続化し、ドレイン後に更新する。再試行・出力先変更・起動回復は同じ記録から同じ判定を再現する。記録のない旧ジョブは従来の行・フレームの幅で判定し、引き継ぎをしない。

窓先頭 `W0` で表示中だったフレーム `f_prev` は、次の条件をすべて満たすときだけ、`W0` から表示する先頭フレームとして 1 枚保存する（K14）。

1. `f_prev` は `W0` より前で最も遅いフレーム。窓内にフレームがあれば、その直前のフレームである。
2. 同じ boot、同じ映像セッション、同じ寸法。
3. `f_prev` の受信から `W0` までが 1 本の映像被覆区間に含まれる。切断・ギャップ・未開始なら引き継がない。
4. 直前の IDR から `f_prev` までが同じ boot・寸法で pin 済み（既存の遡りで満たす）。
5. 同じ boot の後続フレーム（受信順は撮影順と一致しない）がすべて `W0` の 1 tick 以上後に撮影されている。`W0` 前に撮影され `W0` 後に届いたフレームや、ちょうど `W0` に撮影されたフレームがあれば `f_prev` は `W0` の画面ではないので引き継がない。

保存は既存の preroll 経路で `[IDR..f_prev)` を復号準備に使い、`f_prev` を IDR として再符号化する。`timeline.sqlite` の `f_prev` の撮影時刻は書き換えない。manifest の `video.carried_in` に `frame_index: 0`、`captured_boottime_ns`、`display_start_boottime_ns: W0`、`evidence: "coverage"` を記録する。最後のフレームは `min(T, 映像被覆の終端)` まで表示を保持し、MP4 の最終 `stts` に実時間長を書く。`edts`／`elst` は 0 個で単一 `avc1` のまま。30 秒静止して 30 秒保存すると 1 枚で約 30,000 ms の映像になり、20 秒で接続が切れれば 20 秒で打ち切って `video_shortfall` を立てる。概要と保存後調査の `carried_in` はこの記録をそのまま返し、引き継ぎがないときは null。旧形式（schema 3/4）と `carried_in` のないバンドルの読取は変わらない。

### 孤立した先頭フレームと映像コミットの整合

リングが古いチャンクを回収すると、窓の先頭が IDR を失った P フレームで始まることがある。復号準備用の preroll も引き継ぎフレームもなく、先頭から復号できない場合、保存は次のように映像を確定する（K15）。

1. 窓内で最初のキーフレーム（IDR）より前の孤立した P フレームを破棄し、そこから先を `video.mp4` に書く。番号は 0 から振り直し、0 番は IDR になる。`streams.video.status` は先頭が欠けたことを示す `partial`。破棄した区間は映像の `stream_gap` として記録し、`video_coverage_ms` は書いた先頭フレームより前を数えない。`actual_video_duration_ms` は書いた MP4 の再生尺で、不足すれば `video_shortfall` が立つ。
2. 窓内に復号できる IDR がなければ映像を書かない。`status: absent`、`frame_count: 0`、`frames` 0 行、`video.mp4` なし、`actual_video_duration_ms`・`video_coverage_ms` は 0、`video_shortfall` は true。映像ツールは整合性エラーではなく `NO_VIDEO` を返す。
3. `timeline.sqlite` の `frames`、`streams.video.frame_count`、MP4 のサンプル数は常に一致する。ログ・タップの表示フレーム、画面区間、音声との原点（`audio_lead_in_offset_ns`）は書いたフレーム列から求める。

確定前に `arb-storage` のバンドル書き込みが映像の宣言を検査する。`status` が `absent`・`excluded` 以外、または `frame_count > 0` なら、0 バイトより大きい `video.mp4` と `frame_count` と同数の `frames` 行が必要。`absent` なら `frame_count`・`frames` 行は 0 で `video.mp4` はない。違反したバンドルは公開せず、保存は再試行可能な失敗になる。[判定](../../core/crates/arb-core/src/video_commit.rs)・[MP4 化](../../core/crates/arbd/src/capture.rs)・[確定前検査](../../core/crates/arb-storage/src/bundle_io.rs)

### 要求尺と測定結果

| 結果 | 読み方 |
| --- | --- |
| `requested_duration_secs` | 利用者の要求秒数。実際の録画尺とは独立 |
| `actual_video_duration_ms` | MP4 の最終サンプルの表示時間まで含む再生尺。測定済みで映像なしなら 0 |
| `actual_log_duration_ms` | 同一 boot 内の確定ログの最小・最大時刻の差。0〜1 件なら 0。隔離ログと時計アンカーを含めない。行の出現幅であり、被覆時間ではない |
| `log_coverage_ms` | 保存窓のうち logcat ストリームが動作していた時間（窓 ∩ logcat 被覆区間）。ギャップ・切断・未開始の区間を含めない。被覆の記録がない旧ジョブでは省略 |
| `video_coverage_ms`・`carried_in_frame`・`static_hold_ms`・`info_codes` | 映像の被覆時間（書いた先頭フレームより前と、映像を書かなかった保存は数えない）、引き継ぎフレームの有無、窓の両端で同じフレームを保持した時間。静止の保持は警告ではなく `info_codes` の `video_static_hold` で示す（ログ単体では省略） |
| `window_shortfall` | `video_shortfall`（映像保存時のみ）と `log_shortfall`（両モード）の論理和。映像は MP4 の再生尺、ログは `log_coverage_ms`（記録がない旧ジョブは `actual_log_duration_ms`）で、それぞれ `actual_ms + 1000 < requested_secs × 1000` のとき true。ちょうど 1 秒不足したストリームのフラグは false |
| `video_frame_count`・`log_count`・`unanchored_log_count` | 映像枚数、隔離分を含むログ総数、そのうち隔離ログ数。未測定と確認済みの 0 を区別する |
| `truncated_unanchored_count`・`discontinuity_count`・`warnings` | 全ストリームの隔離切り捨て、不連続の数、短尺・音声なし・末尾未確認などの警告。空の警告だけでは完全取得を保証しない |

警告の正本は `warning_details`（`code` と `message` の組の配列）。`warning_codes`（機械判定用）と `warnings`（表示文）は、これを同じ順序で射影した同じ長さの配列で、`window_shortfall` にも文がある。順序は `video_shortfall`、`log_shortfall`、`window_shortfall`、`logcat_overflow`、`unanchored_truncated`、`audio_absent`、`stream_discontinuity`、`tail_unconfirmed`。`logcat_completeness` は `status`、`dropped_count`、`overflow_events`、`unanchored_count`、`unanchored_truncated_count` を分ける。破損チャンク等で窓内のログに欠損の可能性があれば既知の 0 を撤回し `dropped_count=null` とする。端末の送信キューで失われたフレーム数も失われたログ行数へ換算できない。完全性を裏付けられなければ `unverified`、溢れを観測した場合の `overflow_detected` は件数不明でも保持する。[端末統計のワイヤ](../../device-agent/PROTOCOL.md)・[保存時の完全性判定](../../core/crates/arbd/src/capture.rs)

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
| 保存窓の先頭に非 IDR がある | 復号準備用データだけ窓外から参照 | 保存映像を再生すると窓より前の時刻が表示される |
| 先頭の IDR がリングから回収済みで窓が P フレームから始まる | 最初の IDR より前を破棄し、残りを `partial` で `video.mp4` に書く。IDR がなければ `absent`・0 枚 | `video.mp4` なしで `partial`・`frame_count > 0` を確定し、映像ツールが整合性エラーになる |
| 30 秒静止した画面を 30 秒保存 | 被覆が続いていれば引き継ぎフレーム 1 枚を約 30 秒保持。`video_shortfall=false`、`carried_in_frame=true` | 映像 0 枚、または末尾が最後の画面変化から約 33 ms で終わる |
| 最後のフレームの後に切断・ギャップ | 引き継がず、または被覆の終端で打ち切り、`video_shortfall=true` | 切断中も同じ画が表示され続ける映像を作る |
| 健全な 30 秒保存で両端 0.5 秒に行がない | `log_coverage_ms≈30000` で `log_shortfall=false`。`actual_log_duration_ms` は 28,999 のまま | 行の幅だけで `log_shortfall=true` にする |
| 警告の配列 | `warning_codes`・`warnings`・`warning_details` が同じ長さ・順序 | `window_shortfall` だけ表示文がない |

根拠は Trail #96・#112・#119・#132／PR #537・#136／PR #540・#151／PR #554、Issue #564・#570、Round 3B の 600 秒保存（W3B）。K1〜K5・K14・K15、S2・S3、G1〜G5、TCB-03／04／07／13／15。[TCB 台帳](../proofs/tcb-axioms.md)
