# 初期版のIDE UI設計

**独立した `Android Replay Buffer` ToolWindowを採用する。** 初期配置は右側、1つのパネルとし、Logcatとは独立して取得・保存状態を表示する。Logcatへの埋込み、独自ヘッダー、ログ閲覧機能の再実装は行わない。この文書は [#12](https://github.com/shinma06/android-replay-buffer/issues/12) の設計決定であり、UIの実装・IDE受入済みという意味ではない。[製品要件](../requirements.md) を正本とし、実装は [#14](https://github.com/shinma06/android-replay-buffer/issues/14) で行う。既存CLI原型のコマンド・設定・ファイルを変更しない。

## 配置の比較と根拠

対象は [既存基盤](../plugin-development.md) と同じRabbit 1 `2026.2.1.8` / `AI-262.9437.185.2621.16467767`。設計調査ではこのbuildのSDK JAR・XMLを読取り、`javap`でAPI署名を確認した。IDEを起動したり、プラグインをインストールしたり、ADBや端末を操作したりしていない。

| 候補 | SDKで確認した入口 | 普段の操作と保守 | 判断 |
| --- | --- | --- | --- |
| Logcat内の常設パネル | `android.jar` 内 `META-INF/logcat.xml` はLogcat factory・サービスを登録する。公開の `Logcat.PopupActions` は右クリック用。宣言されたLogcatの3つのextension pointはconsole filter、exception rewriter、R8 mappingで、常設パネル追加用ではない | ログ調査中の距離は近い。既存ToolWindowのContentManagerに別contentを入れる案は汎用API上は考えられるが、Logcatの分割タブ・factory・状態復元との共存契約は確認できない。Swingツリーへの挿入やfactory置換は内部構造に依存する | 初期版では不採用。公開右クリックactionがあることと常設UIの安定した拡張点があることを混同しない |
| 独立ToolWindow | 公開 `com.intellij.toolWindow` / `ToolWindowFactory` / `ContentFactory` / `Content.setDisposer` | Logcatを閉じても操作可能。右側なので通常bottomにあるLogcatと併用でき、IDE標準で移動・隠す・再表示できる。入口が1つ増える | 採用。取得サービスの寿命を表示タブから独立させ、ネイティブ配置を使う |

公開EPによる宣言とcontent寿命の扱いは [Tool Windows](https://plugins.jetbrains.com/docs/intellij/tool-windows.html)、Android固有EPの分類は [公式一覧](https://plugins.jetbrains.com/docs/intellij/android-plugin-extension-point-list.html) を確認した。「埋込みは絶対に不可能」とは断定せず、対象SDKで確認できた契約に基づき初期版の保守負担を判断した。将来、公式の常設拡張契約が追加された場合は操作距離の改善として再評価できる。

## 操作の具体配置

IDEの View → Tool Windows → Android Replay Buffer から開く。初回は機能無効、ToolWindowは常設登録し、取得の有効状態で表示可否を変えない。起動時に勝手に前面へ出さない。ToolWindowを開いたこと自体では取得を開始しない。

| 場所 | 表示・操作 | 契約 |
| --- | --- | --- |
| パネル上部 | `取得を有効にする` トグル、`設定…` | トグルは即時。`無効にすると未保存のデータと再試行待ちの内容を破棄します` を説明する。設定は無効・接続待ち・保存中・失敗中・indexing中も操作可能 |
| 対象行 | `端末: <名前>（実機／Emulator・接続状態）`、`アプリ: <package>（自動／手動）` | 端末は表示のみ。選択UI・同時取得を追加しない。アプリ行の `変更…` は設定画面を開く |
| 取得状態行 | 無効／接続待ち／取得中／復旧中／一部取得失敗。各取得種類の状態 | 保存中でも取得状態を表示し続ける。色だけで区別しない |
| 保存対象の情報 | `保持時間: 180秒`、保存窓の基準時刻、動画／端末ログ／アプリログの取得済み範囲・欠落 | 完全切断中は `切断前の記録を保持（基準: …）`、再接続後は現在窓へ戻る。取得不足・0件を区別 |
| 主操作 | `直前180秒を保存` | 適用済み保持時間にラベルを追従。20秒しかないときも待たず最大20秒を保存。実時間窓を過去に広げない |
| 保存状態行 | 待機／保存中／保存完了／保存失敗。直近の完成フォルダへの `フォルダを開く` | 取得とは別の状態。完成前の出力を成功と表示しない |
| 保存失敗カード | 理由、固定対象の時刻・取得種類・寿命、`再試行`、`保存先を変更して再試行…`、`破棄` | 詳細は[保存失敗設計](../plugin-design.md)へ従う。解決まで新規保存だけを停止し、取得は継続 |

パネル例（数値と名前は説明用。pixel配置・可読性は実IDEで受入する）:

```text
Android Replay Buffer
[✓ 取得を有効にする]                 [設定…]
端末: Pixel（Emulator・接続済み）
アプリ: com.example.app（自動）      [変更…]
一部取得失敗 — 動画を自動復旧中
動画: 20秒欠落 / 端末ログ: 取得中 / アプリログ: 取得中
保持時間: 180秒 / 保存基準: 現在
[直前180秒を保存]
保存: 保存中…（取得は継続しています）
```

通常保存は毎回のフォルダ・名前・長さ入力を求めない。初期版には保存ショートカットの既定割当、画質編集、音声、タップ表示、複数端末UI、メニューバーを追加しない。メニューバーは初期版完成直後の別段階 #15。

### 取得状態と不足種類

各種類を `取得中／待機／復旧中／未取得／欠落あり` と理由で表示する。ログ0行と取得処理の失敗を区別し、健康な取得中にログが発生していないだけなら障害としない。

| 条件 | 主表示 | 保存と次操作 |
| --- | --- | --- |
| 無効 | `無効 — 未保存の記録はありません` | 保存不可。設定編集可能。有効化で接続済みなら即取得 |
| 未接続 | `接続待ち` | データ0なら保存不可、理由を表示。接続で自動開始 |
| 全種類が取得中 | `取得中` | 取得済みデータがあれば保存可。設定時間まで待たない |
| 完全切断 | `接続待ち — 切断前の記録を保持` | 残るデータを保存可。切断確定時刻で窓を凍結。無効化まで保存可能、容量の上限は#11/#14で確定 |
| 接続中で取得処理が全停止 | `復旧中` | 自動復旧を直ちに試行。現在の実時間窓に残るデータを保存可。完全な端末切断と同じ凍結を勝手に適用しない |
| 一部だけ失敗 | `一部取得失敗 — 動画／端末ログ／アプリログ: <理由>` | 健康な側を継続。現在窓を進め、過去欠落も保存対象情報へ示す |
| 自動アプリ未解決 | `アプリ: 未選択 — 自動選択できません。設定でpackage名を指定してください` | 動画・全体ログは継続。アプリログ未取得を表示し、他データを保存可 |
| 複数端末が接続 | `初期版は1台に対応しています。ほかの端末を切断してください` | 新しい端末へ勝手に切替えない。既に取得中の1台を維持し、未選択状態なら1台になるまで接続待ち |

保存ボタンの可否は取得状態名だけで決めず、現在の保存窓にデータがあるか・保存要求が処理中か・失敗固定対象が残るかで決める。保存中は二重要求を受け付けず `保存中…` と理由を表示する。障害から復帰しても保存対象窓に欠落が残る間は、その欠落を消さない。

## 設定・明示適用・対象アプリ

Settings → Tools → Android Replay Buffer をproject-level `Configurable`で常設する。1プロジェクトの契約とし、複数project共有方式を追加しない。有効状態と適用済み設定はprojectのworkspace用 `PersistentStateComponent`に保存し、VCS共有設定へ絶対パスや有効状態を入れない。初回 `enabled=false` / 保持180秒 / アプリ自動。保存先未指定なら設定に案内し、保存不可の理由を表示する。ユーザーが設定するのは保存先と取得条件であり、外部ツール導入やPATHではない。

| 設定欄 | 動作 |
| --- | --- |
| 保持時間（秒） | 整数・正数を検証。上限は録画エンジンと容量実測後に#14で固定し画面に示す。初期値180を保証し、黙ってclampしない |
| 保存先フォルダ + `参照…` | IDE標準folder chooser。空・ファイル・不正pathを項目の近くに表示。実書込み失敗は#11の復旧操作へ。入力文字列をshellへ展開しない |
| 対象アプリ `自動（Android Studioの実行対象）／手動` | 手動時にpackage入力を使用。モードに応じたpackage欄の可否は変えるが、機能無効を理由に設定を操作不能にしない |
| 自動の解決結果 | Run configuration名・実package・未解決理由を読取表示。ログの本文やcredentialを設定画面へ出さない |
| 説明 | `編集だけでは取得条件を変更しません。「適用」または「OK」で反映します。` |

`reset`は適用済み値をdraftへ読み、編集はdraftだけを変更する。`isModified`で比較、`apply`で全項目を検証し、一式を固定してサービスへ渡す。検証失敗は部分適用しない。Cancelは未適用draftを捨てる。機能トグルは取得条件draftに含めず即時処理し、設定Cancelで無効化が取り消されない。無効中のApplyは保存だけを行い取得しない。

取得中のApplyは `設定を反映中` を表示して必要な取得種類のみ切替え、旧設定を新設定と混在させない。完了までは新規保存を保留せず一時的に操作不可とし、理由を表示する。再起動前の未保存バッファ・固定された失敗対象は消さず、同じsequenceに適用境界と欠落を記録する。保持時間を短くした場合だけ新しい窓の外を期限処理する。失敗対象は自身の固定条件を保ち、通常設定変更で書き替えない。適用不能なら旧稼働条件と保存済み設定の関係を明示し、稼働反映成功を偽装しない。詳細な容量・再起動契約は#11/#14と照合する。

自動モードは選択されたAndroid実行構成の実 `applicationId`を取得する。namespace・ソースmanifestのpackage文字列を推測で使わず、variant/applicationIdSuffixを反映する。選択変更・構成編集・Gradle model更新時に解決し直す。これは適用済み自動モード内の実行対象追従であり、設定画面のdraft編集による変更とは区別する。アプリの変更時刻とpackageを記録し、同じsequence内の古い対象ログを新しいpackageのログと誤表示しない。保存対象はクリック時点の対象履歴を固定し、ファイル内の対応情報は#13に従う。

手動modeとpackageはApply後に優先し、Run configurationを変えても上書きしない。自動へ戻す操作もApplyで反映する。packageの構文はAndroidのapplication ID規則と取得方式の制約を#14で検証し、空・制御文字・shell注入を拒否する。アプリ終了・PID変更はpackage指定を消さず、新しいprocessを自動追従する。テスト構成や非Android構成など解決できない場合は未選択と理由を示し、手動指定へ案内する。自動機能を省略する理由にはしない。

## 保存失敗・通知・寿命

#11のPM指定契約に合わせ、失敗対象はimmutableな最大1件。新規保存停止中も通常取得を続け、再試行と保存先変更では同じ対象を使う。カードには基準時刻・窓・対象アプリ履歴・不足種類と `無効化またはproject終了で破棄されます` を示す。`保存先を変更して再試行…` はフォルダ選択後の明示的な `適用して再試行` で受付ける。失敗対象の保存先変更はその要求だけに適用し、通常設定の保存先を無断変更しない。破棄後は新規保存が可能になる。無効化は通常バッファと失敗対象を破棄し、project終了でも未保存対象を引継がない。保存済み成果物は削除しない。完全切断窓の寿命や容量による取得停止の理由表示は#11で確定した契約を使い、UIに別の期限を設けない。

保存完了はPlatform `NotificationGroupManager` / `NotificationAction`で通知し、`フォルダを開く` を置く。成功通知は完成したその要求のフォルダを保持し、後続保存のフォルダへすり替えない。不足がある成功は `保存しました（動画に欠落があります）` のように不足を明記する。保存失敗は通知からToolWindowの失敗カードを開く。自動復旧の各試行で通知を連発せず、現在状態をパネルへ反映する。

フォルダを開く操作はRabbitで確認した `RevealFileAction.openDirectory(Path)` を利用する候補とし、存在・directory・OSサポートを再確認する。開けない場合は簡潔な理由とpathコピーを提供する。Finder専用command・HTTP URL・shell文字列で実装を固定しない。project破棄後の通知actionは取得サービスを再生成せず、安全に失効させる。[公式通知API](https://plugins.jetbrains.com/docs/intellij/notification-types.html) の利用とOSで開けることの実受入は区別する。

## 実装のAPI境界

UIには取得エンジンの実装、LogcatのSwingツリー、タイムライン変換を持たせない。新たな汎用interface群やDBを先に作らず、projectの取得所有者に表示snapshotと明示操作をまとめる。PMのコア契約へ接続する最小の値と操作は次のとおり。型名・API名は実装担当間で確定するための案であり、新たな公開SDKを要求するものではない。

- 表示snapshot: `revision`、`enabled`、`captureState`、端末ID/名前/種別/接続状態、アプリ選択mode/解決済みpackage/未解決理由、適用済み保持秒数/保存先/設定revision、窓の基準時刻/凍結有無、動画/端末ログ/アプリログそれぞれの取得状態・窓内の取得範囲/欠落理由。
- 保存状態: `idle / writing / failed / completed` と要求ID、固定した対象窓/sequence/端末/アプリ履歴、不足種類、失敗分類、完成済みdirectory。UIは稼働エンジンや可変buffer参照を直接受け取らない。0件判定と `canSave`/操作不能理由は同じsnapshotから導く。
- 操作: `setEnabled(value)`、`applySettings(validatedDraft)`、`save()`、`retry(requestId)`、`retryAtDirectory(requestId, directory)`、`discard(requestId)`。要求IDが現在の失敗対象と一致しなければ再試行・破棄を拒否し現在状態へ戻す。settings draftはUIだけで保持する。folderを開く操作は完成directoryに対するUI action。

状態購読はcontent作成時に現在snapshotを取得して以後の更新を反映し、再表示で古いsnapshotを再利用しない。モデル側の単調なrevisionを用い、遅い通知が新しい無効化/適用/対象変更を巻き戻さない。

| 境界 | 使用する公開API・実装時の確認 |
| --- | --- |
| ToolWindow | `ToolWindowFactory` / `ContentFactory` / `Content.setDisposer`。DumbAwareでindexing中も状態・設定・トグルを利用できる。パネルを隠すだけでは取得を止めない |
| 自動取得の開始 | `ProjectActivity` とproject serviceを候補に、project open時に永続enabledを読む。ToolWindowのlazy生成に復元開始を置かない。Rabbit SDKにexecute署名があることを確認済み、実装とロードは未検証 |
| 設定 | `com.intellij.projectConfigurable` / `Configurable` / `ShowSettingsUtil` / `TextFieldWithBrowseButton` / `PersistentStateComponent`。適用snapshotは不変として渡し、取得処理がUI componentを保持しない |
| 実行対象 | Platform `RunManager.selectedConfiguration` / `RunManagerListener.TOPIC`。Android固有部分はRabbitの `AndroidRunConfigurationBase.getApplicationIdProvider()` / `ApplicationIdProvider.getPackageName()` を小さな境界へ閉じる。署名は確認済み、公開Java visibilityだけで長期互換性を保証しない。実装ではInternal/Experimental指定、variant解決、indexing、model未準備・例外を確認しPlugin Verifierも行う |
| Android依存 | 現行plugin.xmlはPlatform依存のみ。Android APIを使う実装IssueでSDK bundled pluginとplugin.xml依存を明示する。[公式手順](https://plugins.jetbrains.com/docs/intellij/android-studio.html) とRabbitのdescriptorを照合し、Android Studio固有API使用時のmodule依存も確認する。Logcat UI classへの依存は不要 |
| 非同期・寿命 | 取得・保存・filesystem/端末待ち・model解決はbackground、Swing更新はEDT。ToolWindow更新は `ToolWindowManager.invokeLater`。project serviceのCoroutineScope/Disposableに仕事を束ね、contentの購読はcontent寿命に束ねる。project終了/無効化と遅延完了の競合をrequest ID・世代・disposedの再確認で拒否する |
| 操作の競合 | update時の可否だけを信用せずクリック時もserviceで再確認。保存要求の固定と受付を直列化し、Apply/保存/無効化が同時でも二重保存・破棄後成功通知を生まない。取消しと外部process停止の完了を区別する |

[設定EP](https://plugins.jetbrains.com/docs/intellij/settings-guide.html)、[永続化](https://plugins.jetbrains.com/docs/intellij/persisting-state-of-components.html)、[threading](https://plugins.jetbrains.com/docs/intellij/threading-model.html)、[Disposable](https://plugins.jetbrains.com/docs/intellij/disposers.html) を境界の根拠とする。accessible name・label association・tooltip・Tab移動・IDE theme/DPIをネイティブSwing部品で維持する。端末種別と状態の文字表示を省かない。DBはこのUI設計では使用しない。

## Cursor知見の採用範囲

固定main `33c51dc01c1cc68ff1c035c8c3ce02654e613ce7`、develop `2ac9b2962fc94157639f53d428a64e4236e5d58f`を調査した。先行のtests/merge/GUI結果をこの製品のpassへ転記しない。

| 出典 | 採用判断とこの製品の確認 |
| --- | --- |
| [mainのfactory](https://github.com/shinma06/cursor-in-android-studio/blob/33c51dc01c1cc68ff1c035c8c3ce02654e613ce7/src/main/kotlin/com/cursoragent/toolwindow/CursorAgentToolWindowFactory.kt) | main採用済みの公開factory/content/dispose構成を採用。UI-12-01/07で再表示と寿命を確認する。コードそのものの移植はしない |
| [developのfactory](https://github.com/shinma06/cursor-in-android-studio/blob/2ac9b2962fc94157639f53d428a64e4236e5d58f/src/main/kotlin/com/cursoragent/toolwindow/CursorAgentToolWindowFactory.kt)、[Issue #192](https://github.com/shinma06/cursor-in-android-studio/issues/192)、[PR #193](https://github.com/shinma06/cursor-in-android-studio/pull/193) | developだけの独自header/`ToolWindowImpl.decorator`・寸法調整は不採用。標準headerで足り、内部APIと専用icon調整を増やさない。theme/DPI/keyboard確認はUI-12-07へ適合する |
| [設定](https://github.com/shinma06/cursor-in-android-studio/blob/2ac9b2962fc94157639f53d428a64e4236e5d58f/src/main/kotlin/com/cursoragent/settings/AgentSettingsConfigurable.kt)、[永続化tests](https://github.com/shinma06/cursor-in-android-studio/blob/2ac9b2962fc94157639f53d428a64e4236e5d58f/src/test/kotlin/com/cursoragent/settings/AgentSettingsStateTest.kt)、[PR #315](https://github.com/shinma06/cursor-in-android-studio/pull/315) | Configurableのapply/reset/disposeと旧状態の安全なdefaultを採用。表示設定の拡張はdevelopだけ。application共有をproject-localへ適合し、CTL-01〜05とUI-12-01/02を本製品で確認する |
| [固定設定境界test](https://github.com/shinma06/cursor-in-android-studio/blob/2ac9b2962fc94157639f53d428a64e4236e5d58f/src/test/kotlin/com/cursoragent/service/AgentSettingsBoundaryTest.kt)、[PR #243](https://github.com/shinma06/cursor-in-android-studio/pull/243) | developだけの設定検証と受理済み対象の固定を取得条件・保存要求へ適合。UI-12-02/05でApplyと再試行の対象不変を確認する |
| [actionのtests](https://github.com/shinma06/cursor-in-android-studio/blob/2ac9b2962fc94157639f53d428a64e4236e5d58f/src/test/kotlin/com/cursoragent/ui/header/ToolWindowChatActionsTest.kt)、[通知](https://github.com/shinma06/cursor-in-android-studio/blob/2ac9b2962fc94157639f53d428a64e4236e5d58f/src/main/kotlin/com/cursoragent/notification/AgentNotificationService.kt)、[PR #322](https://github.com/shinma06/cursor-in-android-studio/pull/322) | indexing中の設定・クリック時再確認・破棄後操作の拒否を適合。通知の基礎はmainにもあるがdevelopの拡張をmain受入とはしない。UI-12-05/06/07へ接続 |

Cursor固有の会話・ACP・権限・Agent session・header pixel値・製品ID・検証履歴・GUI passは非適用。

## 実装受入Case案と残条件

以下は#14とQAへ接続する未実装Case案で、実行結果ではない。設計PR自身のCase JSONはGUI不要とし、この表の将来観察をpassにしない。全Caseで固定source SHA / ZIP SHA-256 / Rabbit build / 実ロードPlugin版 / fixtureを照合し、共有GUI lease取得後に操作する。実機とEmulatorの両方で取得・保存関係を確認する。

| ID | 手順 | 観察可能な期待 |
| --- | --- | --- |
| UI-12-01 | 初回projectでパネル・設定を開く。無効のまま編集Apply、接続済み1台で有効化、IDE再起動 | 初回無効・設定常時編集可。無効時Applyで取得しない。有効化で即取得し、パネルを開かなくても次回enabled復元で自動開始 |
| UI-12-02 | 取得中に保持/保存先/手動packageを編集しCancel、再編集Apply。切替中に保存・無効化 | 編集/Cancelで旧条件維持。Applyのみ一式反映。旧バッファ・失敗固定対象の不意破棄・二重要求・停止後再開がない |
| UI-12-03 | Android variantとRun configurationを変更。自動未解決、手動Apply、自動へ戻す。アプリを再起動 | 実applicationId表示。未解決でも動画/全体ログ継続。手動優先・解除はApply。PID変更に追従し、旧対象ログと新対象を区別 |
| UI-12-04 | 20秒取得で保存。動画のみ/全体ログのみ/アプリログのみ障害、完全切断、再接続 | 180秒まで待たず保存。取得できる側を継続し不足を種類別表示。切断時は凍結基準、再接続で現在窓、同sequence・欠落維持 |
| UI-12-05 | 保存中連打、保存先不在/権限/容量不足で失敗。時間経過・設定変更後に再試行/保存先変更/破棄、無効化 | 二重保存なし。1件固定・同じ対象の再試行・寿命表示。取得継続、新規保存だけ停止。破棄で解除、無効化で未保存対象消去、既保存不変 |
| UI-12-06 | 連続2保存後それぞれの通知でフォルダを開く。欠落を含む保存と開く失敗 | 通知ごとの完成フォルダを開く。未完成は成功通知なし。不足を明示。OSで開けない場合は理由とコピー操作 |
| UI-12-07 | indexing、パネルを隠す/再表示、theme/DPI/keyboard変更、保存途中project close | 設定・状態・トグルを操作可能。隠しても取得継続。ラベル可読、フォーカスとTab順正常。closeで自身の取得・未保存対象終了、遅延UI/通知なし |
| UI-12-08 | 0台/1実機/1Emulator/複数接続、選択中端末を切断 | 名前・種別・状態が判別できる。無断端末切替なし。複数選択・同時取得を導入せず理由を表示 |

#14で保持上限・アプリ構文・Android API注釈/Verifier・実applicationId解決・取得snapshotの具体型・容量・適用失敗の復旧を確定する。#13で取得済み範囲/欠落の時間表現と保存後の対応確認を確定する。#11の保存失敗・容量・寿命と照合し、PMが統合設計へリンクする。独立レビュー、実装build、UI/端末受入、main promotionは未完了。
