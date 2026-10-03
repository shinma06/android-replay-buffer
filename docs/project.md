# プロジェクト情報

| 項目 | 現在の事実・正本 |
|---|---|
| 目的 | Android操作直前の画面・logcat・timelineを保存し、不具合の再現情報を取り逃さない。Android Studioプラグインとして利用できるようにする |
| Repository | `shinma06/android-replay-buffer` |
| 開発対象 | Android Studioプラグイン。`plugin/` にKotlin/Gradle・情報表示action。録画機能の接続は未実装 |
| 利用可能な原型 | macOS向けPython CLI、Python 3.9以上。`replayd` / `replay save,status,stop`。[保全方針](cli-origin.md) |
| 依存 | adb、scrcpy、ffmpeg。Python追加依存なし。任意YAMLは既存実装の条件に従う |
| 実装 | `plugin/` が開発対象。CLI原型は `replay_buffer/`、入口 `bin/`、設定 `config.json.example`、起動例 `launchd/` を維持 |
| 仕様 | [README](../README.md)、[原CLI README](cli-origin/README.md)、[既存設計書](../設計書.md)。実装との差は確認して扱い、設計の記述だけで実装済みとしない |
| テスト | `tests/test_log_buffer.py` の既存2ケース。実機録画・再接続・動画保存を網羅するものではない |
| 開発ハーネス | Python 3.11以上、標準ライブラリ、Bash、macOS/Linux。CLIの要求版を変更しない |
| 統合先 | 製品変更は `develop`。固定候補を `main` へpromotion。GUI不要toolingは `main`。main/developは作成済み |
| 必須checks | `test` / `PR policy` / `Acceptance gate` / `Agent review`。実適用は [導入Issue #1](https://github.com/shinma06/android-replay-buffer/issues/1) でreadbackする |
| 管理 | [Project #5](https://github.com/users/shinma06/projects/5)、[Milestone #2](https://github.com/shinma06/android-replay-buffer/milestone/2)、[作業管理](work-management.md) |
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

CLI変更ではprocessの所有と停止、再接続、バッファ容量、時刻対応、保存失敗時のデータ保全、ログの秘密情報を確認します。Android Studio側のLifecycle/EDT/dispose/coroutineはプラグイン実装時に適用します。
