# Android Replay Buffer 仕様索引

判定の基準は `develop` の [bdc0d8ba（PR #556 統合）](https://github.com/shinma06/android-replay-buffer/commit/bdc0d8ba7a1a061ed0221b3ad292bc7220b4a15b)。**[実装済み]** はこの版の実装で照合できる契約、**[既知制約 / TCB-xx]** は観測や証明の前提、**[設計]／[既知差分]** は目標と基準版の違いを示す。別のビルドを調べる場合は対象の PR がそのビルドに含まれるかを先に確認する。

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
| [端末選択](device-selection-policy-table.json) | serial の検証と候補選択 |
| [操作結果](ui-feedback-contract-table.json) | GC・登録解除・保存・実体状態・RPC エラー等の期待値 |
| [MCP ツール](mcp-tools-table.json) | 公開 20 ツールの入力・出力型・注釈・タスク対応 |
| [再生トレース](playback-s6-traces.json) | S6 の時計、遅延応答、再取得、タブの解放 |
| [TCB 公理台帳](../proofs/tcb-axioms.md) | K1〜K12、S1〜S6、G1〜G8、M1〜M2、構造ガードと実機・外部実装への依存 |

## 症状から判定する

| 観測した症状 | 判定 | 前提と境界 | 詳細 |
| --- | --- | --- | --- |
| 初期設定でトレイアイコンが見えない | 仕様 | トレイ初期 OFF。IDE での録画は独立する。 | [SPEC-01](SPEC-01-architecture.md) |
| 同じ IDE の二窓のうち一窓を閉じたら録画も終了 | バグ | アプリケーションが生きている限り共有の子は維持する。 | [SPEC-01](SPEC-01-architecture.md) |
| 親付きデーモンが IDE の強制終了後も常駐し続ける | バグ | 対応版の子は stdin EOF で終了する。CLI 単体起動はこの条件の外。 | [SPEC-01](SPEC-01-architecture.md) |
| CLI を終了しても単体デーモンが残る | 仕様 | 親付きフラグを付けていない単体デーモンは常駐できる。 | [SPEC-01](SPEC-01-architecture.md) |
| 停止→再開で停止前の証跡が全部消えた | 条件付きバグ | 同じ serial・boot_id で窓と保持予算内なら失われてはならない。別端末・別 boot・期限外は対象外。 | [SPEC-02](SPEC-02-time-selection-bundle.md) |
| 60 秒を要求、映像尺がちょうど 59 秒で不足フラグなし | 仕様 | 判定は 1 秒を超える不足。58,999 ms なら不足。 | [SPEC-02](SPEC-02-time-selection-bundle.md) |
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
| フレーム範囲指定で共有 MP4 と編集済みバンドルの両方ができた | 既知差分 | 基準版はトリム経由。単体出力の既定化は PR #557。 | [SPEC-03](SPEC-03-protocol-media.md) |
| S5 の検査成功を理由に実機再起動後の重複排除も合格と報告 | 判定誤り | モデルは永続クラッシュ ID を仮定。TCB-16 は未解決。 | [SPEC-04](SPEC-04-testing-and-harness.md) |
| PR の 4 チェックだけで全 OS の IDE 配布受け入れを完了扱い | 判定誤り | 実 GUI・配布物・オーナー承認等は別ゲート。 | [SPEC-04](SPEC-04-testing-and-harness.md) |
| 切断した端末を待っていて ON・待機中になる | 仕様 | 取得意図が有効。録画中の表示は接続・取得を確認したとき。 | [SPEC-05](SPEC-05-ide-studio-ux.md) |
| 音声の長い詳細を開くと同じ内容なのにポーリングごとに閉じる | バグ | 同じバナーなら展開を維持する。 | [SPEC-05](SPEC-05-ide-studio-ux.md) |
| 長文バナーの左アクセント線が本文の途中で切れる | バグ | 展開後も本文の高さに追従する。 | [SPEC-05](SPEC-05-ide-studio-ux.md) |
| 検索欄に O と打つと OUT が動く | バグ | 検索入力中はショートカットから保護する。 | [SPEC-05](SPEC-05-ide-studio-ux.md) |
| トリムハンドルにプレイヘッドが追従し、離すと端が跳ね戻る | 既知バグ | Item #18 の設計は独立とゼロスナップバックを要求。基準版はフレーム確定。 | [SPEC-05](SPEC-05-ide-studio-ux.md) |
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
| [#154](https://entire.com/gh/shinma06/-/trails/154) | [PR #557](https://github.com/shinma06/android-replay-buffer/pull/557) の統合後は共有動画・品質サマリ・ログの既定／フレーム位置・UTC・メモ・エラー区分と操作結果表を再照合 |
| [#155](https://entire.com/gh/shinma06/-/trails/155)・[#156](https://entire.com/gh/shinma06/-/trails/156) | Item #18。メディアの時間範囲と K13、Studio の状態分離と S7、最後に編集永続化・RPC／MCP の時間指定・全ストリームの整合を段階別に照合 |

各規範の根拠は対応する SPEC の実装リンク、[要件](../requirements)、[検証記録](../verifications)で確認できる。履歴の時点と基準版で値が異なる例は、実ファイルの拡張子（SPEC-07）、現行ツール数と過去 AVD 試験・通常 Kani 集合の本数（SPEC-04）で個別に示した。
