# プロジェクト情報

## プロジェクトの目的

初期版の具体的な動作・開発順序・未決事項は[製品要件](requirements.md)を正本とします。以下の目的と現行実装の事実を区別し、要件への記載を実装・受入済みとは扱いません。

**Android開発の試験や日常の動作確認で、録画開始を意識せず操作し、必要な時に1ボタンで直前の画面操作とlogcatを保存して、試験エビデンスの採取と不意に発生したバグの調査に役立てる。**

対象は、Androidアプリの実際の動作を確認する開発者・テスト担当者。主な利用場面は次の2つ。

- **IT（テスト）のエビデンス採取**: 正常・異常を問わず、実施した試験の動画とログを証跡として格納する。
- **日常の動作確認中の新規バグ調査**: 予期していなかった不具合に気づいた時点で、その直前の操作とログを保存する。再現手順がまだ分からない段階でも、発生までの操作経緯を振り返り、再現条件や原因を調べる手掛かりにする。

バックグラウンド取得を有効にしておけば、不具合の発生を予測して個別に録画を始める必要がない。取り逃した現象を記録するために、もう一度発生するのを待つ負担を減らす。

### 解消したい煩わしさ

発案者の現在の試験・動作確認には、次の負担がある。

- logcatの表示が流れ、必要な操作時点のログを後から確保しづらい。
- 使用中の録画手段には最大3分という制約があり、試験時間に合わせた扱いが必要になる。この時間は利用者の現状の説明であり、すべてのAndroid環境に共通する上限とは扱わない。
- 試験のたびに先に録画を開始する必要があり、開始を忘れると操作をやり直すことになる。
- 試験手順を間違えると録画を終了し、再度開始してから試験をやり直す必要がある。
- 日常の動作確認で不意にバグが発生しても、録画していなければ直前の操作が残らず、記憶に頼って再現手順を探すことになる。

### 目指す使い方

GeForceアプリやOBSのリプレイバッファから着想を得て、直近の操作を後から保存する方式を採用する。

1. 利用者が保持時間N秒を設定し、バックグラウンド取得を有効にする。
2. 取得が有効な間、画面録画とlogcatを継続して取得し、直近N秒分を更新しながら保持する。
3. 利用者は個別の録画開始・終了を操作せずに試験や日常の動作確認を行う。手順を間違えた場合も、取得を継続したまま試験をやり直す。
4. 試験の証跡が必要な時や予期しない不具合に気づいた時に、1ボタンで直前N秒分の動画と対応するログをまとめて指定先へ保存し、取得を続けながら次の操作へ進む。

保存の対象は、試験結果や不具合発生までの経緯を確認できる画面操作と、その操作に対応する端末全体・対象アプリのログ。初期版の保持時間は180秒を標準として変更可能にする。設定時間に満たなければ取得済み時間分を保存し、中断・復旧を含む実時間の窓で扱う。無効化で未保存バッファを破棄する一方、切断時は保存可能な状態で保持する。保存済みエビデンスは自動削除しない。詳細と残る境界条件は[製品要件](requirements.md)に従う。

### Android Studioプラグイン化の方針

現行のmacOS向けPython CLIは原型として維持する。一方、ローカル環境の設定や外部ツールの準備、CLI操作が、他の開発者へ利用を広げる際の障壁になっている。

今後はAndroid Studioから取得の開始・停止、保持時間や保存先の設定、取得状態の確認、1ボタンでの保存を行えるプラグインを目指す。利用者がOSごとの手動セットアップやコマンド操作を意識せず、普段の開発環境で試験や動作確認に集中できることを重視する。

対応の優先順はmacOS、Windows、Linux。Android StudioでAndroid開発ができる環境を前提に、録画に必要な追加ツールの個別インストールやPATH設定を利用者に求めないことを要件とする。projectのAndroid SDKからadbを解決し、固定scrcpy server・時計測定DEX・JCodecを同梱する[方式を採用](design/timeline.md)した。対応環境と動作は固定した製品ZIPで検証する。IDE内の初期版完成直後はmacOSメニューバー連携を最初に進める。方式の採用をプラグイン機能の実装済み・受入済みと扱わない。

今後の機能判断では、試験や動作確認に伴う記録操作を減らせるか、予期しない不具合を含め必要な区間の動画とログを取り逃さず保存できるか、他の開発者も導入して使えるかを基準にする。

## 現在の事実・正本

| 項目 | 現在の事実・正本 |
|---|---|
| 目的 | Android操作直前の画面・logcat・timelineを保存し、不具合の再現情報を取り逃さない。Android Studioプラグインとして利用できるようにする |
| Repository | `shinma06/android-replay-buffer` |
| 開発対象 | Android Studioプラグイン。`plugin/` に取得・保存コア、ToolWindow、設定、SDK/対象アプリの解決、情報表示actionを実装した初期版候補。製品受入は[QA #27](https://github.com/shinma06/android-replay-buffer/issues/27)で管理し、実機は利用者保留 |
| 利用可能な原型 | macOS向けPython CLI、Python 3.9以上。`replayd` / `replay save,status,stop`。[保全方針](cli-origin.md) |
| CLI原型の依存 | adb、scrcpy、ffmpeg。Python追加依存なし。任意YAMLは既存実装の条件に従う。プラグインの採用方式とは区別する |
| 実装 | `plugin/` が開発対象。CLI原型は `replay_buffer/`、入口 `bin/`、設定 `config.json.example`、起動例 `launchd/` を維持 |
| 仕様 | [製品要件](requirements.md)、[README](../README.md)。原型は[原CLI README](cli-origin/README.md)、[CLI原型の設計（凍結）](cli-origin/design.md)。要件・設計の記述だけで実装済みとしない |
| テスト | CLIは `tests/test_log_buffer.py` の既存2ケース。プラグインは `plugin/src/test/` のJVMテストで時計・保存・process所有・合成wire・設定/寿命を検証する。実機/IDE受入は別のCaseで確認する |
| 開発ハーネス | Python 3.11以上、標準ライブラリ、Bash、macOS/Linux。CLIの要求版を変更しない |
| 統合先 | 製品変更は `develop`。固定候補を `main` へpromotion。GUI不要toolingは `main`。main/developは作成済み |
| 必須checks | `test` / `PR policy` / `Acceptance gate` / `Agent review`。実適用は [導入Issue #1](https://github.com/shinma06/android-replay-buffer/issues/1) でreadbackする |
| 管理 | [Project #5](https://github.com/users/shinma06/projects/5)、[基盤・残QAのMilestone #2](https://github.com/shinma06/android-replay-buffer/milestone/2)、[初期版のMilestone #3](https://github.com/shinma06/android-replay-buffer/milestone/3)、[作業管理](work-management.md) |
| Android Studio | Rabbit 1 / JDK 25 / Gradle 9.7.1 / Kotlin 2.4.20。ID `io.github.shinma06.android-replay-buffer`。[開発手順](plugin-development.md)、[共通知見](android-studio.md) |

## 実行する確認

```bash
python3 scripts/bootstrap.py
python3 scripts/check.py
python3 scripts/workflow/product_check.py
# JDK 25 / JAVA_HOMEが必要
python3 scripts/workflow/plugin_check.py
# cleanなcommit済みのIssue差分。targetに合わせbaseを選ぶ
python3 scripts/workflow/change_impact.py --base origin/main --run-tests
```

check.pyは管理ファイル・リンク・symlink・秘密候補の限定検査とハーネス回帰試験、product_check.pyはCLI原型21ファイルの保全照合と既存unittest、plugin_check.pyはGradleのcheck/buildPlugin/verifyPluginStructureです。daemonやADB、録画、launchd、IDEを起動しません。IDE/実機受入はCase JSONで別に管理します。

CLI変更ではprocessの所有と停止、再接続、バッファ容量、時刻対応、保存失敗時のデータ保全、ログの秘密情報を確認します。Android Studio側はLifecycle/EDT/dispose、非同期設定反映、購読解除と取得process終了を確認します。DBを使用しないためDBクエリ検証は対象外です。
