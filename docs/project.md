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
| 管理 | [Project #5](https://github.com/users/shinma06/projects/5)、現行は [Milestone #3 — Android Studio内の初期版](https://github.com/shinma06/android-replay-buffer/milestone/3)。完了済み [Milestone #1](https://github.com/shinma06/android-replay-buffer/milestone/1) は初回ハーネス導入の履歴。各作業は対応 Issue の実 Milestone を確認する。[作業管理](work-management.md) |
| Android Studio候補 | [固定develop候補 `3a9af24`](https://github.com/shinma06/android-replay-buffer/tree/3a9af24231c3db339c0b3a82ba8aa8eb4cecb189/plugin/)にコア・ToolWindow・設定・SDK/対象アプリ解決・情報表示actionを実装。Rabbit 1 / JDK 25 / Gradle 9.7.1 / Kotlin 2.4.20、ID `io.github.shinma06.android-replay-buffer`。[開発手順](https://github.com/shinma06/android-replay-buffer/blob/3a9af24231c3db339c0b3a82ba8aa8eb4cecb189/docs/plugin-development.md)。製品main昇格は未完了 |

## 製品候補と受入の現在地

昇格状況の以下の記載は2026-10-13のmain `f8ec628` 時点です。昇格後の実状態は[#87](https://github.com/shinma06/android-replay-buffer/issues/87)・[#10](https://github.com/shinma06/android-replay-buffer/issues/10)で読み戻してください。

開発対象は `plugin/` のAndroid Studioプラグインです。固定develop候補は `3a9af24231c3db339c0b3a82ba8aa8eb4cecb189`、標準ZIPのSHA-256は `ca74694b93c0b101e19348c9826a22d03285662fe5ada9e7006a94d2c59c173b`。mainには製品候補がまだ統合されていないため、候補にだけ存在する文書は固定sourceへリンクしています。昇格後のローカル正本は `docs/requirements.md`、`docs/cli-origin.md`、`docs/cli-origin/README.md`、`docs/cli-origin/design.md`、`docs/plugin-development.md` です。[製品要件](https://github.com/shinma06/android-replay-buffer/blob/3a9af24231c3db339c0b3a82ba8aa8eb4cecb189/docs/requirements.md)・[CLI保全方針](https://github.com/shinma06/android-replay-buffer/blob/3a9af24231c3db339c0b3a82ba8aa8eb4cecb189/docs/cli-origin.md)を参照してください。

全139 Case/208出典のうち、初期Agent範囲105件は[QA #27で正式採用](https://github.com/shinma06/android-replay-buffer/issues/27#issuecomment-6136845141)済みです。段階受入の充足であり、完全GUI受入ではありません。REAL34・人間確認の延期は[QA #65](https://github.com/shinma06/android-replay-buffer/issues/65)、製品main昇格は[#87](https://github.com/shinma06/android-replay-buffer/issues/87)、初期版完了は[#10](https://github.com/shinma06/android-replay-buffer/issues/10)で管理します。mainの47件の再利用証明・旧観察・失敗と現行の独立評価/保証別影響条件は維持します。

開発順は **初期版完成 → [#206 全技術スタックのstable更新](https://github.com/shinma06/android-replay-buffer/issues/206) → [#103 CLI内包](https://github.com/shinma06/android-replay-buffer/issues/103)** です。[過去管理欄整備 #237](https://github.com/shinma06/android-replay-buffer/issues/237)は初期版後です。

## 実行する確認

```bash
python3 scripts/bootstrap.py
python3 scripts/check.py
python3 scripts/workflow/product_check.py
# cleanなcommit済みのIssue差分。targetに合わせbaseを選ぶ
python3 scripts/workflow/change_impact.py --base origin/main --run-tests
```

check.pyは管理ファイル・リンク・symlink・秘密候補の限定検査とハーネス回帰試験、product_check.pyは全CLI Pythonモジュールのcompile/importと既存2ケースです。CLI原型保全checkerがある候補/昇格後ツリーでは、先にmanifestによる21ファイルの保全照合も行います。必須`test`と移行中の旧jobはPython 3.9で同じ検査と、未読込みモジュールに3.10構文を入れた所有fixtureの拒否・元の内容への復旧回帰を実行します。構文・import確認はAPI/録画/実機互換性全体の保証ではありません。daemonやADB、録画、launchdを起動しません。候補/昇格後ツリーで `plugin/` と checker が存在する場合、プラグイン変更は `python3 scripts/workflow/plugin_check.py`（JDK 25 / JAVA_HOMEが必要）でGradleのcheck/buildPlugin/verifyPluginStructureを実行します。プラグインがまだないmainで存在しないbuildを成功や安全なskipとして扱いません。build成功はIDE受入を意味しません。

CLI変更ではprocessの所有と停止、再接続、バッファ容量、時刻対応、保存失敗時のデータ保全、ログの秘密情報を確認します。Android Studio側はLifecycle/EDT/dispose、非同期設定反映、購読解除と取得process終了を確認します。DBを使用しないためDBクエリ検証は対象外です。
