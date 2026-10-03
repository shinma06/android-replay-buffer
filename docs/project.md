# プロジェクト情報

## プロジェクトの目的

**Android開発のIT（テスト）で、録画開始を意識せず試験を行い、必要な時に1ボタンで直前の画面操作とlogcatをエビデンスとして保存できるようにする。**

対象は、Androidアプリの実際の動作を確認し、動画とログを試験エビデンスとして格納する開発者・テスト担当者。不具合を発見した時だけでなく、正常に実施できた試験の証跡を残す場面にも使う。

### 解消したい煩わしさ

発案者の現在の試験作業には、次の負担がある。

- logcatの表示が流れ、必要な操作時点のログを後から確保しづらい。
- 使用中の録画手段には最大3分という制約があり、試験時間に合わせた扱いが必要になる。この時間は利用者の現状の説明であり、すべてのAndroid環境に共通する上限とは扱わない。
- 試験のたびに先に録画を開始する必要があり、開始を忘れると操作をやり直すことになる。
- 試験手順を間違えると録画を終了し、再度開始してから試験をやり直す必要がある。

### 目指す使い方

GeForceアプリやOBSのリプレイバッファから着想を得て、直近の操作を後から保存する方式を採用する。

1. 利用者が保持時間N秒を設定し、バックグラウンド取得を有効にする。
2. 取得が有効な間、画面録画とlogcatを継続して取得し、直近N秒分を更新しながら保持する。
3. 利用者は試験ごとの録画開始・終了を操作せずに試験を行う。手順を間違えた場合も、取得を継続したまま試験をやり直す。
4. 必要な時に1ボタンで直前N秒分の動画と対応するログをまとめて指定先へ保存し、取得を続けながら次の試験へ進む。

保存の対象は、試験結果を確認できる画面操作と、その操作に対応するログ。継続取得用の一時バッファと保存済みエビデンスを区別し、直近区間の更新によって保存済みエビデンスを失わないことを目指す。取得開始直後や端末切断時など、指定時間分を確保できない場合の表示・保存条件は、実装時に定義・検証する。

### Android Studioプラグイン化の方針

現行のmacOS向けPython CLIは原型として維持する。一方、ローカル環境の設定や外部ツールの準備、CLI操作が、他の開発者へ利用を広げる際の障壁になっている。

今後はAndroid Studioから取得の開始・停止、保持時間や保存先の設定、取得状態の確認、1ボタンでの保存を行えるプラグインを目指す。利用者がOSごとの手動セットアップやコマンド操作を意識せず、普段の開発環境で試験に集中できることを重視する。

プラグイン化だけでOS・ソフトウェアへの依存がなくなるとは保証しない。対応OS・Android Studio・端末の範囲、ADB接続の前提、録画に必要なツールの同梱・導入方法は、実装と検証を通じて決める。上記は製品の目標であり、プラグイン機能の実装済み・受入済みを示すものではない。

今後の機能判断では、試験前後の操作を減らせるか、必要な区間の動画とログを取り逃さず保存できるか、他の開発者も導入して使えるかを基準にする。

## 現在の事実・正本

| 項目 | 現在の事実・正本 |
|---|---|
| 目的 | Android操作直前の画面・logcat・timelineを保存し、不具合の再現情報を取り逃さない。今後Android Studioプラグインから利用できるようにする |
| Repository | `shinma06/android-replay-buffer` |
| 現行製品 | macOS向けPython CLI、Python 3.9以上。`replayd` / `replay save,status,stop` |
| 依存 | adb、scrcpy、ffmpeg。Python追加依存なし。任意YAMLは既存実装の条件に従う |
| 実装 | `replay_buffer/`、入口 `bin/`、設定 `config.json.example`、起動例 `launchd/` |
| 仕様 | [README](../README.md)、[既存設計書](../設計書.md)。実装との差は確認して扱い、設計の記述だけで実装済みとしない |
| テスト | `tests/test_log_buffer.py` の既存2ケース。実機録画・再接続・動画保存を網羅するものではない |
| 開発ハーネス | Python 3.11以上、標準ライブラリ、Bash、macOS/Linux。CLIの要求版を変更しない |
| 統合先 | 製品変更は `develop`。固定候補を `main` へpromotion。GUI不要toolingは `main`。導入PR統合後、そのmain SHAからdevelopを新設する |
| 必須checks | `test` / `PR policy` / `Acceptance gate` / `Agent review`。実適用は [導入Issue #1](https://github.com/shinma06/android-replay-buffer/issues/1) でreadbackする |
| 管理 | [Project #5](https://github.com/users/shinma06/projects/5)、[Milestone #1](https://github.com/shinma06/android-replay-buffer/milestone/1)、[作業管理](work-management.md) |
| Android Studio | プラグイン未実装。SDK/JDK/Gradle/Plugin IDは未選定。[共通知見の入口](android-studio.md)から着手時に適合する |

## 実行する確認

```bash
python3 scripts/bootstrap.py
python3 scripts/check.py
python3 scripts/workflow/product_check.py
# cleanなcommit済みのIssue差分。targetに合わせbaseを選ぶ
python3 scripts/workflow/change_impact.py --base origin/main --run-tests
```

check.pyは管理ファイル・リンク・symlink・秘密候補の限定検査とハーネス回帰試験、product_check.pyは既存CLIのunittestです。daemonやADB、録画、launchdを起動しません。プラグインbuild/ZIP/IDE fixtureは、実際のGradleプロジェクトを導入するIssueで接続します。存在しないbuildを成功や安全なskipとして扱いません。

CLI変更ではprocessの所有と停止、再接続、バッファ容量、時刻対応、保存失敗時のデータ保全、ログの秘密情報を確認します。Android Studio側のLifecycle/EDT/dispose/coroutineはプラグイン実装時に適用します。
