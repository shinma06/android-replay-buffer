# 初期版の固定ZIP受入

[QA Issue #27](https://github.com/shinma06/android-replay-buffer/issues/27)は[実装 #14](https://github.com/shinma06/android-replay-buffer/issues/14)のnative子です。Project #5 / Milestone #3、PMは[#10](https://github.com/shinma06/android-replay-buffer/issues/10)、QAは初期版QAチャット。ここは準備手順であり、製品機能・実機・Emulatorのpass記録ではありません。[人間向けQA](human-qa.md)と[受入規約](README.md)に従い、具体Caseの正本は[issue-27.json](changes/issue-27.json)です。

利用者は実機を用意できないため**実機受入を保留**しています。`-REAL`全Caseは環境不足・利用者保留としてblocked、`-EMU`は未実施pendingです。実機の再確認質問は不要です。実機対応保証を出さず、全必要Caseを求めるmain gateと初期版完成条件も緩めません。再開条件は利用者が専用実機を用意し、PMが改めて実行を割り当てた時点です。

## 設計と試験範囲

準備の同期baseはdevelop `e84d5fbf9f1ca3b23dc29be805480680d862d237`、[要件](../requirements.md)と[保存設計](../plugin-design.md)です。UIは[#12](https://github.com/shinma06/android-replay-buffer/issues/12)、同期・録画方式は[#13](https://github.com/shinma06/android-replay-buffer/issues/13)の採用結果に追随します。UIのPR #28は同期baseへ統合済みです。[採用UI設計](../design/ide-ui.md)のUI-12-01〜08を以下のCaseへ接続し、実装/GUIの完了とは区別します。同期精度などをこの計画が独自に決めません。

受入前にPM/実装担当が以下を固定し、Issue/Case前提へ追記します。値や観察が不足するCaseは開始せずblockedにします。

- #12: 採用入口、設定の正数/上限/package検証、Applyと自動選択/手動優先/解除/PID追従、indexing・非表示・disposeの契約。
- #13: 録画エンジンと提供方法、動画PTS/両ログ/時計対応、精度閾値と測定の不確かさ、切出し境界、時計変化・空白・epoch、長時間試験時間、受取人の照合方法。
- #14: 固定画質/可読性/負荷、最大保持時間、動画/log byte上限と空き容量下限、復旧間隔・timeout、安全なfault injection、対応OS/Android版。
- #11保存設計: 失敗1件の固定、再試行/保存先変更/破棄、partial扱い、セッション寿命、取消し/確定競合、完全切断時の凍結窓。SAVE-FAIL-01〜09を省略しない。

1プロジェクト・同時接続1台で行います。複数端末選択、macOSメニューバー、専用viewer、音声、タップ表示、完全な回転追従は初期版の追加合格条件にしません。回転の非致命性と同期は必須です。Windows/Linuxの動作はmacOSの結果で保証しません。

## 環境準備（実行前）

今回の読取確認: macOS arm64、Android Studio full build `AI-262.9437.185.2621.16467767`、JBR `25.0.3`。標準SDKはplatform-tools `37.0.1` / Emulator `37.1.11` / platform `android-37.0` / build-tools `36.0.0`あり、system image / cmdline-tools / 標準AVDはなし。この時点ではIDE起動、install、SDK追加、AVD作成起動、ADB照会をしていません。共有GUI leaseは別作業が保持中で、時間切れを所有移譲と扱いません。

1. PMが他操作者と停止/画面状態/leaseの正規解放を調整し、QAのIssue/run/source/予算/許可対象を割り当てる。
2. QAは[operations](../operations.md)の共有leaseを取得し、最初の操作・install・起動・再起動直前にtoken/期限をprivateで確認する。タスク別lease directoryで並行操作しない。
3. 同じRabbit SDKの専用IDE sandbox/config/systemと新規の非VCS共有設定を用意し、通常IDE/他プロジェクトを開かない。fixtureは[replay-marker](../../tests/fixtures/replay-marker/README.md)を専用プロジェクトとして使用する。
4. Android StudioのSDK Managerで**Android SDK Command-line Tools**の版を選んで導入し、正確な版/packageとSDK rootを記録する。ネットワーク・容量・license操作はPMの環境割当後に行う。SDK全体の更新や既存packageの置換はしない。
5. 同梱sdkmanagerの一覧で実在するAPI 29以上の`arm64-v8a` imageを選び、package ID/版を固定する。候補は`system-images;android-37;google_apis;arm64-v8a`だが存在/対応は未照合。なければ利用可能な版をPMが決め、未存在のpackageを準備済みとしない。
6. Android Studio Device Managerで製品専用の新規AVDを作り、image/API/ABI/端末profileを記録する。初回はfresh data、ユーザーアカウントなし。既存AVDのwipe/snapshot変更はしない。専用AVD以外を停止しない。
7. fixture 2 APKのclean source/hash/署名を固定して専用対象へだけ導入。`appA`/`appB`のAndroid App Run configurationを選べることを確認する。複数接続している場合は他端末を勝手に切断せずPMへ戻す。
8. 実機の準備は保留。再開時には専用端末/API 29以上、接続許可、個人情報のない状態、対象1台と所有を確認し、同じ製品ZIPで試験する。

SDK導入は[公式SDK Manager](https://developer.android.com/studio/intro/update#sdk-manager)、package列挙/固定は[sdkmanager](https://developer.android.com/tools/sdkmanager)、AVD作成は[Device Manager](https://developer.android.com/studio/run/managing-avds)／[avdmanager](https://developer.android.com/tools/avdmanager)に従います。QA用SDK/fixture準備と、ENV-03の製品利用者へ追加録画ツールの導入を求めない条件を混同しません。

## 固定buildと記録

必要変更のdevelop統合後、PMがcleanなcandidate sourceを固定し標準`buildPlugin`で一度ZIPを生成します。ZIP SHA-256、Plugin ID/version/source、Rabbit full build/JBR、API Verifier結果を同じZIPへ対応づけます。固定ZIPをsandboxへInstall Plugin from Diskで導入し、Settings → Plugins / About表示と実配置JARを照合します。source ZIP、別hash、`-dirty`版、旧ロード実体で観察しません。再起動後も再照合します。

fixtureは同じ最終candidateから[build.py](../../tests/fixtures/replay-marker/build.py)で2 APKを生成し、`build/fixture-identity.json`のsource/dirty/APK hashと画面/logのsourceを照合します。専用debug鍵と証明書fingerprintをprivateに保管し、APKのbuild成功と実ロード/描画観察を区別します。通常IDEの設定、上流fixture ID/署名鍵/ランタイム状態をコピーしません。

Caseごとに実機/Emulator別のIDで記録します。共通記録はcandidate/source、製品ZIP hash、実ロード版/JAR、IDE/JBR、fixture source/2 APK hash、OS/API/ABI、run、観察者、timezone付きISO8601時刻、前提、実観察、private証拠へのopaque参照、次操作です。serial/host/絶対パス/tokenはprivateだけに保存します。例示結果や観察時刻を先に埋めません。

- pending: まだ観察していない。現時点のEmulator全Case。
- blocked: 実機保留・環境不足・安全な再現経路不足・測定の不確かさ超過など。製品failと区別し原因/owner/再開条件を記録。
- fail: 期待した製品動作に反する実観察。別の実在open修正Issue/PR、修正候補、再確認を接続。
- pass: 当該candidate/hashでの実観察と必須identity/evidenceを満たした場合だけ。過去build/fixture build/CI/mergeを代用しない。

`gpt`と`human`は既存schemaのキーです。一方の観察を他方のpassへコピーしません。promotionの各Case結果とartifact `plugin`のhashを同じ最終candidateへ揃えます。

## 実行順と同期測定

環境/build識別→初回OFF/接続/設定→短時間/180秒/連続保存→両ログ/アプリ選択/同期→切断/片系/長い空白→保存失敗/容量/取消し→close/回転/再確認の順で行います。Emulatorだけを先行でき、実機blockedを消しません。

同期ではfixtureのpackage/run/eventを両ログと照合し、動画で初めて番号が見えるフレームPTSと直前PTSを記録します。REQUESTは操作callback、DRAWはCanvas命令、FRAME_COMMITはcallback観察で、いずれも真の表示時刻とはみなしません。[公式frame commit仕様](https://developer.android.com/reference/android/view/ViewTreeObserver#registerFrameCommitCallback(java.lang.Runnable))を根拠に、request→draw→commit、callback dispatch、render→表示→録画、frame間隔、wall/elapsedサンプリング幅を測定の不確かさへ含めます。#13の対応方式/閾値を採用し、fixtureだけで物理表示時刻が測れたとは主張しません。

保存先障害と取得用一時領域障害を分けます。段階別write/final renameやENOSPCの試験は、実装の承認済みfault injectionまたは容量を制限した使い捨て領域でのみ行います。ホスト全体を埋めず、共有ADB serverを止めず、所有不明processをkillしません。再現方法が不足するCaseはblocked。fault injection/単体試験だけの成功を、実機/Emulatorの受入passへ置き換えません。

## Caseと要件の対応

同じシナリオを`-REAL`と`-EMU`へ分け、全Caseの前提/操作/期待/根拠/状態/修正/再確認/次操作をJSONに持たせています。

| Case（REAL/EMU別） | 要件 | 確認 |
| --- | --- | --- |
| `ENV-READY` | ENV-01, ENV-02, ENV-03 | 追加録画ツールの手動導入・PATHなし |
| `FIXTURE-MARKERS` | LOG-01, LOG-02, SYNC-01 | fixture表示と両ログの識別 |
| `INITIAL-OFF` | CTL-02, CTL-04, UI-01, UI-02 | 初回OFFと設定入口 |
| `ENABLE-CONNECTED` | CTL-01, UI-01 | 接続済み有効化 |
| `ENABLE-WAIT` | CTL-01, UI-01 | 未接続ONから自動開始 |
| `STATE-RESTORE` | CTL-02, UI-01 | ON/OFFと設定復元 |
| `SETTINGS-APPLY` | CTL-04, CTL-05, BUF-01, BUF-06 | 編集と明示適用 |
| `SETTINGS-INVALID` | CTL-04, CTL-05 | 設定入力検証 |
| `BUFFER-180` | BUF-01, BUF-03, SAVE-01 | 180秒実時間窓 |
| `BUFFER-SHORT` | BUF-02, SAVE-01 | 開始20秒の保存 |
| `SAVE-CONTINUE` | BUF-04, SAVE-02 | 保存中継続・連続保存 |
| `REALTIME-GAP` | BUF-03, BUF-06, SYNC-01 | 60秒内20秒の空白 |
| `RECONNECT` | BUF-05, BUF-06, UI-01 | 切断前保存と再接続 |
| `PARTIAL-CAPTURE` | BUF-07, LOG-01, UI-01 | 動画・全体ログ・アプリログ片系障害 |
| `LOG-SEPARATION` | LOG-01, LOG-02 | 両ログの別保存 |
| `APP-SELECTION` | LOG-02, UI-01 | 自動・手動・再起動追従 |
| `SYNC-PRECISION` | SYNC-01, BUF-03, BUF-06 | 映像・両ログの同期精度 |
| `SYNC-DRIFT` | SYNC-01 | 長時間・時計変化 |
| `SAVE-OUTPUT` | SAVE-01, SAVE-02, SAVE-03, SAVE-04 | 日時フォルダ・通知・保全 |
| `DISABLE` | CTL-03, CTL-04 | OFFで停止・未保存破棄 |
| `DISPOSE` | CTL-03, BUF-06, UI-01 | close/終了と遅延完了 |
| `UI-LIFECYCLE` | UI-01, UI-02, CTL-04 | ToolWindow再表示・keyboard |
| `ROTATION` | VIDEO-04, BUF-06, SYNC-01 | 回転の非致命性 |
| `VIDEO-POLICY` | VIDEO-01, VIDEO-02, VIDEO-03 | 固定画質・音声なし |
| `SAVE-FAIL-01` | SAVE-05, BUF-04 | 同じ失敗対象の再試行 |
| `SAVE-FAIL-02` | SAVE-02, SAVE-03, SAVE-04, SAVE-05 | 部分出力・最終移動失敗 |
| `SAVE-FAIL-03` | BUF-07, SAVE-05, SYNC-01 | 部分取得と書込み失敗の区別 |
| `SAVE-FAIL-04` | BUF-02, BUF-04, SAVE-02, SAVE-05 | 0件・短時間・同時刻保存 |
| `SAVE-FAIL-05` | SAVE-05, CTL-05, BUF-04 | 失敗保持と操作競合 |
| `SAVE-FAIL-06` | SAVE-05, CTL-03 | 保存取消し・寿命 |
| `SAVE-FAIL-07` | SAVE-05, BUF-07 | 保存先/一時領域の容量不足 |
| `SAVE-FAIL-08` | BUF-03, BUF-05, BUF-06, SAVE-05 | 長い切断の凍結窓 |
| `SAVE-FAIL-09` | SAVE-04, SAVE-05 | 異常終了後の所有確認 |

## 既存QA・main・資源の引継ぎ

基盤[#7](https://github.com/shinma06/android-replay-buffer/issues/7)のPLUGIN-LOAD/API互換と再起動を同じ最終ZIPで確認します。既存Caseの開発段階説明が製品実装で変更された場合は、PMが適用する期待値を明示してから観察し、古い文面のpassを捏造しません。

文書[#8](https://github.com/shinma06/android-replay-buffer/issues/8)／[#24](https://github.com/shinma06/android-replay-buffer/issues/24)／[#26](https://github.com/shinma06/android-replay-buffer/issues/26)はGUIなしの文書/CLI原型保全/設計Case接続とmain反映をPMが別に照合します。#27準備PRのmergeだけでそれらや#14/#10/Milestoneをcloseしません。

全candidate commitとmerge済みdevelop PRの対応、各Case、同じZIP/hash/ロードidentityをpromotionへ揃えてgate/CI/独立レビューを確認します。現在は実機全Caseがblockedのため、Emulatorがpassになっても全Case main gate/初期版の実機を含む完成条件は満たしません。PMへ残条件を引き継ぎ、Issue #27をopenで維持します。Marketplace/release公開・scheduled jobは対象外です。

終了時はCase結果、残条件、再開owner、private証拠を残し、自分の処理だけ停止/引継ぎしてleaseを解放します。専用AVD/fixture/sandbox/署名鍵/固定APKはQA再開用に保持し、所有・停止・不要の確認前に削除しません。共有SDK/cache/ADBは保持します。

## Cursor知見の適合

固定main `33c51dc01c1cc68ff1c035c8c3ce02654e613ce7` とdevelop `2ac9b2962fc94157639f53d428a64e4236e5d58f`を区別して参照しました。[GUI調整](https://github.com/shinma06/cursor-in-android-studio/blob/2ac9b2962fc94157639f53d428a64e4236e5d58f/docs/development/gui-coordination.md)のIssue #83由来の共有lease/fixture/Case追跡と、[ZIP手順](https://github.com/shinma06/cursor-in-android-studio/blob/2ac9b2962fc94157639f53d428a64e4236e5d58f/docs/development/plugin-zip-delivery.md)のIssue #127/#219由来の同じbytes/source/hash/ロード照合を本製品へ適合します。上流の旧IDE・Rabbitの検証履歴は本製品のpassへ転記しません。Release/schedule/製品ID/ACP・上流fixture署名・個人設定・runtime状態は非適用です。こちらではfixture assemble/lint、Case schema、管理checks/CLI原型保全を実行し、GUI未観察を残します。
