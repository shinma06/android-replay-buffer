# Android Studioプラグインの開発

`plugin/` は独立したKotlin/JVMのIntelliJ Platformプラグインです。Android端末で動くアプリではないため、Android Gradle Plugin、XML View/ViewBinding、Composeは使用しません。初期版候補はToolWindowと常設設定から取得・保存を操作し、Toolsメニューの「Android Replay Bufferについて」で開発状況とロードした版を表示します。固定ZIPの製品受入は[QA #27](https://github.com/shinma06/android-replay-buffer/issues/27)で別に管理します。

## 固定した開発環境

| 項目 | 値 |
| --- | --- |
| Plugin ID | `io.github.shinma06.android-replay-buffer` |
| Kotlin package | `io.github.shinma06.replaybuffer` |
| Android Studio SDK | Rabbit 1 `2026.2.1.8` / `AI-262.9437.185.2621.16467767` |
| 宣言するIDE範囲 | `262.9437.185` 〜 `262.*`。実受入は固定Rabbitで実施し、全patchの動作を保証した意味ではない |
| JDK / JVM target | 25 |
| Kotlin | 2.4.20、IDE同梱stdlibを使用 |
| IntelliJ Platform Gradle Plugin | 2.19.0 |
| Gradle Wrapper | 9.7.1、distribution SHA-256固定 |
| 開発版 | `0.1.0-dev.<source SHA>`。未commit差分があるbuildには `-dirty` が付く |

SDK版は[公式Android Studio一覧](https://plugins.jetbrains.com/docs/intellij/android-studio-releases-list.html)と対象IDEのproduct-infoで照合しています。Kotlinの[公式互換表](https://kotlinlang.org/docs/whatsnew2420.html)はGradle 9.7.0までを明記しており、9.7.1はpatch版をこのプロジェクトで実build検証します。JDK 25は対象IDEの最小要求に合わせています。

[公式のAndroid Studioプラグイン開発手順](https://plugins.jetbrains.com/docs/intellij/android-studio.html)に従いAndroid Studio SDKを使用します。projectのAndroid SDK、Run configurationのapplicationId、Gradle同期・Android modelの変更を読むため、同梱Android pluginのAPIを利用します。Terminal・JCEFへの依存はありません。録画用の固定scrcpy server、時計測定DEX、JCodecをZIPに含め、利用時に取得する追加ツールはありません。

## ビルド

JDK 25を用意して `JAVA_HOME` に設定します。macOSで対応Android Studioがインストール済みなら、その同梱JBRを使えます。

```bash
export JAVA_HOME="/Applications/Android Studio.app/Contents/jbr/Contents/Home"
python3 scripts/workflow/plugin_check.py
```

既定では固定SDKをGradleが取得します。初回はネットワーク接続とSDK展開用の空き容量が必要です。同じSDKが手元にある場合だけ、明示して取得を省けます。

```bash
REPLAY_PLATFORM_PATH="/Applications/Android Studio.app/Contents" \
  python3 scripts/workflow/plugin_check.py
```

`REPLAY_PLATFORM_PATH` は共通scriptが `-PuseLocalPlatform=true -PplatformPath=...` に変換します。Gradleを直接呼ぶ場合も両方指定します。個人のGradle設定に `platformPath` だけがあっても既定SDKを置き換えません。指定先が固定Rabbitと異なる場合はbuildを失敗させます。

標準コマンドは次と同じです。JVMテストには合成データと所有する模擬processを使用します。テスト成功をIDE・端末・プレーヤーの実受入へ代用しません。

```bash
plugin/gradlew -p plugin --no-daemon check buildPlugin verifyPluginStructure
```

ZIPは `plugin/build/distributions/` に出ます。原型PythonコードはZIPへ同梱しません。CIの `test` でも同じ変更影響判定から実行し、ZIPを14日保持のActions artifactとして保存します。これは検証用でありMarketplace/Release公開ではありません。長期保存する候補は保持期限内に取得し、SHA-256とsourceを記録してください。

## IDEでロード確認する

[GUI leaseと共有状態の手順](operations.md)を守り、別作業のAndroid Studio・ADB・daemonを操作しないでください。通常利用しているIDEへの上書きインストールより、Gradleの開発用sandboxと使い捨てfixtureを使います。

```bash
# GUI lease取得・固定buildとfixtureの確認後に実行
plugin/gradlew -p plugin runIde
```

`runIde`はsandboxを準備してIDEを起動します。自動起動は行いません。原型CLIの起動やUSB端末への接続もありません。

配布ZIPを受入する場合は、cleanな固定commitから一度 `buildPlugin` した同じZIPをsandbox IDEにInstall Plugin from Diskで導入します。先にZIPのSHA-256を記録し、ロード後にSettings → PluginsおよびTools → Android Replay Bufferについてで `0.1.0-dev.<固定SHA>` を確認します。再buildした別ZIPや `-dirty` 版を同じ候補の証拠へ流用しません。

[Case JSON](verification/changes/issue-3.json)のIDEロード・情報表示・IDE再起動を確認し、観察者・日時・ロード版・ZIP hashを記録します。Plugin VerifierによるAPI互換性検査やこのGUI受入は、ZIP構造検査と別です。基盤のCaseも初期版と同じ固定ZIPへ対応づけ、未実施項目は[QA #7](https://github.com/shinma06/android-replay-buffer/issues/7)と[初期版QA](https://github.com/shinma06/android-replay-buffer/issues/27)で追跡します。

## 初期版候補の利用手順

この手順は取得コア（[#31](https://github.com/shinma06/android-replay-buffer/issues/31)）とIDE接続（[#32](https://github.com/shinma06/android-replay-buffer/issues/32)）を含む固定ZIPの受入対象です。旧い情報表示だけの基盤版では取得・保存を行えません。機能の実装、API互換性、IDEでのロード、録画・保存の受入はそれぞれ別に確認します。実行結果は[初期版QA](https://github.com/shinma06/android-replay-buffer/issues/27)を正本とし、未実施の版を利用可能と扱いません。

1. **版を確認する**: 配布された固定ZIPを導入し、PluginsとTools → Android Replay Bufferについての版が受入対象に一致することを確認します。Android開発用projectと、そのprojectで設定したAndroid SDKを使います。録画用のscrcpy、ffmpeg、PythonやPATH設定を追加する手順はありません。
2. **設定する**: Settings → Tools → Android Replay Buffer、またはAndroid Replay Buffer ToolWindowの設定から、保存先と保持時間を指定します。標準は180秒、設定範囲は1〜900秒です。保存先には作成済みフォルダの絶対パスを指定します。未作成フォルダやファイルは理由を表示して拒否し、旧設定を維持します。保存先が未指定でも取得でき、保存前に指定します。対象アプリは選択中のAndroid Run configurationから自動取得するか、package名を手動指定します。編集した値は「適用」または「OK」で反映します。取得が無効でも設定できます。
3. **取得を有効にする**: ToolWindowで取得を有効にし、対象の実機またはEmulatorを1台接続します。初回は無効で、有効状態はprojectごとに保存し、IDE再起動後にも復元します。取得状態、端末、対象アプリ、動画と両ログの状態を確認して操作します。対象アプリが解決できない場合は、Run configurationやGradle同期を確認するか手動指定します。

   起動時に保存済み設定を反映できない場合は理由を表示し、有効状態を実際の取得状態に合わせます。設定を修正してから取得を有効にしてください。復元中に自分で切り替えた有効状態は、復元失敗によって巻き戻しません。projectの保存場所やIDE設定領域を安全に確認できない場合は、初期化失敗を表示します。アクセス権を確認し、projectを開き直してください。
4. **必要な時に保存する**: 保存操作で直前の保持時間分を保存します。開始直後なら取得済み分、中断があればその欠落を含む実時間の窓が対象です。保存中も取得を続け、保存後もバッファを空にしません。成果物のREADMEで動画・両ログの取得範囲と欠落を確認し、MP4とJSONLを対応する窓内時刻で参照します。区間ごとの動画は欠落を詰めてつながるものではありません。ToolWindowの欠落詳細には現在窓内の理由・記録時刻の範囲を表示し、取得復旧後も窓内に残る欠落を確認できます。時刻が未確定の境界は未確定と表示します。
5. **失敗・切断を扱う**: 保存失敗時は、失敗カードの固定端末・アプリ選択・対象アプリ履歴を確認し、保持された同じ対象の再試行、保存先変更、破棄から選びます。取得中の端末やアプリを変更しても、この固定対象は変わりません。解決するまでは次の保存を受け付けず、取得は続けます。完全切断中は切断前のバッファを保存できますが、再接続後は現在の保持窓へ戻るため古いデータが範囲外になります。無効化やproject終了では未保存バッファと失敗保存の対象を破棄し、保存済みフォルダは残します。

前回の端末処理が片付け待ちの場合は、取得状態や欠落とは別に件数を表示します。対象端末を接続すると、取得が無効でも現在のproject SDKのadbで終了処理だけを再確認します。この確認で取得を開始することはありません。終了情報は同じprojectのIDE再起動後にも保持し、片付けられた情報だけを削除します。未知・不正な終了情報は保全するため、接続後も件数が残る場合があります。初期化中に表示がないことを、片付け完了の確認に使わないでください。現候補の終了情報保護はPOSIX権限を扱える環境を前提とし、Windowsの対応は未実装です。

動画の切出しには復号に必要な窓外の直前フレームがファイル内部へ含まれ得ます。READMEと`frames.jsonl`でprerollを区別します。窓外の画像をファイルへ一切含めてはいけない用途への適合は保証しません。時計対応が不明な区間や片系の欠落も、全体の成功表示だけで判断せず成果物の状態を確認してください。[同期・切出し契約](design/timeline.md)と[保存失敗時の契約](plugin-design.md)に詳細があります。

候補コア（[#31 / PR #36](https://github.com/shinma06/android-replay-buffer/pull/36)）の実装値は次のとおりです。CLI原型の設定値とは別です。

| 項目 | 候補の実装値 |
| --- | --- |
| 映像 | H.264、最大サイズ1920、最大30fps、8,000,000bit/s、音声なし |
| 取得リングの上限 | 動画1GiB、ログ32MiB。時間窓に加えて容量も制限する |
| 空き容量の下限 | 128MiB。保存用に固定した対象は通常のリング更新で消さない |
| 取得中断後の再試行 | 初回は即時。連続失敗時は1秒、2秒、4秒、以後5秒待つ |

これらは設定した上限・試験値です。実際の画質、フレーム間隔、文字の読みやすさ、負荷、保持できる量と容量不足時の動作は、同じ固定ZIPの`VIDEO-POLICY`・同期・容量Caseで測定します。保存対象の固定による追加の保持と寿命は[保存設計](plugin-design.md)に従います。

## 実装と受入の責務

製品の動作と優先順は[製品要件](requirements.md)を正本とします。CLIの既存設定値や過去の設計から、将来プラグインの方式を自動決定しません。

1. **採用設計を実装へ接続**: 保存失敗時は1件の固定対象を保持して再試行・保存先変更・破棄、入口は独立ToolWindowと常設設定、取得は固定scrcpy serverとJVM mux、同期は端末elapsedの共通軸を採用した。[設計の入口](plugin-design.md)、[IDE UI](design/ide-ui.md)、[時刻・録画方式](design/timeline.md)を正本とする。設計の採用を実機動作の合格に置き換えない。
2. **取得エンジンと接続契約を検証**: projectのAndroid SDKからadbを解決し、固定server・時計測定DEX・JCodecをPlugin ZIPへ同梱する。利用者による追加ツールの手動導入や実行時ダウンロードに依存しない。processの所有、接続先、timeout、取消し、入力サイズ・エラー・並行要求、同梱依存のidentityを検証する。他の利用者が起動したprocessや共有adb serverを停止しない。Python CLIと既存JSON IPCの互換性は引き続き保全する。
3. **初期版の取得・状態表示・保存を実装**: 有効化トグルと状態復元、接続自動取得、明示的な設定適用、180秒の実時間バッファ、両ログ、1ボタン保存、同期、自動復旧と同一シーケンスを要件に照合する。接続・process待ちはEDTで行わず、dispose後のUI反映を抑止する。macOSを先に実機/Emulatorで検証する。
4. **初期版候補を受入**: 同一ZIPで要件に対応するCaseとAPI互換性を確認し、developからmainへpromotionする。基盤の既存QAを含め、未実施のまま製品完成と扱わない。Marketplace公開は別の依頼範囲。
5. **macOSメニューバー連携へ最初に着手**: IDE内初期版の完成直後に実施する。その後のOS対応はWindows、Linuxの順を基本にし、後続TODOは指定優先度と利用者の計画に従う。

この順番は実装と受入の対応を確認する入口です。設定・package名・wire入力を検証し、UIのEDT制約、古い非同期結果の破棄、project/contentの寿命、保存中の取得継続、取消しと所有processの終了を確認します。DBを使用しないためDBクエリ検証は対象外です。JVMでの回帰確認をIDE・端末の観察へ代用しません。
