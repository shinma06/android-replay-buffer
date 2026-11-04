# GUI lease・private引継ぎ・復旧

## Android CLIによる開発支援

[Android CLI](https://developer.android.com/tools/agents/android-cli) は任意の開発支援です。導入されていれば `android --version` と対象commandの `--help` で実際の機能を確認して活用します。製品同梱のCLIとは別であり、製品利用者やCIの必須依存には追加しません。

公式資料の調査は `android docs search '検索語'`、返された `kb://` URLの取得は `android docs fetch kb://...` を使えます。検索はローカル索引の初回downloadを伴い、検索結果だけではAPIの挙動を確定できません。必要な本文・API referenceを確認し、公開記録には通常の公式HTTPSリンクと確認した事実を残します。実際のcommand・版・成功/失敗はIssueに記録し、未実施のIDE/端末受入とは区別します。

SDK・IDE情報、解析、使い捨てAndroid fixtureのbuild/install/runを支援に使う場合は、対象project/deviceを明示し、下記の資源影響判定と必要なGUI lease・保全・復元の条件を適用します。`describe` 等もmetadata生成やbuildを伴い得るためhelpで副作用を確認します。Androidアプリ用のinstall/runをIntelliJ plugin ZIPの導入・ロード確認の代わりにしません。`screen` / `layout` 等の補助観測は[Computer Useの規約](computer-use.md)とCase契約に従い、必要な実UI操作を実施したと読み替えません。媒体・生ログ・端末識別子・認証値はprivateに保持します。

[`android init`](https://developer.android.com/tools/agents/android-cli/commands/init) は検出した複数agentの設定へskillを導入するため、通常の利用開始には不要です。skill追加が必要な場合だけ、導入版のhelpで対象agent/projectと変更先を確認して範囲を限定します。未依頼の全体設定変更・一括初期化は行いません。

## GUI lease

GUIの観察・入力と今後のGUI試験は`agent-desktop`を第一選択とし、操作前に[対象指定とフォールバック](computer-use.md)を読む。操作結果が元画面に見えない場合は、別ウィンドウ等への反映と対応する対象切替を確認する。主経路の具体的な未対応・取得/入力の制約がある操作だけ、用途に合うPeekabooまたは利用中agentの純正Computer Useへ切り替える。両fallbackに一律の優先順は付けず、同じ資源範囲・必要なlease・保全・上限と新しい対象確認を使う。実拒否・認証要求は別経路で回避しない。

排他の要否はツール名や画面全体ではなく、操作ごとの共有可変状態と観測・証拠への影響で判断する。現在のPID・起動instance・window・snapshotを照合し、対象と予期する結果先のアプリ状態を独立して所有し、他担当の操作・観測を妨げないと確認できる対象限定のbackground AX/ref操作は、globalなホスト予約なしで並行してよい。異なるwindow、対象指定flag、exit 0だけを独立性の根拠にしない。semantic clickでもdialogや共有アプリ状態を変え得るため、入力先だけでなく結果先を含めて判断する。

前面・global key・物理入力・cursor・clipboard・display、trace/session出力、profile/plugin、ADB/device/daemon、録画・fixture等を共有する操作、またはその共通影響が具体的に不明な部分には、既存の共有GUI leaseと担当間調整を適用する。観測も他担当の対象状態や証拠へ干渉するなら同じ扱いとし、競合しない準備・解析・対象限定操作まで停止しない。activeな旧来の全体排他予約、またはownerの影響範囲が不明な予約がある場合は、そのownerと対象・副作用の範囲を合意するまで重ねない。

`scripts/workflow/gui_lease.py`は、同一project内および別project間の実競合に使うホスト/OSユーザーごとの協調予約であり、OSの入力を遮断せず、対象別予約や優先順も機械的に強制しない。共有予約とGitHub上の担当・待機状態を確認し、必要な区間の空きがあれば、本人への枠単位の追加確認なしで取得する。新しいlock機構やproject別の代替予約は作らない。

実際の共有ホスト競合では、現在操作中のownerが安全な区切りで保全・通常復帰・解放/引継ぎを行い、次は準備済みのARB作業を他projectより優先する。これはこの共有環境の利用順であり、leaseの奪取、実行中操作の横取り、強制終了、人間の停止指示の上書き、未準備のARB作業による空き枠の先取りを許可しない。担当GPTセッション同士が既存の許可された連絡経路で対象範囲・停止確認・順番を調整し、必要なら進行役が整合する。通常の枠調整を本人への許可依頼に置き換えない。実際の人間操作を認めた場合は競合する操作を止める。

予約の有無は安全性や作業範囲の代用ではない。[継続判断](workflow.md#公開操作の承認範囲と失敗の扱い)に従い、担当が対象・保存先・保全/復元・回数/時間上限を整えて録画・保存等を進める。既存の割当と計画内なら、枠ごとの本人承認やPMのGOを取り直さない。新しい運用判断は[Omil事前相談](workflow.md#運用プロファイルとomil事前相談)へ戻し、本人にしか解消できない境界だけを本人へ戻す。空き予約を未依頼の操作や権限拡張の根拠にしない。

```bash
python3 scripts/workflow/gui_lease.py status
python3 scripts/workflow/gui_lease.py acquire --owner gui-session --issue 12 --run run-12-a --head FULL_40_CHARACTER_SHA --minutes 45
python3 scripts/workflow/gui_lease.py renew --token PRIVATE_LEASE_TOKEN --minutes 15
python3 scripts/workflow/gui_lease.py release --token PRIVATE_LEASE_TOKEN
```

SHAとtokenは実値へ置き換えます。返却にはtoken・host等が含まれるため、チャット/GitHubにそのまま貼らず、private記録に保存します。1回の期限は1〜45分。renewは試験全体の予算を延長する許可ではありません。

既定保存先は `/tmp/cursor-in-android-studio-gui-<uid>` でCursorプラグインと共有する。namespace名は既存の予約との互換性のため維持し、排他が必要な操作では全project/clone/worktreeがこの同一予約を使う。`--state-dir`は隔離テスト専用で、別の保存先を作って競合を回避したことにしない。既存ハーネスが別のleaseを使うホストでは、全操作者と旧予約の範囲・正規解放/移行を整合し、旧予約を無視して操作しない。CAS等への引継ぎではこの環境の共有ホスト規約として伝え、別環境の優先順や予約機構を暗黙に変更しない。

leaseが必要な区間の最初の操作と、共有状態に影響するinstall/起動/再起動/送信/復元の直前にtoken/期限を確認する。期限切れでも別ownerのacquireは拒否する。元ownerまたは調整担当が旧処理停止・画面状態・未保存データを確認し、現在tokenでreleaseしてから次担当へ渡す。優先順やPID・時間を理由に他ownerの予約を削除しない。

対象アプリ・fixture・buildを識別し、source SHAと実ロードartifactのhashを照合する。古い画面要素・座標を使い回さず最新状態から操作し、描画を保証する場面では実画像も確認する。終了時は自分の処理だけ停止/引継ぎ、Case別結果と次操作を残し、取得したleaseをreleaseする。個別Caseが明示する予約・native-only経路と元の観察/失敗は保持する。

## 固定Plugin ZIPのターミナル導入

通常の検証版導入は[JetBrains公式のローカルZIP配置](https://youtrack.jetbrains.com/projects/MP/articles/SUPPORT-A-2276/Installing-plugins-from-a-local-file-in-IntelliJ-Products-e.g.-PyCharm)を使う。ZIP内の `pluginName/lib` 構造を対象profileのpluginsディレクトリへ保って配置し、同じIDE/profileを通常起動する。[`installPlugins`](https://www.jetbrains.com/help/idea/install-plugins-from-the-command-line.html)の引数はplugin IDとrepository URLであり、local ZIP引数や更新用flagを創作しない。GitHubのSource code ZIPは導入物ではない。

1. 共有lease、対象IDEのfull build/JBR、専用profileの実config/plugins/system/log経路、使い捨てfixture、現在PID/起動識別を確認する。標準ディレクトリ名だけで推定せず、実起動設定のoverrideも照合する。固定source、標準buildのZIP SHA-256、Plugin ID/version、同梱全3JARの一覧/hashをprivate記録へ結ぶ。
2. 未保存作業と進行中の取得/保存を保全し、自分の取得をOFFにして、対象IDEだけを通常終了する。process終了を確認するまで配置しない。通常終了できない、対象が不明、実tool/OSの拒否・認証要求がある場合はその工程を止めて引き継ぐ。強制kill、EULA自動同意、セキュリティ解除は使わない。
3. 旧Pluginディレクトリと今回影響する自製品設定を、所有・hash・mode・復元先付きでprivateに保全する。未知の同名ディレクトリや他Pluginは置換しない。ZIPのCRC、単一のPlugin rootとID/version、絶対パス/`..`/symlink等の展開先逸脱、全JARを先に検査し、plugins検索対象外の新規stagingへ展開する。既存ディレクトリへ直接展開しない。
4. 検査済みstagingと対象を同じfilesystemに置き、対象Pluginディレクトリだけを限定renameで退避・配置する。旧新JARをmergeせず、旧版をplugins検索対象内に残さない。配置後の全ファイル/JAR hashとmode、固定ZIPとの一致を確認する。途中失敗はIDEを起動せず、今回の対象だけを保全済み旧版へ戻す。
5. 同じIDE/profile/fixtureを通常起動し、新しいPID/起動識別、実ロードしたPluginのsource/versionと全3JARのZIP一致、Tools → Android Replay Bufferについて/actionの実接続、取得OFF・streamなし・記録未開始・不要なADB起動なしを確認する。Caseが要求する通常再起動後も同じ実体を照合する。配置成功やexit 0を実ロード/受入PASSにしない。
6. ロード失敗/版違い/所有状態不明なら新しい試験を始めず、原診断をprivateへ保存し、通常終了後に今回のPlugin/設定だけを復元して旧hash/modeと通常起動を確認する。並行作業を古い全体backupで上書きしない。成功/失敗・元観察・最終状態・残工程を既存QAへ記録し、共有leaseを終了/引継ぎする。

Settings → PluginsのInstall Plugin from Diskや版探索は通常QAの必須操作にしない。Settings GUIは自製品設定の表示・入力・Apply/Cancel・検証・アクセシビリティ・Lifecycle/復元の確認に使い、自製品About/actionの代表native接続も維持する。原Caseの旧導入指定と観察/失敗は履歴として保持し、[trusted正式計画](verification/amendments/initial-agent.json)の有効methodだけを適用する。方法の変更だけで固定成果物を再build/再導入せず、[不変成果物と保持事実の対応確認](verification/responsibility.md#trusted-main改訂と旧観察の再評価)に従って不足する事実だけ実施する。

## 試験の準備・実行上限

準備（計画確認・スクリプト/fixtureの用意・静的検査・証拠の保存先確認）と、測定（取得ON・操作・Save・解析）と、復元/cleanupを分けます。端末/IDEを触る準備にも対象識別と上記の資源影響判定を適用し、共有状態を触る区間はleaseを取得します。計測時刻を遅らせて実際の取得時間・容量・共有環境占有を隠しません。

測定の開始条件、終了/中止条件、取得時間・試行回数・容量と復元時間を既存計画にまとめます。trustedな現行の保証目的/必要証拠・精度と本人指定上限、tool制約を保持します。30分/300event等の旧方法の回数・時間は[有効な方法改訂](verification/responsibility.md)に従って選び、原文だけを理由に再導入しません。担当が定めた内部の準備/ARM/cleanup上限は過去実測に余裕を見込み、次の試行前にPMが[Omil事前相談](workflow.md#運用プロファイルとomil事前相談)を経て理由と影響を記録して調整できます。実行中/事後の期限延長や無制限の再試行はしません。

準備段階で上限に達した場合は「試験未実施・準備失敗」と記録し、製品のfailやpassに置き換えません。同じ準備失敗を同じ計画で繰り返さず、原因と次の変更を先に確定します。小さな補助手順の変更は影響に応じて検査し、元契約・権限・取得内容・副作用を変えない変更に全試験計画の再レビューを自動追加しません。正式な独立レビューや必要な再レビューは維持します。

## private handoff registry

`scripts/workflow/handoff_registry.py`は公開可能なrecordと、実パス/hostのprivate対応表を結ぶ小さなPython APIです。coordinatorそのものではありません。

- `register(storage, public, source, host)` はopaque IDとversionをpublicに追加し、digestとsource/hostをprivate側へ保存。
- `resolve(storage, public, host)` はID、所有者、0600、host、digestを照合し、不一致なら拒否。
- storageはgit common-dir配下など、同じrepositoryの本人専用の保管先。storage/registry directoryは本人所有の0700を要求し、symlinkや権限不整合を変更せず拒否する。公開ファイルへ置かない。
- publicにはowner/Issue/PR/HEAD/base/target/scope/停止宣言などを含め、パスや秘密情報は入れない。
- `source`は実在するowned worktreeの絶対パスを呼出側で確認する。registryだけではwriter停止・Git clean・remote SHAは保証しない。

public recordの変更は古い登録のdigestと一致しなくなります。registry喪失・別host・改変・権限不整合時に勝手に再生成やowner差替えをせず、元ownerと復旧します。バックアップはprivateに保ち、実行中の登録を別projectへ移しません。

## Git hooks

bootstrapは当該repositoryのcore.hooksPathだけを設定し、既存custom hooksを検出すると停止します。共通git-dirを持つworktreeにも影響するので導入を共有します。guardはmain/master/developへの直接push/削除、Issue番号のないbranch、別branch/別HEAD/dirtyのpush、非fast-forwardを拒否します。branch名が正しくてもIssueの実在やclaimを確認するものではありません。

pre-pushは一時テストrepositoryへのGit環境変数の混入を防いでからchange_impact.py経由で必要な確認を実行します。ローカルhooksは事故防止で、サーバー保護の代用ではありません。ローカル設定を無効にして通す運用を作りません。
