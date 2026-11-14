# 保存後の切り取りと、シーク位置の静止画（要件）

状態: **要件確定（実装前）**。`develop`（`5af6ac26c91b98b664b054427b106b4094000e38`、schema 4）の現行コードに基づく。判断は本文書で確定する。実装は利用者承認後。

この文書は Trail [#19](https://entire.com/gh/shinma06/-/trails/19)（Issue [#16](https://github.com/shinma06/android-replay-buffer/issues/16)）の要件正本である。2026-11-11 の先行案（`2bfa9b7e`、schema 3 / `session.json` / `video-001.mp4` / 「音声はない」）は現行コードと合わないため、本文書で置き換える。

元の要望（Trail #19）: 保存した動画を IDE 内で開き、前後をカットして保存する。トリミング前と後の2本を保存するオプションを付ける。シークバーで選んだ位置の画面をキャプチャして PNG で保存する。

## 用語

| 用語 | 意味 |
|---|---|
| 保存フォルダ | 「直前N秒を保存」等でできる1回分の完成フォルダ。名前は `replay-<yyyyMMdd-HHmmss>-<manual\|crash>[-logs]-<8桁ID>`（[SaveLayout.kt](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/SaveLayout.kt)）。使う文字は `[a-z0-9-]` だけ |
| 動画内の時刻 | プレイヤーの再生位置（0 から）。`video-frames.jsonl` の `movie_pts_us`〜`movie_end_us` |
| 記録窓の時刻 | 保存範囲の先頭を 0 とする、動画・ログ・タップ・音声共通の時刻。各行の `t_ns`（schema 4。旧 `window_ns` ではない） |
| 開始点・終了点 | 切り取って残す範囲の始めと終わり（In / Out）。動画内の時刻で指定し、表示中のコマの境界に合わせる |
| 切り取り | 開始点より前と終了点より後を除いた、**新しい**保存フォルダを作ること |
| 静止画（frame） | 保存クリップの、今のシーク位置に表示されている1コマを PNG にしたもの。端末の「今の画面」を撮る既存スクリーンショットとは別 |

## 0. 現行コードで分かっていること（根拠と仮定）

確認対象は `/workspace/android-replay-buffer` の `develop` @ `5af6ac2`。この箱に実保存フォルダの実物はなく、以下はコードと設計文書からの読み取りである（実物観察は 未確認。実装時の単体テストは合成 fixture で足り、実フォルダでの照合は受入）。

### 保存物（現行）

[plugin-design.md 保存物の構成](../plugin-design.md#保存物の構成)、[SaveLayout.kt](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/SaveLayout.kt)、[SaveWriter.kt](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/SaveWriter.kt):

| ファイル | 内容 | 切り取りへの影響 |
|---|---|---|
| `README.md` | 固定見出しの要約。値は `replay.json` と同じ事実 | 切り取ったこと・元フォルダ・範囲を書いて作り直す |
| `replay.json` | schema 4。`complete`・`save_id`・`folder_name`・`trigger`・`files`・`counts`・`coverage`・`timebase`・`files_sha256` など | 範囲外を落とし、掛かる gap は端で切り詰め、ハッシュは再計算。任意 field `trim` を追加する（schema 番号は 4 のまま） |
| `video.mp4` | H.264 の MP4 1本。切れ目はファイル内の parts。任意で AAC（`record_audio`） | 表示秒とファイル内位置は一致しないことがある（preroll / edit list）。バイト位置では切らない |
| `video-frames.jsonl` | 1行1frame。`movie_pts_us`〜`movie_end_us`、`t_ns`、`clock_alignment_known` など | 動画内時刻↔`t_ns` の換算表。正本 |
| `video-source.h264` | サイズ変更やタップ描画で再エンコードしたときだけ。元 Annex B | 残す場合は新しい範囲の packet と `offset/size/hash` を一致させる |
| `logcat-device.jsonl` / `logcat-app.jsonl` | 各行 `t_ns` / `t_uncertainty_ns` | 元と同じ採否規則 |
| `touches.jsonl` | 動画を要求した保存だけ。`t_ns` と `movie_us` | `movie_us` を新しい動画で付け直す。元が null なら null のまま |

作成中は `.<名前>.partial`、完成名へ上書きなしで改名。旧形式（`session.json` / `README.txt` / `video-001.mp4`）は移行も対象にもしない。

### 画面（現行）— シークバーはまだない

保存後の IDE 操作は、ToolWindow と通知の「フォルダを開く」だけである（[ui-ux.md](../design/ui-ux.md)、[ReplayToolWindowFactory.kt](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/ui/ReplayToolWindowFactory.kt)、[ReplayProjectService.kt](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/ide/ReplayProjectService.kt)）。**保存クリップを IDE 内で再生・シークする UI は存在しない。** 既存の `screenshot` は接続中端末の「今の画面」であり、保存クリップの1コマではない（[ScreenshotSave.kt](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/ScreenshotSave.kt)、[SHOT-02](../requirements.md#第2版の追加要件スクリーンショット)）。

**仮定（本要件で埋める）:** 「シークバー位置の静止画」を満たすため、保存クリップ用の最小プレイヤー（エディタタブ、シークバー、コマ送り）をこの機能の一部として新設する。プレイヤーが使えない経路（CLI / MCP）では、同じ中核処理を動画内時刻 `--at` / `--from` / `--to` で呼ぶ。

### 同梱部品（現行）

- プラグインは **JCodec** を同梱し、サイズ混在やタップ描画の再エンコードに使っている。
- [project.md](../project.md) により、利用者環境への **ffmpeg / Python / scrcpy の別インストールは前提にしない**。切り取り・復号も JCodec と既存 writer 経路で行う（親依頼の「必要なら ffmpeg 再エンコード」は、同梱 JCodec による再エンコードとして解釈する）。
- CLI / MCP は GUI と同じ処理を共有し、変更時は `docs/cli.md`・`replay --help`・`plugin/src/main/cli/README.txt`・`docs/mcp.md` を同時に更新する（[AGENTS.md](../../AGENTS.md)）。

## 1. 範囲と対象外

### 範囲

- 完成済み（`replay.json` の `complete=true`、`.partial` でない、schema 4、`video.mp4` と `video-frames.jsonl` がある）保存フォルダを対象に、開始点・終了点で切り取った**新しい**保存フォルダを作る。
- 同じプレイヤー（または CLI の時刻指定）で、今の表示コマを PNG にする。
- 中核処理は画面に依存せず、IDE・CLI・MCP が同じ結果を返す。
- ログ・タップ・（あれば）音声・`replay.json` / `README.md` も同じ範囲にそろえる。

### 対象外

| 対象外 | 理由 |
|---|---|
| 保存前バッファ上での範囲指定 | 別機能。本機能は保存済みフォルダだけ。Issue #16 の「保持範囲内の開始・終了位置を指定して保存」「初期版は設定時間分を即保存」は、保存時の範囲指定にも読めるが、Trail #19 と本機能は保存後の切り取りに限定する |
| 複数範囲の切り出し・複数保存の結合 | 時刻対応が複雑になる |
| 拡大縮小・圧縮率変更・GIF/WebM・ぼかし・文字入れ | 別機能 |
| ログ専用（`-logs` / 動画なし）フォルダの時間切り出し | プレイヤーも MP4 もない |
| schema 4 より前、または手編集でハッシュが崩れた保存 | 理由を示して拒否 |
| 外部アップロード | 対象外 |
| プレイヤー上でのログ編集 | 範囲内の件数表示まで |

## 2. 確定した既定と根拠

依頼は判断をこちらに委譲している。旧案の未決事項は次で確定する。

| 項目 | 既定 | 根拠 |
|---|---|---|
| 元の保存 | **常に残す。切り取りは新しいフォルダだけ。CLI / MCP / GUI いずれでも元を消す操作は提供しない**（旧案の「確認付き置き換え」も今は出さない） | Trail #19 の「トリミング前と後の2本を保存するオプション」は、常に両方残すことで満たす。SAVE-04 と AGENTS.md の「データの削除で止まる」。削除は後の Trail に分けられる |
| 切り取り方式 | **先頭 GOP だけ再エンコード（K2）。残りは元 packet をコピー。I-frame へ黙って丸めない** | [timeline.md](../design/timeline.md) は開始点の黙った丸めを禁じる。edit list だけだと切った前の画面がファイルに残る。全体再エンコードは最長 900 秒で重い。JCodec は既に同梱 |
| 出力場所 | 元と**同じ親フォルダ**に新規フォルダ | 元と並ぶ。元の中には書かない（SAFE-01） |
| フォルダ名 | `replay-<yyyyMMdd-HHmmss>-trim-<8桁ID>`（切り取り開始のローカル時刻、文字は `[a-z0-9-]` のみ） | 現行の命名規則に合わせる。UTC や秒の小数点、`T`/`Z` は使わない |
| 静止画の場所 | 既存スクリーンショットと同じ保存先（SHOT-02） | 画像の置き場を増やさない |
| 静止画の名前 | `frame-<yyyyMMdd-HHmmss>-<8桁ID>.png` | 既存 `screenshot-…`（端末の今の画面）と先頭で区別する |
| IDE 再生 | Compose + JCodec でコマ表示・シーク（純 Java） | 3 OS で同じ。JCEF / 外部 ffmpeg / Android プラグイン内部 FFmpeg は使わない |
| CLI / MCP | **初回から同じ中核を公開する**。置き換え（削除）は置かない | AGENTS.md の GUI/CLI/MCP 同一 |

音声: 元に AAC がある場合、切り取り後の `video.mp4` にも同じ範囲の音声を残す。`coverage.audio` / `audio_track` を範囲に合わせて更新する。音声だけ欠ける失敗は保存失敗とし、映像だけ成功として公開しない（mux 不整合を避ける）。`record_audio=false` や元に track が無い保存は、従来どおり音声なし。

## 3. 利用の流れ

1. 保存が完成する（従来どおり通知と「フォルダを開く」）。
2. 通知と ToolWindow 直近結果に **「動画を開く」** を足す。`…` メニューに「保存した動画を開く…」（フォルダ選択）。IDE で完成フォルダの `video.mp4` を開いたとき、同じフォルダに schema 4 の `replay.json` があればこのプレイヤーで開く。
3. エディタタブのプレイヤー: 画面、シークバー（開始・終了の旗、欠落は斜線＋「欠落」、時計不明は点線＋「時刻不明」、タップ位置の目盛り）、`1コマ戻る` / 再生 / `1コマ進む`、`ここを開始にする`（`I`）、`ここを終了にする`（`O`）、`この画面をPNG保存`、要約 `m:ss.mmm〜m:ss.mmm（…秒）を残す`、主ボタン `切り取って保存`。
4. 切り取りは背景処理。進捗と中止。成功通知は「切り取った動画を保存しました · フォルダを開く · 動画を開く」。元フォルダは変えない。
5. CLI 例: `replay trim <保存フォルダ> --from 5.2 --to 12.0`（stdout は新フォルダのパスだけ）。`replay frame <保存フォルダ> --at 7.533`（stdout は PNG のパス）。MCP は `trim_replay` / `export_frame`。時刻は動画内の秒（小数可）。既存コマンドの意味は変えない。

## 4. 機能要件

### 4.1 時刻（TRIM-T）

| ID | 要件 |
|---|---|
| TRIM-T01 | 開始点・終了点は動画内の時刻で指定し、`video-frames.jsonl` で `t_ns` へ換算する。バイト位置や平均 fps では換算しない |
| TRIM-T02 | ログ・タップ・音声サンプルは、元の保存と**同じ採否規則**で選ぶ。端からの距離が `t_uncertainty_ns` 以内の行は残し、範囲外と証明できる行だけ除く。`t_ns=null` の行は除外と断定せず残す |
| TRIM-T03 | 範囲が `clock_alignment_known=false` に掛かるときは、その旨を処理前に示し（CLI は stderr 警告）、ログは多めに残す。不明を既知へ書き換えない |
| TRIM-T04 | 新しいフォルダは通常の保存と同じ形式（schema 4、同じファイル名規則、同じ読み方）。動画内時刻と `t_ns` は新しい開始点を 0 として付け直し、`replay.json` の `trim` に元へ戻す差分を残す |
| TRIM-T05 | 範囲内の gap / `timebase.clock_steps` / 欠落はそのまま残し、範囲外だけ除く。掛かるものは端で切り詰める。端が null のものは null のまま。gap を 0 秒に詰めない |
| TRIM-T06 | タップの `movie_us` と音声 segment の movie 時刻は新しい動画で付け直す。元が null なら null のまま |
| TRIM-T07 | `replay.json` の `trim` に、元の `save_id`・`folder_name`、元の `replay.json` と `video.mp4` の SHA-256、元の動画内時刻での開始/終了、対応する `t_ns`（不明なら null）、方式 `leading_gop_reencode` を書く。`trigger` は元を引き継ぐ（`manual` / `crash`）。フォルダ名の種類部分は常に `trim` |

### 4.2 切り取り精度とメディア（TRIM-M）

| ID | 要件 |
|---|---|
| TRIM-M01 | 開始点は選んだコマの表示開始、終了点は選んだコマの表示終了。コマの途中で切らない。I-frame へ黙って丸めない |
| TRIM-M02 | 方式は先頭 GOP の再エンコード + 以降の元 packet コピー。開始点より前の画像が、一般的なプレイヤーで見えないこと（ファイル内 preroll を残さない）。再エンコード区間の画質は既存のタップ描画/正規化と同程度を目標とし、劣化があっても範囲外の画面が残るより優先する |
| TRIM-M03 | 元に AAC があれば同じ範囲を mux する。track の有無と `coverage.audio` が食い違ってはならない |
| TRIM-M04 | `video-source.h264` は、元にあった場合だけ、新しい範囲に対応する分を残す。元に無い切り取りで新たに作らない（コピー区間だけなら不要） |
| TRIM-M05 | 外部 ffmpeg バイナリに依存しない |

### 4.3 保全（SAFE）

| ID | 要件 |
|---|---|
| SAFE-01 | 元フォルダには書き込まない（読むだけ） |
| SAFE-02 | 開始前に元の `files_sha256` と実ファイルを照合し、不一致なら理由を示して止める |
| SAFE-03 | 出力は `.<名前>.partial` に書き、検証後に上書きなしで完成名へ改名する。失敗・中止では完成フォルダを出さない。`.partial` の扱いは既存保存と同じ |
| SAFE-04 | 元フォルダを削除・置換する API / UI を置かない |
| SAFE-05 | 開ける・切れるのは完成済み schema 4 の、動画付き保存だけ |
| SAFE-06 | 切り取り中も取得は止めない |
| SAFE-07 | ファイル I/O は既存の `HostPlatform.saveHandles`（`PosixSaveHandles` / `WindowsSaveHandles`）だけを通す。呼び出し側で OS 分岐しない |

### 4.4 静止画（FRAME）

| ID | 要件 |
|---|---|
| FRAME-01 | 今表示中（または `--at` が属する）の**表示コマ**を、元の画素サイズで PNG にする。プレイヤーの表示倍率や HiDPI キャプチャは使わない |
| FRAME-02 | 保存先は SHOT-02 のスクリーンショット先。既存ファイルを上書きしない。作成中は既存スクショと同じ `.partial` 規則 |
| FRAME-03 | 時刻の正確さは `video-frames.jsonl` のそのコマの `movie_pts_us`〜`movie_end_us` に属する位置であること。PNG の tEXt（または同等の付属メタデータ）に元 `folder_name` / `save_id`、動画内時刻、`t_ns`（不明なら書かない）を入れる |
| FRAME-04 | 端末の `show_touches` や `tap_display=VIDEO` で動画に焼き込まれた点は、そのコマに写っていれば残る。後から点を足したり消したりしない |
| FRAME-05 | 通知は既存スクショに近い「保存場所を開く」。削除を出す場合は既存スクショと同じ隔離削除だけを使い、元の保存フォルダは触らない |
| FRAME-06 | 色一致の証明にはしない（既存の再エンコード注記と同じ） |

### 4.5 容量・エラー

| ID | 要件 |
|---|---|
| ERR-01 | 開始前に出力サイズを見積もり、既存の空き容量下限を保てなければ始めない。書込み中も既存の `checkSpace` 相当を続ける |
| ERR-02 | ファイル全体をメモリに載せない |
| ERR-03 | IDE では背景タスクで進捗と中止を出す。画面を固めない |
| ERR-04 | 拒否・失敗の例（日本語の理由、推測を事実にしない）: 未完成 / schema 不一致 / ハッシュ不一致 / 動画なし / 範囲が空または開始≥終了 / 範囲が動画の表示区間外 / 書き込み不可 / 容量不足 / 中止。CLI の失敗は 0 以外の終了コードと stderr。stdout には成功パスだけ |
| ERR-05 | 保存先に書けないときは、IDE では「別の場所に保存…」を案内できる。別の場所でも元は変えない |

## 5. 受入条件（検証可能な項目）

テスト合格と IDE / 端末での受入は別に記録する。

| ID | 条件 |
|---|---|
| AC-01 | 合成 fixture で、①切れ目なし、②複数 parts / gap、③ `video-source.h264` あり、④タップあり、⑤ AAC あり、の各保存を切り取り、新しいフォルダが schema 4 として読める |
| AC-02 | 切り取った動画の最初と最後の表示コマが、指定したコマと `video-frames.jsonl` で一致する。I-frame 丸めがない |
| AC-03 | 開始点より前の画像が、出力 MP4 の通常再生で見えない |
| AC-04 | ログ・タップ・音声の採否が TRIM-T02 に従い、`t_ns=null` の行が落ちない |
| AC-05 | 範囲内の gap / clock_steps が残り、範囲外は残らず、null 端は null のまま |
| AC-06 | 元フォルダの全ファイル SHA-256 が切り取りの前後で変わらない |
| AC-07 | 中止・容量不足・例外で完成フォルダが残らない |
| AC-08 | PNG が指定コマ・元解像度で、名前が `frame-` で始まり、スクショ先に上書きせず、メタデータに時刻が残る |
| AC-09 | CLI の `trim` / `frame` の stdout がそれぞれ新フォルダ / PNG のパスだけであり、同じ入力で中核の出力が GUI 呼び出しと同じ（ID・日時を除くバイト一致を単体で確認） |
| AC-10 | 同じ入力・同じ範囲の出力が Linux / macOS / Windows で、名前の ID・日時を除きバイト一致する（`os-parity`） |
| AC-11 | 既存の `start|stop|save|status|screenshot|settings` の意味と終了コードが変わらない |
| AC-12 | 実端末または Emulator で作った保存をプレイヤーで開き、シーク・コマ送り・In/Out・切り取り・PNG ができること — **本箱では KVM 不可のため実装時は 未確認**。単体と CLI で代替し、VM 受入に残す |

## 6. テスト計画

| 層 | 内容 |
|---|---|
| 単体（必須、この実装で通す） | 中核の切り取りと frame 出力。AC-01〜AC-09, AC-11 を fixture で。AAC あり/なし、gap、`t_ns=null`、ハッシュ不一致の拒否、`.partial` 失敗、元フォルダ不変 |
| CLI / MCP 契約 | `docs/cli.md` / `docs/mcp.md` / help / README.txt と実装が一致。既存サブコマンドの回帰 |
| os-parity | AC-10 を CI の既存ジョブに載せる（入力 fixture をリポジトリへ。大きな実動画は短い合成 MP4 で足りる） |
| IDE UI | プレイヤーの開閉、ボタン無効理由、通知。可能なら UI テスト。実再生の滑らかさは測定して記録し、不合格でもコマ送りと PNG が正確なら切り取り機能自体は止めない |
| VM 受入（後続、本箱では 未確認） | [vm-acceptance.md](../verification/vm-acceptance.md) への行追加は実装後の別作業でもよいが、項目案は AC-12 |

## 7. 実装時の注意

- 本文書は枝 `devin/1794359900-video-trim`（Trail #19）の要件正本。実装は承認後。`origin` への push、PR、`entire trail` の書き込み・checkpoint push、Devin Cloud への連絡はしない（この箱の依頼制約）。
- 追加依存は原則禁止（JCodec と既存ハンドルで足りる）。
- ユーザー向け文言は日本語。エージェント向けコメントは増やさない。
- 固定行番号のソースリンクは置かない（ファイルと記号で示す）。
- 本箱に JDK 27 がある場合は `/workspace/devin-cli-arb/toolchain/jdk-27` と `ANDROID_HOME=/home/box/Android/Sdk`（`android-37.2` r01 導入済み）を使って `python3 scripts/workflow/plugin_check.py --tests-only` または対象テストを実行する。
