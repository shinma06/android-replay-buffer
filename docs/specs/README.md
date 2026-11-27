# Android Replay Buffer の全体地図と仕様索引

変更前に、① 状態の所有者と守る条件を選ぶ → ② 変更影響表で他層と証明を辿る → ③ 症状の前提と実装を突き合わせる。仕様の基準は Item #17 と Item #18 の全段階を含む [develop の e428b638](https://github.com/shinma06/android-replay-buffer/commit/e428b638c690fe35f447fb1ba4477bd92a6b36d4)。

<a id="ownership"></a>

## 責務所有権と層をまたぐ不変条件

| 領域 | 正本（書き手・判定元） | 混同を防ぐ境界 | 守る条件 | 現行の検査・証明の境界 |
| --- | --- | --- | --- | --- |
| 録画・リング | `arbd` が取得世代を所有し、`arb-storage` がチャンクと pin を管理 | 設定の録画意図／接続中の取得、プロジェクト窓／共有の子 | 旧世代の遅延通知は新世代を変えない。停止・再開の引継ぎは同じ serial・boot・保持窓内。pin 中は追い出さない | K4・K5、S1〜S3、クラッシュ時の S5 と永続 ID の仮定 TCB-16 |
| 時計・窓 | `arb-core::time` と `selection`、端末ビーコン／ホスト時計 | 壁時計／経過時間、確定／不確実、保存の閉区間／実時間トリムの半開区間 | 同一 boot の Anchored は誤差区間の交差、Bounded は窓への完全包含で採用。部分交差する Bounded は不確実。未同期を確定列へ混ぜない | K1〜K3、G4、Miri M2 |
| 保存物・GC | `arb-storage` の確定 manifest と外部カタログ、公開は `arbd` | WORM 本体／編集・リース・退避、媒体切断／恒久欠損 | 実体削除には管理内・現行形式・退避済み・猶予条件・リース 0・対象ジョブ 0・同一性が必要。登録解除は実体に触れない | K5、S2、G1・G2、RS-4、KON-7、TCB-03・TCB-04 |
| IDE アイコン | 操作の意味ごとの `ArbIcons` | Swing の Icon／Jewel の IconKey／XML の静的フィールド | 同じ操作は全画面で同じアイコンを使い、デザイン変更は正本 1 箇所に集約する | アイコン構造テスト・XML の解決試験 |
| Studio・再生 | `PlaybackController` の時計、選択リデューサの Selection、`ArbStudioController` の TrimPreview | 再生位置／範囲／一時的な境界画像 | 範囲編集は再生位置を動かさず、tick は選択を動かさない。リリースした ns を保持し、中断でプレビューを解放。再生中の編集は次の開始に適用 | K8・K10・K12、S6・S7 と Kotlin リプレイ、Miri M1（SHM） |
| 書き出し | `arbd::media_engine` と `inspect_export` が RPC 別に生成・公開 | Studio の完全バンドル／CLI・MCP の単独 MP4 | 同じ `[IN, OUT)` を映像・音声・ログ・入力に適用。Studio の共有動画は新規バンドル内、単独共有の既定ではカタログ増分 0 | K9・K13、G3、S4、四ストリーム統合試験、Miri M2（MP4 パーサ） |
| 音声・端末 API | `arb-core::audio_policy` と音声方針表、端末の AAC ハンドシェイク | 起動時の希望／観測結果、非対応 API／実行時失敗 | API 28〜32 は既定で音声なし。33〜37 を含む 33 以上は端末出力を維持して試行。互換方式は 30 以上で明示選択。方式の自動切替なし | K11、G8、S3、KON-6、TCB-17 |
| 入力・描画 | 端末 getevent、`arbd` の軸校正、保存座標の読取投影 | 注入操作／観測、端末軸／画面ピクセル、オーバーレイ／焼き込み | 校正済み軸と既知寸法では範囲外を端に制限。概要・クロップ・Studio・焼き込みは同じ位置。確定済み入力は書き換えない | K6・K7、軸・読取の回帰、G3、TCB-08 |
| RPC・CLI・MCP | `arbd` のディスパッチと構造化結果、MCP ツール表と交渉版 | デーモン業務エラー／伝送断、RPC 名／公開ツール名 | 同一要求は同じ結果。4 版とも 20 ツールを 1 応答で列挙。GC・forget は全非空区分と件数、保存成功は共有判定から投影 | RS-1〜5、KON-1〜7、実 stdio・スキーマ検査、TCB-19・TCB-21 |

<a id="impact"></a>

## 変更影響マトリクス

| 変更の入口 | 同時に読む SPEC | 他層へ辿る順序と回帰の観測点 | 対応する証明・試験 |
| --- | --- | --- | --- |
| 録画開始・停止、リング上限、親終了 | 01・02・05・07 | device-agent → `live_adb.rs`・`capture.rs` → ring／catalog → CLI・MCP・IDE 状態。再接続・保存中に端末や pin を失わないか | K4・K5、S1〜S3・S5、G2・G5 |
| アンカー、ドレイン、ログ検索・概要 | 02・03・06・07 | 端末統計 → wire → 選別・保存 → RPC → CLI／MCP・Studio。スリープ、チャンク破損、静止中の同じログで所属・欠落数・表示フレームを比較 | K1〜K3・K10、S3、G4・G6、操作結果表、ログ検索試験 |
| カタログ・リース・ゴミ箱・再試行 | 01・02・05・06 | catalog／bundle_io → `arbd` → CLI／MCP・IDE。切断・パス置換・閲覧中に実体を保護し、全結果区分を表示するか | K5、S2、G1、RS-4、KON-7、操作結果表 |
| 操作アイコンの追加・変更 | 05 | `ArbIcons` → Tool Window・Studio・設定・Running Devices・メニュー・通知。同じ操作の全入口と XML 宣言を照合 | アイコン構造テスト |
| トリム・シーク・ループ | 02・03・05・06 | Kotlin の三状態・時計 → edit／export → `trim_range.rs` → 映像・音声・ログ・入力。静止中に両端を置き、再読込・ループ・Studio 同梱と MCP 単独出力を比較 | K10・K13、S6・S7、G3、PlaybackControllerTest、四ストリーム統合試験 |
| 書き出し形式・共有ファイル名 | 02・03・05・06 | Studio のエディタ／CLI／MCP → `export.trim` または `export.share_mp4` → 公開・登録。元の WORM、共有動画の場所、追加カタログ行数を確認。フレーム PNG は `save.screenshot` と保存先を共有し、既存ファイルを保護する | S2・S4、K13、G3、実 stdio スキーマ検査 |
| Android API 下限・音声方式 | 03・05・07 | 方針表 → arb-core → arb-adb／scrcpy 引数 → arbd の実状態 → Studio／CLI／MCP。API 32・33、取得失敗、明示互換時の消音を比較 | K11、G8、S3、KON-6、端末スモーク |
| 軸校正・ジェスチャー・焼き込み | 02・03・05・07 | device-agent → touch_calibration → bundle_io → 概要／クロップ・Studio・共有動画。旧記録の読取、軸端、静止フレーム内の操作を照合 | K6・K7・K13、G3、ストレージ読取・メディア統合試験 |
| RPC・MCP の引数・エラー・表示名 | 01・03・04・05・06 | ディスパッチ → CLI／MCP 契約表・4 版 → Kotlin IPC・表示。新旧版と未知引数、失敗後の接続、表示テキストを照合 | RS-5、KON-5〜7、共有表、実 stdio、ArbBrandingGuardTest |

<a id="evidence"></a>

## 対応する仕様と PDD 証明

上表の番号から仕様を開き、変更する命題をコードと照合する。共有値を変える場合は先に契約表・トレース・反例を更新する。OS、コーデック、実機や IDE の前提は TCB 台帳で確認する。

| 仕様 | 問い合わせの入口 |
| --- | --- |
| [SPEC-01](SPEC-01-architecture.md) | 誰がデータを書き、IDE を閉じると何が止まるか |
| [SPEC-02](SPEC-02-time-selection-bundle.md) | どの秒・端末の記録が入り、何を物理削除できるか |
| [SPEC-03](SPEC-03-protocol-media.md) | どの RPC を使い、何を映像の時刻・フレームとするか |
| [SPEC-04](SPEC-04-testing-and-harness.md) | その証明・試験でどこまで合格と言えるか |
| [SPEC-05](SPEC-05-ide-studio-ux.md) | IDE、Running Devices、ビューアで何が見えるべきか |
| [SPEC-06](SPEC-06-mcp-cli-contracts.md) | MCP と CLI でどの入力・結果・失敗を扱うか |
| [SPEC-07](SPEC-07-device-capture-audio-input.md) | どの端末・音声・タッチ・ログを観測できるか |

| 正本・台帳 | 照合するデータ |
| --- | --- |
| [音声方針](audio-policy-table.json) | API・方式・端末側の結果から起動引数、状態、原因、表示分類を引く |
| [端末選択](device-selection-policy-table.json) | serial の検証、開始・停止時の録画意図と端末の永続化、再起動後の候補選択（S3） |
| [操作結果](ui-feedback-contract-table.json) | GC・登録解除・保存・実体状態・RPC エラー等の期待値 |
| [MCP ツール](mcp-tools-table.json) | 公開 20 ツールの入力・出力型・注釈・タスク対応 |
| [再生トレース](playback-s6-traces.json) | S6 の時計、遅延応答、再取得、タブの解放 |
| [トリムトレース](studio-trim-s7-traces.json) | S7 の三状態・確定境界・中断・遅延応答。Kotlin 本番と 22 ケース・122 ステップを共有 |
| [スキーマ検査](../../core/scripts/check-mcp-trim-schema.py)・[表示名ガード](../../ide-plugin/src/test/kotlin/io/github/shinma06/arb/ide/ArbBrandingGuardTest.kt) | 交渉版別の 20 ツールと実書き出し、IDE と仕様書の表示名 |
| [TCB 公理台帳](../proofs/tcb-axioms.md) | K1〜K13、S1〜S7、G1〜G8、M1〜M2、構造ガードと実機・外部実装への依存 |

## 症状から判定する

| 観測した症状 | 判定 | 前提と境界 | 詳細 |
| --- | --- | --- | --- |
| 初期設定でトレイアイコンが見えない | 仕様 | トレイ初期 OFF。IDE での録画は独立する。 | [SPEC-01](SPEC-01-architecture.md) |
| 同じ IDE の二窓のうち一窓を閉じたら録画も終了 | バグ | アプリケーションが生きている限り共有の子は維持する。 | [SPEC-01](SPEC-01-architecture.md) |
| 親付きデーモンが IDE の強制終了後も常駐し続ける | バグ | 対応版の子は stdin EOF で終了する。CLI 単体起動はこの条件の外。 | [SPEC-01](SPEC-01-architecture.md) |
| CLI を終了しても単体デーモンが残る | 仕様 | 親付きフラグを付けていない単体デーモンは常駐できる。 | [SPEC-01](SPEC-01-architecture.md) |
| 停止→再開で停止前の証跡が全部消えた | 条件付きバグ | 同じ serial・boot_id で窓と保持予算内なら失われてはならない。別端末・別 boot・期限外は対象外。 | [SPEC-02](SPEC-02-time-selection-bundle.md) |
| 60 秒を要求、映像尺がちょうど 59 秒で `video_shortfall=false` | 仕様 | 58,999 ms なら true。ログ尺は独立して評価し、`window_shortfall` は両者の論理和。 | [SPEC-02](SPEC-02-time-selection-bundle.md) |
| ログ単体にログが 1 件あるが実ログ尺は 0 | 仕様 | 同一 boot の確定ログの最大−最小時刻で測る。 | [SPEC-02](SPEC-02-time-selection-bundle.md) |
| 時刻不明ログが映像と同時刻の確定行として並ぶ | バグ | 同期できない証跡は隔離する。 | [SPEC-02](SPEC-02-time-selection-bundle.md) |
| ゴミ箱に入れた直後の通常 GC で消えない | 仕様 | 既定の猶予は最終退避から 24 時間。 | [SPEC-02](SPEC-02-time-selection-bundle.md) |
| 「ゴミ箱を空にする」が閲覧リース中のバンドルを消す | バグ | 猶予期間を省いてもリースとジョブは保護する。 | [SPEC-02](SPEC-02-time-selection-bundle.md) |
| 管理ルートは存在し、その下の親を移動したら missing | 仕様 | 管理ディレクトリ配下の欠損と判定できる。 | [SPEC-02](SPEC-02-time-selection-bundle.md) |
| 外部媒体の切断中に一括 forget が行を消した | バグ | 親の可用性不明は storage_unavailable。再接続可能な行を維持する。 | [SPEC-02](SPEC-02-time-selection-bundle.md) |
| 保存先の容量不足で再試行時の pin が消えた | バグ | FailedRetryable は保持し出力先を変えて再試行できる。 | [SPEC-02](SPEC-02-time-selection-bundle.md) |
| 旧保存形式 schema 3/4 を開いたら元ファイルが更新された | バグ | 閲覧・再生・新規書き出しは元フォルダを変更しない。 | [SPEC-02](SPEC-02-time-selection-bundle.md) |
| 不正引数で操作に失敗したが接続表示は維持 | 仕様 | -32602 は業務エラー。対して -32601 は旧デーモン扱い。 | [SPEC-03](SPEC-03-protocol-media.md) |
| 静止時間が長いほどシークバーとログ位置がずれる | バグ | 実 PTS が揃う範囲では共通のメディア時間を使う。 | [SPEC-03](SPEC-03-protocol-media.md) |
| 同じファイルサイズの別動画なのに古いサムネイルが出る | バグ | フィルムストリップは元動画の SHA-256 も照合する。 | [SPEC-03](SPEC-03-protocol-media.md) |
| CLI／MCP の単独 MP4 でトリム済み行も増えた | 条件付きバグ | `keep_bundle=true` の明示時のみ登録。時刻・フレーム範囲を指定しただけでは登録しない。 | [SPEC-03](SPEC-03-protocol-media.md) |
| S5 の検査成功を理由に実機再起動後の重複排除も合格と報告 | 判定誤り | モデルは永続クラッシュ ID を仮定。TCB-16 は未解決。 | [SPEC-04](SPEC-04-testing-and-harness.md) |
| PR の 4 チェックだけで全 OS の IDE 配布受け入れを完了扱い | 判定誤り | 実 GUI・配布物・オーナー承認等は別ゲート。 | [SPEC-04](SPEC-04-testing-and-harness.md) |
| 切断した端末を待っていて ON・待機中になる | 仕様 | 取得意図が有効。録画中の表示は接続・取得を確認したとき。 | [SPEC-05](SPEC-05-ide-studio-ux.md) |
| 音声の長い詳細を開くと同じ内容なのにポーリングごとに閉じる | バグ | 同じバナーなら展開を維持する。 | [SPEC-05](SPEC-05-ide-studio-ux.md) |
| 長文バナーの左アクセント線が本文の途中で切れる | バグ | 展開後も本文の高さに追従する。 | [SPEC-05](SPEC-05-ide-studio-ux.md) |
| 検索欄に O と打つと OUT が動く | バグ | 検索入力中はショートカットから保護する。 | [SPEC-05](SPEC-05-ide-studio-ux.md) |
| トリムハンドルに再生位置が追従、または離すと端が跳ね戻る | バグ | 停止中の時計は不動。静止中のサブフレーム時刻でも制限後の ns を保持する。 | [SPEC-05](SPEC-05-ide-studio-ux.md) |
| トリム成功でビューアが閉じず完了バナーが残る | 仕様 | 同じ範囲の未書き出しガードを解除し、その場で結果を確認できる。 | [SPEC-05](SPEC-05-ide-studio-ux.md) |
| 焼き込みを OFF にしたら次の書き出しでも OFF | 仕様 | 初期 ON、変更後は IDE の前回設定を使う。 | [SPEC-05](SPEC-05-ide-studio-ux.md) |
| ログ検索が既定で 100 件で終わった | 仕様 | MCP／CLI は続きのカーソルを返す。内部 RPC の既定は 10,000。 | [SPEC-06](SPEC-06-mcp-cli-contracts.md) |
| 画像調査の既定応答に PNG 本体が入らない | 仕様 | 必要な画像を resource_link から読み取れる。 | [SPEC-06](SPEC-06-mcp-cli-contracts.md) |
| GC プレビューで消去または期限切れリースの変更が生じた | バグ | dry_run は読み取り専用。 | [SPEC-06](SPEC-06-mcp-cli-contracts.md) |
| 非同期タスクをキャンセルしたのに受理済みの保存物が残った | 仕様 | 既に発行済みの副作用を巻き戻す契約はない。 | [SPEC-06](SPEC-06-mcp-cli-contracts.md) |
| 2 台同時接続で録画先を自動選択しない | 仕様 | 未選択なら選択待ち。選択済みの端末がなければその端末を待つ。 | [SPEC-07](SPEC-07-device-capture-audio-input.md) |
| API 33 実機の音声開始失敗に「音声非対応」と出た | バグ | 対応 API での失敗は実行時の原因を表示する。 | [SPEC-07](SPEC-07-device-capture-audio-input.md) |
| output_compat で端末スピーカーが消音 | 仕様 | 消音を伴う明示オプトイン方式。 | [SPEC-07](SPEC-07-device-capture-audio-input.md) |
| adb shell input tap が映像に反応してもタップ件数は 0 | 観測制約 | Framework 注入はカーネルの getevent に現れる保証がない。 | [SPEC-07](SPEC-07-device-capture-audio-input.md) |
| ログのパッケージ名の大小文字だけが異なり一方が上書きされた | バグ | 出力相対パスは大小文字を区別しない FS でも衝突を避ける。 | [SPEC-07](SPEC-07-device-capture-audio-input.md) |
| Studio の「共有用 MP4」でトリムバンドルも登録された | 仕様 | 同じバンドル内に `share.mp4` または `share-taps.mp4` を同梱する。 | [SPEC-03](SPEC-03-protocol-media.md) |
| ハンドル操作中に Space・矢印・I／O が効かず、Escape で中断 | 仕様 | 入力欄以外のドラッグ・プレビュー中はキーを抑止する。中断後に古い境界画像が戻ればバグ。 | [SPEC-05](SPEC-05-ide-studio-ux.md) |
| 再生中に選択端を動かしても現在のループ端は同じ | 仕様 | 新しい区間を次の再生開始に適用する（T-3）。 | [SPEC-05](SPEC-05-ide-studio-ux.md) |
| 静止画の 15.5〜18 秒を選ぶと、焼き込みなしの出力が 2.5 秒・1 枚 | 仕様 | 元の 1 枚が区間全体を保持。範囲内の音声・ログ・入力も同じ原点を使う。 | [SPEC-03](SPEC-03-protocol-media.md) |
| 時刻指定に片端しかなく、フレーム指定もあるのに失敗 | 仕様 | 時刻対は両端必須。不正範囲の RPC は `-32602`。 | [SPEC-03](SPEC-03-protocol-media.md) |
| 概要のエラー件数と代表行数が違う | 仕様 | フィルタ一致の生行総数とグループ数を区別し、代表行に回数・初回／最終オフセットを表示する。 | [SPEC-06](SPEC-06-mcp-cli-contracts.md) |
| 同じ主ログで概要と検索の表示フレームが異なる | バグ | 同一 boot の ns 時刻・同一 VFR 表を使う。UTC の表示精度はミリ秒。 | [SPEC-03](SPEC-03-protocol-media.md) |
| ログに影響するチャンク破損後も欠落 0・完全と報告 | バグ | 行数不明は null。送信キューのフレーム欠落数もログ行数に換算しない。 | [SPEC-02](SPEC-02-time-selection-bundle.md) |
| 最新 MCP のスクリーンショットにインライン画像がない | 仕様 | 既定は保存パスとメタデータ。旧版の既定と異なり PNG 本体は明示指定する。 | [SPEC-06](SPEC-06-mcp-cli-contracts.md) |
| Task 非対応ツール、または不正な taskId で引数エラー | 仕様 | ID の欠落・型違い・未知／期限切れには個別の理由を返す。 | [SPEC-06](SPEC-06-mcp-cli-contracts.md) |
| API 37 の軸外タップが画面端に出た | 仕様 | 端へクランプする。校正前の現行形式の記録も既知寸法なら読取時に補正する。 | [SPEC-07](SPEC-07-device-capture-audio-input.md) |

## 設計決定を遡る

| Entire Trail | 統合・照合のポイント |
| --- | --- |
| [#96](https://entire.com/gh/shinma06/-/trails/96) | Rust ワークスペース、時刻・WORM・メディアの基盤。Git 上の起点は `6fbbdbb0` |
| #97〜106 | AAC・共有映像、ログ単体とクラッシュ、Studio、MCP、配布、Compose/Jewel、複数端末と UI 配線。VER-12〜22 |
| #107〜116 | 旧実装の整理、Android 9、命名、スワイプ／Running Devices、保存・SHM・VFR 監査、実 AVD、develop 統合。[VER-32](../verifications/VER-32-develop-promotion.md) |
| #117〜124 | 配布ゲート、IDE 親紐付き、I/O 回復と IDE／端末修正、五層検証と拡張証明の実験。[VER-33](../verifications/VER-33-ide-parent-bound-lifecycle.md)・[VER-34](../verifications/VER-34-main-release-gate.md) |
| #125〜138 | 証明候補と昇格、再生固着、forget／即時 GC、通信障害、リングとログの規模、GC の同一性、IDE エラー表示と Kani 計測。GC の統合は #136／PR #540 |
| #139〜149 | 条件付き編集の提案、VFR と音声、共有表・GOP 予算の実験、方式選択、S6 と昇格。提案や実験だけでは本番保証にならない |
| #150〜153 | バナー、再開・保存証跡、ズーム・微調整、MCP 版交渉と 20 ツール。PR #553〜556 |
| [#154](https://entire.com/gh/shinma06/-/trails/154) | [PR #557](https://github.com/shinma06/android-replay-buffer/pull/557) で単独共有、ログ座標・UTC、品質サマリ・メモ・エラー集約、4 版の MCP 契約を統合 |
| [#155](https://entire.com/gh/shinma06/-/trails/155)・[#156](https://entire.com/gh/shinma06/-/trails/156) | Item #18。[メディアと K13](https://github.com/shinma06/android-replay-buffer/pull/558)、[Studio と S7](https://github.com/shinma06/android-replay-buffer/pull/560)、[実時間編集・四ストリームと書き出し](https://github.com/shinma06/android-replay-buffer/pull/561)を統合 |

各規範の根拠は対応する SPEC の実装リンク、[要件](../requirements)、[検証記録](../verifications)で確認できる。履歴の時点と基準版で値が異なる例は、実ファイルの拡張子（SPEC-07）、現行ツール数と過去 AVD 試験・通常 Kani 集合の本数（SPEC-04）で個別に示した。
