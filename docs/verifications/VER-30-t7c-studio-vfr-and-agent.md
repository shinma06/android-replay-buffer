---
id: VER-30
type: verification
name: T-7C Studio の可変フレームレート実 PTS 再生・ログの分割取得・タブを閉じる際の viewport 回収・device-agent の UID 不明ヘッダーと統計プローブ停止・常時録画 3 状態表示・トレイ表示設定・旧設定の一回移行の検証記録
verifies:
  - SPEC-01
  - SPEC-02
  - SPEC-03
  - SPEC-04
  - REQ-09
  - REQ-10
  - REQ-11
  - REQ-13
---

# VER-30: T-7C Studio 実 PTS 再生・ログ分割取得・viewport 回収・device-agent 修正 検証記録

記録日: 2026-11-24（macOS arm64 / JDK 27 / Android SDK build-tools 37.0.0）

対象は昇格前監査の H8・M1・M2・M10・M11 と、常時録画の 3 状態表示、メニューバー / システムトレイ表示の設定、旧設定の一回限りの移行。

## 1. 修正と回帰試験

| 項目 | 修正 | 回帰試験（失敗先行の根拠） |
| --- | --- | --- |
| H8 可変フレームレートを平均 FPS で再生 | `ArbBundleFileEditor` が `media.open_viewport` の `frame_pts_ns`（フレームごとの実 PTS）を `PlaybackController(framePtsNs = ...)` に渡す。再生・シーク・ループ折り返しは同じ PTS 表を使う。`frame_pts_ns` を返さない旧 `arbd` では従来どおり平均 FPS に縮退 | `studioEditorWiresRealVfrFramePtsNsForPlayheadTimingAndPostLoopAudioAlignment`: 先頭フレームが 5 秒静止し、その後 30 fps で 10 フレーム続く合成録画をエディタ経由で開き、仮想時計で 1.0 秒・4.9 秒はフレーム 0 のまま、5.0 秒でフレーム 1、5.105 秒でフレーム 4、ループ折り返し後の音声位置がフレーム 1 の実 PTS − `av_offset_ms`（5.000 s − 50 ms）に一致することを検査。修正前のエディタは `framePtsNs` を渡さないため、1.0 秒時点で既に先へ進む |
| M1 タブを閉じる時の viewport 残留 | lease・viewport・SHM reader・`PlaybackController` の登録と `disposed` 判定を `lifecycleLock` 下で不可分に行う。`open_viewport` 応答時に既に破棄済みなら、その場で（EDT 外）`media.close_viewport` を呼ぶ。登録済みなら `dispose()` が一度だけ閉じる。共有 IPC 接続は閉じない | `closingEditorMidOpenViewportClosesReturnedViewportWithoutLeakingOrBreakingSharedIpc`: `open_viewport` を latch で止めた状態でタブを閉じ、その後に応答させる。viewport と lease がともに 0 に戻り、`close_viewport` はちょうど 1 回、共有トランスポートは開いたままで、続く `system.status` が `active_viewports == 0` を返す |
| M2 ログが多いと Studio タブ全体を開けない | 映像を先に開き、ログは別ワーカーで取得する。各ページの最後の行番号を排他的カーソル `after_seq` とし、同一時刻の行も含めて最大 5,000 行をデーモンと同じ順で表示する。応答上限に達したらページサイズを半分にする。旧デーモンでは上限付き一括取得に戻り、部分取得時は画面に表示する。 | 1 KiB × 5,000 行、同一時刻の大きな行、時刻と行番号の逆転、カーソル未対応の各ケースで取得した行番号を照合する。 |
| M10 UID 不明の 24 byte 旧ヘッダー | `LoggerEntryParser` は 20 byte（v1）・24 byte（v2/v3）ヘッダーで `uid` を必ず `UNKNOWN_U32` とし、`1000` を推定しない。同じパーサーで直前に 28 byte・`uid=1000` を読んだ後もバッファの残りを流用しない。この不変条件をクラス文書に明記。自動保存などの判定側（`arbd/src/capture.rs`）は並行タスク T-7A の担当 | `Aos9CompatTest.testM10Legacy24ByteHeadersStrictlyEmitUnknownUidNeverSystem1000`: 28 byte `uid=1000` の直後に 24 byte（events / system）と 20 byte のレコードを読み、すべて `UNKNOWN_U32` で `1000` ではないこと、wire 往復でも保たれることを検査 |
| M11 logcat 統計プローブが停止対象から漏れる | 統計プローブの子 `Process` を `LogcatWorker` が所有し、`stop()` とプローブ期限（4 秒）の両方で `destroyForcibly()`、標準入出力のクローズ、`waitFor` による回収を行う。期限切れ後も統計ループは次の周期で再開する | `testM11StatsProbeChildDestroyedAndReapedOnStopMidLine`: 1 行の途中で割込みを無視して待ち続ける偽 `Process` を使い、`stop()` 後に子プロセスの強制終了・stdout クローズ・回収、読取と統計の両スレッド終了を検査。`testM11StatsProbeTimeoutDestroysHungChildAndRecoversStatsLoop`: 期限で強制終了・回収され、次のプローブが実行される。修正前は子をローカル変数でしか持たず、`stop()` から到達できなかった |
| 常時録画の 3 状態表示 | Tool Window と Running Devices のトグルで `OFF（停止中）`・`ON・待機中（端末接続で自動開始）`・`ON・REC（取得中）` を区別。端末 0 台でも ON にできる（`config.update(capture_enabled = true)`）。`DumbAwareToggleAction.isSelected` は `captureEnabled || captureActive` で、切断・再接続をまたいで ON 表示を保つ。`capture_enabled` を受け付けない旧 `arbd` では、従来どおり `capture.start` / `capture.stop` のみで切り替える | `alwaysOnCaptureArmingWorksWithZeroDevicesAndDistinguishesAllThreeStates`、`toggleFallsBackToDirectCaptureStartWhenDaemonRejectsCaptureEnabled` |
| メニューバー / システムトレイ表示の設定 | `ArbLocalSettingsState.trayEnabled`（既定 `false`）と設定画面の `メニューバー / システムトレイアイコンを表示する`（`ArbLabeledCheckbox`）。適用時と IDE 起動時（`trayEnabled == true` の場合）に同梱 `arb-tray` を EDT 外で起動し、OFF 適用時と IDE 終了時（`ArbdDaemonService.dispose()`）にはプラグインが起動した `arb-tray` だけを終了する（`arbd` は止めない）。設定の適用は専用の 1 スレッドで順に処理し、停止は旧プロセスの終了まで待ってから次の起動に進む（旧プロセスが単一起動ロックを持ったままだと新しいトレイが即終了するため） | `trayEnabledDefaultsFalseRoundTripsInSettingsAndControlsManagedTrayProcessLifecycle`: 既定値、`isModified` / `apply` の往復、`syncTrayState(true → true → false → true)` で二重起動しないこと、`dispose()` で終了すること。`quickTrayOffThenOnWaitsForOldTrayToExitAndEndsRunning`: 終了に 200 ms かかる旧トレイで OFF → ON を続けて適用し、旧プロセスの終了前に新プロセスを起動せず、最後にトレイが動いていること |
| 旧設定の一回限りの移行 | `LegacyV1SettingsMigrator.kt` に隔離（呼出しは `ArbStartupActivity` の 1 行と専用試験のみ。`main` 昇格時に削除予定と冒頭に明記）。`.idea/workspace.xml` を読み取り専用・XXE 無効で解析し、`retentionSeconds` を 5〜600 に収め、絶対パスのみ採用、廃止項目は無視して `config.update` と IDE ローカル設定へ移す。IntelliJ の XmlSerializer は既定値の項目を省略するため、旧コンポーネントがあれば省略項目は旧版の既定値（180 秒、映像＋ログ、標準画質、録音・自動保存・取得 OFF）として移す。動画品質は旧版が保存する表示文字列（`軽量（1024px / 15fps / 2Mbps）` など）も解釈する。`capture_enabled` を受け付けない `arbd` では、その項目を除いて再送し、他の設定は移す。移行済みフラグ `migratedFromV1` が立った後は上書きしない。`arbd` 未接続時は次回起動で再試行 | `LegacyV1SettingsMigratorTest`（11 件）: 設定あり・なし、範囲外の値、2 回目の実行、壊れた XML と DOCTYPE、`arbd` 未接続、省略項目が旧版の既定値になること、表示文字列の動画品質、`capture_enabled` 非対応の `arbd` |

## 2. ローカル検証結果

- `./device-agent/build.sh --with-scrcpy-audio`: PASS（`ProtocolTest` 19 件、`Aos9CompatTest` 5 件、DEX 生成）。
- `(cd ide-plugin && ./gradlew test buildPlugin --no-daemon)`（JDK 27）: PASS。T-7C の試験は `ArbT7cStudioVfrAndLifecycleTest` 11 件と `LegacyV1SettingsMigratorTest` 11 件。
- `codex review --base origin/v2-develop`: 指摘 4 件（旧設定の省略項目・動画品質の表示文字列・`capture_enabled` 非対応時の移行失敗・トレイの OFF → ON 競合）をすべて修正し、上記の回帰試験を追加。
- `./core/scripts/verify.sh`: PASS（fmt / clippy `-D warnings` / test / GPL・AGPL ゼロ / `sara check --strict` / REQ カバレッジ）。

T-7A・T-7B を含む統合ブランチで、同時刻ログのカーソル継続と旧デーモンへの切替、実 PTS の表示時間、トレイからの待機解除、設定保存失敗時の再試行、プロジェクト同時起動を検証した。

## 3. 未検証

- 実 Android 端末・Android Studio GUI・エミュレータでの受け入れ（可変フレームレート録画の再生、常時録画の待機から自動開始、メニューバーアイコンの表示と消去）は未実施。判定ロジックはヘッドレスの単体試験で検証した。
- Windows・Linux での IDE プラグインの動作は GitHub Actions `os-parity` の結果で確認する（ローカルは macOS のみ）。
