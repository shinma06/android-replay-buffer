---
id: SPEC-05
type: spec
name: Android Studio の録画操作・通知・Studio ビューア
satisfies:
  - REQ-01
  - REQ-03
  - REQ-04
  - REQ-08
  - REQ-09
  - REQ-11
  - REQ-12
  - REQ-13
---

# SPEC-05 Android Studio の録画操作・通知・Studio ビューア

## コックピットと設定 [実装済み]

Tool Window は録画状態・接続端末・保存アクションと、保存物／ゴミ箱のライブラリを表示する。UI は Compose/Jewel と IDE のアイコンを使う。絵文字を操作ラベルにせず、本番画面に Swing ウィジェット／Swing UI DSL を追加しない（KON-3）。アプリケーションレベルのデーモン接続を共有し、描画やアクションの更新中に同期 RPC を実行しない。[Tool Window](../../ide-plugin/src/main/kotlin/io/github/shinma06/arb/ui/ArbToolWindowContent.kt)・[構造契約](SPEC-01-architecture.md)

| 取得意図と実状態 | 期待表示・操作 |
| --- | --- |
| 録画意図 OFF、取得停止 | OFF。明示開始で `capture_enabled` を有効化 |
| 有効、端末・選択・接続待ち | ON・待機中。未接続端末への録画成功を表示しない。候補が複数なら利用者が選ぶ |
| 有効、接続して取得中 | ON・REC。ログ単体モードではその旨を併記。停止で常駐アームも解除 |
| デーモン切断・容量停止 | 原因を優先表示する。録画意図を現在の録画成功と取り違えない |

端末の可否と音声の「非対応」は共有表で決まる。録画中は今回の接続で実際に使う方式を表示し、設定と異なる場合は区別する。保存失敗時にはジョブの再試行・保存先変更を可能にする。[コントローラー](../../ide-plugin/src/main/kotlin/io/github/shinma06/arb/ui/ArbToolWindowController.kt)・[端末と音声](SPEC-07-device-capture-audio-input.md)

**アラートバナー。** `ArbSoftBanner` の左アクセント線は折返し・展開後も本文高さに追従する。音声の理由が短ければそのまま表示し、長文・scrcpy stderr は要約を表示する。「詳細を表示」で受信した原文を展開し、「詳細をコピー」で原文を取得する。同じバナー内容をポーリングで受信しても展開を維持する。原因そのものが変わった場合は新しいバナーとして扱える。[共通バナー](../../ide-plugin/src/main/kotlin/io/github/shinma06/arb/ui/ArbJewelTheme.kt)・[音声詳細の表示](../../ide-plugin/src/main/kotlin/io/github/shinma06/arb/ui/ArbToolWindowContent.kt)

**Running Devices。** Android プラグインが提供する `StreamingToolbarSecondary` に開始／停止、直前の保存、スクリーンショットを登録する。組み込みエミュレータと物理端末ミラーの両パネルが対象。スクリーンショット保存先は Logcat 設定の継承が初期 ON。解決できない環境では ARB の保存先へフォールバックする。継承を OFF にして別のフォルダを指定できる。[登録先](../../ide-plugin/src/main/resources/META-INF/arb-android.xml)・[保存先の解決](../../ide-plugin/src/main/kotlin/io/github/shinma06/arb/ide/ArbIdeHelpers.kt)・[VER-27](../verifications/VER-27-t6i-running-devices-and-burn-taps.md)

## 保存通知とカタログ [実装済み]

`ArbNotifications` から操作結果を通知する。バンドル保存では「ビューアで開く」（既に開いた場合を除く）、「フォルダを開く」「パスをコピー」（パス取得時）、「ゴミ箱へ」を提供。退避後の通知は「元に戻す」1 クリックで復元する。画像は「画像を開く」。書き出し後のアクションはバンドル ID・出力パス・元の退避結果に従う。

`savedBundleId` が返す成功 ID で通知を分ける。`failed_retryable` は応答に ID があっても成功ではない。GC・forget は構造化された全非空結果区分と各件数を表示する。媒体切断は「見つからない」と区別する。業務エラー時は操作の理由を表示し、古いデーモンは ER-07 に従う。[通知実装](../../ide-plugin/src/main/kotlin/io/github/shinma06/arb/ide/ArbNotifications.kt)・[操作結果の正本](ui-feedback-contract-table.json)

## 再生・編集・書き出し [実装済み]

- Studio の時計は単調時計からメディア時刻を計算する。静止 VFR サンプルの保持中も時刻は進む。倍率は 0.25／0.5／1／2 倍。時間シークは保持区間内の時刻を維持し、コマ送りは隣の表示開始へ移動する。古い応答で新しい再生位置を巻き戻してはならない。
- タブが専用の接続・viewport・SHM reader を所有する。失効時は再取得して同じ位置へ戻り、有限回の復旧に失敗すれば理由と位置を表示して停止する。タブ破棄時には再取得と競合しても資源を解放する。S6 の 16 トレースが時計・遅延・再接続を照合する。[S6 共有トレース](playback-s6-traces.json)
- タイムラインは 1x〜8x のズーム、パン、ハンドルの ±1 フレーム／±0.1 秒の微調整を提供する。I／O は現在位置を IN／OUT に設定する。検索欄に入力フォーカスがある間は文字入力を優先する。編集セッションの正本は実時間の両端。フレーム番号だけの旧セッションでは OUT フレームの表示終端までを復元する。
- 共有動画へのタップ／スワイプ焼き込みは初期 ON。チェックの変更を IDE ローカル設定に保存し次回に引き継ぐ。表示用オーバーレイの切替とは独立する。トリム書き出し成功後もタブを閉じず、出力先等の完了バナーを表示する。書き出した選択と現在の選択が同じなら未書き出しの終了確認を解除し、その後に範囲を変更すれば再び保護する。PNG 1 枚の書き出しでトリム済み扱いにしてはならない。[Studio の操作](../../ide-plugin/src/main/kotlin/io/github/shinma06/arb/ui/ArbStudioContent.kt)・[エディタの書き出し](../../ide-plugin/src/main/kotlin/io/github/shinma06/arb/ide/ArbBundleFileEditor.kt)・[終了確認](../../ide-plugin/client-core/src/main/kotlin/io/github/shinma06/arb/state/TrimCloseState.kt)

[既知制約 / Trail #139] 別々の IDE が同じ編集セッション ID を更新すると、最後に書かれた範囲が残る。`edit.update_session` に期待バージョンによる競合検出はない。失敗時にそのタブの未保存範囲を再送できることと、別の IDE の上書きを防ぐことは別の条件である。[更新処理](../../core/crates/arbd/src/lib.rs)

## 実時間トリムの三状態と再生ループ [実装済み]

ハンドルで変更するのは Selection、境界画像を要求するのは TrimPreview、再生位置の所有者は Playhead。三つを同じフレーム番号で代用すると、長い静止画の途中で指定した時刻を失う。

| 対象 | 成立すべき不変条件（MUST） |
| --- | --- |
| Selection | 長さが正なら `0 ≤ inTimeNs < outTimeNs ≤ durationNs`。最小幅は 1 ms と全尺の小さい方。空映像で正の区間を作らない |
| Playhead | IN／OUT を掴む・動かす・離す・キャンセル・微調整・ズーム・パンで再生位置を変更しない。シーク、再生 tick、遅延 RPC も選択範囲を変更しない |
| TrimPreview | ハンドル操作・微調整ボタン長押し中は独立した時刻のプレビューと明示。離す・キャンセル・タブ切替・破棄で消え、再生位置の画像に戻る |
| 確定 | 境界内に制限したポインター時刻をそのまま保存する。長い静止区間の途中でもフレーム境界へ戻さない |
| 明示ショートカット | I／O は再生位置から対応する選択端へ一方向に値を写し、再生位置を動かさない |
| キーと中断 | テキスト入力欄は文字入力を優先。それ以外でドラッグまたはプレビュー中は Space・左右・I／O 等を抑止し、Escape で候補を破棄する。破棄後の遅延プレビュー応答を採用しない |
| 再生ループ（T-3） | `PlaybackController` は保持中も ns の位置から開始し、OUT 到達で停止、ループ時は IN を起点に折り返す。再生中の選択編集は次回の再生用に保持し、進行中の時計・再生境界を動かさない |
| 出力 | Studio の既定の「共有用 MP4」も `export.trim(include_share_mp4=true)`。四ストリームの完全な子バンドルを「トリム済み」に登録し、同じフォルダに共有動画を添える。[出力の比較表](SPEC-03-protocol-media.md) |

実時間境界・端点画像・遅延デコードの採否は S7、時計・再接続は S6、編集中の再生区間とループは `PlaybackControllerTest` が検査する。[コントローラー](../../ide-plugin/src/main/kotlin/io/github/shinma06/arb/ui/ArbStudioController.kt)・[再生ループ試験](../../ide-plugin/client-core/src/test/kotlin/io/github/shinma06/arb/playback/PlaybackControllerTest.kt)・[S7 共有トレース](studio-trim-s7-traces.json)

## 仕様 vs バグ

| 操作 | 正常仕様 | バグ |
| --- | --- | --- |
| 音声理由を展開・同じ内容をポーリング | 展開が残り原文をコピーできる | 毎回閉じる、先頭行しかコピーできない |
| 端末なしで録画を有効化 | ON・待機中 | 記録済みと表示する |
| 検索欄に「I」「O」を入力 | 検索文字が変わる | 選択範囲が動く |
| 書き出し完了 | タブと結果が残り、同じ範囲の警告を解除 | 自動で閉じる、失敗を完了とする |
| VFR 保持中にハンドルを離す | 許容範囲内で離した ns に残る | 再生位置が一緒に動く、またはフレーム境界へ跳ね戻る |

根拠は Trail #99・#100・#102・#104〜106・#110・#111、#130／PR #535、#137／PR #541、#146／PR #549、#148／PR #551、#150／PR #553、#152／PR #555。K10・K13・S6・S7・KON-1〜7、TCB-10／18／19。[VER-20](../verifications/VER-20-t6b-compose-jewel-ui.md)・[TCB 台帳](../proofs/tcb-axioms.md)
