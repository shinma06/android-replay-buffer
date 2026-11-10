# プロジェクト情報

| 項目 | 現在の事実・正本 |
|---|---|
| 目的 | Android操作直前の画面・logcat・timelineを保存し、不具合の再現情報を取り逃さない。Android Studioプラグインとして利用できるようにする |
| Repository | `shinma06/android-replay-buffer` |
| 製品の入口 | Android StudioのToolWindowと、同じPlugin ZIP内のmacOS向け`bin/replay` / `bin/replayd`。CLIは起動済みの対象projectだけを操作する |
| 製品runtime | 対応Android StudioのJBR・Kotlin runtime、projectのAndroid SDKのADB。同梱取得engine・writerを共有し、Python/scrcpy/ffmpegの別インストールは不要 |
| 実装 | `plugin/`に製品source・CLI入口・tests・配布定義を集約。Python原型は現行treeから退役し、[固定Git履歴](cli-origin.md)から復旧できる |
| 仕様 | [製品要件](requirements.md)、[保存設計](plugin-design.md)、[CLI互換・移行](cli.md)。実装・自動テスト・実機受入を区別する |
| テスト | `plugin/src/test/`のcore・IDE接続・CLI通信/設定/保存結果・配布物検査。実機録画とIDE操作の受入は[Case台帳](verification/README.md)で別途確認する |
| 開発 | Devinが担当し、セッションはEntireに記録する。進め方は[AGENTS.md](../AGENTS.md)、旧運用は[legacy-ops](legacy-ops/README.md) |
| 統合先 | 製品変更は `develop`。実機・IDEで確認した候補を `main` へ反映し、`vX.Y.Z`タグでGitHub Releaseを作る |
| 必須checks | `test`（`plugin_check.py`） |
| 管理 | Entire Trails。既存のGitHub Issueは背景情報で、目的を達成したか不要になった時点で閉じる |
| Android Studio plugin | `plugin/`に取得コア・ToolWindow・設定・SDK/対象アプリ解決・情報表示actionを実装。初期版はmain反映済み。第2版のbuild SDKはRabbit 2（#391で更新、互換性受入は別）/ JDK 27（JVM/API target 25）/ Gradle 9.8.1 / Kotlin 2.4.21、ID `io.github.shinma06.android-replay-buffer`。[現行の開発手順](plugin-development.md)と固定候補ごとの受入を区別する |

## 製品候補と受入の現在地

2026-10-17に[#87](https://github.com/shinma06/android-replay-buffer/issues/87)・[#10](https://github.com/shinma06/android-replay-buffer/issues/10)のCLOSED（2026-10-13）を読み戻しました。初期版の製品main反映・Agent段階は完了済みで、第2版の実装・受入・main反映は[#280](https://github.com/shinma06/android-replay-buffer/issues/280)で管理します。第2版のsource/build更新だけで初期版の観察を新版のPASSにしません。

初期版の途中候補 `3a9af24231c3db339c0b3a82ba8aa8eb4cecb189`、標準ZIP SHA-256 `ca74694b93c0b101e19348c9826a22d03285662fe5ada9e7006a94d2c59c173b`は履歴です。現行の正本は[製品要件](requirements.md)・[CLI互換・移行](cli.md)・[plugin開発](plugin-development.md)です。

初期版の全139 Case/208出典のうち、初期Agent範囲105件は[QA #27で正式採用](https://github.com/shinma06/android-replay-buffer/issues/27#issuecomment-6136845141)済みです。段階受入の充足であり、完全GUI受入ではありません。REAL34・人間確認の延期は[QA #65](https://github.com/shinma06/android-replay-buffer/issues/65)、製品main昇格は[#87](https://github.com/shinma06/android-replay-buffer/issues/87)、初期版完了は[#10](https://github.com/shinma06/android-replay-buffer/issues/10)で管理します。mainの47件の再利用証明・旧観察・失敗と現行の独立評価/保証別影響条件は維持します。

開発順は **初期版完成 → [#206 全技術スタックのstable更新](https://github.com/shinma06/android-replay-buffer/issues/206) → [#103 CLI内包](https://github.com/shinma06/android-replay-buffer/issues/103)** です。[過去管理欄整備 #237](https://github.com/shinma06/android-replay-buffer/issues/237)は第2版の今回scopeに含めず、既存計画として保持します。

## 実行する確認

```bash
python3 scripts/workflow/plugin_check.py
```

CLI変更ではprocessの所有と停止、再接続、バッファ容量、時刻対応、保存失敗時のデータ保全、ログの秘密情報を確認します。Android Studio側はLifecycle/EDT/dispose、非同期設定反映、購読解除と取得process終了を確認します。DBを使用しないためDBクエリ検証は対象外です。
