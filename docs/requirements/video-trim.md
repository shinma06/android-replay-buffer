# 保存した動画の切り取りと画面のPNG保存（Trail #19 要件案）

状態: **要件案（実装前）**。この文書は製品コードを変えません。末尾の「[利用者に決めてほしいこと](#8-利用者に決めてほしいこと)」が決まったら、実装用の要件として確定します。

元の要望（原文）:

> 保存後の動画をIDE内で開き、前後をカットして保存する。トリミング前の動画とトリミングした動画の2本を保存するオプションを付ける。シークバーで選んだ位置の画面をキャプチャしてPNGで保存する。

用語:

| 用語 | 意味 |
|---|---|
| 保存フォルダ | 「直前N秒を保存」で作られる1回分のフォルダ（`replay-<日時>-<ID>`）。動画・2種類のログ・タップ位置・説明をまとめて持つ |
| 動画内の時刻 | プレイヤーに表示される再生位置（0秒から始まる）。`frames.jsonl`の`movie_pts_us` |
| 記録窓の時刻 | 保存した範囲の先頭を0とする、動画・ログ・タップ共通の時刻。各行の`window_ns` |
| 開始点・終了点 | 切り取って残す範囲の始めと終わり（In/Out） |
| 切り取り | 開始点より前と終了点より後を取り除いた、新しい保存フォルダを作ること |

## 1. 利用場面・頻度・成功の姿

### 誰が・なぜ切り取るか

保存は「不具合が起きた瞬間に直前N秒（最長900秒）を1回押して残す」操作です（[SAVE-01](../requirements.md#保存と表示)、[保持時間の上限](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/ReplayModels.kt#L24-L30)）。押す瞬間には範囲を選べないため、**保存した動画には不具合と関係のない時間が前に長く付く**のが普通です。切り取りはその後始末です。

| 場面 | 主な利用者 | 頻度（推定） | 切り取る理由 | 求めること |
|---|---|---|---|---|
| A. チャットで不具合の瞬間を共有する | アプリ開発者 | 保存ごとに時々 | 30〜900秒のうち要点は数秒。長い動画は相手が見ない | 短く・小さく・どのプレイヤーでも再生できる。切り取った前の画面（個人情報など）が残らない |
| B. 不具合報告（Issue）に添付する | 開発者・テスター | 不具合報告ごと | 再現手順の部分だけ残したい。添付サイズを減らしたい | 動画と同じ範囲のログ・タップ位置が揃って付く。時刻の対応が崩れない |
| C. 待ち時間を削る | 開発者 | 長い保持時間を使う人ほど多い | 起動待ち・操作していない時間を前後から除く | 数秒単位の雑な指定で十分。すぐ終わる |
| D. ある瞬間の画面を画像で残す | 開発者・デザイナー | 時々 | 表示崩れ・一瞬だけ出たエラーをIssueやレビューに貼る | 見ている1コマを元の解像度でPNGにする |
| E. エージェントが保存から必要な部分だけ取り出す | AIエージェント（MCP） | 未知 | ログで見つけた時刻の画面や前後数秒だけを報告に使う | 画面操作なしで同じ処理を使える（[§5.3](#53-climcpとの対応)） |

頻度は利用統計がないため推定です（このプラグインは利用統計を取りません）。[UXの利用場面表](https://github.com/shinma06/android-replay-buffer/blob/devin/1794356235-compose-ux/docs/design/ui-ux.md#L5-L14)の「③ 保存の直後（保存ごと）」に続く操作で、「② 不具合が起きた瞬間」ほど頻繁ではないと考えます。

### 成功の姿

- 保存完了の通知から**1分以内**に、範囲を選んで切り取った動画を保存できる（操作の目標。処理時間は[§7](#7-受入条件)で実測して確定）。
- 切り取った動画は、Android Studioを持たない相手のパソコンの一般的な動画プレイヤーで再生でき、見えるのは選んだ範囲だけ。
- 切り取った保存フォルダのログ・タップ位置は、動画の同じ瞬間を指し続ける。
- 元の保存は、利用者が明示的に「置き換える」を選んで確認しない限り、1バイトも変わらない。
- Linux・macOS・Windowsで同じ入力から同じ結果になる（[AGENTS.md](../../AGENTS.md)）。

## 2. 今の保存に何が入っているか

### 確認方法と限界

- 根拠は保存処理のコード（`SaveWriter.kt`）と設計文書です。
- **このVMには実際の保存フォルダがありませんでした**（`session.json`・`video-001.mp4`を全体検索して0件）。以下はコードから読んだ内容で、実物の観察ではありません。実装時に、実端末・Emulatorで作った保存フォルダで確認します（[AC-01](#7-受入条件)）。
- 依頼文の「`video-*.mp4`の複数ファイル」は、現行の実装には**ありません**。動画は常に1本の`video-001.mp4`で、途中の切れ目は1本の中の「区間（parts）」として表します（下表）。

### 保存フォルダの中身

保存フォルダ名は`replay-<UTC日時>-<UUID>`で、まず`.<名前>.partial`に書いてから完成名へ原子的に改名します（[SaveWriter.kt L170](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/SaveWriter.kt#L170)、[SavePublication.kt L18-L23](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/SavePublication.kt#L18-L23)）。

| ファイル | 内容 | 切り取りへの影響 | 根拠 |
|---|---|---|---|
| `video-001.mp4` | H.264動画1本。接続の切れ目・画面サイズ変更ごとに独立して復号できる「区間」を同じファイル内に並べる。区間の間の既知の空白はMP4のedit list（再生範囲の指定）で表す。**表示範囲より前の画像（decoder preroll）をファイル内部に含むことがある** | 表示されている秒数とファイル内の位置は一致しない。バイト位置で切れない | [L194](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/SaveWriter.kt#L194)、[L277](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/SaveWriter.kt#L277)、[L412](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/SaveWriter.kt#L412)、[L510](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/SaveWriter.kt#L510)、[timeline.md L115](../design/timeline.md#L115)、[L127](../design/timeline.md#L127) |
| `frames.jsonl` | 1行1コマ。区間番号、元PTS、MP4内の位置、**動画内の時刻（`movie_pts_us`〜`movie_end_us`）**、記録窓の時刻（`window_ns`、表示開始・確認済み終端）、時計対応の確認有無（`clock_alignment_known`）、表示の有無（`presented`）、prerollか、誤差 | 動画内の時刻⇔記録窓の時刻の換算表。切り取り範囲をログ側の時刻へ写す唯一の根拠 | [L381-L405](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/SaveWriter.kt#L381-L405) |
| `video-source.bin` | 画面サイズが途中で変わり再エンコードした保存だけ。元の全packet | 切り取り後も`frames.jsonl`のoffset/size/hashと一致させる必要がある | [L190](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/SaveWriter.kt#L190)、[L507](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/SaveWriter.kt#L507) |
| `logcat-device.jsonl` / `logcat-app.jsonl` | 端末全体のログ / 対象アプリのログ。各行に`window_ns`・誤差・元のbyte列 | 範囲外の行を除く。時刻不明の行の扱いが必要 | [L447-L456](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/SaveWriter.kt#L447-L456) |
| `touches.jsonl` | 動画取得中だけ。タップ1sampleごとの端末入力時刻・`window_ns`・**動画内の位置`movie_us`**・座標 | `movie_us`は新しい動画の時刻で付け直す必要がある。表示外・時刻不確定の行は`null` | [L466-L485](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/SaveWriter.kt#L466-L485)、[plugin-design.md L15](../plugin-design.md#L15) |
| `session.json` | schema 3。保存窓の開始/終了、時計、`coverage`（種類ごとの取得状態）、`gaps`（欠落区間）、`parts`（区間一覧）、`video_run_boundaries`、`video_missing_ranges`、`video_tail`（最後の画像を表示保持した未確認末尾）、`touches`、全ファイルのSHA-256（`files_sha256`） | 範囲外のgap・区間を落とし、範囲に掛かるものは切り詰める。ハッシュは新しいファイルで計算し直す | [L557-L629](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/SaveWriter.kt#L557-L629)、[timeline.md L99](../design/timeline.md#L99)、[L119-L121](../design/timeline.md#L119-L121) |
| `README.txt` | 人が読む説明（区間一覧・同期状態・preroll注意など） | 切り取ったこと・元の保存・範囲を追記する | [L489-L522](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/SaveWriter.kt#L489-L522) |

ログ専用モード（`LOG_ONLY`）の保存にはMP4がありません（[L513](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/SaveWriter.kt#L513)）。

### 時刻の対応を保つための要件

今の保存は「動画・端末ログ・アプリログ・タップが**同じ記録窓の時刻**を共有する」ことを中心に作られています（[timeline.md L65](../design/timeline.md#L65)）。切り取りはこの約束を壊してはいけません。

| ID | 要件 |
|---|---|
| TRIM-T01 | 開始点・終了点は**動画内の時刻**（利用者が見ている位置）で指定し、`frames.jsonl`で記録窓の時刻へ換算する。ファイルのバイト位置や「平均fps×秒」で換算しない（可変fps・区間・空白があるため） |
| TRIM-T02 | ログ・タップは、元の保存と**同じ採否規則**で選ぶ。範囲の端からの距離が誤差以内の行は残し、範囲外と証明できる行だけ除く（[timeline.md L63](../design/timeline.md#L63)、[L131](../design/timeline.md#L131)）。時刻不明の行（`window_ns=null`）は除外と断定せず残す |
| TRIM-T03 | 範囲が時計対応不明の区間（`clock_alignment_known=false`）に掛かるときは、その旨を保存前に1行で示し、ログは保守的に残す（多めに残る）。不明を「同期済み」と書き換えない |
| TRIM-T04 | 切り取り後のフォルダは、**通常の保存と同じ形式・同じ読み方**にする。動画内の時刻・記録窓の時刻は新しい開始点を0として付け直し、元の時刻へ戻すための差分（`trim.source_movie_offset_us`・`trim.source_window_offset_ns`）を`session.json`の追加fieldに残す（schemaは3のまま追加。タップ位置と同じ扱い: [plugin-design.md L15](../plugin-design.md#L15)） |
| TRIM-T05 | 範囲内のgap・欠落区間・時刻不明はそのまま残し、範囲外のものだけ除く。範囲に掛かるgapは範囲の端で切り詰める。端が不明（null）のgapはnullのまま。gapを0秒に詰めない |
| TRIM-T06 | タップの`movie_us`は新しい動画で付け直す。元で`null`だった行は`null`のまま |
| TRIM-T07 | `session.json`の`trim`に、元の`save_id`・元フォルダ名・元の`session.json`と`video-001.mp4`のSHA-256・元の動画内時刻での開始/終了・記録窓での開始/終了（不明ならnull）・方式（[§4.2](#42-切り取り方式)）を記録し、元との対応を後から検証できるようにする |

## 3. 操作と画面の流れ

UIは[UX原則](https://github.com/shinma06/android-replay-buffer/blob/devin/1794356235-compose-ux/docs/design/ui-ux.md#L16-L26)（ブランチ`devin/1794356235-compose-ux`）に従い、同じCompose/Jewelで作ります（[同 L53](https://github.com/shinma06/android-replay-buffer/blob/devin/1794356235-compose-ux/docs/design/ui-ux.md#L53)）。

### 3.1 入口

| 入口 | 表示 | 根拠（UX原則） |
|---|---|---|
| ToolWindowの直近の結果 | `保存しました 10:23 · フォルダを開く · 動画を開く` | 切り取りは「③ 保存の直後」に行う。直近の結果1行に結果を表す操作名を1つ足すだけで、パネルは増やさない（「同じ情報を2か所に出さない」「記憶より再認」） |
| 保存完了の通知 | 既存の`フォルダを開く`の隣に`動画を開く` | 通知は保存の詳細を担う場所（[ReplayProjectService.kt L444-L447](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/ide/ReplayProjectService.kt#L444-L447)） |
| IDEで`video-001.mp4`を開く | 同じフォルダに`session.json`（schema 3）があればプレイヤーで開く | 過去の保存も、IDE標準の「ファイルを開く」で迷わず開ける |
| ToolWindowの`…`メニュー | `保存した動画を開く…`（フォルダ選択） | 頻度が低いので主操作に並べない（「頻度の高い操作ほど上に大きく」の裏返し） |

### 3.2 プレイヤー（エディタタブ）

動画は広い面積が必要なので、ToolWindowではなく**エディタのタブ**に開きます。ToolWindowは今の「正常時は1行」の小ささを保ちます。

上から次の順に置きます。

1. **画面:** 選んだ位置の1コマ。縦横比を保って枠に収める。
2. **シークバー:** 再生位置のつまみと、開始点・終了点の2つの旗。範囲外は暗くする。欠落区間は斜線＋文字`欠落`、時計対応不明は点線＋文字`時刻不明`で示す（「色だけに頼らない」）。タップがあった位置に小さな目盛りを付け、不具合の瞬間を探しやすくする。
3. **操作の列:** `◀ 1コマ戻る`、`▶ 再生 / ⏸ 停止`、`1コマ進む ▶`、`ここを開始にする`、`ここを終了にする`、`この画面をPNG保存`。
4. **選択の要約と主ボタン:** `0:05.200〜0:12.000（6.8秒）を残す` と主ボタン`切り取って保存`（ボタン名に結果を書く）。範囲が未選択なら主ボタンを無効にし、理由を下に1行で出す。
5. **詳細（初期は畳む）:** 現在コマの区間番号・記録窓の時刻・時計対応・範囲内のログ行数とタップ数・出力の見積りサイズ。（「段階的な開示」「⑥ 診断」）

| 操作 | 内容 | 根拠 |
|---|---|---|
| シーク | つまみのドラッグ・クリックで、その時刻に表示されるコマ（`movie_pts_us ≤ t < movie_end_us`）を出す | 動画内の時刻を正本にする（TRIM-T01） |
| コマ送り | `←`/`→`で前後の**表示されたコマ**へ移る。固定秒数ではない（可変fpsのため、[timeline.md L76](../design/timeline.md#L76)）。`Shift+←/→`で1秒 | 不具合の1コマを狙えるようにする |
| 開始点・終了点 | `I`/`O`キーまたはボタン。開始点はコマの表示開始、終了点はコマの表示終了に合わせる（コマの途中で切らない） | 一般的な動画編集ソフトと同じキー。端の1コマが半端に残らない |
| 再生 | 確認用。滑らかさより位置の正確さを優先し、復号が追いつかないときはコマを飛ばして時刻に合わせる | [§4.1](#41-ide内での再生) |
| 切り取って保存 | 背景処理で進捗と`中止`を表示。終わったら通知 `切り取った動画を保存しました · フォルダを開く · 動画を開く`。プレイヤー下部にも1行 | 既存の保存通知と同じ形 |
| 元の扱い | 既定は「元も残す」。置き換えは明示の選択と確認画面のあとだけ（[§5.1](#51-元の保存を守る)、[決めること1](#8-利用者に決めてほしいこと)） | 「データの削除で止まる」（[AGENTS.md](../../AGENTS.md)）、[SAVE-04](../requirements.md#保存と表示) |
| PNG保存 | 今表示しているコマを元の解像度でPNGにする。通知はスクリーンショットと同じ`保存場所を開く`・`削除`（[ReplayProjectService.kt L281](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/ide/ReplayProjectService.kt#L281)） | 同じ種類の結果は同じ見せ方 |
| 失敗 | 目立つ枠に理由・`再試行`・`別の場所に保存…`。元の保存は変えない | 既存の保存失敗表示と同じ（[ui-ux.md L34](https://github.com/shinma06/android-replay-buffer/blob/devin/1794356235-compose-ux/docs/design/ui-ux.md#L34)） |

### 3.3 出力の場所と名前（推奨案）

| 出力 | 場所 | 名前の例 | 理由 |
|---|---|---|---|
| 切り取った保存 | 元の保存フォルダと**同じ親フォルダ** | `replay-2026-11-11T01-20-00.123Z-<元ID>-trim-0m05.200s-0m12.000s-<8桁ID>` | 元と並んで見つけやすい。同じ場所なら既存の原子的改名がそのまま使える。元フォルダの中には書かない |
| 動画内の画面PNG | スクリーンショットの保存先（[SHOT-02](../requirements.md#第2版の追加要件スクリーンショット)） | `frame-<元の保存名の日時>-0m07.533s-<UUID>.png` | 画像は画像の置き場に集める。付属ファイルは作らず、元の保存・動画内時刻・記録窓の時刻はPNGのテキスト情報（tEXt）に入れる |

端末の「今の画面」を撮る既存のスクリーンショット（`screenshot-<日時>-<UUID>.png`、[ScreenshotSave.kt L90-L92](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/ScreenshotSave.kt#L90-L92)）とは、名前の先頭で区別します。

## 4. 技術の選択肢

### 4.0 プラグインとIDEがすでに持っているもの（確認結果）

Android Studio `2026.2.2.8`（プラグインの対象版、[build.gradle.kts L52](../../plugin/build.gradle.kts#L52)）のLinux版公式配布物（`android-studio-rabbit2-linux.tar.gz`、[公式リリース一覧](https://jb.gg/android-studio-releases-list.xml)から取得。build `AI-262.10968.63.2622.16730706`）を展開して調べました。**macOS版・Windows版の中身は未確認**です。

| 部品 | 有無 | 確認内容 |
|---|---|---|
| JCodec 0.2.5（純Java、H.264の復号・符号化・MP4読み書き） | **プラグインが同梱済み** | [build.gradle.kts L56](../../plugin/build.gradle.kts#L56)。画面サイズ混在時の復号→再エンコードに既に使用（[MixedVideoNormalizer.kt L3-L4](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/MixedVideoNormalizer.kt#L3-L4)、[timeline.md L162](../design/timeline.md#L162)） |
| Compose / Jewel | **IDE同梱** | `lib/modules/intellij.platform.compose.jar`、`intellij.platform.jewel.ui.jar`、skiko（描画）など。UXブランチが`composeUI()`で利用 |
| 動画再生の部品（Compose/Jewel） | **なし** | 同梱moduleは画像・Markdown向けで、動画プレイヤーはない |
| JCEF（Chromium） | **このLinux配布物にはなし** | `libcef`やJCEF用runtimeが見つからず、JBR（Java 25.0.4）のmodule一覧にもない。公式文書は「2026.2からWeb Browser (JCEF) pluginが提供し、明示依存が必要」「`JBCefApp.isSupported()`で確認し、非対応時の代替を用意する」と定める（[JCEF公式文書](https://plugins.jetbrains.com/docs/intellij/embedded-browser-jcef.html)）。H.264などの特許codecの再生は未対応の課題が未解決（[JBR-6382](https://youtrack.jetbrains.com/issue/JBR-6382)、Open） |
| JavaFX（Media） | **なし** | JBRのmodule一覧にない。公式文書では3rd partyプラグインのJavaFX利用は2020.2から非推奨、JavaFX Runtime for Pluginsは2025.1から提供終了（同上） |
| FFmpeg 7.1.1（bytedeco JavaCPP 1.5.12） | **Androidプラグインの内部に同梱** | `plugins/android/lib/ffmpeg-7.1.1-1.5.12*.jar`。Running DevicesのH.264表示が使う内部部品（`com.android.tools.idea.streaming.device.VideoDecoder`）。公開APIではなく、版はAndroid Studioの更新で変わり得る。Linux版にはLinux用nativeだけが入る。ビルド設定は`--enable-version3 --enable-libopenh264`等で`--enable-gpl`は見当たらない（ライセンス区分の確定は未調査）。[JavaCPP Presets FFmpeg](https://github.com/bytedeco/javacpp-presets/tree/master/ffmpeg) |
| ffmpeg・Pythonなどの外部インストール | **前提にしない** | [project.md L8](../project.md#L8) |

### 4.1 IDE内での再生

| 案 | 仕組み | 3 OSで同じ動作 | 依存の追加 | 正確なコマ表示 | 評価 |
|---|---|---|---|---|---|
| **P1. Compose＋JCodec（推奨）** | JCodecで必要なコマだけ復号し、Composeの画像として表示 | ◎ 純Java。OS別処理なし | なし（両方とも既存） | ◎ `frames.jsonl`と同じ時刻表で選べる | 再生の滑らかさは未測定（1920px・30fpsでの復号速度を試作で測る）。シーク・コマ送り・PNGは同じ復号で済む |
| P2. JCEF（HTML5 video） | 埋め込みChromiumでMP4を再生 | ✕ Linux版に同梱なし。H.264再生も保証されない | Web Browser pluginへの依存 | △ ブラウザの時刻はコマ単位でない | 不採用 |
| P3. JavaFX Media | JavaFXのMediaPlayer | ✕ OSごとのnativeを自前同梱 | 大（OS別native） | △ | 不採用（公式に非推奨） |
| P4. Androidプラグイン内部のFFmpeg | 内部APIを呼んで復号 | △ 配布物ごとに中身が違い得る | 公開APIでない内部部品 | ◎ | 不採用（Android Studio更新で壊れる。OS差の検査も難しい） |
| P5. OSの動画プレイヤーで開く | 既存の`フォルダを開く`の延長 | ✕ OSごとにプレイヤーが違う | なし | ✕ 範囲指定ができない | 確認用の補助としてのみ（`フォルダを開く`で既に可能） |

### 4.2 切り取り方式

前提: 録画はI-frame（単独で復号できるコマ）を約1秒ごとに要求していますが、端末がそれより長くすることがあります（[DeviceCapture.kt L151](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/DeviceCapture.kt#L151)、[verification/README.md L135](../verification/README.md#L135)）。開始点の直前のI-frameより前には戻れないため、方式によって「正確さ」「速さ」「元の画面が残るか」が変わります。また既存設計は、**開始点を次のI-frameへ黙って丸めることを禁止**しています（[timeline.md L127](../design/timeline.md#L127)）。

| 方式 | 開始点の正確さ | 速さ | 画質 | 切った前の画面がファイル内に残るか | 一般プレイヤーでの互換 | 実装 |
|---|---|---|---|---|---|---|
| K1. コピー＋edit list | ◎ 表示はコマ単位 | ◎ 最速（復号なし） | ◎ 元のまま | **残る**（直前I-frameからの最大1GOP、通常約1秒・それ以上もあり得る） | △ edit listを無視するプレイヤーでは前の画面が一瞬映る。既存保存と同じ未確認事項（[SaveWriter.kt L510](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/SaveWriter.kt#L510)） | 既存の保存と同じ処理の再利用 |
| **K2. 先頭GOPだけ再エンコード（推奨）** | ◎ | ○ 再エンコードは先頭の1GOP分だけ | ○ 先頭の数コマだけJCodec（高画質設定）で作り直し、残りは元のまま | **残らない** | ○ edit listに頼らない。区間ごとに設定が変わるMP4は既存保存でも使う形（[timeline.md L27](../design/timeline.md#L27)） | JCodecの既存符号化処理を流用。区間をまたぐ設定切替の検証が必要 |
| K3. 全体を再エンコード | ◎ | ✕ 範囲の長さに比例して遅い | △ 全体が作り直し（現行設定はQP12で、ファイルが大きくなりやすい） | 残らない | ◎ | 既存処理の流用。最長900秒では長時間・高負荷 |
| K4. I-frameへ丸めてコピー | ✕ 最大1GOPずれる | ◎ | ◎ | 残らない | ◎ | **既存規則で禁止**（利用者の選んだ範囲を黙って変えるため） |

どの方式も、ffmpegなど利用者の環境の追加インストールは不要です（JCodecはプラグイン同梱）。

### 4.3 画面のPNG保存

| 案 | 内容 | 評価 |
|---|---|---|
| **F1. JCodecで復号して保存（推奨）** | 表示中のコマを直前のI-frameから復号し、元の解像度でPNGにする。プレイヤー表示と同じ画像 | 依存追加なし。3 OSで同じ |
| F2. 画面表示のキャプチャ | プレイヤー部品を画像化 | ✕ 表示倍率・HiDPIで解像度が変わる |
| F3. 端末で撮り直す | 既存スクリーンショット | ✕ 過去の瞬間は撮れない（別機能として残す） |

色は端末の色指定が不明な場合があるため、「元と色が一致する証明ではない」ことは既存の変換と同じ扱いにします（[SaveWriter.kt L507](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/SaveWriter.kt#L507)）。タップ位置の点は、端末で表示設定をONにしたとき動画に焼き込まれたものだけが写ります（[plugin-design.md L17](../plugin-design.md#L17)）。

### 4.4 3 OSで同じ動作にするために

- 復号・符号化・MP4の読み書き・PNG化はすべて純Java（JCodec・JDK標準のPNG書き出し）で行い、OSごとの分岐を作らない。
- ファイルの作成・改名・削除は既存の`HostPlatform.saveHandles`（`PosixSaveHandles` / `WindowsSaveHandles`）だけを通す（[AGENTS.md](../../AGENTS.md)、[SaveOutputHandles.kt L14](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/SaveOutputHandles.kt#L14)）。
- 同じ入力・同じ開始点/終了点なら、名前のIDと日時を除き**出力ファイルがOS間でバイト単位で一致**することを`os-parity`の検査に加える。

## 5. データの保全と安全

### 5.1 元の保存を守る

| ID | 要件 |
|---|---|
| SAFE-01 | 切り取りは元の保存フォルダに書き込まない。元の中身は読むだけ |
| SAFE-02 | 切り取りの前に、元の`session.json`の`files_sha256`と実ファイルを照合する。一致しない（後から編集された・壊れた）場合は理由を示して止める |
| SAFE-03 | 出力は既存の保存と同じく`.<名前>.partial`に書き、内容を検証してから`renameExclusive`で完成名へ**上書きなしで**公開する（[SavePublication.kt L18-L23](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/SavePublication.kt#L18-L23)）。途中で失敗・中止したら未完成の`.partial`だけを片付け、完成フォルダは作らない |
| SAFE-04 | 「置き換える」は、切り取った保存の公開と検証が成功した**後**にだけ元を消す。消す前に確認画面で消えるフォルダ名と中身（動画・ログ）を示す。削除は既存スクリーンショット削除と同じ「隔離してから消す」手順を使う（[ScreenshotSave.kt L27-L43](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/ScreenshotSave.kt#L27-L43)） |
| SAFE-05 | 置き換えはフォルダ単位。元フォルダの中の`video-001.mp4`だけを差し替えることはしない（ログ・`frames.jsonl`・ハッシュと食い違うため） |
| SAFE-06 | 切り取り中も取得（録画）は止めない。切り取りの負荷が原因で取得に欠落が出ないことを確認する |
| SAFE-07 | 開けるのは完成済み（`.partial`でなく、`session.complete=true`）の保存だけ |

### 5.2 空き容量と大きなファイル

| ID | 要件 |
|---|---|
| SIZE-01 | 開始前に出力サイズを見積もり（範囲内の動画＋先頭GOP＋ログ＋16 MiBの余裕）、既存と同じ下限128 MiBを残せなければ始めない。書き込み中も既存の`checkSpace`と同じ確認を続ける（[SaveWriter.kt L163-L169](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/SaveWriter.kt#L163-L169)、[ReplayModels.kt L32-L35](../../plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/ReplayModels.kt#L32-L35)） |
| SIZE-02 | 元の動画は最大約1 GiB・60,000 packet（同上）。ファイル全体をメモリに読み込まず、区間ごとに読み書きする |
| SIZE-03 | 処理はIDEの背景タスクで行い、進捗・中止を出す。IDEの画面を固めない |
| SIZE-04 | 保存先の親フォルダに書けない・容量不足のときは`別の場所に保存…`を案内する。別の場所でも元は変えない |

### 5.3 CLI・MCPとの対応

今のCLI（`replay start|stop|save|status|screenshot|settings|mcp`）とMCP（`save_replay`、`take_screenshot`ほか）はGUIと同じ処理を共有し、3 OSで同じ動作を約束しています（[cli.md L36-L48](../cli.md#L36-L48)、[mcp.md L47-L48](../mcp.md#L47-L48)）。切り取りの扱いは[決めること6](#8-利用者に決めてほしいこと)で選びます。どの案でも次を守ります。

- 切り取り・PNG化の中核処理は画面に依存しない形で作り、IDE・CLI・MCPが同じ処理を呼べるようにする。
- 追加するなら既存コマンド・終了コードを変えない追加にする（[AGENTS.md](../../AGENTS.md)）。案: `replay trim <保存フォルダ> --from 5.2 --to 12.0`（stdoutに新フォルダのパス）、`replay frame <保存フォルダ> --at 7.533`（PNGのパス）、MCPは`trim_replay`・`export_frame`。
- CLI・MCPからは「置き換え（元の削除）」をできなくする。確認画面のない経路で保存を消さないため。

## 6. 対象外

| 対象外 | 理由・扱い |
|---|---|
| 保存する前に範囲を選ぶ（バッファ上での切り取り） | 別の機能（[SAVE-01](../requirements.md#保存と表示)の後続TODO）。本機能は保存済みフォルダが対象 |
| 複数範囲の切り出し・つなぎ合わせ・複数保存の結合 | 時刻の対応が複雑になる。必要なら切り取りを繰り返す |
| 拡大縮小・切り抜き・圧縮率の変更・GIF/WebMへの変換 | 画質・形式の機能は別Trail |
| ぼかし・塗りつぶし・文字入れ（個人情報の隠し） | 共有時の需要はあるが別Trail。切り取りで範囲外の画面が残らないことまでを扱う |
| 音声 | 録画自体が音声を持たない（[VIDEO-02](../requirements.md#映像の範囲)） |
| プレイヤー上でのログ表示・ログの編集 | 範囲内のログ行数を詳細に出すまで。ログの閲覧は既存のテキスト/IDEで行う |
| ログ専用（動画なし）の保存の時間範囲切り出し | プレイヤーがないため対象外 |
| schema 3より前の保存・手で編集された保存 | SAFE-02で理由を示して開かない |
| 外部サービスへのアップロード | 対象外 |
| 置き換えの取り消し | 確認画面で防ぐ。消したものの復元は提供しない |

## 7. 受入条件

| ID | 条件 |
|---|---|
| AC-01 | 実端末またはEmulatorで作った、①途切れのない保存、②接続の切れ目（複数区間・gap）を含む保存、③画面サイズ変更（`video-source.bin`あり）を含む保存、④タップ記録ありの保存、の4種でプレイヤーが開き、シーク・コマ送り・開始点/終了点の指定ができる |
| AC-02 | 切り取った動画の最初と最後のコマが、プレイヤーで選んだコマと一致する（`frames.jsonl`で照合）。I-frameへの丸めがない |
| AC-03 | 採用方式でファイル内に開始点より前の画像が含まれない（K2/K3の場合）。K1の場合はその旨がREADMEと保存前の表示にある |
| AC-04 | 切り取った保存の端末ログ・アプリログ・タップは、元の保存の同じ行と一致し、範囲の端の採否が元と同じ規則に従う。時刻不明の行は失われない |
| AC-05 | 範囲内のgap・時刻不明・区間が`session.json`とREADMEに残り、範囲外のものは残らない。端が不明なgapはnullのまま |
| AC-06 | 切り取った動画がVLC・各OS標準プレイヤー・Chromeで再生でき、選んだ範囲だけが映る（実施したプレイヤーとOSを記録する） |
| AC-07 | 元の保存フォルダの全ファイルのSHA-256が切り取りの前後で変わらない（置き換えを選ばない場合） |
| AC-08 | 置き換えでは、確認画面なしに元が消えない。新しい保存の公開・検証に失敗したら元は残る |
| AC-09 | 中止・容量不足・IDE終了で、完成フォルダが中途半端に残らない（`.partial`のみ、次回片付け） |
| AC-10 | PNGが元の解像度で、選んだコマと同じ画像になる。スクリーンショットの保存先に上書きなしで保存され、通知から開く・削除ができる |
| AC-11 | 同じ入力・同じ範囲の出力が、Linux・macOS・Windowsでバイト単位で一致する（名前のID・日時を除く）。CIの`os-parity`で確認する |
| AC-12 | 最長（900秒・約1 GiB）の保存で、メモリ使用が上限内に収まり、切り取り中も取得が続く。処理時間を測定して記録する |
| AC-13 | （CLI/MCPを含める場合）CLI・MCPの結果がIDEと同じ出力になる |

テストの合格とIDE・端末での受入は別に記録します（[AGENTS.md](../../AGENTS.md)）。

### 試作で先に確かめること

| 項目 | 理由 |
|---|---|
| JCodecの復号速度（1920px・30fps・Mainなど端末の実出力） | P1の再生の滑らかさとシークの待ち時間が決まる |
| K2の先頭GOP再エンコードと元のコマを1本につないだMP4が、各プレイヤーで再生できるか | 推奨案の成立条件 |
| macOS版・Windows版Android Studio 2026.2.2.8の同梱物 | Linux版しか確認していない |

## 8. 利用者に決めてほしいこと

| # | 決めること | 選択肢 | 推奨 |
|---|---|---|---|
| 1 | 元の保存の扱い | (a) 既定で元も残し、置き換えは確認付きの明示操作 / (b) 既定は置き換え、「元も残す」オプション（原案の書き方） / (c) 常に両方残す（置き換え機能なし） | **(a)**。元の要望の「2本保存するオプション」を満たしつつ、うっかり証拠を消す事故を防ぐ。保存済みデータは自動削除しない既存方針（SAVE-04）とも合う |
| 2 | 切り取り方式 | (a) K1 コピー＋edit list（最速、前の画面が最大1GOP残る） / (b) K2 先頭GOPだけ再エンコード / (c) K3 全体を再エンコード | **(b)**。選んだコマで正確に切れ、切った前の画面がファイルに残らず、処理は速い。試作でプレイヤー互換が確認できなければ(a)に戻し、前の画面が残ることを表示する |
| 3 | 切り取った保存の置き場所 | (a) 元と同じ親フォルダに`<元の名前>-trim-<開始>-<終了>-<ID>` / (b) 元フォルダの中の`trims/` / (c) 設定の保存先フォルダ | **(a)**。元と並んで見つかり、元フォルダを一切変えずに済む |
| 4 | 動画内の画面PNGの置き場所 | (a) スクリーンショットの保存先 / (b) 元の保存フォルダと同じ親フォルダ / (c) 毎回選ぶ | **(a)**。画像の置き場を1か所にまとめる。名前は`frame-…`で端末スクショと区別する |
| 5 | IDE内の再生方式 | (a) Compose＋JCodec（純Java） / (b) JCEF（埋め込みブラウザ） / (c) 再生は外部プレイヤーに任せ、IDEはコマ表示と範囲指定だけ | **(a)**。3 OSで同じ動作になり、依存を増やさない。(b)はLinux版Android Studioに同梱がなくH.264再生も保証されない |
| 6 | CLI・MCPでも切り取り・PNG化をできるようにするか | (a) IDEだけ / (b) 最初からIDE・CLI・MCP同時 / (c) IDEを先に出し、同じ中核処理でCLI・MCPを次のTrailで追加 | **(c)**。最初の版の検証範囲を絞りつつ、中核処理は最初から共通にして後で差が出ないようにする。CLI・MCPから置き換えはできなくする |
