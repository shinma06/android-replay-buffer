# プロジェクト情報

| 項目 | 現在の事実・正本 |
|---|---|
| 目的 | Android操作直前の画面・logcat・timelineを保存し、不具合の再現情報を取り逃さない。Android Studioプラグインとして利用できるようにする |
| Repository | `shinma06/android-replay-buffer` |
| 製品の入口 | Android StudioのToolWindow・設定画面と、導入済みプラグインの`bin/replay`（Windowsは`bin\replay.cmd`、Linux / macOS / Windowsで同じ動作）。CLIと`replay mcp`は起動済みの対象projectだけを、GUIと同じ処理で操作するリモコン |
| 製品runtime | 対応Android StudioのJBR・Kotlin runtime、projectのAndroid SDKのADB。同梱取得engine・writerを共有し、Python/scrcpy/ffmpegの別インストールは不要 |
| 実装 | `plugin/`に製品source・CLI入口・tests・配布定義を集約。Python原型は現行treeから退役し、[固定Git履歴](cli-origin.md)から復旧できる |
| 仕様 | [製品要件](requirements.md)、[保存設計](plugin-design.md)、[CLI](cli.md)。実装・自動テスト・実機受入を区別する |
| テスト | `plugin/src/test/`のcore・IDE接続・CLI通信/設定/保存結果・配布物検査。結果とIDE・端末の未検証項目は[検証方針](verification/README.md)に従って記録する |
| 開発 | Devinが担当し、セッションはEntireに記録する。進め方は[AGENTS.md](../AGENTS.md)、旧運用は[legacy-ops](legacy-ops/README.md) |
| 統合先 | 製品変更は `develop`。`develop`のCI（Linux・macOS・Windows）が通るたびに、そのZIPでプレリリース`early`を差し替える（VM受入なしの開発版で、Latestにはしない）。VMでIDE・エミュレータ受入をした候補を `main` へ反映し、`vX.Y.Z`タグでGitHub Releaseを作る |
| 必須checks | `test`（`plugin_check.py`）と `os-parity`（macOS・Windows） |
| 管理 | Entire Trails。既存のGitHub Issueは背景情報で、目的を達成したか不要になった時点で閉じる |
| Android Studio plugin | `plugin/`に取得コア・ToolWindow・設定・SDK/対象アプリ解決・情報表示actionを実装。初期版はmain反映済み。第2版のbuild SDKはRabbit 2（#391で更新、互換性受入は別）/ JDK 27（JVM/API target 25）/ Gradle 9.8.1 / Kotlin 2.4.21、ID `io.github.shinma06.android-replay-buffer`。[現行の開発手順](plugin-development.md)と固定候補ごとの受入を区別する |

## 検証とリリース

`develop` はCIの `test` / `os-parity` を通して統合します。IDE・実機で観察できない項目は理由を付けて未検証と記録し、統合を止めません。`main` 昇格時のみ [VM受入](verification/vm-acceptance.md) を行います。手順は [検証方針](verification/README.md)・[リリース規則](release.md)、旧候補の記録は [旧運用の保管先](legacy-ops/README.md) を参照してください。

## 実行する確認

```bash
python3 scripts/workflow/plugin_check.py
```

CLI変更ではprocessの所有と停止、再接続、バッファ容量、時刻対応、保存失敗時のデータ保全、ログの秘密情報を確認します。Android Studio側はLifecycle/EDT/dispose、非同期設定反映、購読解除と取得process終了を確認します。DBを使用しないためDBクエリ検証は対象外です。
