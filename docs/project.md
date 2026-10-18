# プロジェクト情報

| 項目 | 現在の事実・正本 |
|---|---|
| 目的 | Android操作直前の画面・logcat・timelineを保存し、不具合の再現情報を取り逃さない。Android Studioプラグインとして利用できるようにする |
| Repository | `shinma06/android-replay-buffer` |
| mainで利用可能な原型 | macOS向けPython CLI、Python 3.9以上。`replayd` / `replay save,status,stop` |
| CLI原型の依存 | adb、scrcpy、ffmpeg。Python追加依存なし。任意YAMLは既存実装の条件に従う |
| CLI原型の実装 | `replay_buffer/`、入口 `bin/`、設定 `config.json.example`、起動例 `launchd/` |
| 仕様 | [README](../README.md)、[CLI原型の設計（凍結）](https://github.com/shinma06/android-replay-buffer/blob/3a9af24231c3db339c0b3a82ba8aa8eb4cecb189/docs/cli-origin/design.md)（移設後は `docs/cli-origin/design.md`）。実装との差は確認して扱い、設計の記述だけで実装済みとしない |
| テスト | `tests/test_log_buffer.py` の既存2ケース。実機録画・再接続・動画保存を網羅するものではない |
| 開発ハーネス | 主検査/CIはPython 3.15.0 stable。管理/開発scriptの対応下限はPython 3.15以上、標準ライブラリ、Bash、macOS/Linux。配布CLIのPython 3.9以上とは別。[runtime選択・移行](setup/README.md#runtimeと配置) |
| 統合先 | 製品変更は `develop`。固定候補を `main` へpromotion。GUI不要toolingは `main`。main/developは作成済み |
| 必須checks | 共通集合は `test` / `PR policy` / `Acceptance gate` / `Agent review`。Python原型の実3.9検査は`test`に含め、旧`CLI Python 3.9` jobも移行中は実行する。実保護の旧contextは[branchごとの移行条件](setup/github.md#runtime検査の共通testへの段階移行)を満たしてから外す |
| 管理 | [Project #5](https://github.com/users/shinma06/projects/5)、現行は [Milestone #4 — 第2版](https://github.com/shinma06/android-replay-buffer/milestone/4)。[Milestone #3 — 初期版](https://github.com/shinma06/android-replay-buffer/milestone/3)はAgent段階完了。完了済み [Milestone #1](https://github.com/shinma06/android-replay-buffer/milestone/1) は初回ハーネス導入の履歴。各作業は対応 Issue の実 Milestone を確認する。[作業管理](work-management.md) |
| Android Studio plugin | `plugin/`に取得コア・ToolWindow・設定・SDK/対象アプリ解決・情報表示actionを実装。初期版はmain反映済み。第2版 #206のbuildはRabbit 1 / JDK 27（JVM/API target 25）/ Gradle 9.8.1 / Kotlin 2.4.21、ID `io.github.shinma06.android-replay-buffer`。[現行の開発手順](plugin-development.md)と固定候補ごとの受入を区別する |

## 製品候補と受入の現在地

2026-10-17に[#87](https://github.com/shinma06/android-replay-buffer/issues/87)・[#10](https://github.com/shinma06/android-replay-buffer/issues/10)のCLOSED（2026-10-13）を読み戻しました。初期版の製品main反映・Agent段階は完了済みで、第2版の実装・受入・main反映は[#280](https://github.com/shinma06/android-replay-buffer/issues/280)で管理します。第2版のsource/build更新だけで初期版の観察を新版のPASSにしません。

初期版の途中候補 `3a9af24231c3db339c0b3a82ba8aa8eb4cecb189`、標準ZIP SHA-256 `ca74694b93c0b101e19348c9826a22d03285662fe5ada9e7006a94d2c59c173b`は履歴です。現行の正本は[製品要件](requirements.md)・[CLI保全方針](cli-origin.md)・[plugin開発](plugin-development.md)です。

初期版の全139 Case/208出典のうち、初期Agent範囲105件は[QA #27で正式採用](https://github.com/shinma06/android-replay-buffer/issues/27#issuecomment-6136845141)済みです。段階受入の充足であり、完全GUI受入ではありません。REAL34・人間確認の延期は[QA #65](https://github.com/shinma06/android-replay-buffer/issues/65)、製品main昇格は[#87](https://github.com/shinma06/android-replay-buffer/issues/87)、初期版完了は[#10](https://github.com/shinma06/android-replay-buffer/issues/10)で管理します。mainの47件の再利用証明・旧観察・失敗と現行の独立評価/保証別影響条件は維持します。

開発順は **初期版完成 → [#206 全技術スタックのstable更新](https://github.com/shinma06/android-replay-buffer/issues/206) → [#103 CLI内包](https://github.com/shinma06/android-replay-buffer/issues/103)** です。[過去管理欄整備 #237](https://github.com/shinma06/android-replay-buffer/issues/237)は第2版の今回scopeに含めず、既存計画として保持します。

## 実行する確認

```bash
python3 scripts/bootstrap.py
python3 scripts/check.py
python3 scripts/workflow/product_check.py
# cleanなcommit済みのIssue差分。targetに合わせbaseを選ぶ
python3 scripts/workflow/change_impact.py --base origin/main --run-tests
```

check.pyは管理ファイル・リンク・symlink・秘密候補の限定検査とハーネス回帰試験、product_check.pyは全CLI Pythonモジュールのcompile/importと既存2ケースです。CLI原型保全checkerがある候補/昇格後ツリーでは、先にmanifestによる21ファイルの保全照合も行います。必須`test`と移行中の旧jobはPython 3.9で同じ検査と、未読込みモジュールに3.10構文を入れた所有fixtureの拒否・元の内容への復旧回帰を実行します。構文・import確認はAPI/録画/実機互換性全体の保証ではありません。daemonやADB、録画、launchdを起動しません。プラグイン変更は `python3 scripts/workflow/plugin_check.py`（JDK 27 / JAVA_HOME、SDK API 37.2 revision 1 / ANDROID_HOMEが必要）でGradleのcheck/buildPlugin/verifyPluginStructureを実行します。build成功はIDE受入を意味しません。

CLI変更ではprocessの所有と停止、再接続、バッファ容量、時刻対応、保存失敗時のデータ保全、ログの秘密情報を確認します。Android Studio側はLifecycle/EDT/dispose、非同期設定反映、購読解除と取得process終了を確認します。DBを使用しないためDBクエリ検証は対象外です。
