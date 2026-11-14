# メニューバー操作の要件（Trail #24）

Android Studioを前面に出さずに、macOSのメニューバー（Windowsの通知領域、Linuxのトレイ）から取得・保存などを操作できるようにするための要件です。**この文書は要件フェーズの成果物で、状態は「要件確定（実装前）」です。製品コードはまだ書かず、実装は利用者の承認を得てから別の作業で行います。** 決定が必要だった点は、末尾の「[確定した判断](#確定した判断)」に確定済みの内容としてまとめています。

用語:

- **メニューバー:** macOSのメニューバー右側のアイコン（Appleの呼び名は「menu bar extra」）。この文書ではWindowsの通知領域、Linuxのトレイも含めて「メニューバー」と呼びます。
- **取得:** 端末の画面とログを記録し続け、直前N秒を保存できる状態にすること。
- **project:** Android Studioで開いているアプリのフォルダ。設定や取得はprojectごとです。

根拠の表記: リポジトリの根拠は `develop`（`5af6ac26` 時点）の **ファイル名と型・関数名** で示し、腐りやすい行番号リンクは置きません。GUIの原則は、`develop` に取り込み済みの `docs/design/ui-ux.md`（Trail #21）を使います。OSの仕様は公式ドキュメントと公式ソースだけを根拠にし、確認できなかった点は「未確認」と書きます。

## 0. 前提（今の製品の事実）

| 事実 | 根拠 |
|---|---|
| メニューバー連携は、IDE内の初期版の次に最初に着手する予定。「前面にないAndroid Studioを操作でき、保存・有効/無効・状態確認など、**ほとんどの操作**を行う」。IDE終了後の単独動作は未決で、要件へ自動追加しない。 | `docs/requirements.md` の開発順序と、TODO表の「macOSメニューバー連携 #15」 |
| 本書は #15 の「ほとんどの操作」から、**頻度が高く急ぐ操作と状態の表示に範囲を縮小**したものです（2章の棚卸し）。範囲縮小であることをここで明示します。 | 本書2章 |
| 開発順序は「macOS最優先 → メニューバー連携 → Windows、Linuxの順で広げる」で、「初期版をmacOS専用構造へ固定しない」。したがって本機能も **コードは全OS共通の規則で実装し、受入・実画面確認の優先はmacOSを先行し、Windows・Linuxはその順** とします。 | `docs/requirements.md` の開発順序 |
| GUI・CLI・MCPは、IDE内の同じ設定・記録バッファ・保存処理を使う。CLIは独自に録画・保存せず、IDEを起動もしない。IDEを終了すると取得も終了する。 | `docs/cli.md` 冒頭 |
| 操作の入口は、ToolWindow、設定画面、Running Devicesの3ボタン（取得ON/OFF・直前N秒を保存・スクリーンショット）、Logcatの「直前のログを保存」、CLI、MCP。どれも `ReplayProjectService` の同じ操作を呼ぶ。 | `META-INF/plugin.xml`、`ui/RunningDevicesActions.kt`（`ReplayCaptureToggleAction` / `ReplaySaveAction` / `ReplayScreenshotAction` / `ReplayLogSaveAction`）、`docs/design/ide-ui.md`「Running Devicesの操作ボタン」「Logcatのログ専用保存」 |
| 1プロジェクト・1台の利用が前提（ENV-01/ENV-02）。複数projectの同時利用は初期版では検討対象外。複数端末が接続されていても取得するのは選んだ1台だけで、勝手に選ばない。 | `docs/requirements.md` ENV-01/ENV-02 |
| 取得をOFFにすると、未保存のバッファは破棄される（「一時停止」はない）。 | `docs/requirements.md` 無効化の行 |
| CLIは、接続先が複数あるときは前面のwindowや作業フォルダから推測せず失敗する。 | `docs/cli.md` の `--project` の説明、`cli/ReplayCliEndpoint.kt` の `select` |
| 状態は、取得 `DISABLED / WAITING / CAPTURING / RECOVERING / PARTIAL / MULTIPLE_DEVICES`、保存 `IDLE / WRITING / FAILED / COMPLETED / CANCELLED`、クラッシュ自動保存 `OFF / WAITING / ARMED / SUSPENDED` で表される。保存の内容は `SaveContent`（`ALL` / `LOGS_ONLY`）、要求元は `SaveOrigin`（`TOOL_WINDOW` / `RUNNING_DEVICES` / `LOGCAT_TOOLBAR` / `CLI` / `CRASH_AUTOSAVE`）で区別され、`replay.json` の `save_request` に記録される。メニューの有効・無効判定には `ReplaySnapshot` の `canSave` / `saveDisabledReason` / `canSaveLogs` / `saveLogsDisabledReason` / `canSave(content)` / `saveDisabledReason(content)` がある。 | `core/ReplayModels.kt`（`CaptureState` / `SavePhase` / `AutoSaveState` / `SaveContent` / `SaveOrigin` / `ReplaySnapshot`）、`docs/design/ide-ui.md`「Logcatのログ専用保存」 |
| OSで動作を変えない。OS固有のコードは、`core/HostPlatform.kt` が選ぶOS別の実装の中だけに置く。現在 `HostPlatform` が選ぶのはファイル保存向けの `saveHandles`（`PosixSaveHandles` / `WindowsSaveHandles`）と `privateFiles`（`PosixPrivateFiles` / `WindowsPrivateFiles`）だけで、トレイ等のUIのOS別実装はまだない。 | `AGENTS.md`、`core/HostPlatform.kt` |
| project設定は `ReplaySettingsStore`（`@Service(Service.Level.PROJECT)`、`Storage(WORKSPACE_FILE)`）に保存され、開くprojectごとに別になる。IDE全体（Applicationレベル）の設定置き場は今のコードにはない。 | `settings/ReplaySettingsStore.kt` |

## 1. 利用場面：メニューバーが他の入口より優れる場面

`docs/design/ui-ux.md` の「利用場面と頻度」（①〜⑥）に、「Android Studioが前面にない」という条件を重ねて考えます。

| 場面 | 頻度 | ToolWindow / Running Devices | CLI / MCP | メニューバー |
|---|---|---|---|---|
| A. 実機を手に持って操作確認中。IDEはブラウザや仕様書の後ろ | 開発中は毎日・長時間 | IDEを前面に出し、パネルを探す必要がある | ターミナルを前面に出してコマンドを打つ必要がある | 画面上端（タスクバー）から1〜2クリックで保存。他のwindowの並びを崩さない |
| B. 不具合が起きた瞬間（場面②） | ときどき・最重要 | IDEの切り替えに数秒かかり、その間も古い記録は押し出されていく | コマンド入力に時間がかかる | 最短で「直前N秒を保存」を押せる |
| C. エミュレータを全画面・別Spaceで操作中 | エミュレータ利用時はよくある | Spaceを切り替える必要がある | 同左 | macOSの全画面では、メニューバーはポインタを画面上端に寄せると出る（後述）。Spaceを離れずに操作できる |
| D. 取得が正常に回っているかを見たい（場面①） | 常時（ほぼ見ない） | IDEの前面でパネルが開いているときだけ見える | `status` を打てば分かる | アイコンの形で常に分かる |
| E. 保存できたか確かめ、フォルダを開く（場面③） | 保存ごと | 通知とパネルで分かる。IDEが後ろだと見落とす | 出力のパスを使う | 直近の結果をメニューに1行。フォルダを開ける |
| F. 異常の調査・再試行（場面④）、設定（場面⑤）、診断（場面⑥） | まれ | 最適（詳細・入力欄・ダイアログがある） | スクリプト向け | 向かない。IDEのToolWindow・設定画面を開く導線だけを置く |
| G. エージェントやスクリプトからの自動操作 | 用途次第 | 向かない | 最適 | 向かない（人が押す前提） |

結論: メニューバーが勝つのは **A〜Eの「IDEが前面にない状態で、頻度が高く急ぐ操作と、一目で見る状態」** だけです。F・Gは既存の入口が優れているので、メニューバーには導線だけを置きます。これが、#15 の「ほとんどの操作」から縮小した本機能の操作範囲です（確定D2）。

## 2. 今ある操作と状態の棚卸しと、メニューバーに置くか

判定の記号: **◎** メニューに操作として置く、**○** 表示または他画面への導線だけ置く、**×** 置かない。

判定に使う原則（`docs/design/ui-ux.md`「原則と根拠」）:

- **頻度:** 頻度の高い操作ほど上に置く。
- **例外による管理:** 正常時は1行、異常時だけ展開する。
- **段階的な開示:** 詳しい情報は畳む。消さない。
- **色だけに頼らない:** 状態はアイコン（形）と文字を重ねて示す（WCAG 1.4.1）。
- **記憶より再認:** ボタン名に結果を書く。無効のときは理由を出す。
- **重複させない:** 保存完了の詳細は通知に任せる。

加えて、Appleの指針「メニューバーからしか使えない機能を作らない」（[Apple HIG: The menu bar](https://developer.apple.com/design/human-interface-guidelines/the-menu-bar)）と、GNOMEの指針「すべての機能をアプリのwindowから使えるようにし、状態アイコンのメニューだけにある操作を作らない」（[GNOME Status Icon Migration Guidelines](https://wiki.gnome.org/Initiatives/StatusIconMigration/Guidelines)）に従い、**メニューバーにしかない操作は作りません。**

### 2.1 ToolWindow（`docs/design/ui-ux.md`「ToolWindow」）

| 項目 | 判定 | 理由 |
|---|---|---|
| 状態の見出し（`● 取得中 · 端末名 · package`） | ○ | 場面Dで最も見たい情報。アイコンの形と、メニュー先頭の1行の文字で示す（色だけに頼らない）。 |
| 取得のON/OFF | ◎ | 場面Aで「これから試す」ときに頻繁に使う。OFFは未保存の記録を破棄するので、文言に結果を書く（記憶より再認）。確認は置かずToolWindow・Running Devicesと揃える（確定D4）。 |
| 直前N秒を保存（主ボタン） | ◎ | 最重要・最頻の操作（場面②）。メニューの最上部に置き、項目名に秒数を入れる（例「直前180秒を保存」）。保存できないときは無効にして、理由を項目名に添える。判定は `ReplaySnapshot` の `canSave` / `saveDisabledReason` を使う。 |
| スクリーンショット | ◎ | ToolWindowとRunning Devicesで常設の主な操作（`docs/design/ide-ui.md`）。急ぐ場面でも使う。有効・無効と理由は `ReplayProjectService` の `screenshotDisabledReason` と揃える。 |
| Logcatを開く／閉じる | × | IDEの中のパネルを切り替える操作で、IDEが前面にないときは意味がない。 |
| 取得する端末の選択欄 | × | 2台以上が関係するときだけ出る注意欄（ENV-02）。端末の切替は未保存の記録の破棄やserial記憶を伴うため、状況と対象をまとめて見られるToolWindowで行う。複数端末のときはメニューの注意1行と導線に留める。 |
| 直近の結果（成功）`保存しました 時刻 · フォルダを開く` | ◎ | 場面Eで、IDEを出さずに保存先を開ける。ToolWindowと同じく1行だけにし、詳細は通知に任せる（重複させない）。 |
| 直近の結果（失敗）と「同じ対象を再試行」「別の保存先で再試行…」「未保存の対象を破棄」 | ○ | 失敗は「保存に失敗しました — Android Studioで確認…」の1行と、ToolWindowを開く導線だけ置く。再試行先の選択はフォルダ選択ダイアログが要り、破棄は取り消せないので、理由と対象をまとめて見られるToolWindowで行う（段階的な開示）。 |
| 注意（取得エラー、アプリ未解決、複数端末、反映待ち、片付け待ち） | ○ | 異常のときだけ1行出す（例外による管理）。「設定を開く…」「Android Studioで確認…」などの導線を添える。 |
| 詳細（各記録の状態と秒数、区間、不足区間、保存先、クラッシュ自動保存の状態、SDK、記録セッション、保存対象） | × | 場面⑥はほぼない。詳しく見るのはToolWindow。メニューに出すと最重要の操作が埋もれる。 |
| 設定への導線（「設定…」） | ○ | 導線だけ置く。開くとAndroid Studioが前面に出ることを許容する（設定は場面⑤でまれ）。 |
| ToolWindow自体を開く | ○ | 「Android Studioで表示」として置く。F（調査）への唯一の入口になる。 |

### 2.2 設定画面（`docs/design/ui-ux.md`「設定画面」、`docs/mcp.md` の `update_settings` の項目一覧）

| 項目 | 判定 | 理由 |
|---|---|---|
| 保存先、保持時間、対象アプリ（自動／手動・package名） | × | 最初の1回・まれ（場面⑤）。入力と検証が要り、設定画面の「適用」と同じ検証をする必要がある。 |
| 取得モード（`VIDEO_AND_LOGS` / `LOG_ONLY`）、動画品質 | × | 取得OFFでしか変えられない制約がある（`docs/cli.md` の設定の読み書き）。メニューで変えると、なぜ変えられないかの説明が要る。 |
| タップ位置の表示（`tap_display`） | × | 同上。取得OFFでの変更で、選択肢（OFF/VIDEO/DEVICE）の意味の説明が要る。設定画面で行う。 |
| 端末の再生音の録音（`record_audio`） | × | 同上。取得OFFでの変更で、Android 13以上・`VIDEO_AND_LOGS`限定の制約がある。設定画面で行う。 |
| 取得する端末（`device`） | × | ToolWindowの選択欄と同じ理由（2.1）。serialを選ぶ操作はメニューには置かない。 |
| スクリーンショットの保存先 | × | まれ。フォルダ選択が要る。 |
| クラッシュ時の自動保存のON/OFF | × | まれ。状態（待機中・停止中など）は、異常のときだけ注意の1行に出す。 |
| CLIの場所とコピー | × | ターミナル・エージェント向けの準備。メニューバーの利用者と場面が違う。 |
| （新規）メニューバーに表示するか | 設定画面に置く | Appleは「メニューバーに出すかは、アプリではなく利用者が決める」としている（[Apple HIG](https://developer.apple.com/design/human-interface-guidelines/the-menu-bar)）。既定はOFF（確定D3）。**IDE全体の設定として保存し、projectの `ReplaySettingsStore` には置かない**（7章 実装契約）。 |

### 2.3 Running Devicesの3ボタン、Logcatのボタン、通知の操作

| 項目 | 判定 | 理由 |
|---|---|---|
| 取得ON/OFF・直前N秒を保存・スクリーンショット（`ui/RunningDevicesActions.kt` の `ReplayCaptureToggleAction` / `ReplaySaveAction` / `ReplayScreenshotAction`） | ◎ | 同じ3操作をメニューにも置く。有効・無効の規則と理由は、これと同じ判定を使う（`docs/design/ide-ui.md`「Running Devicesの操作ボタン」）。 |
| Logcatの「直前のログを保存」（`ReplayLogSaveAction`、`SaveContent.LOGS_ONLY`） | × | 動画込みの「直前N秒を保存」を主操作に絞るため（確定D2）。ログのみ保存は調査向けの操作で、Logcatツールバー・CLI `save --logs-only`・MCP `logs_only` に残る。`LOG_ONLY` 取得モードでは、メニューの「直前N秒を保存」がそのままログの保存になる（保存可能かは `canSave(content)` が判定）。 |
| 保存完了通知の「フォルダを開く」「パスをコピー」（`ide/ReplayProjectService.kt` の `notifySave`） | 「フォルダを開く」は◎、コピーは× | フォルダを開くは場面Eで使う。パスのコピーはまれで、通知とToolWindowにある。 |
| スクショ通知の「保存場所を開く」「保存したファイルを削除」（`docs/requirements.md` SHOT-03） | 「開く」は直近の結果に含める。削除は× | 削除は取り消せず、まれ。通知に任せる。 |
| About（Tools メニュー） | × | まれ。 |

### 2.4 CLIとMCP（`docs/cli.md` のコマンド表、`docs/mcp.md` のtool表）

| CLI | MCP | 判定 | メニューバーでの対応 |
|---|---|---|---|
| `start` | `start_capture` | ◎ | 取得のON |
| `stop` | `stop_capture` | ◎ | 取得のOFF（未保存の記録を破棄） |
| `save` | `save_replay` | ◎ | 直前N秒を保存。CLIは取得OFFなら取得を始めるが、メニューはGUIと同じく、保存できないときは無効にして理由を出す |
| `save --logs-only` | `save_replay`（`logs_only: true`） | × | メニューには置かない（2.3） |
| `status` | `get_status` | ○ | アイコンとメニュー先頭の状態1行 |
| `screenshot` | `take_screenshot` | ◎ | スクリーンショット |
| `settings` | `get_settings` | × | 設定の一覧はメニューに出さない（保持秒数だけは保存の項目名に出る） |
| `settings set` | `update_settings` | × | 設定画面で行う（`tap_display`・`record_audio`・`device`を含む） |
| `mcp` | ― | × | エージェント向けの入口。人の操作とは関係ない |
| ― | `list_projects` | ○ | 開いているprojectが複数あるときの、projectごとのサブメニュー（3章） |

**CLI・MCPの命令、設定、終了コードは変えません**（`AGENTS.md`、`docs/cli.md`）。

### 2.5 アイコンで示す状態

製品に「一時停止」の状態はありません（OFFは記録を破棄します）。そのため「一時停止」は作らず、今ある状態をそのまま示します。アイコンは単色（macOSのテンプレート画像または単色フォールバック。4章）でも区別できるよう **形で** 区別し、同じ意味の文字をメニュー先頭の行にも出します。マウスを重ねたときの文字（ツールチップ）はOSによって出ないことがあるため（[TrayIcon](https://docs.oracle.com/en/java/javase/25/docs/api/java.desktop/java/awt/TrayIcon.html)「platform dependent」）、文字の主な置き場所はメニュー先頭の行にします。

| 優先 | 状態（元の値） | アイコンの形（案） | メニュー先頭の文字（案） |
|---|---|---|---|
| 1 | 保存失敗が未解決（`SavePhase.FAILED`） | 三角に「!」 | 保存に失敗しました（未解決） |
| 2 | 保存中（`SavePhase.WRITING`）、スクショ保存中 | 丸に下向き矢印 | 保存中… |
| 3 | 一部取得できていない（`PARTIAL`）、端末が複数（`MULTIPLE_DEVICES`）、取得エラー | 丸に「!」 | 一部の記録を取得できていません／端末が複数あります |
| 4 | 復旧中（`RECOVERING`） | 丸に回転矢印 | 復旧中 |
| 5 | 接続待ち（`WAITING`） | 点線の丸 | 端末の接続を待っています |
| 6 | 取得中（`CAPTURING`） | 塗りつぶした丸 | 取得中 · 端末名 · package |
| 7 | 取得OFF（`DISABLED`） | 中抜きの丸 | 取得OFF |

複数のprojectがあるときは、優先が最も高い状態をアイコンにします（3章）。

### 2.6 推奨するメニューの形（1projectのとき）

```
Android Replay Buffer — MyApp            ← 見出し（押せない）
● 取得中 · Pixel 8 · com.example.app     ← 状態（押せない）
────────────
直前180秒を保存
スクリーンショット
────────────
保存しました 10:21 — フォルダを開く       ← 結果があるときだけ
保存に失敗しました — Android Studioで確認… ← 失敗のときだけ（成功の行と入れ替え）
アプリを特定できません — 設定を開く…     ← 注意があるときだけ
────────────
✓ 取得する（OFFで未保存の記録を破棄）
────────────
Android Studioで表示
設定…
```

AppleはメニューバーにはPopoverより通常のメニューを推奨しています（[Apple HIG](https://developer.apple.com/design/human-interface-guidelines/the-menu-bar)）。Java標準の`PopupMenu`（通常のメニュー）で作れる項目だけを使います。

## 3. Android Studioが閉じているとき・複数のIDE/projectがあるとき

### 3.1 方式A・方式Bの比較

| 観点 | A: Android Studioの起動中だけ（プラグイン内、Java標準の`SystemTray`） | B: IDEと独立した常駐アプリ |
|---|---|---|
| 仕組み | プラグインがIDEの中でアイコンを出し、`ReplayProjectService`を直接呼ぶ | 別のアプリが常駐し、CLIと同じ接続先一覧（`cli/ReplayCliEndpoint.kt`）からIDEを探して操作する |
| IDEが閉じているとき | アイコンも消える | アイコンは残るが「Android Studioが起動していません」と出すだけ。取得はIDEの中にしかないので、録画も保存もできない（`docs/cli.md`）。IDEを起動する機能はCLIの方針と食い違う |
| 新しい取得処理 | 作らない | 作らない（作ると別の録画エンジンになり、「同じ処理を使う」前提が崩れる） |
| 配布・導入 | プラグインのZIPだけ。追加の導入なし | 常駐アプリの配布、ログイン時の自動起動（macOS・Windows・Linuxで別々の仕組み）、macOSでは署名と公証が必要になる見込み（未確認）。アンインストールも別 |
| 複数のIDEプロセス | IDEごとにアイコンが1つ（Android Studioの正式版とPreviewを同時に開くと2つ） | 1つのアイコンで全IDEのprojectをまとめられる |
| 安全性 | 新しい通信路が増えない | CLIの接続先（本人だけが読めるフォルダ。`docs/cli.md` の接続先の節）を常駐プロセスが常に読む。通信路は既存のものを使えるが、常時接続の扱いを決める必要がある |
| 状態の更新 | IDE内の状態変化をすぐ反映できる | CLIの`status`を定期的に問い合わせる必要がある（今のCLIに購読の仕組みはない） |
| 工数・リスク | 小さい | 大きい（新しい配布物とOS別の自動起動） |

**方式Aを採用します（確定D1）。** Bの利点は「IDEが閉じていてもアイコンがある」ことですが、IDEが閉じていると取得も保存もできないので、得られるのは「起動していません」の表示だけです。#15 の「IDE終了後の単独動作は未決で、要件へ自動追加しない」とも整合します。Bが本当に必要になった場合は、別Trailで要件から検討します。

### 3.2 方式Aで、どのprojectを操作するか

| 状況 | 動き | 根拠 |
|---|---|---|
| Android Studioが閉じている | アイコンを出さない | 操作できる対象がない |
| 開いているprojectがない（Welcome画面だけ） | アイコンを出さない（または「projectを開いてください」だけ）。どちらにするかは実装時に統一 | Running Devicesのボタンも「projectを開いてください。」と出す（`ui/RunningDevicesActions.kt`） |
| projectが1つ | 2.6のメニューをそのまま出す | 対象は1つで迷いがない |
| 同じIDEでprojectが複数（windowが複数） | projectごとのサブメニュー（`MyApp ● 取得中 ▸`）を並べ、中に同じ操作を置く。アイコンは最も優先の高い状態 | CLIは前面のwindowから推測しない（`docs/cli.md`）。メニューでも前面windowや「最後に触ったproject」で推測せず、利用者がproject名を見て選ぶ。MCPの`list_projects`と同じ考え方（確定D7） |
| 別のIDEプロセス（正式版とPreviewなど）で別のprojectを開いている | IDEプロセスごとにアイコンが出る。各アイコンは自分のIDEのprojectだけを操作する | Java標準の`SystemTray`はアプリ（プロセス）ごとに1つ（[SystemTray](https://docs.oracle.com/en/java/javase/25/docs/api/java.desktop/java/awt/SystemTray.html)）。ほかのプロセスのprojectを操作するにはBが必要 |
| 同じprojectを複数のIDEで開いている | 各IDEのアイコンは自分のIDEのprojectを操作する。取得は各IDEで独立なので、それぞれの状態を出す | CLIはこの状態を「対象が複数」として拒否する（`cli/ReplayCliEndpoint.kt` の `select`）。メニューはIDEの中にあるので対象は一意。ただし2つのアイコンが同じproject名になるので、見出しにIDE名を添える |

1プロジェクト利用の前提（`docs/requirements.md` ENV-01）は変えません。**サブメニューは「複数開いていても取り違えない」ためのUIであり、複数projectの同時取得を新たに保証するものではありません。**

## 4. OSごとの実現性

### 4.1 共通の土台（Java標準の`SystemTray` / `TrayIcon`）

- `SystemTray`はOSによっては存在しないか使えず、`SystemTray.isSupported()`で確認してから使う。WindowsではTaskbar Status Area、GNOMEではNotification Area、KDEではSystem Trayに出る（[SystemTray](https://docs.oracle.com/en/java/javase/25/docs/api/java.desktop/java/awt/SystemTray.html)）。
- `TrayIcon`は画像・ツールチップ・メニューを持つ。右クリックでメニューを出す。左クリックなどの動き（`ActionEvent`）は「一部のOSでは」と書かれていて、OSで違う（[TrayIcon](https://docs.oracle.com/en/java/javase/25/docs/api/java.desktop/java/awt/TrayIcon.html)）。→ **OSで動作を揃えるため、左クリックだけの特別な動作は作らず、全操作をメニューに置く。**
- Android Studioの中で動くので、Android Studio同梱のJBR（JetBrains Runtime）のAWT実装をそのまま使う。追加の配布物は要らない。
- 失敗時の扱いを主経路として決めます：**アイコンを出せない（`isSupported()`がfalse、または登録に失敗した）ときは、エラーやIDEの不調を起こさずアイコンを出さず、設定画面に理由を1行出す**（MB-02）。

### 4.2 macOS

| 項目 | 事実 | 根拠 |
|---|---|---|
| 実体 | JDKのmacOS実装は、`NSStatusBar`の`statusItemWithLength:`で`NSStatusItem`を作る | OpenJDK `src/java.desktop/macosx/native/libawt_lwawt/awt/CTrayIcon.m` |
| ダーク／ライト | システム属性`apple.awt.enableTemplateImages`を設定すると、画像はテンプレート画像として扱われ、色を捨ててテーマに合わせて自動で見やすくなる（macOSだけ） | [TrayIcon（Implementation Note）](https://docs.oracle.com/en/java/javase/25/docs/api/java.desktop/java/awt/TrayIcon.html)、OpenJDK `sun.lwawt.macosx.CTrayIcon`（`CTrayIcon.java`）と `CTrayIcon.m` |
| 注意点 | この属性はJVM全体で最初に1回だけ読まれる（`CTrayIcon`のstatic初期化）。IDEの起動後にプラグインから設定して効くかは未確認。**効かない場合に備え、明暗どちらの背景でも見える単色アイコンへのフォールバックを必須要件とする**（実装時の任意確認ではなく、MB-14として受入基準に入れる） | 同上 `CTrayIcon.java` |
| アイコンの作り方 | 黒と透明だけの記号（テンプレート）にする。メニューを出す。メニューバーだけの機能にしない。出すかは利用者が選べるようにする | [Apple HIG: The menu bar](https://developer.apple.com/design/human-interface-guidelines/the-menu-bar) |
| 隠れる場合 | 場所が足りないとメニューバー項目は隠れることがある。全画面のアプリではメニューバーは隠れ、ポインタを画面上端に寄せると出る | [Apple HIG: The menu bar](https://developer.apple.com/design/human-interface-guidelines/the-menu-bar) |

### 4.3 Windows

| 項目 | 事実 | 根拠 |
|---|---|---|
| 実体 | 通知領域（タスクバー右端）。Javaの`SystemTray`はこれに対応する。IntelliJ自身もWindowsの通知に`SystemTray`と`TrayIcon`を使っている | [SystemTray](https://docs.oracle.com/en/java/javase/25/docs/api/java.desktop/java/awt/SystemTray.html)、[IntelliJ `SystemTrayNotifications.java`](https://github.com/JetBrains/intellij-community/blob/master/platform/platform-impl/src/com/intellij/ui/SystemTrayNotifications.java) |
| 隠れる場合 | 新しいアイコンは「隠れているアイコン」（オーバーフロー、`^`の中）に入ることがあり、利用者がドラッグで常に表示に移せる | [Microsoft: タスクバーの通知領域のカスタマイズ](https://support.microsoft.com/en-us/windows/customize-the-taskbar-notification-area-e159e8d2-9ac5-b2bd-61c5-bb63c1d437c3) |
| 指針との差 | Microsoftは通知領域を「一時的な通知や状態のためのもの」とし、「すぐ対応すべき重要情報や、プログラム・コマンドへの素早いアクセスのためのものではない」としている | [Microsoft Learn: Notification Area](https://learn.microsoft.com/en-us/windows/win32/shell/notification-area) |

Windowsの指針は「素早い操作の入口にしない」としているため、Windowsでは状態の表示が主で、操作は補助と位置づけます。この緊張は残したまま、それでもOSで動作は揃えます（同じメニューを出す）。初回に「常に表示に移す」方法をToolWindowで1行案内するかは実装時に決めます。

### 4.4 Linux

| 項目 | 事実 | 根拠 |
|---|---|---|
| JavaのX11実装 | 古いトレイの仕組み（XEmbed。X11の`_NET_SYSTEM_TRAY_S<n>`の持ち主）を探し、いなければ使えない | OpenJDK `sun.awt.X11.XSystemTrayPeer`（`XSystemTrayPeer.java`）と `XToolkit` |
| JBRのWayland実装 | `isTraySupported()`は常に`false`（未実装） | JBR `sun.awt.wl.WLToolkit`（`WLToolkit.java` の `isTraySupported`） |
| GNOME | 3.26から状態アイコンは既定で表示されない。状態アイコンを必須にせず、すべての機能をwindowから使えるようにする指針 | [GNOME Status Icon Migration Guidelines](https://wiki.gnome.org/Initiatives/StatusIconMigration/Guidelines) |
| AppIndicator / StatusNotifierItem | 新しい仕組みはD-Busで、アイテム・Watcher・Hostの3者で動く。古いトレイ仕様の後継 | [freedesktop StatusNotifierItem](https://specifications.freedesktop.org/status-notifier-item/latest/) |
| GNOMEでの拡張 | Ubuntuが保守するGNOME Shell拡張で、AppIndicator/KStatusNotifierItemと古いトレイアイコンを表示できる（拡張の導入が前提。Ubuntuで既定有効かは本調査では未確認） | [ubuntu/gnome-shell-extension-appindicator](https://github.com/ubuntu/gnome-shell-extension-appindicator) |
| KDE Plasma | StatusNotifierItemはKDE由来の仕組み。Plasma Workspaceには`xembed-sni-proxy`があり、名前のとおり古いトレイアイコンを中継すると見られる（中身は未確認） | [freedesktop StatusNotifierItem](https://specifications.freedesktop.org/status-notifier-item/latest/)、[Plasma Workspace `xembed-sni-proxy`](https://invent.kde.org/plasma/plasma-workspace/-/tree/master/xembed-sni-proxy) |
| Xfce | パネルの「statustray」プラグインがStatusNotifierと古いトレイの両方に対応（4.15.4以降） | [xfce4-panel NEWS](https://gitlab.xfce.org/xfce/xfce4-panel/-/blob/master/NEWS) |

つまりLinuxでは、Javaの`SystemTray`は **X11（またはXWayland）で、古いトレイを表示できるパネルがあるときだけ** 使えます。素のGNOMEや、JBRのWaylandモードでは`SystemTray.isSupported()`が`false`になり得ます。その場合は4.1の主経路どおり、**アイコンを出さず、設定画面に理由を出します**（MB-02）。StatusNotifierItemに対応するにはD-Busを直接使う実装が要り、依存ライブラリが増えるため、初期版では対応しません（確定D6。必要になれば別Trail）。

### 4.5 OSで動作を揃える方法（`AGENTS.md`）

| 層 | 中身 | OSの違い |
|---|---|---|
| メニューの中身を決める部分 | projectの状態（`ReplaySnapshot`）から、項目名・有効/無効と理由・アイコンの種類・並び順を決める純関数。操作は`ReplayProjectService`の既存の関数（`setEnabled` / `save` / `screenshot` / `openDirectory`。`ide/ReplayProjectService.kt`）を呼ぶ | なし（全OS共通） |
| 表示先の実装 | 決めたメニューとアイコンをOSに出す。「表示先がない」ときは何も出さず、設定画面の「メニューバーに表示する」の下に理由を1行出す | OS別の実装の中だけ。選ぶのは`core/HostPlatform.kt`の1か所だけ |

表示先は、`HostPlatform`が今選んでいる `saveHandles` / `privateFiles`（どちらもファイル保存の実装）に混ぜず、**同じ「OS判定は`HostPlatform`の1か所」原則に従う、トレイ表示用のsiblingファサード**（`HostPlatform`から選ぶ別の実装）とします。ファイル保存とUI表示の責務を分け、呼ぶ側はOSで分岐しません（7章 実装契約）。

規則は全OSで同じです:「表示先があれば同じメニューを出す。なければ出さず理由を出す。メニューにしかない操作はない」。表示先の有無はOSや環境で変わりますが、これは今の`SystemTray.isSupported()`の扱いと同じで、動作の規則は変わりません。

### 4.6 CI・VMで確かめられること

| 確かめる内容 | 方法 | 確実さ |
|---|---|---|
| メニューの中身（状態ごとの項目名、有効/無効と理由、アイコンの種類、優先順位、複数projectのサブメニュー） | 純関数のメニューモデルの単体テスト。CIの`test`と`os-parity`（macos-latest・windows-latest。`.github/workflows/ci.yml`）で全OS | 高い。画面は不要 |
| メニューの操作が`ReplayProjectService`の既存操作を呼ぶこと、取得処理が増えないこと | 単体テスト（偽の表示先を使う） | 高い |
| 表示先がないときの扱い | 画面のないCI（headless）では`isSupported()`が`false`になる前提で、理由の表示を確かめる | 高い（ただしheadlessでの戻り値は実装時に確認） |
| LinuxのX11で実際にアイコンが出てメニューが動くこと | VMで、古いトレイを持つパネル（Xfceなど）を動かして確認 | 中くらい。VMの準備が要る。**未確認枠** |
| Waylandモードでアイコンが出ず、理由が設定画面に出ること | 同上（JBRのWayland実装は`isTraySupported()`が`false`） | **未確認枠** |
| macOSのメニューバー、テンプレート画像／単色フォールバック、全画面時の表示、IDE（JBR）の中での動き | macOSの実機または画面ログインのあるマシンで手動確認 | CIでは確かめられない。**未確認枠**。受け入れ時に「確認した／していない」を報告する |
| Windowsの通知領域、オーバーフロー | Windowsの実機または画面ログインのあるマシンで手動確認 | 同上。**未確認枠** |

## 5. 通知

| 通知 | 範囲 | 理由 |
|---|---|---|
| 保存完了・保存失敗・スクショ完了（今のIDE通知） | 変えない | 詳しい内容と「フォルダを開く」は通知に任せる原則（`docs/design/ui-ux.md`「原則と根拠」）。今の通知は`ide/ReplayProjectService.kt`の`notifySave`・`screenshotError`など |
| メニューバーのアイコンと「直近の結果」の行 | 範囲内 | IDEが後ろにあっても、保存中→完了／失敗が形と文字で分かる。これだけで場面Eは満たせる |
| OSの通知（macOSの通知センター、Windowsのトースト、Linuxのlibnotify）で、Android Studioが前面にないときの「保存失敗」「クラッシュ時の自動保存が行われた」 | 範囲内（確定D5） | 失敗と自動保存は、利用者が自分で押していないか、気づかないと記録を失う。IntelliJにはOS別の通知の仕組み（Windows=`SystemTray`、macOS=通知センター、その他=libnotify）があり（[IntelliJ `SystemNotificationsImpl.java`](https://github.com/JetBrains/intellij-community/blob/master/platform/platform-impl/src/com/intellij/ui/SystemNotificationsImpl.java)）、OS別の実装を自前で持たずに済む見込み。**既存のIDE通知がIDE非前面時に自動でOS通知へ送られるかは未確認で、送られるなら新しい通知は作らない**（重複させない） |
| 保存成功のOS通知 | 範囲外 | 自分で押した操作の結果はアイコンと直近の結果で分かる。毎回のOS通知はうるさい |

## 6. 範囲外

- IDEと独立した常駐アプリ（方式B）と、IDEを起動する機能。Bが必要になれば別Trailで要件から検討する（確定D1）。
- LinuxのStatusNotifierItem / AppIndicatorへの対応。必要になれば別Trail（確定D6）。
- 新しい録画・保存の処理。メニューバーは`ReplayProjectService`の既存操作を呼ぶだけ。
- メニューでの設定の編集（保存先・保持時間・対象アプリ・取得モード・動画品質・`tap_display`・`record_audio`・`device`など）。設定画面とToolWindowの注意欄へ案内するだけ。
- メニューからのログのみ保存（`save --logs-only`相当）。Logcatツールバー・CLI・MCPに残す（確定D2）。
- 保存失敗時の再試行・別の保存先での再試行・破棄をメニューで行うこと。ToolWindowへ案内するだけ。
- 詳細情報（区間・不足区間・SDK・セッションIDなど）の表示。
- 「一時停止」など、今の製品にない状態や操作。
- 複数projectの同時取得の保証、複数端末の同時取得（`docs/requirements.md` ENV-01）。サブメニューは対象を推測しないためのUIであり、同時利用の保証ではない。
- 別のIDEプロセスのprojectの操作（方式Aでは各IDEのアイコンが自分のprojectだけを操作する）。
- OS全体で効くキーボードショートカット（OSごとの別の仕組みが要る。IDE内のキー割り当ては別のTODO）。
- メニューバーからしか使えない操作。
- CLI・MCPの命令、設定、終了コードの変更。
- この要件フェーズでのコード実装。

## 7. 実装契約（コード側との取り決め）

方式Aを採用するにあたり、実装で守るコード上の取り決めをここで確定します。

- **保存の要求元:** `core/ReplayModels.kt` の `SaveOrigin` に **`MENU_BAR` を追加**し、メニューからの保存・ログのみ保存は `ReplayProjectService.save(SaveRequest(origin = SaveOrigin.MENU_BAR))` を呼ぶ。`replay.json` の `save_request.origin` には `MENU_BAR` が入る。既存の値（`TOOL_WINDOW` / `RUNNING_DEVICES` / `LOGCAT_TOOLBAR` / `CLI` / `CRASH_AUTOSAVE`）の意味は変えない。スクリーンショットには要求元を記録する欄がなく、`screenshot()` をそのまま呼ぶ。
- **トレイ表示の置き場:** `core/HostPlatform.kt` が選ぶ実装は、ファイル保存向けの `saveHandles` / `privateFiles` だけに留め、トレイ表示は混ぜない。代わりに、同じ「OS判定は`HostPlatform`の1か所」原則に従う **トレイ用のsiblingファサード**（`HostPlatform`から選ぶ表示先実装）を新設する。呼ぶ側のコードはOSで分岐しない。
- **「メニューバーに表示する」の保存先:** **IDE全体の設定**として保存する。projectの `ReplaySettingsStore`（`Service.Level.PROJECT` + workspaceファイル）は、開くprojectごとに別になるため使わない。既存のstoreは汚さず、Applicationレベルの永続化（Applicationサービスの `PersistentStateComponent` など）を新設する。
- **メニューモデルの純関数化:** `ReplaySnapshot`（`captureState` / `canSave(content)` / `saveDisabledReason(content)` / `save` / `lastSave` / `autoSave` / `devices` など）と `screenshotDisabledReason` から、項目名・有効/無効と理由・アイコン種別・並び順を決める純関数にし、表示先実装はその結果を描くだけにする。単体テストは偽の表示先で行い、CIの`test`・`os-parity`で全OSを通す。
- **CLI・MCPの契約は変えない**（`docs/cli.md` の規則どおり、`replay --help`・`plugin/src/main/cli/README.txt`・`docs/mcp.md` も変えない）。

## 8. 受け入れ基準

「確かめ方」列の **単体** はCIの`test`・`os-parity`で機械的に保証する範囲、**実画面** はOSの実画面が要るため受入時まで **未確認** とし、受入報告で「確認した／していない」を明記する枠です。

| ID | 基準 | 確かめ方 |
|---|---|---|
| MB-01 | 「メニューバーに表示する」をONにすると、表示先があるOSでは、Android Studioの起動中にアイコンが出る。OFFにするか、最後のprojectを閉じるか、IDEを終了すると消える。 | 実画面（未確認）。設定の永続化・出し分けの判定は単体 |
| MB-02 | 表示先がない環境（素のGNOME、Waylandモード、headlessなど）では、エラーやIDEの不調を起こさずアイコンを出さず、設定画面に理由を1行出す。ほかの入口はすべて今までどおり使える。 | 単体（headless前提）＋実画面（GNOME・Wayland、未確認） |
| MB-03 | アイコンは2.5の7状態を形で区別でき、メニュー先頭の行に同じ意味の文字が出る。色を除いても区別できる。 | 単体（状態→形・文字の対応）＋実画面（見え方、未確認） |
| MB-04 | Android Studioが別のwindowの後ろにあるとき・エミュレータが全画面のときに、IDEを前面に出さずに「直前N秒を保存」が2クリック以内（1projectのとき）で実行でき、保存はToolWindowと同じ処理・同じ保存先・同じ通知になる。 | 実画面（未確認）。「同じ処理を呼ぶ」は単体 |
| MB-05 | 「スクリーンショット」「取得する」のON/OFFは、Running Devicesのボタンと同じ有効・無効と理由で動く。取得OFFの項目名は未保存の記録が破棄されることを示す。 | 単体＋実画面 |
| MB-06 | 保存中はアイコンと先頭の行が保存中になり、完了後は「保存しました 時刻 — フォルダを開く」から保存先を開ける。失敗時は失敗の状態になり、「Android Studioで確認…」でToolWindowが開き、そこで再試行・破棄ができる。 | 単体（モデル）＋実画面（未確認） |
| MB-07 | projectが複数あるときは、project名のサブメニューで選んでから操作する。前面のwindowや最後に触ったprojectで対象を推測しない。 | 単体＋実画面 |
| MB-08 | 同じ操作をメニュー・ToolWindow・Running Devices・CLI・MCPのどれで行っても、他の入口の表示が同じ状態に更新される。取得のプロセスや記録バッファは増えない。 | 単体 |
| MB-09 | メニューにしかない操作がない（すべてToolWindowか設定画面かRunning Devicesでもできる）。 | 単体・レビュー |
| MB-10 | Linux・macOS・Windowsで、メニューの項目・順番・有効/無効・文言が同じ。OSを判定するのは`core/HostPlatform.kt`だけで、それ以外のコードはOSで分岐しない。 | 単体（os-parity）＋コード構造 |
| MB-11 | CIの`test`と`os-parity`で、メニューの中身と操作の呼び出しの単体テストが全OSで通る。 | 単体 |
| MB-12 | 通知は確定D5の範囲だけ出し、同じ出来事でIDE通知とOS通知が二重に出ない。 | 単体＋実画面（未確認） |
| MB-13 | CLI・MCPの命令、設定、終了コードが変わらない。 | 単体（既存のCI） |
| MB-14 | macOSではテンプレート画像で明暗両テーマに追従する。`apple.awt.enableTemplateImages`がプラグインからの起動後設定で効かない場合は、明暗どちらの背景でも見える単色アイコンに必ずフォールバックする。 | 実画面（未確認） |

**実画面確認の未確認枠**（受入時まで未確認のままとし、報告で扱いを明記する）: macOSメニューバーの表示・全画面時の動き・テンプレート画像または単色フォールバックの見え方（MB-01/03/04/14）、Windowsの通知領域とオーバーフロー（MB-01/04）、LinuxのX11パネルでの表示（MB-01/04）、Waylandで出ないことと理由表示（MB-02）。実画面確認の優先順は **macOS → Windows → Linux** で、コードは全OS共通の規則のままとします（0章・確定D1/D6）。

## 確定した判断

利用者に判断を委譲されていた点を、レビューの推奨どおり確定しました。状態は **要件確定（実装前）** で、実装は利用者の承認を得てから行います。

| ID | 確定 | 理由 |
|---|---|---|
| D1 | 方式A（必要なら後でBを別Trail） | IDE終了後は取得も保存もできない。#15も単独動作を自動追加しない |
| D2 | 保存・スクショ・取得ON/OFF・状態・直近結果・IDE/設定への導線 | 頻度の高い操作に限定 |
| D3 | 既定OFF、設定とToolWindowの案内でON | Apple / Windows / GNOMEの常駐抑制指針 |
| D4 | OFFを置く、確認なし、項目名に破棄を書く | ToolWindow / Running Devicesと揃える |
| D5 | IDE非前面時のみ、保存失敗とクラッシュ自動保存をOS通知。既存IDE通知が既にOSへ出るなら新設しない | 二重通知禁止 |
| D6 | 初期はJava `SystemTray`のみ。無ければ理由。StatusNotifierItemは別Trail | 依存を増やさない |
| D7 | 複数projectはサブメニュー。推測しない | CLIの「推測しない」と揃える。同時取得の保証にはしない（ENV-01） |

---

**注意（この枝のCIについて）:** この枝は文書の差分だけですが、`os-parity (windows-latest)` が failure になっています。docsだけの差分でも発生しているため、本文の内容の是非とは切り分けて扱い、実装時に`develop`の同ジョブで再確認します。
