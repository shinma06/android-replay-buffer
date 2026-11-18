# android-replay-buffer「時間の定義」実装仕様書（Trail別）

- 対になる文書: `time-definition-policy.md` 改訂4（以下「方針書」）。方針書が「何をなぜ」、本書が「どこをどう」を定める。食い違いがあれば方針書を正とし、実装を止めて報告する。
- 基準コミット: `develop` `c0d4336`（以下「develop」）と PR #460 `59cc95f`（以下「#460」）。**行番号はすべてこの2つから取り、書く前に該当行を読み直した。** `ReplayTrim.kt`・`SavedReplay.kt`・`ReplayPlaybackEngine.kt`・`ReplayFrameExport.kt`・`ReplayCli.kt:166` は #460 にだけある。#460 を入れると `SaveWriter.kt` などの行番号はずれる（例: manifest の `"schema"` は develop `:731`、#460 `:700`）。着手時に関数名で引き直すこと。
- パスの省略: `core/X.kt` は `plugin/src/main/kotlin/io/github/shinma06/replaybuffer/core/X.kt`。テストは `plugin/src/test/kotlin/io/github/shinma06/replaybuffer/` の下。
- 本書は設計文書で、コードは変更していない。型・関数・テスト・固定データの名前はすべて**案**で、既存の命名に合わせて変えてよい。式・境界値・不変条件・受入条件は変えてはならない。
- 表記: 「事実」は行番号つき。「仮」「候補」「未検証」はその旨を書く。OSのAPIの仕様（定数の値、DLL名など）は各OSの公開文書に基づく記述で、**本調査では実行して確かめていない**（T3b-1で確かめる）。
- 改訂（本書の第2版）: オーナー指摘の7点を反映した。Windowsの時計を `QueryInterruptTimePrecise`／`QueryUnbiasedInterruptTimePrecise` に（6.3）、T0をT0-aとT0-bに分割（5章・6.4・6.4b・6.5・6.10・7章）、「3秒で止めない」をT3aに（6.7）、自走・遅延のサンプルと `certain()` の規則とD11（6.7）、JNAの直接マッピングと代案（6.3・7.4）、schema 4 へのPC休止の欠落の扱い（6.5・2章のD15・U13）、休止の区間の上限とI3（4.1・4.5）。ほかの箇所は変えていない。

## 目次

0. 子セッションへの共通指示
1. 用語集
2. 決定済みの事項と、まだ決まっていない事項
3. `System.nanoTime()` の全呼び出し箇所（develop）
4. 共通の型と式（複数のTrailが参照する）
5. Trailの順序
6. Trail別の仕様（T1、TR、T3b-1、T0-a、T0-b、T3b-2、T2、T3a、T4a、T4b、T5）
7. T0の計測手順と判定基準（7.6はオーナー向けの手順書）
8. 費用の比較（このセッションが実装する場合／子セッションが実装する場合）
9. 未計測・未検証の一覧

---

## 0. 子セッションへの共通指示

1. **1 Trail = 1枝 = 1子セッション。** 枝は `develop` から `devin/<slug>`。子セッションは実装・テスト・pushまで行い、`entire trail` の操作（作成・ready・merge）は親セッションが行う（`AGENTS.md`）。
2. 着手前に読むもの: `AGENTS.md`、方針書の4章、本書の0〜5章、担当Trailの節。ほかのTrailの節は「依存と順序」に挙がったものだけ読めばよい。
3. 完了の条件（全Trail共通）: `python3 scripts/workflow/plugin_check.py` が通る。CIの `test`（Ubuntu、`.github/workflows/ci.yml:15-17,45`）と `os-parity`（macOS・Windows、`:52-58`）が通る。**テストをOSで飛ばさない**（`Assume` や OS名での早期returnを使わない。例外は6.3に書いた「実物の時計を読むテスト」だけで、これも3 OSすべてで実行する）。
4. 呼び出し側でOS分岐しない。OS固有のコードは `core/HostPlatform.kt` が選ぶOS別の実装の中だけに置く。
5. 利用者向けの文言（UI、docs、PR本文）は日本語。録画・logcat・生の診断記録・認証情報を公開しない。
6. **全Trail共通の停止条件**（当てはまったら実装を止め、親セッションへ報告する）:
   - 方針書7章のオーナー判断がまだ出ていないのに、その判断に依存する変更が必要になった。
   - `main` への変更、force-push、履歴の書き換えが必要になった。
   - 本書に無い仕様・UXの変更（文言の意味、既定値、CLIの契約、終了コード、保存物の形）が必要になった。
   - OSの権限の変更（管理者権限、macOSの権限ダイアログ、Windowsの特権、udev規則など）が必要になった。
   - 計測値が仮の値と大きく食い違った（基準は7.5）。
   - 本書の行番号・現状要約が実際のコードと合わない（基準コミットより後の変更で前提が崩れている）。
   - 既存テストを、本書が「変える」と明記していないのに書き換えないと通らない。
7. 範囲外に気づいた不具合は直さず、報告に書く。

---

## 1. 用語集

| 用語 | 意味 |
| --- | --- |
| **H** | PCの経過時間。休止（スリープ）中も進む単調な時計（方針書4.1・4.6）。単位ns。原点はOSごとに違ってよく、差だけを使う。保存要求・受信・往復測定の送受信・接続・切断の時刻はHで測る |
| **A** | PCの「休止を含まない」単調な時計。Hと組で同時に読み、休止の検出にだけ使う（4.1） |
| **締切時計** | `System.nanoTime()`。タイムアウト・再試行・監視にだけ使う。HやAと混ぜない |
| **D** | 端末BOOTTIME（`SystemClock.elapsedRealtimeNanos()`、`clock/ClockProbe.java:20,23`）。bootごと。動画・音声・タップ（端末MONOTONIC）とログ（端末epoch）は、時計サンプルの端末内offsetでDへ写す（`core/CaptureClock.kt:259-302`） |
| **seq** | 現行コードの `sequence`。`D + segmentOffset`（`CaptureClock.kt:58-63`）。boot・段差をまたいで1本に並べた端末側の軸。現行の `t_ns = seq − 窓の起点`（`SaveWriter.kt:457,530,553`） |
| **θ** | `D − H`。真値は見えないので下限 `θlo` と上限 `θhi` で持つ |
| **ρ** | DとHの進み方の差の上限（無次元）。仮の値 500ppm（`500e-6`）。T0で決める。コードでは定数にせず引数で渡す |
| **N** | 設定の秒数（`replaySeconds`、1〜900。`core/ReplayModels.kt:31,38`） |
| **S** | 保存要求を受理した瞬間のH。凍結中は凍結した瞬間のH |
| **W** | 窓の長さ。`W = min(N×1e9, S − 取得開始のH)`。Hだけで決まる |
| **T_S** | Sに対応する端末側の値（seq）。等級つき |
| **t** | 窓内位置（ns）。起点が0、終点がW。長さ・位置を表す唯一の軸で、MP4の時間軸と同じ |
| **時計サンプル** | 往復測定1回分（`ClockSample`、`CaptureClock.kt:12-24`）。`sent`・`received` がPC側、`before`・`mono`・`wall`・`after` が端末側 |
| **有効なサンプル** | `valid`: 端末内の読取幅 `after − before ≤ 2ms`（`CaptureClock.kt:21`） |
| **時計区間** | 現行の `region`。再起動または端末時計の段差を有効なサンプルで検出すると `startRegion()`（`CaptureClock.kt:159-162`、呼び出しは `:55`）で切り替わる |
| **錨区間** | 同じθの上下限を使い続けられる期間。**切れ目は「時計区間の切り替え」と「PC休止」の2つだけ**（方針書4.3）。識別子は `(時計区間の番号, PC休止の通し番号)` |
| **往復測定の上下限** | サンプル1個から得るθの範囲。`θhi = D読み − sent`、`θlo = D読み − received`（読取幅を外側に足す。4.3） |
| **片方向制約** | 「記録は発生より後に届く」ことから得るθの下限。記録の端末時刻をd、受信Hをhとすると `θ ≥ d − h` |
| **片方向制約の最小形** | 錨区間ごとに値を1つだけ持つ形。`d − h` が最大だった記録（遅れが最小だった記録）の値を、古さに応じてρで割り引いたもの（4.3）。**必須** |
| **片方向制約の全面採用** | すべての記録の制約と往復測定の上下限を逐次交差させて締める処理、および交差が空になったことによる破断の検出。**影運用**（T0の判定で決める） |
| **等級** | 時刻の確かさ。`measured`（測定）／`estimated`（推定）／`arrival`（到着基準）。定義は4.4 |
| **測定** | 現行で確定する条件と同じ（`CaptureClock.kt:281-301` で `elapsed` が非null。終点は `certain()` `:165-167`）。次のサンプルが無い末尾でも、最後の有効なサンプルの受信から5秒以内なら測定 |
| **3区分** | 窓 `[0, W]` をストリームごとに「確認済み／末尾未確認／欠落」へ重なりなく余りなく分けたもの（4.5） |
| **確認済み** | 同じ接続が続いている間で、前後2つの受信に挟まれた区間。端末側で落ちた分が無いことまでは保証しない |
| **末尾未確認** | 接続は続いているが、挟む受信が無い区間（最後の受信から終点まで、など） |
| **欠落** | 中断（gap）、取得開始前、PC休止、検出された取りこぼし |
| **接続（連続取得）** | ストリームごとの、途切れのない1回の読み取り。`addGap`（`core/CaptureStore.kt:902`）が呼ばれるたびに次の接続になる |
| **影運用** | 計算はするが保存物・画面・CLI/MCPの出力を変えないこと。結果は診断記録とテストだけが見る |
| **凍結** | 完全切断のとき窓の終点を固定すること（`fixedEnd`、`CaptureStore.kt:136,602-606`） |
| **137秒型／49秒型** | 過去障害（PR #445/#446）の型。137秒型: 時計不明の区間の記録が窓外と判定できず残った。49秒型: 保存時に対応が無く、詰まってからまとめて届いた古い記録が残った |
| **TR** | ReplayTrim 整理Trail（#460の後に予定されている、`ReplayTrim.kt` 998行の1クラスの分割・整理）。本書の範囲外だが順序に関係する |

---

## 2. 決定済みの事項と、まだ決まっていない事項

### 2.1 決定済み（変えない。変えたくなったら停止して報告）

| # | 事項 | 出どころ |
| --- | --- | --- |
| D1 | 窓の終点はS、長さは `W = min(N, S − 取得開始のH)`。Hは休止を含む | 方針書4.2 |
| D2 | 窓の中の位置・順序・間隔は端末時刻の差で決める | 4.2 |
| D3 | 端末↔PCの対応は上下限で持つ。線形回帰は使わない | 4.3 |
| D4 | 錨区間の切れ目は「時計区間の切り替え」と「PC休止」だけ。交差が空になったことによる検出は影運用 | 4.3（改訂3） |
| D5 | 片方向制約は最小形だけ必須 | 4.3 |
| D6 | 等級は 測定／推定／到着基準。測定は現行の確定と同じ条件・同じ値 | 4.3 |
| D7 | 250msの独立した定数は廃止。端末時刻で判定する境界は位置の範囲で、PC時刻で判定する境界は `S − W − ρ×W` | 4.4 |
| D8 | 長さは「確認済み区間の合計」。3区分の和は常にW。数える軸はt | 4.7 |
| D9 | PC休止は欠落（理由: PC休止）。ADB接続が休止をまたいで続いても確認済みに数えない | 4.6・4.7（改訂3） |
| D10 | 保存物は schema 5。読み手は4と5の両方。schema 4 の保存物は書き換えない。schema 4 のトリムは旧式（#460 `ReplayTrim.kt:908-922`）を凍結して残し、結果もschema 4 | 4.8・4.9 |
| D11 | クラッシュ自動保存の受付条件は現行のまま（`CaptureStore.kt:780`） | 4.4の9 |
| D12 | PC時計のOS別の取得は `HostPlatform` に隔離し、**JNA**で呼ぶ（既存の `PosixSaveHandles` / `WindowsSaveHandles` と同じ手段）。新しい依存・ネイティブバイナリは足さない | 4.6（改訂3）、オーナー指示 |
| D13 | `System.nanoTime()` は「時間軸」と「締切・待ち時間」に分け、時間軸だけをHに替える。既定値の引数も対象。2つを混ぜない | 4.6（改訂3） |
| D14 | PR #460 を今の長さの定義のまま先に入れる | 方針書5章・7章の4 |
| D15 | schema 5 の意味が変わるのはT4bの1回だけ。**これは schema 5 についての決定で、schema 4 の出力がT4bの前に変わるかどうかは決めていない。** T3b-2で schema 4 の保存物に理由「PC休止」の欠落を出すか（出すと `availableSeconds` も変わる）は、方針書7章の7でオーナーが決める（U13、6.5の4・9） | 5章、7章の7（改訂4） |

### 2.2 まだ決まっていない事項

| # | 事項 | 誰が | いつまでに | 決まるまでの扱い |
| --- | --- | --- | --- | --- |
| U1 | ρの値 | T0の計測結果からオーナー（親セッションが案を出す） | T4bの着手前（T0判定） | 500ppmを仮の値として引数で渡す |
| U2 | 片方向制約を全面採用するか | 同上 | T4bの着手前（T0判定。T2の影の記録を使う） | 影運用のまま |
| U3 | PC時計の手段をOSごとに確定（候補どおりでよいか、休止の検出のしきい値） | 同上 | **2段で決める。** (1) T3b-2の着手前: T3b-1の3 OSのCI（APIが呼べる、単調、最小の増分が1ms未満）とT0-aの結果（7.4の3〜6）で「候補どおりに進める」を決める。(2) 実際の休止での確認（7.4の1・2）: Linuxは T0-a でできればT0-a、macOS・WindowsはT0-b、遅くともT5の受入（A4）で確定する | T3b-1は候補どおりに実装する。(2)が済むまで、そのOSのPC時計の手段は「実休止は未確認」と報告に書く |
| U4 | 休止を含む時計が得られない環境の扱いを「保存物に印を付けて続行」のままでよいか | オーナー | T3b-2の着手前 | 方針書4.6の(b)のとおり実装する前提で書いた |
| U5 | 取得がN秒に満たない窓での `t_ns` の原点のずれを許すか（方針書7章の6） | オーナー | T4bの着手前 | 本書は「許す」前提。許さない場合の差分は6.9の4の(h)に書いた |
| U6 | CLI・MCPに3区分の秒数を出すか（7章の5） | オーナー | T4bの着手前 | 本書は「出す」前提。出さない場合はT4bのCLI/MCPの項を外す |
| U7 | 表示「確認済み X秒 / 窓 W秒」（7章の1）、推定等級の受け入れ（7章の2）、自動保存の条件の維持（7章の3）、#460を先に入れる順（7章の4） | オーナー | 1・2はT4bの前、3はT2の前、4はT4aの前 | 双方とも採用を推奨済み。正式な回答を待つ |
| U8 | FFM（`java.lang.foreign`）を使うか | オーナー | 任意（決まらなければJNAのまま） | **JNAで実装する。** FFMはリポジトリ内で未使用。IDE上でネイティブアクセスの許可が要るかは未検証 |
| U9 | 端末側の欠落（logcatなど）を検出する仕組みを足すか | オーナー | 本計画の外 | 検出しない。限界をREADMEと画面に書く（T4b） |
| U10 | リポジトリ外に schema 4 を読む利用者がいるか | オーナー | T4bの着手前 | 不明。いる場合は移行の告知が要る |
| U11 | 時計helperの自走の間隔、往復測定を続ける頻度 | T0-aの結果から親セッション | T3aの着手前 | 仮: 自走1秒ごと、往復は現行どおり（最初の5回は連続、以後1秒ごと。`DeviceCapture.kt:354-355`） |
| U12 | TRの担当と時期、分割後のファイル構成 | オーナー・親セッション | T4aの着手前 | 6.2に本計画からTRへの要請を書いた |
| U13 | T3b-2の時点で、schema 4 の保存物に理由「PC休止」の欠落を出すか（方針書7章の7）。(a) 出す（推奨）、(b) T4bまで出さず、診断記録にだけ書く | オーナー | T3b-2の着手前 | 本書は(a)の前提で書いた。(b)の場合の差分は6.5の4「保存物」に書いた |

---

## 3. `System.nanoTime()` の全呼び出し箇所（develop `c0d4336`）

本番コード（`plugin/src/main` と `plugin/src/clock`）の全67か所。分類: **軸**＝時間軸（Hに替える）、**締切**＝締切・待ち時間（`System.nanoTime()` のまま）、**端末**＝端末側で動くコード（PCの時計ではない）。テスト内の呼び出しは対象外（T3b-2で、時間軸の値を作っている箇所だけ固定値か `FakeHostClock` に替える）。

### 3.1 時間軸（27か所。T3b-2でHに替える）

| 位置 | 現状 | 変更後 |
| --- | --- | --- |
| `core/CaptureClock.kt:78` | `now(host: Long = System.nanoTime())` | 既定値を削除（必須の引数にする） |
| `core/CaptureClock.kt:98` | `hostAnchor(host = …)` | 同上 |
| `core/CaptureClock.kt:165` | `certain(host = …)` | 同上 |
| `core/CaptureClock.kt:173` | `endUncertainty(host = …)` | 同上 |
| `core/CaptureStore.kt:191` | `touch(…, host = …, rotation)` | 既定値を削除 |
| `core/CaptureStore.kt:207` | `touchLost(…, host = …)` | 同上 |
| `core/CaptureStore.kt:218` | `touchResynced(…, host = …)` | 同上 |
| `core/CaptureStore.kt:372` | `audio(packet, owner, host = …)` | 同上。呼び出し `DeviceCapture.kt:225` がHを渡す |
| `core/CaptureStore.kt:419` | `frame(packet, owner, host = …)` | 同上。呼び出し `DeviceCapture.kt:192` がHを渡す |
| `core/CaptureStore.kt:476` | `app(…, host = …, poll)` | 同上。呼び出し `DeviceCapture.kt:88,106,109,119` がHを渡す |
| `core/CaptureStore.kt:517` | `configureAuto(…, host = …)` | 同上 |
| `core/CaptureStore.kt:565` | `log(source, owner, host = …)` | 同上（呼び出し `DeviceCapture.kt:327` は既に渡している） |
| `core/CaptureStore.kt:623` | `prune(seconds, host = …)` | 同上。呼び出し `ReplayCore.kt:156,336` がHを渡す |
| `core/CaptureStore.kt:706` | `capture(settings, trigger, host = …, request)` | 同上。呼び出し `ReplayCore.kt:177` がHを渡す（これがS）。`:266` は既に渡している |
| `core/CaptureStore.kt:912` | `addGap` の中で直接 `System.nanoTime() − it.received > 5秒` | `addGap` に `host` 引数を足し、それを使う |
| `core/DeviceCapture.kt:98,100` | `psStart`・`psEnd`（`ReporterPoll` の開始・終了。クラッシュ自動保存の受付で受信時刻と比べる） | Hを読む |
| `core/DeviceCapture.kt:302` | `started`（`store.clock.hostAnchor(started)` へ渡す、`:303`） | Hを読む |
| `core/DeviceCapture.kt:312` | Logcat 1件の受信時刻 | Hを読む |
| `core/DeviceCapture.kt:345` | 往復測定の送信時刻 `sent` | Hを読む |
| `core/DeviceCapture.kt:352` | 往復測定の受信時刻 | Hを読む |
| `core/DeviceCapture.kt:372,389` | タップの受信時刻 | Hを読む |
| `core/DeviceCapture.kt:450` | 注入タップの受信時刻 | Hを読む |
| `core/ReplayCore.kt:250` | `configureAuto(host = …)` | 既定値を削除 |
| `core/ReplayCore.kt:256` | `autoSave(host = …)` | 同上 |
| `core/ReplayCore.kt:330` | `backendStarted`（`:335` でサンプルの `received` と比べる） | Hを読む |

（行数の内訳: `CaptureClock` 4、`CaptureStore` 11、`DeviceCapture` 9（`:98,100,302,312,345,352,372,389,450`）、`ReplayCore` 3、合計27行。`:98,100` と `:372,389` をそれぞれ1行にまとめた表なので、表は25行。）

**引数を省いた呼び出し**（既定値を通して `System.nanoTime()` を読んでいる。既定値を消すとコンパイルが通らなくなるので、漏れは機械的に見つかる）:

- `core/CaptureStore.kt` の `clock.now()`: `:178, :250, :268, :290, :311, :325, :326, :347, :387, :390, :604, :616`
- 同 `clock.endUncertainty()`: `:314, :581, :605, :805`
- 同 `clock.certain()`: `:297, :819, :850, :914`
- 同 `clock.hostAnchor()`: `:911`
- `core/ReplayCore.kt` の `configureAuto()`: `:150, :167, :279, :329, :337, :387`。`autoSave()`: `:280`。`streams(…)`（`host` 引数が無く、中で `clock.endUncertainty()`・`clock.certain()` を読む）: `:338, :406`
- これらを含む関数（`status` `:283`、`clockStatus` `:336`、`freeze` `:602`、`resume` `:610`、`end` `:616`、`streams` `:802`、`addGap` `:902` など）に `host: Long` を足し、入口（`DeviceCapture`・`ReplayCore`）で読んだHを渡す。**1つの出来事につきHは1回だけ読み、同じ値を下へ渡す**（同じ出来事の中で2回読まない）。

### 3.2 締切・待ち時間（39か所。変えない）

| 位置 | 用途 |
| --- | --- |
| `core/DeviceCapture.kt:129,137` | 復旧の再試行間隔（試行が10秒以上続いたか） |
| `core/DeviceCapture.kt:300,301` | 有効な時計サンプルを最大5秒待つ |
| `core/DeviceCapture.kt:508,509,511` | 終了時のjoinの締切 |
| `core/DeviceCapture.kt:557,559,568,583` | 動画接続の締切と再試行 |
| `core/DeviceCapture.kt:594,599,606,608` | `VideoReadMonitor` の監視 |
| `core/OwnedAdb.kt:78,88,103,105,110,112,116` | ADBプロセス終了の締切 |
| `core/SaveWriter.kt:89` | 保存処理の上限（4時間） |
| `core/SaveWriter.kt:182` | 正規化の開始時刻（処理時間） |
| `core/SaveWriter.kt:380,397` | 復号の上限（10秒） |
| `cli/ReplayCliEndpoint.kt:278,300` | ハンドシェイクの締切 |
| `cli/ReplayCliSaveResult.kt:14` | 保存結果を待つ締切 |
| `cli/ReplayMcpServer.kt:56,151` | workerの経過と超過の判定 |
| `ide/ReplayProjectService.kt:381,384,421` | IDE側の処理の締切 |
| `ide/LogcatTargetDevice.java:43,73,133` | 標準Logcatの準備の締切 |
| `ui/LogcatToolWindowButton.kt:63,122` | UIの締切 |

（`DeviceCapture` 15、`OwnedAdb` 7、`SaveWriter` 4、`cli` 5、`ide` 6、`ui` 2、合計39。）

注: `DeviceCapture.kt:340,346` の3秒の打ち切りは `timer.schedule` で、`System.nanoTime()` は使っていない（締切の側）。

### 3.3 そのほか

| 位置 | 分類 | 扱い |
| --- | --- | --- |
| `plugin/src/clock/java/…/clock/ClockProbe.java:21` | 端末 | 端末MONOTONICの読み。変えない（T3aで出力の形だけ変わる） |
| #460 `ui/ReplayPlaybackEngine.kt:33,55,64,91` | 再生の時計 | 再生位置を進めるための時計（音声の再生位置、無音時は `nanoTime` の差）。収録の時間軸とは別物で、**置き換えない**。既に注入できる（`:33`） |
| #460 `cli/ReplayMcpServer.kt:36,70,182` | 締切 | 変えない |
| `core/SaveWriter.kt:193,738`（`ZonedDateTime.now()`、`created_at`） | PCの日時 | 表示用。時間軸に使わない。変えない |
| `core/ScreenshotSave.kt:100`、`core/LocalCaptureOwnership.kt:208` | PCの日時 | 対象外 |

合計: 27 + 39 + 1 = 67。

---

## 4. 共通の型と式

複数のTrailが使う定義をここに置く。名前は案。式と境界の扱いは確定。時刻・長さはすべて `Long` のns。区間は**半開区間 `[from, to)`** で持ち、和の計算で端を二重に数えない。

### 4.1 PC時計（T3b-1で入れ、T3b-2で使う）

```kotlin
/** PCの時計を1回読んだ結果。h と awake は続けて読んだ組。 */
internal data class HostReading(
    val h: Long,                 // H: 休止を含む経過時間 (ns)
    val awake: Long,             // A: 休止を含まない経過時間 (ns)
    val includesSuspend: Boolean // false = 退避中（h も awake も System.nanoTime()）
)

internal interface HostClock {
    val method: String           // "linux_boottime" / "mach_continuous" / "windows_interrupt_precise" / "nanotime_fallback"
    fun read(): HostReading
}

/** 検出したPC休止。H上の半開区間。 */
internal data class HostSuspend(val fromH: Long, val toH: Long, val asleepNs: Long)

/** 時間軸の読み口。プロセスに1つ。テストでは FakeHostClock を渡す。 */
internal class HostTime(private val clock: HostClock, private val thresholdNs: Long = 100_000_000 /* 仮。T0で決める */) {
    @Volatile var onSuspend: (HostSuspend) -> Unit = {}
    private var last: HostReading? = null
    val method get() = clock.method
    @Synchronized fun now(): Long {
        val r = clock.read()
        val p = last
        last = r
        if (p != null && p.includesSuspend && r.includesSuspend) {
            val grown = (r.h - r.awake) - (p.h - p.awake)
            if (grown > thresholdNs) onSuspend(HostSuspend(p.h, r.h, grown))
        }
        return r.h
    }
}
```

- **休止の区間は「前回の読み」から「今回の読み」まで**（`[p.h, r.h)`）とする。すべての読みが `HostTime.now()` を通り同期されているので、この間にHを付けられた記録は無い。`asleepNs` は実際に眠っていた長さ（診断用）。
- **この区間は休止そのものより長い。** 前回の読みから眠るまでと、起きてから今回の読みまでの「起きていた時間」も区間に入り、欠落（理由: PC休止）として数えられる。区間の長さは `toH − fromH = asleepNs + 起きていた時間` で、起きていた時間は前回の読みから今回の読みまでのAの増分（`r.awake − p.awake`）に等しい。
- **取り込む起きていた時間の上限**: 取得中は、受信が無くても **1秒ごとに `HostTime.now()` を読む**（`HOST_READ_INTERVAL = 1秒`。往復測定の間隔 `DeviceCapture.kt:355` と同じ値。T3b-2で `DeviceCapture` の既存のタイマーに周期の読みを1つ足す、6.5）。したがって休止が無い間の読みの間隔は「1秒＋タイマーの遅れ」以下で、これが取り込みの上限になる。検査で許す値は `SUSPEND_SPAN_SLACK = 2秒`（仮。間隔1秒＋遅れ1秒。T0-aで 7.3の#16 を測って確定する）。式で書くと `asleepNs ≤ toH − fromH ≤ asleepNs + SUSPEND_SPAN_SLACK`。受信が続いている間は読みの間隔がずっと短いので、取り込みは通常その分小さい。
- この取り込みは**安全側**である。起きていた時間を「確認済み」から外すだけで、休止を「確認済み」に数えることは無い。読みの間隔が上限を超えた場合（PCの高負荷でタイマーが遅れた等）も区間は縮めずにそのまま使い、診断に `host_read_gap` を1行記録する。取得していない間の読みの間隔は制限しない（その間の休止は取得開始より前で、窓に入らない）。
- **境界値**: `grown == thresholdNs` は休止としない（`>` のときだけ）。`grown < 0`（差が減った）は休止としない。初回の読み（`p == null`）は検出しない。退避中（`includesSuspend == false`）は検出しない。
- `onSuspend` は `now()` の同期の中から呼ばれる。受け手は重い処理をせず、休止を記録するだけにする（デッドロックを避けるため、受け手の中で `now()` を呼ばない）。
- OS別の実装は6.3。

### 4.2 窓（T2で影として計算し、T4bで本採用）

- `S` = `capture()` に渡されたH（`CaptureStore.kt:706` の `host`）。凍結中（`fixedEnd != null`）は、凍結した瞬間のH（新しいフィールド `fixedHost`。`freeze()` `:602-606` で記録）。
- 取得開始のH = 現行の `started` を設定した瞬間（`CaptureStore.kt:325`）に読んだH（新しいフィールド `startedHost`）。動画なしの保存では `logsStarted`（`:326`）に対応する `logsStartedHost`。どちらを使うかは現行の `origin(logsOnly)`（`:841-842`）と同じ選び方にする。
- `W = min(N × 1_000_000_000, S − 取得開始のH)`。`W ≤ 0` なら保存できない（現行の「空」と同じ扱い、`:783`）。
- 起点側のずれの上限 `slack = ceil(ρ × W)`。ρ=500ppm、W=30秒で15ms、180秒で90ms、900秒で450ms。
- 位置 `t`:
  - Sと同じ錨区間の記録: `t = W − (T_S − d)`。`d` は記録のseq、`T_S` はSに対応するseq（4.4）。
  - 別の錨区間の記録（別boot、PC休止の前など）: その区間の錨で記録をH上の位置 `h_pos` に置き、`t = W − (S − h_pos)`。
  - PCで観測する出来事（接続・切断・PC休止）: 観測したHを `h_pos` として同じ式。
- 現行の起点は `maxOf(0, origin, end − N)`（`:715`）で、端末時刻で測った取得開始を使う。取得がN秒に満たない窓では、新旧で `t` の原点が「(S − 取得開始のH) と (T_S − 端末時刻で測った取得開始) の差」だけずれる（U5）。

### 4.3 錨（T2）

錨区間ごとに次を持つ。

```kotlin
internal data class AnchorId(val region: Int, val suspendIndex: Int)

/** 往復測定1回から得る θ = D − H の範囲（D は seq ではなく elapsed）。 */
internal data class RoundTrip(val thetaLo: Long, val thetaHi: Long, val sent: Long, val received: Long)
//   thetaLo = before − received      thetaHi = after − sent     （ClockSample の値）

/** 片方向制約の最小形。値は1組だけ。 */
internal data class OneWayFloor(val theta: Long, val atH: Long)   // theta = d − h（記録の elapsed − 受信H）

internal data class ThetaBounds(val lo: Long, val hi: Long?, val roundTrips: Int)  // hi == null は「上限なし」
```

- **往復測定**: 有効なサンプル（`valid`）はすべて使う。応答が遅いもの（往復40ms超、`hostAnchored == false`）も捨てない。読取幅が2msを超えたもの（`valid == false`）は使わない（現行どおり）。
- **Hが `x` のときの範囲**（サンプル1個から）: `lo(x) = thetaLo − ρ×|x − received|`、`hi(x) = thetaHi + ρ×|x − sent|`。ρの項は切り上げ。
- **必須の形での選び方**: 同じ錨区間のサンプルのうち、`hi(x) − lo(x)` が最小のもの1個を使う（同じ幅なら新しい方）。**サンプルどうしの交差は取らない**（交差は影運用）。
- **最小形の更新**: 記録（どのストリームでも。seqではなくelapsedが分かるもの）を受信するたびに、`cand = d − h` を計算し、`cand > floor.theta − ρ×(h − floor.atH)` なら `floor = OneWayFloor(cand, h)` に置き換える。`floor(x) = floor.theta − ρ×|x − floor.atH|`。
  - elapsedが分からない記録（時計サンプルが1個も無い間の動画・音声・タップ・ログ）は、系ごと（端末MONOTONIC系、端末epoch系）に別の `OneWayFloor` を持つ（`d` はその系の生の値）。
- **合成**: `lo'(x) = max(lo(x), floor(x))`、`hi'(x) = hi(x)`。`lo'(x) > hi'(x)` になったら、必須の形では最小形を使わず `lo(x)` のままにし、診断に「矛盾」を1件数える（錨区間は切らない。D4）。
- **錨区間の切り替え**: `CaptureClock.currentRegion()`（`:170`）が変わったとき、または `HostTime.onSuspend` が呼ばれたとき。新しい区間は往復測定0件・最小形なしで始まる。**PC休止では `CaptureClock.startRegion()` を呼ばない**（呼ぶと現行で確定する行の集合が変わり、D6が崩れる）。

### 4.4 等級と位置（T2で影として計算し、T4bで本採用）

**記録の等級**

| 等級 | 条件 |
| --- | --- |
| `measured` | 現行の `MappedTime.sequence` が非null（`CaptureClock.kt:297-299` を通った値）。条件を言い直すと: 前後の有効なサンプルに挟まれているか、最後の有効なサンプルの受信から**5秒以内**（`host − last.received ≤ 5_000_000_000`。ちょうど5秒は測定、`:294` は `>` で外す）、換算誤差 **≤ 20ms**（ちょうど20msは測定、`:297` は `>` で外す）、時計区間の境界をまたがない |
| `estimated` | `measured` でなく、記録を受信した錨区間に有効な往復測定が1回以上ある |
| `arrival` | 記録を受信した錨区間に有効な往復測定が1回も無い |

**終点の等級**: `measured` ⇔ 現行の `windowKnown`（`endUncertainty(S) != Long.MAX_VALUE`、`CaptureStore.kt:72,714`）。そうでなければ、Sの錨区間に往復測定があれば `estimated`、無ければ `arrival`。クラッシュ自動保存の受付は現行の `clock.certain(host)`（`:780`）をそのまま使い、等級からは判定しない。

**行に書く等級** = 記録の等級と終点の等級の悪い方（`measured` < `estimated` < `arrival`）。これで「`measured` の行の集合」は、現行で `t_ns` が入る行の集合（`sequence != null` かつ `windowKnown`、`SaveWriter.kt:457,530`）と一致する。

**T_S と位置**

| 終点の等級 | `T_S` の点の値 | `T_S` の範囲 |
| --- | --- | --- |
| `measured` | 現行の `clock.now(S)`（`CaptureStore.kt:712` と同じ値） | 点の値 ± 現行の `endUncertainty(S)` |
| `estimated` | `S + (lo'(S) + hi'(S)) / 2 + segmentOffset` | `[S + lo'(S), S + hi'(S)] + segmentOffset` |
| `arrival` | `S + floor(S)`（系ごと） | 下限だけ（上限なし） |

| 記録の等級 | `d` の点の値 | `d` の範囲 |
| --- | --- | --- |
| `measured` | 現行の `sequence` | ± 現行の `MappedTime.uncertainty` |
| `estimated` | 同じ時計区間で、記録の元の時刻に最も近い有効なサンプルの端末内offsetで換算した値 | ± (そのサンプルの `readError` + `ρ × 記録の受信Hとサンプルの受信Hの差`)。さらに上限を `受信H + hi'(受信H)` で切る（記録は受信より後には起きていない） |
| `arrival` | 系の生の値 | 幅なし（系の中の差は正確） |

- 行の位置: `t = W − (T_S − d)`（点の値どうし）。`t_latest = W − (T_S下限 − d上限)`、`t_earliest = W − (T_S上限 − d下限)`。上限が無いとき `t_earliest = null`。
- `arrival` では `t = t_latest`（取り得る最も遅い位置）。最小形の定義から、どの記録も `t ≤ W` になる（証明: `T_S − d_i ≥ (S − h_i)(1 − ρ) ≥ 0`）。
- 行と終点がともに `measured` なら、`t` は現行の `t_ns` と同じ式（`seq − 起点`）で、起点だけが4.2の定義に替わる。
- 同じストリームの中の間隔は、どの等級でも端末時刻の差（受信Hで並べ直さない）。
- **端末側の段差は、次の有効なサンプルが来るまで分からない**（方針書6章）。`estimated` の範囲はこの場合に真値を外し得る。凍結後に昇格させない。

### 4.5 3区分（T2で影として計算し、T4bで本採用）

```kotlin
internal data class Span(val from: Long, val to: Long)            // 半開区間 [from, to)、t の軸
internal data class Receipt(val t: Long, val run: Int, val grade: TimeGrade)   // 受信1件: 位置、接続番号、行の等級
internal enum class TimeGrade { MEASURED, ESTIMATED, ARRIVAL }
internal data class Missing(val span: Span, val reason: String)   // reason: 既存のgapの理由 / "PC休止" / "取得開始前"

internal data class Coverage(
    val windowNs: Long,
    val confirmed: List<Span>, val unconfirmed: List<Span>, val missing: List<Missing>,
    val confirmedByGrade: Map<TimeGrade, Long>
) {
    val confirmedNs get() = confirmed.sumOf { it.to - it.from }
    val unconfirmedNs get() = unconfirmed.sumOf { it.to - it.from }
    val missingNs: Long   // 重なりを除いた和
}

internal fun splitCoverage(w: Long, receipts: List<Receipt>, missing: List<Missing>): Coverage
```

**手順**（純粋な関数。入力が同じなら結果も同じ）

1. `M` = `missing` の区間を `[0, w)` に切り、重なりを併合した集合。`missingNs = |M|`。理由別の一覧は切る前の区間を残す（重なってよい）。
2. 接続番号 `run` ごとに、受信の位置の最小 `a` と最大 `b` を取り、`[a, b)` を `[0, w)` に切る。**窓の外の受信も `a`・`b` の計算に入れる**（例: 窓の直前に同じ接続の受信があれば、その接続の確認済みは0から始まる）。全接続の和集合を `C0` とする。
3. `confirmed = C0 − M`。`unconfirmed = [0, w) − M − confirmed`。
4. `confirmedByGrade`: `confirmed` を、隣り合う2つの受信（同じ接続）で区切った小区間に分け、各小区間に「両端の行の等級の悪い方」を付けて合計する。
5. 検査: `confirmedNs + unconfirmedNs + missingNs == w`（等号。丸めなし）。

**入力の作り方**

- **受信**: その種類に絞る前の、同じ読み取りの記録を使う。動画＝全frame。音声＝全packet。端末ログ・アプリログ＝同じlogcat取得の全行（アプリログも端末全体ログの受信を使う）。タップ＝全行。
- **接続番号**: ストリームごとの連番。そのストリームの `addGap`（`CaptureStore.kt:902`）が呼ばれるたびに+1。`"clock"` のgapは接続番号を進めない。
- **窓の直前の受信**: 保持から外す処理（`prune`、`:623-677`）で消えても判定が変わらないよう、ストリームごとに「最後に保持から外した受信」の `(受信H, 位置を計算するための値, 接続番号)` を1件だけ残す。
- **欠落**:
  - 既存のgap（`CaptureGap`、`ReplayModels.kt:163-166`）。端は、seqが分かればそれを位置にし、分からなければ（現行の `fromNs == null` / `toNs == null`）gapを記録したときのH（T2で `CaptureGap` に `fromHost`・`toHost` を足す）を4.2の「PCで観測する出来事」の式で位置にする。**開始が不明なgapを0秒として扱わない**（現行の癖、`:812-816`）。
  - PC休止（`HostSuspend`）: 全ストリームに `Missing([W − (S − fromH), W − (S − toH)), "PC休止")`。
  - 取得開始前: `W < N` のときは窓がちょうど取得開始から始まるので0。ストリームごとの開始が全体の開始より遅い場合（例: 音声を途中で有効にした）は、そのストリームの開始より前を「取得開始前」にする。
  - 検出された取りこぼし: タップの入力bufferあふれ（`CaptureStore.kt:203-210`）と、PCが受信した後の抜け（現行のgapとして既に記録されるもの）。
- **境界値**: 受信が0件 → `confirmed` は空、`unconfirmed = [0, w) − M`。受信が1件だけの接続 → 長さ0で `confirmed` に寄与しない。`w == 0` → すべて0。位置がちょうど0・ちょうど `w` の受信は窓内として扱う（切った結果の長さは0）。

**不変条件**（T2以降、全テストで検査する補助関数 `assertCoverageInvariants` を1つ作る）

- I1: `confirmedNs + unconfirmedNs + missingNs == W`
- I2: `confirmed`・`unconfirmed`・`M` は互いに重ならない
- I3: `confirmedNs ≤ W − |窓内のPC休止の区間の和集合|`（PC休止を確認済みに数えない）。ここでの「PC休止の区間」は4.1の `[fromH, toH)` をtに直したもので、**休止の前後の起きていた時間（最大 `SUSPEND_SPAN_SLACK`、4.1）を含んだままの区間**で検査する。実際に眠っていた長さ `asleepNs` だけを引いた値では検査しない（区間で検査する方が厳しい）。
- I3b: 休止を含む時計が使えるとき、各 `HostSuspend` について `asleepNs ≤ toH − fromH ≤ asleepNs + SUSPEND_SPAN_SLACK`。つまり欠落（理由: PC休止）は実際の休止より最大 `SUSPEND_SPAN_SLACK` 長くてよく、短くてはならない。退避時の検出（6.5の `FallbackSuspendDetector`）の区間は「一致を確かめた2つのサンプルの受信の間」で、往復測定が失敗し続けた分だけ長くなり得るので、I3bは適用しない（I3だけを検査する）。
- I4: `confirmedByGrade` の合計 `== confirmedNs`
- I5: `0 < W ≤ N × 1e9`、MP4の再生時間 `≤ W`（T4b以降）
- I6: `measured` の行の集合 `==` 現行で `t_ns` が入る行の集合（T2で既存テスト全件について検査）

---

## 5. Trailの順序

```text
前提: PR #460 を今のまま develop へ
  │
  ├─ T1 仕様の正本化（先頭）
  │    │
  │    ├─ TR ReplayTrim 整理（本書の範囲外） ──► T4a 読み手の両対応 ─────────────┐
  │    │                                                                          │
  │    └─ T3b-1 PC時計の取得口 ─► T0-a 計測（子セッション） ─► T3b-2 時間軸の置き換え ─► T2 影運用 ─┐
  │              │                    │                              │                      │   │
  │              │                    │                              └─► T3a 自走・3秒で止めない │
  │              │                    └──────────── T0判定（ρ・全面採用）◄─────────────────┘   │
  │              │                                         ▲    │                                │
  │              └─► T0-b オーナーの実機作業 ─ ─ ─ ─ ─ ─ ─ ┘    │   （T0-bはどのTrailも止めない） │
  │                        ┊                                    │                                │
  └────────────────────────┊────────────────────────────────────┴─► T4b 書き手の切替 ◄───────────┘
                           ┊                                              │
                           └ ┄ ┄ ┄ ┄ ┄ ┄ ┄ ┄ ┄ ┄ ┄ ┄ ┄ ┄ ┄  T3a ────────►└─► T5 受入（macOS・Windowsの実休止を含む）
```

| 順 | Trail | 先に終わっている必要があるもの | 触る主なファイル |
| --- | --- | --- | --- |
| 0 | PR #460 | — | （既存のPR） |
| 1 | T1 | なし（#460の前でも後でもよいが、トリムの固定テストは#460の後に足す） | `docs/design/timeline.md`、テスト |
| 2 | TR | #460 | `ReplayTrim.kt`（本書の範囲外） |
| 3 | T3b-1 | T1 | `HostPlatform.kt`、新しい `HostClock*.kt` |
| 4 | T0-a（計測。子セッション） | T1、T3b-1 | `DeviceCapture.kt`（`clock()` と受信箇所）、診断の書き出し |
| 4b | T0-b（オーナーの実機作業。コードは変えない） | T0-aの診断入りのZIP（T3b-1のCIが通っていること） | なし（結果を `docs/verification/time-definition-t0.md` に足す） |
| 5 | T3b-2 | **T3b-1のCIで3 OSのAPIが動いていること**（`HostPlatformClockTest` が3 OSで通る）、**T0-aの結果**（7.4の3〜6、U3の(1)）。**T0-bには依存しない** | `CaptureClock.kt`、`CaptureStore.kt`、`DeviceCapture.kt`、`ReplayCore.kt` |
| 6 | T2 | T1、T3b-2 | 新しいファイル、`CaptureStore.kt`（影の呼び出し） |
| 7 | T0判定 | T0-aの計測、T2。T0-bの結果があれば加える（無ければ「emulatorだけで求めた仮の値」と明記する、7.4） | `docs/verification/`（文書だけ） |
| 8 | T3a | T0-aの計測、T3b-2 | `ClockProbe.java`、`DeviceCapture.kt`（`clock()`）、`CaptureClock.kt`（サンプルの種類、`addLate`・`addFreeRun`） |
| 9 | T4a | #460、TR、T1 | `SavedReplay.kt`、`ReplayTrim*.kt`、`ReplayFrameExport.kt`、CLI/MCPの読み取り |
| 10 | T4b | T0判定、T2、T3b-2、T4a、オーナー判断（U5〜U7、U10）。**T3aには依存しない** | `CaptureStore.kt`、`SaveWriter.kt`、`SaveLayout.kt`、`ReplayModels.kt`、UI、CLI、docs |
| 11 | T5 | T4b、T3a。A4（macOS・Windowsの実休止）はオーナーの実機で行う | `docs/verification/` |

**同じ箇所を2回書き直さないための順**

- `DeviceCapture.clock()`（`:332-358`）は T0-a（計測の記録）→ T3b-2（Hに替える）→ T3a（自走、3秒で止めない）の順に3つのTrailが触る。**この3つは並行させない。** それぞれ触る行は別（T0-aは `:346-353` の例外処理の外側に記録を足す、T3b-2は `:345,352` の時計、T3aは `:346` の「3秒で止める」の廃止とループの形）。**「helperを3秒で止めない」（方針書4.5）を入れるのはT3aだけ**で、T0-a・T3b-2は `:346` の動作を変えない。
- `CaptureStore.capture()`（`:706-787`）と `prune()`（`:623-677`）は T3b-2（引数の既定値）→ T2（影の呼び出しを足す）→ T4b（本体の差し替え）の順。T2は既存の行を変えず、関数の末尾に影の計算を足すだけにする。
- `ReplayTrim.kt` は #460 → TR → T4a（schema 4 用の分離と schema 5 の読み取り）→ T4b（schema 5 のトリムの書き出し）の順。**TRより前にT4aを始めない**（TRが分割するファイルをT4aが先に切ると、TRがやり直しになる）。
- 「TR → T4a」の列と「T3b-1 → T0-a → T3b-2 → T2」の列は、触るファイルが重ならないので並行できる。 T0-b（オーナーの実機作業）はコードを変えないので、T0-aのZIPができた後ならいつ行ってもよく、どのTrailの着手も止めない。
- T3aはT4bの前提ではない。理由: 窓・等級・3区分の定義は、サンプルの頻度に依らず成り立つ。T3aは `estimated`・`arrival` になる頻度を下げるだけである。ただしT5（受入）の前に入れる。T3aとT4bは `CaptureStore.kt` の別の箇所を触るので並行できるが、後から入る方がrebaseする。

---

## 6. Trail別の仕様

各Trailは同じ12項目で書く。共通の完了条件・停止条件は0章。

### 6.1 T1 仕様の正本化

1. **目的**: 方針書4章をリポジトリの設計文書に置き、後続のTrailが壊してはならない現行の振る舞いをテストで固定する。
2. **範囲**: `docs/design/timeline.md` の更新、現行の振る舞いを固定するテストの追加。**やらないこと**: 本番コードの変更、保存物の変更、既存テストの書き換え、UI文言の変更。
3. **変えるファイル**:
   - `docs/design/timeline.md`（既存）: 方針書4.1〜4.9を「これからの定義」として追記し、現行の定義には「schema 4」と見出しを付けて残す。
   - 新規 `core/TimeDefinitionBaselineTest.kt`（テスト）。#460の後なら `core/ReplayTrimCoverageBaselineTest.kt` も足す。
4. **変更の中身**（テストで固定する現行の振る舞い。すべて `host` を明示して渡す既存の書き方、例 `ClockGapFollowupTest.kt:39,67` の `f.at(…)` に合わせる）:
   - 数え方の4つの癖（`CaptureStore.kt:802-826`）: (a) 不明な区間をまたいだ `max − min` を数える、(b) 行が無い静かな区間は短く出る、(c) 開始が不明なgapは0秒として引く、(d) 重なるgapは二重に引く。
   - 不明時の窓: 終点が `latest()` に落ちる（`:712`）、`hostStart = host − N − 250ms`（`:716`）。
   - 末尾5秒: 最後の有効なサンプルの受信からちょうど5秒は確定、5秒+1nsは不明（`CaptureClock.kt:294`、`:165-167`）。
   - 換算誤差: ちょうど20msは確定、20ms+1nsは不明（`CaptureClock.kt:297`）。
   - #445/#446の再現入力（137秒型・49秒型。既存の回帰テストがあれば、それを指す注釈だけ足す）。
   - 行の `t_ns` が入る条件（`SaveWriter.kt:457,530`）: `sequence != null` かつ `windowKnown`。
   - クラッシュ自動保存の受付（`CaptureStore.kt:780`）: `certain(host)` が偽なら `NOT_SAVED_CLOCK_UNKNOWN`。
   - （#460の後）schema 4 のトリムの数え方（#460 `ReplayTrim.kt:908-922`）: 固定入力に対する `availableSeconds` の値。
5. **不変条件**: 追加したテストは**現行コードで通る**（現行の振る舞いの記録なので、失敗したら本書の現状要約が誤っている。停止して報告）。
6. **OS固有の部分**: なし。
7. **試験計画**: テスト名の案 `spanCountsAcrossUnknownInterval`、`quietIntervalShortensAvailableSeconds`、`gapWithUnknownStartSubtractsZero`、`overlappingGapsAreSubtractedTwice`、`endFallsBackToLatestRecordWhenClockUnmapped`、`unknownWindowUsesHostCutoffWithMargin`、`tailExactlyFiveSecondsIsCertain`、`tailFiveSecondsPlusOneIsUnknown`、`errorExactlyTwentyMsIsMapped`、`rowTnsRequiresSequenceAndKnownWindow`、`autosaveRejectedWhenClockUncertain`。固定データは使わず、テスト内で入力を組み立てる。3 OS CI。
8. **受入条件**: (a) 上の11件（＋#460後の1件）が現行コードで通る。(b) 本番コードの差分が0行。(c) `plugin_check.py` と `test`・`os-parity` が通る。(d) `docs/design/timeline.md` に方針書4.1〜4.9の内容があり、「schema 5 で有効になる」と明記されている。
9. **移行と互換**: 影響なし。
10. **依存と順序**: 先頭。トリムの固定テストだけは#460の後（#460の前に着手した場合は、そのテストをTRまたはT4aへ回す）。
11. **危険と停止条件**: 共通のもの。加えて、現行の振る舞いが方針書1章の記述と違っていた場合は停止（方針書の前提が崩れる）。
12. **規模**: テスト約350行、文書約200行。コミット2（文書、テスト）。難しさ: 低。

### 6.2 TR ReplayTrim 整理（本書の範囲外。本計画からの要請だけ書く）

1. **目的**: #460 `ReplayTrim.kt`（998行、`ReplayTrim` クラスが `:118-998`）を分割・整理する。本計画とは別に予定されているTrail。
2. **範囲**: TR自身が定める。**本計画がTRにやらないよう求めること**: 数え方・出力の変更、schema の変更。
3. **本計画からの要請**:
   - schema 4 のcoverageの式（#460 `ReplayTrim.kt:908-922`。`durationNs` `:909`、`videoTimes` `:910`、`spans` `:911`、種類のループ `:912`、`observed` `:915`、`missing` `:916-921`、`availableSeconds` `:922`）と音声の `:924-926` を、**振る舞いを変えずに1つの関数へ切り出す**（名前の案 `schema4TrimCoverage(…)`）。T4aがこれを「schema 4用・凍結」として置く。
   - `transformedManifest`（`:803-963`）の中で、manifestの版に依存する部分と依存しない部分を分ける。
4. **変更の中身**: TRが定める。
5. **不変条件**: 既存の `ReplayTrim*Test`（#460 のテスト8本: `ReplayTrimTest`、`ReplayTrimParityTest`、`ReplayTrimMetadataTest`、`ReplayTrimSourceTest`、`ReplayTrimSpaceTest`、`ReplayTrimVideoReadTest`、`ReplayTrimAudioPrerollTest`、`ReplayTrimAutoTriggerTest`）が変更なしで通る。出力がバイト単位で同じ。
6. **OS固有の部分**: なし。
7. **試験計画**: 既存テスト。T1のトリムの固定テストがあればそれも通る。
8. **受入条件**: TRが定める。本計画としては上の5。
9. **移行と互換**: 影響なし。
10. **依存と順序**: #460の後、T4aの前。T3b-1〜T2とは並行できる。
11. **危険と停止条件**: TRがT4aより遅れる場合、T4aを先に始めない（5章）。TRをやめる場合は、T4aが上の3の切り出しだけを自分で行う。
12. **規模**: TRが見積もる。

### 6.3 T3b-1 PC時計の取得口

1. **目的**: 「休止を含む時計」と「含まない時計」の組を読む取得口を `HostPlatform` に足す。まだ誰も時間軸には使わない（T0の計測と診断が使う）。
2. **範囲**: OS別の実装3つ、退避の実装、`HostPlatform` の選択、`HostTime`（4.1）、単体テスト。**やらないこと**: 既存の `System.nanoTime()` の置き換え（T3b-2）、保存物の変更、休止を欠落にする処理（T3b-2）。
3. **変えるファイル**:
   - `core/HostPlatform.kt`（26行）: 現状は `:8` で `os.name`、`:11` で `windows`、`:16-21` で保存ハンドルを Windows／`"Mac OS X"`／`"Linux"`／それ以外 に分け、`:25` で私有ファイルをWindowsかどうかで分ける。ここに `val hostClock: HostClock` を足し、`:16-21` と同じ3分岐で選ぶ。それ以外のOSは退避の実装。
   - 新規 `core/HostClock.kt`: `HostReading`、`HostClock`、`HostSuspend`、`HostTime`、`NanoTimeHostClock`（退避）、`GuardedHostClock`（下記）。
   - 新規 `core/PosixHostClock.kt`（`darwin: Boolean` を取る。`PosixSaveHandles(darwin)` と同じ形）、新規 `core/WindowsHostClock.kt`。
4. **変更の中身**:
   ```kotlin
   // HostPlatform.kt に足す形
   val hostClock: HostClock = GuardedHostClock(when {
       windows -> WindowsHostClock
       name == "Mac OS X" -> PosixHostClock(darwin = true)
       name == "Linux" -> PosixHostClock(darwin = false)
       else -> NanoTimeHostClock
   })

   internal object NanoTimeHostClock : HostClock {
       override val method = "nanotime_fallback"
       override fun read() = System.nanoTime().let { HostReading(it, it, includesSuspend = false) }
   }

   /** ネイティブ呼び出しが1回でも失敗したら、以後ずっと退避に切り替える（行き来しない）。 */
   internal class GuardedHostClock(private val native: HostClock) : HostClock {
       @Volatile private var failed = false
       override val method get() = if (failed) NanoTimeHostClock.method else native.method
       override fun read(): HostReading {
           if (!failed) try { return native.read() } catch (e: Throwable) { failed = true /* 理由を1回だけ記録 */ }
           return NanoTimeHostClock.read()
       }
   }
   ```
   - **退避に切り替わった瞬間の扱い**: 原点が変わるので、切り替え前後のHを引き算してはならない。`GuardedHostClock` は切り替え時に「切り替え直前の最後の `h`」と「切り替え直後の `System.nanoTime()`」の差をオフセットとして覚え、以後の値に足して**Hを連続させる**（単調増加を保つ。`awake` も同じオフセット）。初回の読みで失敗した場合はオフセット0。
   - **単調性**: `read()` が返す `h` が前回より小さいことは無い前提だが、検査として `HostTime.now()` は `max(前回, 今回)` を返す。
   - **読む順**: `awake` を読み、続けて `h` を読む（間に割り込みが入ると `h − awake` が大きく見える側にずれる。しきい値100ms（仮）はこのずれを吸収するためのもの）。
5. **不変条件**: (a) `read().h` は単調非減少。(b) 続けて2回読んだとき、`(h2 − awake2) − (h1 − awake1)` の絶対値は1ms未満（休止が無い限り。実物の時計のテストで検査）。(c) 退避中は `includesSuspend == false` かつ `h == awake`。(d) `HostTime` は、差がしきい値を**超えた**ときだけ `onSuspend` を1回呼ぶ。 (e) 続けて読んだときの正の増分の最小値は、`h`・`awake` とも1ms未満（時計の刻みが粗くない。3 OS共通）。
6. **OS固有の部分**（JNA。`com.sun.jna`。**直接マッピング**（`Native.register`）で呼ぶ。理由と決まりは表の下の「呼び出し方」。登録は初回の `read()` まで遅らせる）:

   | OS | 休止を含む（H） | 休止を含まない（A） | JNAの宣言（案） | 換算 |
   | --- | --- | --- | --- | --- |
   | Linux | `clock_gettime(CLOCK_BOOTTIME, &ts)`、`CLOCK_BOOTTIME = 7` | `clock_gettime(CLOCK_MONOTONIC, &ts)`、`CLOCK_MONOTONIC = 1` | `object LibcClock { @JvmStatic external fun clock_gettime(id: Int, ts: Pointer): Int }`、`Native.register(LibcClock::class.java, "c")`。`ts` は `Memory(16)` を2つ（H用とA用。最初に1度だけ確保） | `ts.getLong(0) × 1e9 + ts.getLong(8)`（64bitの `timespec`。`PosixSaveHandles.kt:39` と同じ64bit前提の検査を置く）。戻り値が0以外なら例外 |
   | macOS | `mach_continuous_time()` | `mach_absolute_time()` | `object MachClock { @JvmStatic external fun mach_continuous_time(): Long; @JvmStatic external fun mach_absolute_time(): Long; @JvmStatic external fun mach_timebase_info(info: Pointer): Int }`、`Native.register(MachClock::class.java, "c")`。`info` は `Memory(8)`（`numer: uint32`、`denom: uint32`） | `ns = ticks / denom × numer + (ticks % denom) × numer / denom`（あふれを避ける。`numer`・`denom` は初回に1度だけ読む）。`mach_timebase_info` の戻り値が0以外、または `denom == 0` なら例外 |
   | Windows | `QueryInterruptTimePrecise(&t)`（Windows 10以降。戻り値なし） | `QueryUnbiasedInterruptTimePrecise(&t)`（Windows 10以降。戻り値なし） | `object WindowsPreciseClock { @JvmStatic external fun QueryInterruptTimePrecise(t: Pointer); @JvmStatic external fun QueryUnbiasedInterruptTimePrecise(t: Pointer) }`、`Native.register(WindowsPreciseClock::class.java, "kernel32")`。`t` は `Memory(8)` を2つ（最初に1度だけ確保） | `t.getLong(0) × 100`（100ns単位） |

   - **Windowsで `QueryInterruptTimePrecise` / `QueryUnbiasedInterruptTimePrecise` を使う理由**: `QueryInterruptTime` / `QueryUnbiasedInterruptTime` は、OSの時計の刻み（0.5〜15.6ms）ごとにしか値が変わらない。Hに使うと、Windowsだけ往復時間（`received − sent`）と受信時刻が刻みの幅で粗くなり、40ms（`hostAnchored`、`CaptureClock.kt:23`）と20ms（`:290,295`）の判定が他のOSと変わる。`…Precise` の2つは刻みの間も進む値を返す。method名は `windows_interrupt_precise`。
   - **WindowsのDLL**: `kernel32` を第一候補にする（オーナー指定）。どのDLLがこの2つの関数を輸出しているかは本調査では確かめていない。`kernel32` で登録できなければ `kernelbase`、`api-ms-win-core-realtime-l1-1-1` の順に試し、**どれで登録できたかをこのTrailの報告に書く**。3つとも登録できなければ停止（下の11）。刻み単位の版（`QueryInterruptTime`）へは戻さない。
   - **呼び出し方（性能）**: 時計は記録を1件受信するたびに読むので、次の決まりで書く。
     - `Native.load` のインタフェース経由（既存の `PosixSaveHandles.kt:38-43`、`WindowsSaveHandles.kt:62` の形）は、呼び出しのたびに反射と引数の変換が入る。保存ハンドルは保存1回につき数回なのでそれでよいが、時計には使わない。**直接マッピング**（`Native.register(クラス, ライブラリ名)` と `external fun`）にする。同じJNAなので依存は増えない（D12のまま）。
     - 引数と戻り値は、プリミティブ（`Int`・`Long`）と `Pointer` だけにする。`Structure`・`LongByReference`・`Boolean`・配列は使わない（変換と確保が入る）。
     - **確保したメモリを使い回す**: `Memory` は時計の実装1つにつき最初に1度だけ確保し、`read()` のたびに確保しない。`read()` は `@Synchronized` にして、同じ `Memory` に複数のスレッドが同時に書かないようにする（`HostTime.now()` が既に同期しているので、通常は競合しない）。macOSの `numer`・`denom` は初回に1度だけ読んで持つ。`read()` 1回で新しく作ってよいのは `HostReading` 1個だけ。
     - `Native.register` は初回の `read()` で1度だけ行う。失敗（`UnsatisfiedLinkError` ほか）は `GuardedHostClock` が受ける。
     - 所要時間の基準は7.4の「PC時計の手段」の5。満たさなかったときの代案も7.4に書いた。**子セッションの判断で代案に切り替えない**（停止して数値を報告する）。
   - 定数の値・DLL名・単位はOSの公開文書に基づく。**本調査では実行して確かめていない。** このTrailの最初に3 OSのCIで実際に呼び、値が単調に増えること・2つの時計の差が安定していることを確かめる（下の7）。合わなければ停止して報告。
   - **失敗したときの退避**: 読み込みの失敗（`UnsatisfiedLinkError`）、関数の失敗、未対応のOS・CPUは、すべて `GuardedHostClock` が受けて `NanoTimeHostClock` に切り替える。利用者には止めずに続行し、`method = "nanotime_fallback"` を診断に出す（保存物への印はT4b）。
   - 呼び出し側は `HostPlatform.hostClock` だけを見る。`os.name` を見ない。
7. **試験計画**:
   - 注入用の時計（テスト用。`testing/FakeHostClock.kt`）:
     ```kotlin
     class FakeHostClock(private var h: Long = 1_000_000_000_000, private var awake: Long = 500_000_000_000,
                         var includesSuspend: Boolean = true) : HostClock {
         override val method = "fake"
         override fun read() = HostReading(h, awake, includesSuspend)
         fun advance(ns: Long) { h += ns; awake += ns }            // 起きたまま進む
         fun suspend(ns: Long) { h += ns }                         // 休止（H だけ進む）
         fun suspendUnseen(ns: Long) { /* 何もしない */ }           // 退避中の休止（どちらも進まない）
     }
     ```
   - 単体テスト `core/HostTimeTest.kt`: `noSuspendWhenBothAdvanceEqually`、`suspendDetectedWhenGapGrowsBeyondThreshold`、`growthExactlyAtThresholdIsNotSuspend`、`negativeGrowthIsIgnored`、`firstReadNeverReportsSuspend`、`fallbackClockNeverReportsSuspend`、`suspendSpanIsPreviousReadToCurrentRead`。
   - 単体テスト `core/GuardedHostClockTest.kt`: `switchesToFallbackOnceAndStays`、`hStaysMonotonicAcrossSwitch`、`firstReadFailureStartsAtFallback`。
   - **実物の時計を読むテスト** `core/HostPlatformClockTest.kt`: `HostPlatform.hostClock` を1000回読み、(a) `method` が `nanotime_fallback` **でない**（CIの3 OSでネイティブ呼び出しが成功していることの検査）、(b) `h` と `awake` が単調非減少、(c) 100ms眠った前後で `h` の増分が90ms〜2000ms、(d) `h − awake` の変化が1ms未満、(e) **続けて10万回読んだときの、`h` の正の増分の最小値と `awake` の正の増分の最小値が、どちらも1ms未満**（3 OS共通の検査。刻み単位でしか進まない時計を弾く。増分が1回も正にならなければ不合格）、(f) 10万回の読みの所要時間の平均とp99を標準出力に出す（合否には使わない。7.3の#14の参考値として3 OSのCIのログに残す）。OSで飛ばさない（3 OSとも同じ検査を実行する。OSごとに期待する `method` の値だけが違うので、期待値は `HostPlatform` が返す名前の集合 `{linux_boottime, mach_continuous, windows_interrupt_precise}` のどれかであること、と書く）。
   - 休止そのものはCIで起こせない。実際の休止での確認は、T0-a（子セッションのLinux VMで休止できる場合だけ）、T0-b（オーナーのmacOS・Windowsの実機）、T5の受入（A4）で行う。
8. **受入条件**: (a) 上のテストが3 OS CIで通る。特に `HostPlatformClockTest` の(a)と(e)が3 OSで通る。(b) 既存の `System.nanoTime()` の呼び出しが1つも変わっていない（差分で確認）。(c) `build.gradle.kts` に依存の追加が無い。(d) `os.name` を読む箇所が `HostPlatform.kt` の外に増えていない。
9. **移行と互換**: 保存物・出力の変化なし。
10. **依存と順序**: T1の後。T0の前（T0がこの取得口で実機の休止を測る）。TRと並行できる。
11. **危険と停止条件**: CIのどれかのOSで候補のAPIが呼べない、または値が期待と違う（例: Linuxのコンテナで `CLOCK_BOOTTIME` が使えない、Windowsのランナーで `QueryInterruptTimePrecise` がどのDLLからも登録できない、どれかのOSで連続読みの最小の増分が1ms以上になる、直接マッピング（`Native.register`）が使えない）→ 停止して報告（代わりの候補を親セッションと決める。黙って退避にしない）。IDEのJNAの版で宣言が通らない → 停止。
12. **規模**: 本番約220行、テスト約250行。コミット3（型と退避、OS別実装、テスト）。難しさ: 中（ネイティブ呼び出しの宣言を3 OSで確かめる必要がある）。

### 6.4 T0-a 計測（子セッションで実施できる部分）

1. **目的**: 仮の値（ρ、休止検出のしきい値、`SUSPEND_SPAN_SLACK`）と未決の判断（U1〜U3、U11）を決めるための数値を、**子セッション（クラウドのLinux VM）で取れる範囲で**取る。あわせて、オーナーがT0-b（6.4b）でそのまま使える診断入りのZIPと集計スクリプトを用意する。手順と判定基準は7章。
2. **範囲**: 診断用の記録の追加（既定は無効）、集計スクリプト、3 OSのCI、Linux VM＋emulatorでの計測の実施（7.2の「T0-a」の欄）、結果の文書化、7.6の手順書を実物（設定名・ファイルの場所）に合わせて直すこと。**やらないこと**: macOS・Windowsの実機での計測とUSB実機での計測（T0-b）、`DeviceCapture.kt:346` の「3秒で止める」動作の変更（T3a）、schema 4 の `replay.json` の変更、時間軸の変更、UIの追加（診断の有効化は既存の診断設定か環境変数で行う）、録画・ログ本体の公開。
3. **変えるファイル**:
   - `core/DeviceCapture.kt:332-358`（`clock()`）: 現状、応答が3秒以内に来ないと `:346` のタイマーがプロセスを止め、`:350-351` が例外になり、サンプルは残らない。変更後: 例外を捕まえる直前に「送信H・失敗の種類（3秒超／形式不正／プロセス終了）・経過」を診断記録に1行書く（例外の扱い自体は変えない）。成功時も「送信H・受信H・端末の4値」を診断記録に書く。
   - `core/DeviceCapture.kt:192,225,312,389,450`（動画・音声・ログ・タップ・注入タップの受信）: 診断が有効なときだけ「ストリーム名・受信H・端末側の時刻・大きさ」を診断記録に書く。
   - 新規 `core/TimingDiagnostics.kt`: 追記専用の記録（JSON Lines）。保存先はリングと同じ私有ディレクトリ（`HostPlatform.privateFiles` で権限を付ける）。上限つき（例: 64MB で古い方から捨てる）。既定は無効。有効にする方法は環境変数 `REPLAY_TIMING_DIAGNOSTICS=1`（案。IDEとCLIのどちらの起動でも効くこと）。
   - 新規 `scripts/analysis/timing_report.py`: 診断記録と、既存の保存物の `timebase.samples`（`SaveWriter.kt:862-867`）から7.3の表を作る。
   - 新規 `docs/verification/time-definition-t0.md`: 結果（数値だけ）。
4. **変更の中身**: 診断記録の行（案）:
   ```text
   {"k":"probe","sent":H,"received":H,"before":D,"mono":…,"wall":…,"after":D}
   {"k":"probe_fail","sent":H,"at":H,"why":"timeout_3s|format|exit"}
   {"k":"rx","s":"video|audio|log|touch|touch_injected","h":H,"d":端末側の時刻,"n":バイト数}
   {"k":"host","h":H,"awake":A,"nano":System.nanoTime(),"wall":currentTimeMillis,"method":"…"}   // 1秒ごと
   {"k":"gap","s":"…","h":H,"why":"…"}
   ```
   - T0の時点では時間軸はまだ `System.nanoTime()` なので、診断の `H` は `HostPlatform.hostClock.read()` を別に読んだ値を書き、`nano` と並べる（混ぜない。診断の中で並べて記録するだけ）。
   - ログの本文・パッケージ名・端末の識別子は書かない。
5. **不変条件**: (a) 診断が無効のとき、保存物・画面・CLI/MCPの出力がバイト単位で同じ。(b) 診断が有効でも `replay.json` は変わらない。(c) 診断記録は上限を超えない。
6. **OS固有の部分**: なし（T3b-1の取得口を使うだけ）。診断のコードは3 OSのCIで動かす。計測の実施はLinux VM＋emulatorだけ（macOS・Windowsの実機はT0-b）。
7. **試験計画**: `core/TimingDiagnosticsTest.kt`: `disabledWritesNothing`、`probeTimeoutIsRecordedAsFailure`（偽のプロセスで3秒超を再現。既存の `DeviceCaptureCloseTest` などの偽プロセスの作り方に合わせる。実時間で3秒待たないよう、タイマーを注入できる形にする）、`recordIsBoundedBySize`、`recordContainsNoLogText`。3 OS CI。
8. **受入条件**: (a) 上のテストと既存テストが3 OS CIで通る。(b) 7.2の「T0-a」の欄を実施し、7.3の表のうちT0-aで取れる値（#1〜#9、#13〜#16）が埋まった `docs/verification/time-definition-t0.md` がある（実施できなかった項目は「未実施」と理由を書く。T0-bで取る値の欄は空のまま残す）。(c) 7.4の判定基準に照らした案（ρ、しきい値、PC時計の手段、U11）が書かれている。(d) 公開物に録画・ログ本文・端末の識別子が無い。 (e) 診断入りのプラグインZIP（CIの成果物）と `timing_report.py` で、7.6の手順書が最初から最後まで実行できることを、Linux VMで1回通して確かめてある（休止の手順は、VMが休止できなければ除く）。
9. **移行と互換**: 診断記録は保存物ではない。互換の対象外。
10. **依存と順序**: T1、T3b-1の後。T0-aの結果（7.4の「PC時計の手段」の3〜6）と、T3b-1のCIで3 OSのAPIが動いていることがそろえばT3b-2に進める。**T0-b（6.4b）は待たない。** U1・U2の決定（T0判定）はT2の後。
11. **危険と停止条件**: 7.5の基準を超えた値が出た場合は停止して報告。Linux VMで休止（7.2の(e)）や日時の変更（(f)）ができない場合は、未実施と書いて進める（止めない。**VMの休止は、`/sys/power/state` に `mem` があり、`rtcwake` で自動復帰できると確かめた場合だけ行う。** 子セッション自身が動いているVMなので、復帰できない休止を試さない）。
12. **規模**: 本番約250行、スクリプト約300行、テスト約200行、文書約150行。コミット4。難しさ: 中（実装は易しい。計測の実施はLinux VM＋emulatorで、(c)の2時間を含めて1セッションに収まる見込み）。

### 6.4b T0-b オーナーの実機作業（macOS・Windowsの休止、USB実機）

1. **目的**: 子セッションでは実施できない計測（macOS・Windowsの実機での休止と日時の段差、USB接続の実機）を、オーナーが手元の機体で行う。
2. **範囲**: 7.6の手順書の実行、診断記録の集計、結果の数値の記録。**やらないこと**: コードの変更、録画・ログ本文・診断記録そのものの公開（集計した数値だけを渡す）。
3. **変えるファイル**: なし。結果は `docs/verification/time-definition-t0.md` の「T0-b」の欄に足す（オーナーから数値を受け取った親セッション、または次に着手する子セッションが書く）。
4. **変更の中身**: 7.6の手順書。使うのはT0-aの診断入りのZIPと `scripts/analysis/timing_report.py` だけ。
5. **不変条件**: 診断を無効に戻すと、保存物・画面・CLI/MCPの出力は元と同じ（T0-aの不変条件(a)）。
6. **OS固有の部分**: 手順のうち休止のさせ方と日時の変え方だけがOSで違う（7.6に分けて書いた）。集計は同じスクリプト。
7. **試験計画**: なし（計測の実施）。集計スクリプトのテストはT0-a。
8. **受入条件**: (a) 7.6の「記録する表」が、実施した項目について埋まっている。(b) 実施しなかった項目に「未実施」と理由がある。(c) 7.4の「PC時計の手段」の1・2と、7.5のS5・S6に当たるかどうかが、OSごとに書かれている。
9. **移行と互換**: なし。
10. **依存と順序**: T0-aのZIPができた後なら、いつ行ってもよい。**どのTrailの着手条件にもなっていない**（T3b-2はT0-bを待たない）。結果の使い道は2つ: (1) T0判定（ρ。USB実機の基線）に間に合えば加える。(2) macOS・Windowsの実休止の確認は、T5の受入（A4）で必ず行う。T0-bで先に済ませておくと、T3b-2の後・T4bの前に問題（S5・S6）が見つかる。
11. **危険と停止条件**: 7.5のS5（休止を含むはずの時計が休止を含まない）またはS6（休止が検出されない）に当たった → オーナーから親セッションへ伝え、T3b-2が既に入っていれば、そのOSの扱い（U4）を決めるまでT4bに進まない。
12. **規模**: コードなし。オーナーの作業時間の目安: 1 OSあたり、最小の組（7.6の「最小の組」）で約40分、全部で約4時間（2時間の連続取得を含む）。


### 6.5 T3b-2 時間軸の置き換え

1. **目的**: 時間軸に使っている `System.nanoTime()`（3.1の27か所）をHに替え、PC休止を検出して欠落として記録する。
2. **範囲**: 3.1の全箇所、引数の既定値の削除、`HostTime` の配線、PC休止のgap、テストの追従。**やらないこと**: 3.2の締切の変更、窓の式の変更（T4b）、3区分・等級（T2）、保存物の形の変更、`CaptureClock.startRegion()` をPC休止で呼ぶこと。
3. **変えるファイル**: 3.1の表のとおり。加えて:
   - `core/ReplayCore.kt`: `HostTime` を1つ持つ（コンストラクタで受け取り、既定は `HostTime(HostPlatform.hostClock)`）。`DeviceCapture` と `CaptureStore` に同じものを渡す。`onSuspend` を `CaptureStore.hostSuspended` につなぐ。
   - `core/CaptureStore.kt`: 新しい関数 `hostSuspended(s: HostSuspend)`。既存の `addGap`（`:902-923`）を使い、取得中の全ストリームに理由 `"PC休止"` のgapを足す。gapの端は「休止の直前の読みのH」「直後の読みのH」（seqの端は、その時点で `clock.now(host)` が返せば入れ、返せなければ `null`）。休止の一覧 `hostSuspends: List<HostSuspend>` を保持し、`FrozenCapture`（`:42-72`）に載せる。
   - `core/DeviceCapture.kt`: コンストラクタで `HostTime` を受け取り、3.1の9か所を `hostTime.now()` に替える。
4. **変更の中身**:
   - **既定値の削除**: `host: Long = System.nanoTime()` をすべて `host: Long` にする。コンパイルエラーになった呼び出しに、入口で読んだHを渡す。入口は次のとおり: `DeviceCapture` の各受信ループ（1件につき1回読む）、`ReplayCore.save`（`:177` の手前で1回読み、それがS）、`ReplayCore` の周期処理（`:336-338` の `prune`・`streams` に同じ値）、`ReplayCore.autoSave`（`:256`）、状態の通知（`status`・`clockStatus`）。
   - **混ぜない規則の機械的な検査**: `core/` の時間軸のファイル（`CaptureClock.kt`、`CaptureStore.kt`）に `System.nanoTime` という文字列が残らないこと。`DeviceCapture.kt`・`ReplayCore.kt` では3.2に挙げた行だけに残ること。これを検査するテスト `TimeAxisSourceTest` を足す（ソースを読んで数える。3.2の一覧を期待値として持つ）。
   - **PC休止と現行の5秒規則**: 休止の後は、最後のサンプルの受信からのH上の経過が休止の長さだけ増えるので、5秒を超える休止では現行の規則どおり「不明」になる（`CaptureClock.kt:165-167,294`）。これは意図した結果で、追加の処理は要らない。5秒以下の休止では確定のまま残るが、休止の区間はgapとして記録される。
   - **休止を含まない退避中**（`includesSuspend == false`）: 検出はしない。方針書4.6の(b)の「2つの証拠の一致」による検出はこのTrailに含める: (1) `System.currentTimeMillis()` と `h` の差が前回より増えた量、(2) 連続する2つの有効なサンプルで `(elapsedの差) − (受信Hの差)`。両方が正で、差が `往復の幅 + 20ms`（仮）以内で一致したら、その量を `GuardedHostClock` のオフセットに足し（Hを進める）、`HostSuspend` を出す。(1)だけ・(2)だけでは何もしない。この検出は `HostTime` の外（`CaptureStore.hostSuspended` の手前の小さな部品 `FallbackSuspendDetector`）に置き、純粋な関数としてテストする。
   - **保存物（U13。方針書7章の7でオーナーが決める。本書は(a)の前提）**: schema 4 のまま、形（キー）は変えない。`timebase.host_clock` はT4b。
     - **(a) schema 4 でも出す（推奨）**: PC休止のgapは既存の `gaps` に理由 `"PC休止"` として出る（既存の書き出し `SaveWriter.kt:764-768` がそのまま使える）。**このとき、休止をまたいだ保存に限り、schema 4 の出力は次のとおり変わる**（どれも既存のgapと同じ扱いで、新しい規則は足さない）:
       - `coverage.<stream>.availableSeconds` が減る。現行の式は「観測した幅 − そのストリームのgapの長さ」（`CaptureStore.kt:812-820`、定義は `ReplayModels.kt:167`）なので、gapの両端（`from`・`to`）が端末時刻に直せた分だけ引かれる。**`from` が `null` のgapは0として数えるので変わらない**（`:813`）。**`to` が `null`（開いたまま）のgapは、窓の終点までを欠落として数える**（`:814`）。
       - 動画は、休止のgapをまたぐ2フレームが連続と見なされなくなる（`continuousTo`、`CaptureStore.kt:993-999`）。MP4の区切り、`video_gap_scopes`（`SaveWriter.kt:793`）、READMEの「記録の不足・未確認」（`SaveWriter.kt:584-586,628-630`）に、既存の動画のgapと同じ形で現れる。
       - 取得中の画面のストリーム一覧に、理由の文字列がそのまま出る（`ReplayToolWindowFactory.kt:496`）。
       - **gapを開いたままにしない**: 休止の直後は現行の5秒規則で `clock.now(host)` が返らないことがあり、そのとき `to` は `null` になる。開いたままだと、その後の取得がすべて欠落として引かれ続ける。そこで、`to` を付けられなかったPC休止のgapは、次に往復測定が成功した時点の `clock.now(host)` で閉じる（既存の時計のgapの閉じ方 `CaptureStore.kt:344-348` と同じ型）。欠落は、復帰から最初の往復成功までの分だけ実際の休止より長くなる。短くはならない。過去のH（`toH`）そのものを端末時刻に直す手段は現行に無い（`CaptureClock.now` は、渡したHが最新のサンプルより前だと差を0に丸める、`CaptureClock.kt:78-83`）ので、`toH` への換算はしない。
       - クラッシュ自動保存の受付（`CaptureStore.kt:779-780`）は `clock.certain(host)` と `autosave.admissionProblem` で決まり、`:779-780` 自体は `gaps` を見ない。`admissionProblem`（`CrashAutosave.kt:202-211`）も `gaps` を見ない。したがって(a)でも受付の条件は変わらない（D11）。
     - **(b) T4bまで出さない**: `CaptureStore.hostSuspended` は `hostSuspends` への記録と診断記録（6.4の診断記録に、休止1回につき1行。`fromH`・`toH`・`asleepNs`）だけを行い、`addGap` を呼ばない。schema 4 の `gaps`・`availableSeconds`・MP4の区切り・READMEは、休止があっても現行と同じになる。不変条件(b)と `HostSuspendGapTest` の `suspendAddsOneGapPerCapturingStream` は「`hostSuspends` に1件足され、`gaps` は変わらない」に替える。gapの配線はT4bに移る（T4bの規模が本番約60行・テスト約150行増える）。
     - **どちらでも変わらないこと**: 休止の検出、`hostSuspends` の保持、`FrozenCapture` への受け渡し、T2の3区分（`HostSuspend` から直接 `Missing` を作る、4.5）、T4bの schema 5 の出力。**どちらでも起きること**: 時間軸がHになるので、休止が5秒を超えると休止の直後は現行の5秒規則で「不明」になる（上の「PC休止と現行の5秒規則」）。これは(b)でも schema 4 の出力に現れる（`System.nanoTime()` が休止中に止まるOSでは、現行と結果が変わる。3 OSで同じ結果にするための変化で、D1・D13の帰結）。
     - **D15との関係**: D15は「schema 5 の意味が変わるのはT4bの1回だけ」で、(a)でも(b)でも守られる（schema 5 はT4bまで書かれない）。(a)で変わるのは schema 4 の**値**で、キー・型・`availableSeconds` の定義（観測した幅 − 既知のgap）は変えない。ただし「利用者から見える出力の変化はT4bにまとめる」という趣旨は、(a)では休止をまたいだ保存に限って崩れる。この点をオーナーが決める。
5. **不変条件**: (a) `FakeHostClock` で休止を起こさない限り、既存テストの期待値は変わらない（テストの修正は「`host` を明示する」「`FakeHostClock` を渡す」だけ）。(b) 休止を1回起こすと、取得中の全ストリームに理由 `"PC休止"` のgapがちょうど1つずつ足される（U13が(a)の場合。(b)の場合は `hostSuspends` に1件足され、`gaps` は変わらない）。(b2) U13が(a)の場合、PC休止のgapは次の往復測定の成功までに必ず閉じ、`to` が `null` のまま残らない。`availableSeconds` の減り方は、両端が端末時刻に直せたgapについて「gapの長さ」と等しい。(c) 休止の区間にHを付けられた記録は無い。(d) PC休止で `CaptureClock.currentRegion()` は変わらない。(e) `TimeAxisSourceTest` が通る。
6. **OS固有の部分**: なし（T3b-1の取得口だけを使う）。
7. **試験計画**（すべて `FakeHostClock`。3 OS CIで同じ結果）:
   - `core/HostSuspendGapTest.kt`: `suspendAddsOneGapPerCapturingStream`、`suspendShorterThanFiveSecondsKeepsClockCertainButRecordsGap`、`suspendLongerThanFiveSecondsMakesClockUncertain`、`suspendDoesNotStartNewClockRegion`、`noRecordIsStampedInsideSuspendSpan`、`suspendGapReducesAvailableSecondsByItsSpan`、`suspendGapWithUnmappedEndIsClosedOnNextRoundTrip`、`suspendGapSplitsVideoContinuity`（以上3つはU13が(a)の場合。(b)の場合は `suspendLeavesSchema4GapsAndAvailableSecondsUnchanged` に替える）。
   - `core/FallbackSuspendDetectorTest.kt`: `wallJumpAloneIsNotSuspend`（PC日時の段差）、`deviceJumpAloneIsNotSuspend`（端末側だけの飛び）、`bothAgreeingIsSuspend`、`bothDisagreeingInSizeIsNotSuspend`、`bothAgreeingButNegativeIsNotSuspend`。
   - `core/TimeAxisSourceTest.kt`（上記）。
   - 既存テストの追従: `CaptureClockTest`、`CaptureStoreSaveTest`、`CaptureStoreSourceTest`、`ClockGapFollowupTest`、`VideoClockRegionTest`、`CrashAutosaveTest`、`CrashSaveLifecycleTest`、`LogOnlyStoreTest`、`ReplayCoreTest` ほか。期待値は変えない。
8. **受入条件**: (a) 上のテストが3 OS CIで通る。(b) 既存テストの期待値（`assert` の右辺）の差分が0。変わったのは引数の渡し方だけであることを、PR本文に一覧で示す。(c) `TimeAxisSourceTest` が通る。(d) 実際の休止での確認はこのTrailの受入条件にしない（子セッションのVMで休止できた場合だけ、7.2の(e)で保存した `replay.json` の `gaps` に理由 `"PC休止"` があること（U13が(a)の場合。(b)の場合は診断記録に休止の行があること）を参考として報告する）。macOS・Windowsの実休止はT5の受入（A4）で確かめる。
9. **移行と互換**: schema 4 のまま。U13が(a)の場合、`gaps` に新しい理由の文字列 `"PC休止"` が増え、休止をまたいだ保存では `availableSeconds` ほかが4の「保存物」のとおり変わる。**schema 4 の読み手は理由で分岐しない**: `gaps[].reason` は自由な文字列で、決まった値の一覧は無い。#460 `59cc95f` の読み手は、再生の時間軸をフレームの表から作り `gaps` を使わない（`SavedReplay.kt:69-96`）。トリムは `gaps` を範囲（`from_t_ns`・`to_t_ns`）と `kind` だけで切り、長さを数える（`ReplayTrim.kt:859-864,916-920`）。`reason` は読まずにそのまま写す。したがって未知の理由は「そのまま通す・表示する」で、読み手の変更は要らない（#460を入れた後の行で着手時にもう一度確かめる）。リポジトリ外の読み手（U10）が理由の文字列で分岐している場合の影響は分からない。U13が(b)の場合、schema 4 の出力は4の「どちらでも起きること」を除いて変わらない。
10. **依存と順序**: 着手条件は2つ。(1) **T3b-1のCIで3 OSのAPIが動いている**（`HostPlatformClockTest` の(a)〜(e)が Linux・macOS・Windows のランナーで通っている）。(2) **T0-aの結果がある**（7.4の「PC時計の手段」の3〜6を満たし、7.5のS4・S5に当たらない）。U4・U13のオーナー判断も着手前に要る。**T0-b（macOS・Windowsの実機、USB実機）は待たない。** T2・T3aの前。
11. **危険と停止条件**: 既存テストの期待値を変えないと通らない箇所が出た → 停止（時間軸と締切が混ざっていた箇所の可能性がある。3章の分類の誤りとして報告）。U4（退避時の扱い）がオーナーから「止める」と返った → 設計が変わるので停止。
12. **規模**: 本番約400行（多くは引数の追加）、テスト約900行（うち追従が約600行）。コミット5（既定値の削除と配線、`DeviceCapture`、休止のgap、退避の検出、テスト）。難しさ: 中〜高（触る箇所が広く、スレッドをまたぐ。式は変えない）。この数字はU13が(a)の場合で、休止のgapを閉じる処理と3つのテスト（本番約30行・テスト約100行）を含む。(b)の場合は本番約60行・テスト約150行がT4bへ移る。

### 6.6 T2 影運用（錨・等級・3区分を、出力を変えずに計算する）

1. **目的**: 4.2〜4.5の計算を本番コードに入れ、現行の出力と並べて比べる。出力は一切変えない。
2. **範囲**: 錨（往復測定の上下限、最小形）、等級、窓（影）、3区分（影）、全面採用の影（逐次交差と破断の検出）、診断への記録、テスト。**やらないこと**: 保存物・画面・CLI/MCPの出力の変更、`capture()`・`prune()` の判定の変更、schema の変更、`availableSeconds` の変更。
3. **変えるファイル**:
   - 新規 `core/TimeAnchors.kt`: 4.3の型と、錨区間ごとの状態（`AnchorState`）。純粋な関数 `thetaBounds(samples, floor, x, rho)`。
   - 新規 `core/TimeGrades.kt`: 4.4の `TimeGrade`、`gradeOf(…)`、`position(…)`。
   - 新規 `core/WindowCoverage.kt`: 4.5の `splitCoverage` と型。
   - 新規 `core/ShadowWindow.kt`: 影の結果をまとめる型と、現行の結果との比較。
   - `core/CaptureStore.kt`:
     - `VideoEntry`・`AudioEntry`・`LogEntry` には既に `host` がある。タップの行（`TouchEntry`、`TouchEvents.kt:13`）も既に `host` を持つ（`CaptureStore.kt:193` で設定）。型の追加は要らない。
     - 新しいフィールド: `startedHost`・`logsStartedHost`（`:325-326` で `started`・`logsStarted` と同時に設定）、`fixedHost`（`freeze()` `:602-606` で設定、`resume()` `:610-611` で消す）、ストリームごとの接続番号、ストリームごとの「最後に保持から外した受信」。
     - `CaptureGap`（`ReplayModels.kt:163-166`）に `fromHost: Long? = null`・`toHost: Long? = null` を足す（既定値つきなので既存の生成箇所は変えなくてよい。`addGap` `:902` で設定する）。**保存物には出さない**（T4bまで）。
     - 受信の各入口（`frame` `:419`、`audio` `:372`、`log` `:565`、`touch` `:191`）の末尾で `anchors.observe(系, 端末側の時刻, host)` を呼ぶ（最小形の更新）。
     - `capture()` `:706-787` の末尾（`pinned = value` `:784` の直前）で影の計算を呼び、結果を `FrozenCapture` の新しいフィールド `shadow: ShadowWindow? = null` に載せる。**既存の行は1行も変えない。**
   - `core/SaveWriter.kt`: 変えない（影は保存物に出さない）。
   - T0の診断記録（`TimingDiagnostics`）: 有効なとき、保存ごとに影の結果と比較の要約を1行書く。
4. **変更の中身**:
   ```kotlin
   internal data class ShadowWindow(
       val s: Long, val w: Long, val slack: Long,          // 4.2
       val endGrade: TimeGrade, val tS: Long?, val tSLo: Long?, val tSHi: Long?,
       val coverage: Map<String, Coverage>,                 // 種類ごと。4.5
       val rowGrades: Map<TimeGrade, Int>,                  // 等級ごとの行数
       val compare: ShadowCompare
   )
   internal data class ShadowCompare(
       val measuredRowsEqualCurrentKnownRows: Boolean,      // I6
       val maxAbsDeltaTnsMeasuredNs: Long,                  // 測定等級の行の、影の t と現行の t_ns の差の最大（原点のずれを引いた後）
       val originShiftNs: Long,                             // 4.2 の原点のずれ（W < N のときだけ非0）
       val rowsNewlyOutside: Int, val rowsNewlyInside: Int, // 影の所属と現行の所属の違い
       val fullIntersectionEmpty: Int,                      // 全面採用の影: 交差が空になった回数
       val fullVsMinimalMaxWidthGainNs: Long                // 全面採用の影: 必須の形より幅が何ns狭くなったか（最大）
   )
   ```
   - **影の所属判定**（T4bで本採用する規則。ここでは数えるだけ）: 行の位置の範囲 `[t_earliest, t_latest]` が `[0, W]` と交わらなければ窓外。`t_earliest == null` のときは `t_latest < 0` なら窓外。交わるなら窓内。PC時刻だけで言える窓外: 受信H `< S − W − slack` なら、等級に依らず窓外（「記録は受信より後には起きていない」ので、受信が窓の前なら発生も窓の前）。
   - **境界値**: `t_latest == 0` は窓内、`t_latest == −1` は窓外。`t_earliest == W` は窓内、`W + 1` は窓外。受信H `== S − W − slack` は窓内の候補（PC時刻だけでは外せない）、それより1ns前は窓外。`slack` は `ceil(ρ × W)`。ρ=0 のとき `slack == 0`。
   - **全面採用の影**: 同じ錨区間の全サンプルの `[lo(x), hi(x)]` と最小形でない全記録の片方向制約を交差させる。交差が空になったら回数を数えるだけ（錨区間は切らない）。計算量を抑えるため、保存時に窓内の記録についてだけ行う。
   - **性能**: 受信ごとの処理は最小形の更新（比較1回）だけ。保存時の影の計算は行数に比例。影の計算で例外が出ても保存を失敗させない（捕まえて診断に書き、`shadow = null`）。
5. **不変条件**（4.5のI1〜I6。加えて）: (a) 影を入れる前後で、既存テストの期待値と、固定入力に対する保存物がバイト単位で同じ。(b) I6: `measured` の行の集合＝現行で `t_ns` が入る行の集合。(c) `measured` の行では、影の `t` ＝ 現行の `t_ns` − `originShiftNs`。(d) 137秒型・49秒型の入力で、影の所属では窓外になる行が、現行（修正後）でも窓外である（`rowsNewlyInside == 0`）。(e) どの行も `t_latest` は `W` を超えない（`arrival` の証明の検査）。
6. **OS固有の部分**: なし。
7. **試験計画**（すべて `FakeHostClock` と、テスト内で組み立てた入力。3 OS CIで同じ結果）:
   - `core/TimeAnchorsTest.kt`: `roundTripBoundsContainTrueOffset`、`boundsWidenByRhoWithDistance`、`slowRoundTripIsUsedWithWideBounds`、`invalidReadWidthSampleIsIgnored`、`minimalFloorKeepsLeastDelayedRecord`、`minimalFloorDecaysByRho`、`floorAboveUpperBoundIsDroppedAndCounted`、`newRegionStartsEmptyAnchor`、`hostSuspendStartsEmptyAnchorButKeepsClockRegion`。
   - `core/TimeGradesTest.kt`: `measuredIffCurrentSequenceKnown`、`tailExactlyFiveSecondsIsMeasured`、`tailFiveSecondsPlusOneIsEstimated`、`errorExactlyTwentyMsIsMeasured`、`errorAboveTwentyMsIsEstimated`、`noRoundTripInAnchorIsArrival`、`rowGradeIsWorseOfRowAndEnd`、`arrivalHasNullEarliest`、`arrivalNeverExceedsWindowEnd`、`measuredPositionEqualsCurrentTnsMinusOriginShift`。
   - `core/WindowCoverageTest.kt`: `partsSumToWindow`、`emptyStreamIsAllUnconfirmedOrMissing`、`singleReceiptRunConfirmsNothing`、`receiptBeforeWindowExtendsConfirmedToZero`、`tailAfterLastReceiptIsUnconfirmed`、`overlappingGapsAreCountedOnce`、`gapWithUnknownStartUsesHostTime`、`hostSuspendIsMissingEvenWhenRunContinues`、`streamStartedLateIsMissingBeforeStart`、`zeroWindowIsAllZero`、`confirmedByGradeSumsToConfirmed`。加えて、乱数で作った入力1000件で `assertCoverageInvariants`（種は固定）。
   - `core/ShadowWindowTest.kt`（再現入力。名前は固定データの案）:
     - `burst137`: N=30秒。時計が不明な区間（サンプルなしが100秒続く）の間も動画が届き続け、その後サンプルが復帰して保存。現行の窓外判定と影の所属が一致し、MP4に入る範囲が `≤ W`。
     - `burst49`: N=30秒。保存時に対応が無い（最後のサンプルから5秒超）。ADBが19秒詰まり、その後に19秒分の動画がまとめて届いた直後に保存。受信Hは窓内だが、最小形から求めた位置で古い分が窓外になる。
     - `adbStall`: 全ストリームが10秒止まり、まとめて届く。位置は端末時刻の差のままで、受信の順に並べ替わらない。
     - `hostSuspend20s`: 取得中にPCが20秒休止。窓に20秒の欠落（理由: PC休止）が入り、確認済みに含まれない。`W` は休止を含む。
     - `hostWallStep`: PCの日時が1時間進む。何も変わらない（HもAも日時に依らない）。
     - `deviceWallStep`: 端末の日時が飛ぶ。現行どおりログ系の時計区間が切り替わり、動画系は影響を受けない。
     - `deviceReboot`: 端末が再起動。別bootの行は現行どおり `t_ns` なし（影では `estimated` 以下）。
     - `shortCapture`: 取得開始から10秒で保存（N=30秒）。`W == 10秒`、`originShiftNs` が記録される。
   - 既存テスト全件で `ShadowCompare.measuredRowsEqualCurrentKnownRows` を検査する仕組み: `CaptureStore` のテスト用の入口で、`capture()` が返すたびに `shadow.compare` を検査する（テスト用のフラグで有効にし、偽なら失敗）。
8. **受入条件**: (a) 上のテストが3 OS CIで通る。(b) 既存テスト全件で I1〜I6 と 5の(c)(e) が成り立つ（検査の仕組みが全 `capture()` を通っていることを、検査回数を出力して示す）。(c) 本番の保存物が、影を入れる前の枝と同じ固定入力に対してバイト単位で同じ（`SaveLayoutTest` などの既存の比較で足りなければ、比較テストを1本足す）。(d) `SaveWriter.kt` の差分が0行。
9. **移行と互換**: 出力の変化なし。
10. **依存と順序**: T1、T3b-2の後（受信時刻がHになっていること）。T0判定の前（影の記録をT0判定が使う）。T4bの前。
11. **危険と停止条件**: I6が既存テストのどれかで破れた → **停止**（等級の定義が現行の確定と合っていない。4.4を直す必要があり、方針書の変更になる）。`rowsNewlyInside > 0` が137秒型・49秒型で出た → 停止。影の計算で保存が目に見えて遅くなった（900秒の窓で保存時間が5%以上増える）→ 停止して報告。
12. **規模**: 本番約700行、テスト約1300行。コミット6（錨、等級、3区分、影と配線、再現入力、全件検査）。難しさ: **高**（本計画の中心。式の境界と既存の確定条件との一致が要点）。

### 6.7 T3a 時計helperの自走と、3秒で止めない往復測定

1. **目的**: (1) PCからの問い合わせが詰まっても端末側の時計の読みが途切れないよう、helperが自分で定期的に出力する。(2) 応答が遅いことを理由にhelperを止めるのをやめ、遅れて届いた応答を幅の広い上下限として使う（方針書4.5）。どちらも `measured`・`estimated` になる記録を増やすための変更で、「時計が確かか」の判定は変えない。
2. **範囲**: `ClockProbe` の自走、PC側の読み取りを1つのスレッドにまとめること、**応答待ちの締切とプロセスの生存監視の分離**（`DeviceCapture.kt:346` の「3秒で止める」の廃止）、サンプルの種類（往復・遅延・自走）の区別、テスト。**やらないこと**: 窓・等級・3区分の定義の変更、往復測定の廃止（往復は残す。上限 `θhi` は往復からしか得られない）、5秒・20ms・2ms・40msの定数の変更、`certain()`・`endUncertainty()`・`now()`・`hostAnchor()` の式の変更、クラッシュ自動保存の受付条件の変更（D11）、起動時の先頭行の3秒の締切（`:340-341`）の変更、保存物の形の変更。
3. **変えるファイル**:
   - `plugin/src/clock/java/…/clock/ClockProbe.java`（39行）: 現状は `:18-26` でnonceを1行読むたびに4値を1行返す。変更後: 別スレッドで一定間隔（U11。仮に1秒）ごとに、nonceなしの行 `"-\t" + before + "\t" + mono + "\t" + wall + "\t" + after` を出す。出力は1行ずつ同期する。先頭行を `REPLAY_CLOCK_2` にする。
   - `core/DeviceCapture.kt:332-358`（`clock()`）: 現状、`:346` が問い合わせのたびに「3秒後に `adb.stop(process)`」を予約し、応答が3秒以内に来なければプロセスが止まり、`:350-351` が例外になってサンプルは残らない。変更後: 先頭行の検査を `REPLAY_CLOCK_2` に。**`:346` の予約をやめる。** 読み取りを1つのスレッドにまとめ、下の4の状態遷移で、nonceつきの行は往復または遅延のサンプルとして、nonceなしの行は自走のサンプルとして `CaptureClock` へ渡す。生存監視は別のタイマーにする（下の4）。
   - `core/CaptureClock.kt`: `ClockSample`（`:12-24`）に `kind` を足す。`add`（往復。現行のまま）のほかに `addLate`・`addFreeRun` を足す。**既存の `samples`（`certain()`・`endUncertainty()`・`now()`・`hostAnchor()`・`anchorFor` が見る列）には往復のサンプルだけを入れ、遅延と自走は別の列 `aux` に入れる。**
   - T0-aの診断（6.4）: `"k":"probe_fail"` の `why` の `timeout_3s` は「締切を過ぎた（helperは止めていない）」の意味になる。遅れて届いた応答は `{"k":"probe_late","sent":H,"received":H,…}` として書く。
4. **変更の中身**:
   - **サンプルの種類**（案）:
     ```kotlin
     internal enum class ClockSampleKind { ROUND_TRIP, LATE, FREE_RUN }
     //  ROUND_TRIP: 問い合わせに対し、締切（3秒）以内に届いた応答。現行のサンプルと同じ。
     //  LATE:       締切を過ぎてから届いた応答。sent・received は実際の値（往復の幅が3秒を超える）。
     //  FREE_RUN:   helperが自分で出した行。sent は無い（null）。received は行を読んだ時点のH。
     ```
   - **規則R1（時計が確かかは往復だけで決める）**: `certain()`（`:165-167`）・`endUncertainty()`（`:173-176`）・`now()`（`:78`）・`hostAnchor()`（`:98`）・`anchorFor`・時計区間の切り替え（`:50-55` → `startRegion`）は、**`ROUND_TRIP` のサンプルだけ**を見る。式も定数も変えない。`LATE`・`FREE_RUN` は `samples` に入らないので、これらの関数からは見えない。したがって「自走のサンプルで `certain()` が真になるのに、`endUncertainty()` は往復しか見ない」という食い違いは起きない（両方とも往復だけを見る）。窓の終点の等級（4.4）も `endUncertainty(S)` から決まるので、往復だけで決まる。
   - **規則R2（行の換算の延長だけを遅延・自走で行う）**: 記録の端末内の換算（`OffsetTrack.map`、`:284-299`）は2段にする。(i) まず**現行と同じ計算**（`ROUND_TRIP` だけの列）を行う。値が返れば、それをそのまま使う（値も誤差も現行と同じ）。(ii) 現行の計算が値を返さない記録（最後の往復サンプルから5秒を超えた、前後に往復サンプルが無い、など）に限り、`ROUND_TRIP`・`LATE`・`FREE_RUN` を合わせた列で同じ `map`（同じ5秒・20ms・2msの条件）を計算し直す。使うのは端末内offset（MONOTONIC↔BOOTTIME、epoch↔BOOTTIME）と受信Hだけで、`sent` は使わない。端末内offsetは往復に依らない（`:20` の注釈）ので、種類を問わず同じ精度で使える。
   - **規則R3（遅延・自走は時計区間を切り替えない）**: `LATE`・`FREE_RUN` のサンプルが現在の時計区間と合わない（`:50-55` と同じ判定で「切り替え」に当たる）とき、`startRegion` は呼ばない。代わりに `aux` を空にし、次の `ROUND_TRIP` が来るまで `aux` に足さない（延長を止める）。診断に `aux_break` を1行書く。時計区間の切り替えは、現行どおり次の往復のサンプルで起きる。理由: 遅延・自走で切り替えると `certain()` の真偽が変わり、R1とD11が崩れる。
   - **規則R4（上下限への使い方、4.3）**: `LATE` は実際の `sent`・`received` を持つので、4.3の `RoundTrip`（`thetaLo = before − received`、`thetaHi = after − sent`）として**使う**（幅が3秒以上ある「幅の広い上下限」。`valid` でなければ現行どおり使わない）。必須の形では幅が最小のサンプル1個を選ぶので、ほかに狭いサンプルがあれば選ばれないが、その錨区間に往復が1つも無いときは `LATE` が上限 `θhi` を与え、等級は `arrival` ではなく `estimated` になる。`FREE_RUN` は下限 `θ ≥ after − 受信H` だけを与える（片方向制約と同じ形。最小形 `OneWayFloor` の候補に入れる）。**`FREE_RUN` に `sent = received` などの値を入れて往復に見せかけてはならない**（`hostAnchored`（`:23`）と `endUncertainty` の式が偽の往復幅0を使ってしまう）。
   - **R1とR2から起こること**: 往復が5秒を超えて途切れ、自走だけが届いている間は、行の端末内の換算は延長されるが、`certain()` は偽、終点は `measured` でない。行に書く等級は悪い方（4.4）なので `estimated` になる（誤差幅は小さい）。クラッシュ自動保存はこの間、現行と同じく受け付けない。T4bの前（schema 4）では、行の `t_ns` は「`sequence != null` かつ `windowKnown`」（`SaveWriter.kt:457,530`）のときだけ入るので、(ii)で延長された行の `t_ns` が入るのは、保存の時点で終点が確定している場合（往復が回復した後に保存した場合）だけである。
   - **応答待ちの締切と、プロセスの生存監視の分離**:

     | | 応答待ちの締切 | プロセスの生存監視 |
     | --- | --- | --- |
     | 何を見るか | 送った1つの問い合わせに応答が来たか | helperが動いていて、出力が届いているか |
     | 値 | `REPLY_DEADLINE = 3秒`（現行の `:346` と同じ値。意味だけ変える） | `LIVENESS_TIMEOUT = 10秒`（仮。動画の読み取り監視 `VideoReadMonitor`、`DeviceCapture.kt:592-604` の10秒・250ms周期と同じ値） |
     | 過ぎたとき | その問い合わせを「未応答」に分類し、診断に `probe_fail`（`timeout_3s`）を書く。**helperは止めない。例外にしない** | `adb.stop(process)` で止め、現行と同じ例外の経路で `clock()` を抜ける（その後の起動し直しは現行の回復の経路のまま。変えない） |
     | 止める条件 | なし | (1) 出力の終端（EOF）、(2) 読み書きの `IOException`、(3) プロセスの終了、(4) 形式が不正な行（項目数が違う、数値でない、未知のnonce）、(5) **種類を問わず1行も届かない状態が `LIVENESS_TIMEOUT` 続いた** |

     - (5)は「応答が遅い」ではなく「出力が完全に途絶えた」を見る。自走の行が1秒ごとに届くので、正常なら10行分の余裕がある。ADBが10秒を超えて詰まった場合は(5)で止まるが、その時点で往復は5秒を超えて途切れており、時計は既に不明である（現行の3秒より7秒遅く止まるだけで、確定の範囲は狭まらない）。
     - 2つは別のタイマーで持つ。どちらも「締切・待ち時間」なので `System.nanoTime()`／`ScheduledExecutorService` のままでよい（3.2。Hに替えない）。サンプルの `sent`・`received` はH（T3b-2の後）。
   - **往復測定の状態遷移**（未応答の問い合わせは同時に1つだけ）:
     ```text
     IDLE     ─ nonceを送る（sent = H）────────────────► WAITING(nonce, sent)
     WAITING  ─ そのnonceの行が締切までに届く ──────────► add(ROUND_TRIP) → IDLE（現行と同じ間隔で次へ）
     WAITING  ─ 締切（3秒）を過ぎる ────────────────────► probe_fail を記録 → OVERDUE(nonce, sent)   ※止めない
     OVERDUE  ─ そのnonceの行が届く（received = H）─────► addLate(LATE) → IDLE（すぐ次を送る）
     OVERDUE  ─ 新しいnonceは送らない（応答を待ち続ける）
     どの状態でも ─ nonceなしの行が届く ───────────────► addFreeRun(FREE_RUN)（状態は変えない）
     どの状態でも ─ 生存監視の(1)〜(5) ────────────────► 止めて例外（現行の経路）
     ```
     - 境界: 締切は「送信から3秒**を超えた**」で過ぎたとする（ちょうど3秒で届いた応答は `ROUND_TRIP`）。締切の判定と行の到着が競合したときは、行を読んだスレッドが状態を見て決める（`WAITING` なら `ROUND_TRIP`、`OVERDUE` なら `LATE`）。状態は読み取りスレッドだけが書き換え、締切のタイマーは「締切を過ぎた」という印を立てるだけにする。
     - `OVERDUE` の間に新しいnonceを送らない理由: helperは問い合わせを1行ずつ順に処理する（`ClockProbe.java:18-26`）。詰まっている間に送り足しても、詰まりが解けたときにまとめて返るだけで、どれも `sent` が古い。
     - 最初の5回を間隔なしで送る現行の動作（`:354-355`）は変えない。
   - 古いhelperとの互換: helperはプラグインに同梱され、接続のたびに端末へ送る（`DeviceCapture.kt:337`）ので、版の不一致は起きない。先頭行が `REPLAY_CLOCK_2` でなければ現行どおり例外（`:341`）。
5. **不変条件**:
   - (a) **D11（クラッシュ自動保存の受付が変わらない）**: 任意のサンプルの列と任意の `h` について、`LATE`・`FREE_RUN` を入れた場合と、それらを捨てた場合とで、`certain(h)`・`endUncertainty(h)`・`now(h)`・`hostAnchor(h)`・`currentRegion()` が等しい。したがって `CaptureStore.kt:780` の受付結果（受け付ける／受け付けない）も等しい。例外は設けない。
   - (b) `LATE`・`FREE_RUN` を捨てたとき、既存テストの期待値が変わらない。
   - (c) `LATE`・`FREE_RUN` を入れても、現行の計算で値が返る記録の `sequence` と誤差は1つも変わらない。値が返る記録の集合は減らない（増えるだけ）。
   - (d) `FREE_RUN` だけでは `θhi` は狭くならず、`hostAnchored` なサンプルは増えない。
   - (e) 応答が3秒を超えても、(1)〜(5)に当たらない限りプロセスは止まらない（`adb.stop` が呼ばれない）。応答が後から届けば `LATE` が1個入る。
   - (f) 未応答の問い合わせは同時に1つ以下。
   - (g) 1行も届かない状態が `LIVENESS_TIMEOUT` 続くと、プロセスは止まる（ちょうど10秒では止めない。超えたら止める）。
   - (h) 往復が詰まっていても、自走の行が届いていれば記録の換算は延長される（R2の(ii)）。自走の行が時計区間と合わなくなったら延長は止まり、時計区間は切り替わらない（R3）。
   - (i) 詰まってからまとめて届いた記録（137秒型・49秒型）に対し、`LATE` の `received` や自走の受信Hを記録の時刻として使わない（位置は端末時刻。4.4）。
   - (j) 4.5のI1〜I5。
6. **OS固有の部分**: なし（端末側のコードとPC側の共通コードだけ）。
7. **試験計画**（時計は `FakeHostClock`、締切と生存監視のタイマーは注入する。実時間で3秒・10秒待たない。3 OS CI）:
   - `core/ClockSampleKindTest.kt`: `certainIgnoresFreeRunAndLate`、`endUncertaintyIgnoresFreeRunAndLate`、`nowAndHostAnchorIgnoreFreeRunAndLate`、`auxNeverStartsRegion`、`crashAutosaveAcceptanceEqualsAuxDiscarded`（乱数で作ったサンプルの列と `h` で、(a)を全数比較する。種は固定）、`freeRunIsNeverHostAnchored`。
   - `core/ClockFreeRunTest.kt`: `legacyMappedRowsKeepValueAndUncertainty`、`freeRunExtendsRowMappingAfterFiveSeconds`、`freeRunExtendsRowMappingButNotEndCertainty`、`freeRunGivesOnlyLowerBound`、`auxBreakStopsExtensionUntilNextRoundTrip`、`roundTripStalledThreeSecondsStillMapsRows`。
   - `core/ClockLateReplyTest.kt`: `replyAtExactlyThreeSecondsIsRoundTrip`、`replyAfterThreeSecondsIsLateAndHelperSurvives`、`lateReplyGivesWideBoundsAndEstimatedGrade`、`noSecondNonceWhileOverdue`、`timeoutIsRecordedAsProbeFail`、`batchedRecordsAfterStallAreNotPlacedByReceiveTime`（137秒型・49秒型の入力）。
   - `core/ClockLivenessTest.kt`（偽のプロセス）: `eofStopsHelper`、`malformedLineStopsHelper`、`silenceOverTenSecondsStopsHelper`、`silenceOfExactlyTenSecondsDoesNotStop`、`slowReplyWithFreeRunLinesDoesNotStop`、`startupHeaderDeadlineIsUnchanged`。
   - `core/DeviceCaptureClockLineTest.kt`（偽のプロセス）: `noncedLineGoesToRoundTrip`、`freeRunLineGoesToFreeRun`、`unknownHeaderFails`、`interleavedLinesAreNotMixed`。端末側の `ClockProbe` はJVM上で直接は動かせないので、行の形だけを共通の定数で縛る。
   - 既存の `CrashAutosaveTest`・`CrashSaveLifecycleTest` の期待値は変えない。T0-aで足した `probeTimeoutIsRecordedAsFailure`（6.4）は「記録され、かつ止まらない」に直す。
8. **受入条件**: (a) 上のテストと既存テストが3 OS CIで通る。(b) `crashAutosaveAcceptanceEqualsAuxDiscarded` が通る（D11）。(c) T0-aの診断を有効にしたemulator（または実機）で、ADBを5秒止めて再開したとき（7.2の(d)）、helperのプロセスが起動し直されず、診断に `probe_fail` と `probe_late` が残る。20秒止めたときは生存監視で止まり、起動し直される。(d) 同じ負荷のもとで、`measured` の行の割合がT3aの前より下がっていない（7.3の表のうち該当の行を前後で示す）。(e) 保存物の形が変わっていない（T4bの前ならschema 4、後ならschema 5のまま）。
9. **移行と互換**: 保存物の形は変えない。`timebase.samples` に遅延・自走のサンプルを出すかどうか: T4bの前に入る場合は**出さない**（schema 4 の `samples` の意味を変えない）。T4bの後に入る場合は `kind: "late"`・`kind: "free_run"` を付けて出す（schema 5 の項目追加。読み手は未知の `kind` を無視する規則をT4aで入れておく）。T4bの前に入る場合、schema 4 の保存物で変わり得るのは「これまで `t_ns` が空だった行に値が入る」ことだけで、値が入っていた行は変わらない（不変条件の(c)）。
10. **依存と順序**: T0-aの計測（U11）、T3b-2の後。**T4bの前提ではない**（5章）。T5の前。`DeviceCapture.clock()` をT0-a・T3b-2と並行して触らない。4.3のR4（`LATE` を上下限に使う）はT2の錨の計算が入力を受け取る形にしておき、T3aは `LATE` を渡すだけにする（T2とT3aの順はどちらでもよい。後から入る方が相手に合わせる）。
11. **危険と停止条件**: 自走の出力が端末の負荷を目に見えて上げる（T0-aの診断で確認）→ 間隔を見直すため停止して報告。往復と自走の行が混ざって読めない端末がある → 停止。不変条件の(a)〜(c)のどれかが成り立たない → 停止（R1〜R3のどこかが実装とずれている）。`LIVENESS_TIMEOUT` の10秒で、正常な負荷（7.2の(b)）のもとでhelperが止まる → 値を変えずに停止して報告（値は親セッションが決める）。3秒の締切を「止める」以外の形で残すことが既存テストと両立しない → 停止。
12. **規模**: 本番約350行（Java約40行を含む）、テスト約700行。コミット4（自走と行の振り分け、サンプルの種類とR1〜R4、締切と生存監視、テスト）。難しさ: 高（スレッド、状態遷移、現行の確定条件を変えないことの検査）。

### 6.8 T4a 読み手の両対応

1. **目的**: schema 4 と schema 5 の両方を開けるようにする。書き手はまだ schema 4 のまま。schema 4 のトリムの数え方を凍結して分離する。
2. **範囲**: `SavedReplay` の版の受け入れ、版ごとの読み取りの分岐、schema 4 用のcoverage計算の分離、schema 5 の読み取り（固定データで試験）、未知の版の拒否。**やらないこと**: 書き手の変更（`ReplayFiles.SCHEMA` は4のまま）、schema 5 のトリムの**書き出し**（T4b）、UIの表示の変更、schema 4 の保存物の書き換え・変換。
3. **変えるファイル**（#460 `59cc95f` の行番号。TRの後は引き直す）:
   - `core/SavedReplay.kt:140`: 現状 `manifest["schema"].asInt == ReplayFiles.SCHEMA` でなければ拒否。変更後: `schema in ReplayFiles.READABLE_SCHEMAS`（`setOf(4, 5)`）。`SavedReplay` に `val schema: Int` を足す。
   - `core/SaveLayout.kt:17`: `const val SCHEMA = 4` はそのまま。`val READABLE_SCHEMAS = setOf(4, 5)` を足す。
   - `core/ReplayTrim.kt:908-926`（TRで切り出した `schema4TrimCoverage`）: 名前と注釈で「schema 4 用。凍結。変更しない」と明示する。`saved.schema == 4` のときだけ呼ぶ。`saved.schema == 5` のトリムは、T4aの時点では「未対応」として**明示的に拒否**する（利用者向けの文言つき）。T4bで実装する。
   - `core/ReplayTimeline.kt`、`core/ReplayPlayback.kt`、`core/ReplayFrameExport.kt:157-158`（PNGのmetadataへの `t_ns`）: 行の `t_ns` を読む箇所で、schema 5 の `time_grade`・`t_earliest_ns`・`t_latest_ns` を併せて読む小さな型（下記）を通す。
   - `ui/ReplayToolWindowFactory.kt:509`（保存済みの表示がここを通る場合）: 変えない（T4b）。
4. **変更の中身**:
   ```kotlin
   /** 保存物の行の時刻。schema 4 では t があれば measured、無ければ null。 */
   internal data class SavedTime(val t: Long?, val earliest: Long?, val latest: Long?, val grade: TimeGrade?)

   internal fun JsonObject.savedTime(schema: Int): SavedTime = when (schema) {
       4 -> text("t_ns")?.toLong().let { SavedTime(it, null, null, if (it != null) TimeGrade.MEASURED else null) }
       5 -> SavedTime(text("t_ns")?.toLong(), text("t_earliest_ns")?.toLong(), text("t_latest_ns")?.toLong(),
                      text("time_grade")?.let(TimeGrade::fromJson))
       else -> error("unreachable: SavedReplay.open が拒否する")
   }
   ```
   - **版ごとの分岐は `SavedReplay` とこの型の中だけ**に置く。呼び出し側は `saved.schema` を直接見ない（例外はトリムのcoverageの選択1か所）。
   - schema 5 の未知の項目（未知の `time_grade` の値、未知の欠落の理由、`timebase.samples` の未知の `kind`）は、読み手が失敗せず「不明」として扱う。
   - schema 5 の保存物の検査（開くときに行う）: `coverage.<kind>` の `confirmed_ns + unconfirmed_ns + missing_ns == window_ns`。合わなければ「保存物が壊れている」として拒否（既存の検査と同じ扱い）。
   - 再生・コマ送り・書き出しで `t_ns` を使う箇所の、等級ごとの扱い: `measured` は現行どおり。`estimated`・`arrival` は位置として使うが、PNGのmetadataには `arb:time_grade` を併記する（#460 `ReplayCli.kt:166` の「t_nsは既知の場合だけ記録します」の文言の変更はT4b。T4aでは schema 5 を読んだときの動作だけ入れる）。
5. **不変条件**: (a) schema 4 の保存物に対する全操作（開く・再生・コマ送り・書き出し・トリム）の結果が、T4aの前とバイト単位で同じ。(b) schema 4 をトリムした結果は schema 4。(c) schema 6 以上・3以下は拒否。(d) schema 5 の固定データが開け、3区分の和の検査を通る。(e) 書き手は schema 4 のまま（`ReplayFiles.SCHEMA == 4`）。
6. **OS固有の部分**: なし。
7. **試験計画**:
   - 固定データ（`plugin/src/test/resources/…/schema5/`。小さな手書きの保存物。4.5と6.9の形に従う。**T2の `splitCoverage` の出力から機械的に作る**か、手で書いて `assertCoverageInvariants` を通す）: `schema5-measured`（全行 `measured`）、`schema5-mixed`（3等級が混ざる）、`schema5-arrival`（`t_earliest_ns: null`）、`schema5-suspend`（欠落の理由にPC休止）、`schema5-bad-sum`（和が合わない。拒否される）。
   - `core/SavedReplaySchemaTest.kt`: `opensSchema4`、`opensSchema5`、`rejectsSchema3`、`rejectsSchema6`、`rejectsSchema5WithBadCoverageSum`、`unknownGradeIsReadAsUnknown`、`schema4RowWithTnsIsMeasured`、`schema4RowWithoutTnsHasNoGrade`。
   - `core/ReplayTrimSchema4FrozenTest.kt`: T1の固定テスト（`ReplayTrimCoverageBaselineTest`）と同じ入力・同じ期待値。加えて `trimOfSchema4StaysSchema4`、`trimOfSchema5IsRejectedUntilWriterSwitch`（T4bでこのテストを消す）。
   - 既存の #460 のテスト（`ReplayTrim*Test` 8本、`SavedReplay*Test` 2本、`ReplayPlaybackTest` ほか）が変更なしで通る。3 OS CI。
8. **受入条件**: (a) 上のテストが3 OS CIで通る。(b) 既存の #460 のテストの差分が0行。(c) `ReplayFiles.SCHEMA` が4のまま。(d) `schema4TrimCoverage` の本体が TR の切り出し時と同じ（差分は名前と注釈だけ）。
9. **移行と互換**: この時点で、新しい版のプラグインは schema 4 と 5 を読める。古い版のプラグインは schema 5 を拒否する（現行の `:140` の検査による。利用者向けの文言が適切かをこのTrailで確かめ、必要なら「新しい版で保存されたリプレイです」という文言に直す。**ただし古い版の文言は変えられない**ので、T4bのリリースノートに書く）。
10. **依存と順序**: #460、TR、T1の後。T4bの前。「T3b-1 → T0-a → T3b-2 → T2」の列と並行できる。**schema 5 の形（6.9の4の(c)）を先に固定する必要がある**ので、T4aの着手前に、6.9の4の(c)の形についてオーナーの確認（U6・U7）を取る。形が変わるとT4aの固定データをやり直すことになる。
11. **危険と停止条件**: TRがまだ入っていない → 着手しない。schema 4 の出力が1バイトでも変わる → 停止。schema 5 の形がT4aの後で変わることになった → T4aの固定データと読み取りを直すTrailを別に切る（T4bに混ぜない）。
12. **規模**: 本番約300行、テスト約600行、固定データ5組。コミット4。難しさ: 中。

### 6.9 T4b 書き手の切替（schema 5）

1. **目的**: 窓・所属・配置・等級・3区分を影から本採用に切り替え、保存物を schema 5 にする。schema 5 の意味が決まるのはこの1回だけ。
2. **範囲**: `capture()`・`prune()` の窓と所属、行の位置と等級の書き出し、3区分の書き出し、`availableSeconds` の廃止、schema 5 のトリム、UIの表示、CLI/MCPの出力（U6が「出す」の場合）、docs。**やらないこと**: 等級・3区分の計算式の変更（T2で入れたものをそのまま使う。直す必要が出たら停止）、schema 4 の保存物の変換、schema 4 のトリムの数え方の変更、クラッシュ自動保存の受付条件の変更、ρ・しきい値の再調整（T0判定で決まった値を使う）、時計helperの変更（T3a）。
3. **変えるファイル**（develop の行番号。T2・T3b-2の後は引き直す）:

   | 位置 | 現状 | 変更後 |
   | --- | --- | --- |
   | `core/SaveLayout.kt:17` | `SCHEMA = 4` | `SCHEMA = 5` |
   | `core/CaptureStore.kt:712` | `end = fixedEnd ?: clock.now(host) ?: latest()` | `T_S`（4.4）。`latest()` への退避をやめる |
   | `core/CaptureStore.kt:715` | `start = maxOf(0, origin, end − N)` | 窓は `[0, W]`（4.2）。`FrozenCapture` に `s`・`w`・`endGrade` を持たせる |
   | `core/CaptureStore.kt:716`、`:1117-1118` | `hostStart = host − N − HOST_WINDOW_MARGIN_NS`（250ms） | PC時刻だけの窓外判定 `受信H < S − W − ceil(ρ×W)`。定数 `HOST_WINDOW_MARGIN_NS` を削除 |
   | `core/CaptureStore.kt:717-777`（行の選別） | seqと `hostStart` で選別 | T2の影の所属判定を本採用（6.6の4） |
   | `core/CaptureStore.kt:623-677`（`prune`） | `end − N` より古いものを捨てる。不明時は `host − N − 250ms` | 「どの将来のSに対しても窓外と言えるもの」だけ捨てる: 位置の上限が `(今のH − N×1e9 − ceil(ρ×N×1e9))` に対応する位置より前。ストリームごとに「最後に捨てた受信」を1件残す（T2で入れたもの） |
   | `core/CaptureStore.kt:802-826`（`streams`） | `availableSeconds = span − missing` | `splitCoverage` の結果を `StreamSnapshot` に載せる |
   | `core/ReplayModels.kt:167-170` | `StreamSnapshot.availableSeconds: Double` | 削除し、`coverage: Coverage?`（確認済み・末尾未確認・欠落）を持たせる |
   | `core/SaveWriter.kt:457,530,553` | `t_ns` は `sequence != null && windowKnown` のときだけ | 全行に `t_ns`・`t_earliest_ns`・`t_latest_ns`・`time_grade` |
   | `core/SaveWriter.kt:464,531,553` | `t_uncertainty_ns` | 残す（`measured` の行では現行と同じ値。ほかの等級では `(t_latest − t_earliest)/2`、上限なしのとき `null`） |
   | `core/SaveWriter.kt:756-761,1064,1072` | `coverage.<kind>.availableSeconds` | 下の(c)の形 |
   | `core/SaveWriter.kt:764-768` | `gaps[].from_t_ns/to_t_ns`（不明なら `null`） | 常に値を持つ（4.5の入力の作り方）。等級を併記 |
   | `core/SaveWriter.kt:897-908`（`timebase`） | `known`、`window_duration_ns = end − start`、`window_end_uncertainty_ns` | 下の(c)の形 |
   | `core/SaveWriter.kt:650`（README内の表） | `availableSeconds` を表示 | 3区分を表示 |
   | `ui/ReplayToolWindowFactory.kt:479` | `"$label: $state / %.1f秒分"` | 「確認済み X秒 / 窓 W秒」（U7の1） |
   | `core/ReplayTrim*.kt`（TR・T4aの後） | schema 5 のトリムは拒否 | schema 5 のトリムを実装（下の(g)） |
   | #460 `cli/ReplayCli.kt:166` | 「t_nsは既知の場合だけ記録します」 | 「t_nsと時刻の等級（arb:time_grade）を記録します」 |
   | `docs/cli.md`、CLIの `--help`、`plugin/src/main/cli/README.txt`、`docs/mcp.md` | — | U6が「出す」なら4つを同時に更新 |
   | `docs/design/timeline.md`、`docs/plugin-design.md:60` | `coverage.audio` の `availableSeconds` の説明 | schema 5 の説明に更新。schema 4 の説明は「旧」として残す |

4. **変更の中身**:

   **(a) 窓と所属**: 4.2と6.6の4のとおり。`W ≤ 0` は保存しない。凍結中は `S = fixedHost`。クラッシュ自動保存の受付（`:780`）は変えない。

   **(b) 配置**: 4.4のとおり。MP4に入れるframeは、位置の範囲が `[0, W]` と交わるもの。MP4の時間軸は `t`（先頭のframeの `t` が0より前なら0に切る。現行の `SaveWriter` の正規化と同じ扱い）。MP4の再生時間 `≤ W`。

   **(c) schema 5 の形**（`replay.json`。項目名は確定とし、変えるなら停止）:
   ```json
   {
     "schema": 5,
     "timebase": {
       "domain": "device_boottime", "t_origin": "window_start",
       "window_ns": "30000000000",
       "window_end_grade": "measured",
       "window_end_uncertainty_ns": "1500000",
       "rho_ppm": 500,
       "host_clock": { "method": "linux_boottime", "includes_suspend": true, "suspend_undetectable": false },
       "host_suspends": [ { "from_t_ns": "12000000000", "to_t_ns": "14000000000", "asleep_ns": "1990000000" } ],
       "origin_shift_ns": "0",
       "samples": [ "…（現行と同じ。T3a の後は kind を持つことがある）" ]
     },
     "coverage": {
       "video": {
         "state": "CAPTURING", "requested": true, "reason": null,
         "confirmed_ns": "27000000000", "unconfirmed_ns": "1000000000", "missing_ns": "2000000000",
         "confirmed_by_grade": { "measured": "25000000000", "estimated": "2000000000", "arrival": "0" },
         "confirmed": [ { "from_t_ns": "0", "to_t_ns": "12000000000" }, { "from_t_ns": "14000000000", "to_t_ns": "29000000000" } ],
         "unconfirmed": [ { "from_t_ns": "29000000000", "to_t_ns": "30000000000" } ],
         "missing": [ { "from_t_ns": "12000000000", "to_t_ns": "14000000000", "reason": "PC休止" } ]
       }
     },
     "gaps": [ { "kind": "video", "from_t_ns": "12000000000", "to_t_ns": "14000000000", "reason": "PC休止", "time_grade": "measured" } ]
   }
   ```
   行（`video_frames`、ログ、タップのjsonl）:
   ```json
   { "t_ns": "1234567890", "t_earliest_ns": "1234067890", "t_latest_ns": "1235067890", "time_grade": "measured", "t_uncertainty_ns": "500000" }
   { "t_ns": "29100000000", "t_earliest_ns": null, "t_latest_ns": "29100000000", "time_grade": "arrival", "t_uncertainty_ns": null }
   ```
   - nsの値は現行どおり文字列（`SaveWriter.kt:457` ほか）。`rho_ppm` は数値。
   - `availableSeconds` は schema 5 に**書かない**。`timebase.known` も書かない（`window_end_grade` に置き換え）。
   - `suspend_undetectable`: `includes_suspend == false` で、かつ退避の検出（6.5）が働く前提が無かったとき（往復測定が復帰後にまだ成功していない）に `true`。
   - 音声の `coverage.audio` も同じ形（受信＝音声packet）。除外（`EXCLUDED`）のときは現行どおり `state` などだけを持ち、3区分は `null`。

   **(d) 3区分の出力**: 保存時は `splitCoverage(W, …)` を種類ごとに1回。取得中の表示（`streams`）は「今保存したら」の値（`S = 今のH`）で同じ関数を呼ぶ。

   **(e) UI**: 「確認済み 27.0秒 / 窓 30.0秒」。末尾未確認・欠落は、0でないときだけ詳細（ツールチップまたは既存のgap一覧）に出す。`estimated`・`arrival` が含まれるときは「うち推定 2.0秒」を併記。文言は日本語。既存の `ReplayToolWindowModelTest` を更新。

   **(f) CLI/MCP**（U6が「出す」の場合だけ）: 状態を返す出力に、種類ごとの `confirmed_seconds`・`unconfirmed_seconds`・`missing_seconds`・`window_seconds` を足す（既存の項目は消さない・意味を変えない）。GUI・CLI・MCPで同じ `Coverage` から作り、同じ値になる。`docs/cli.md`・`--help`・`README.txt`・`docs/mcp.md` を同じコミットで更新する。終了コードは変えない。

   **(g) schema 5 のトリム**: 入力が schema 5 のとき、トリム範囲 `[a, b]`（MP4の時間軸＝`t`）について、`window_ns = b − a`、3区分は元の `confirmed`・`unconfirmed`・`missing` の区間一覧を `[a, b)` で切って `a` を引いたもの。和は `b − a` になる（元の3区分が `[0, W)` を余りなく分けているため）。行は `t` の範囲で選び、`t_ns`・`t_earliest_ns`・`t_latest_ns` から `a` を引く。等級は変えない。結果は schema 5。T4aで入れた `trimOfSchema5IsRejectedUntilWriterSwitch` を消し、代わりのテストを入れる。schema 4 の入力は `schema4TrimCoverage` のまま（触らない）。

   **(h) U5が「許さない」の場合**: `t` の原点を現行と同じ（端末時刻で測った取得開始）に保つため、`W < N×1e9` の窓に限って `t' = t + origin_shift_ns` を書く、という変更になる。この場合 `[0, W]` と行の位置の範囲がずれ、3区分・MP4の軸との一致が崩れるので、**本書の式のままでは実装できない。停止して、方針書4.2の改訂を求める。**

5. **不変条件**（4.5のI1〜I6をすべて本番の出力で検査。加えて）:
   - (a) `measured` の行: schema 5 の `t_ns` ＝ 切替前（schema 4）の `t_ns` − `origin_shift_ns`。`W == N×1e9` の窓では `origin_shift_ns` は終点の誤差幅以内（終点が `measured` のとき `≤ window_end_uncertainty_ns + ceil(ρ×W)`）。行どうしの間隔は切替前後で同じ（同じストリーム、同じ時計区間）。
   - (b) MP4の再生時間 `≤ W ≤ N×1e9`。137秒型・49秒型の入力で検査。
   - (c) 全行に `time_grade` がある。`arrival` の行は `t_earliest_ns == null`。`measured` の行は `t_earliest_ns ≤ t_ns ≤ t_latest_ns`。
   - (d) クラッシュ自動保存を受け付ける入力の集合が切替前後で同じ（`CrashAutosaveTest`・`CrashSaveLifecycleTest` の期待値が変わらない）。
   - (e) schema 5 を schema 5 のままトリムして、和が `b − a`。通常保存とトリムで同じ `Coverage` の型と同じ切り方を使う。
   - (f) GUI・CLI・MCPの秒数が同じ値。
   - (g) `HOST_WINDOW_MARGIN_NS`・`availableSeconds` という名前が本番コードに残らない（schema 4 用の `schema4TrimCoverage` の中の文字列 `"availableSeconds"` を除く）。
6. **OS固有の部分**: なし。`timebase.host_clock.method` の値だけがOSごとに違う（意図したもの）。それ以外の出力は、同じ入力（`FakeHostClock`）に対して3 OSでバイト単位で同じ。
7. **試験計画**:
   - T2の再現入力8種（`burst137` ほか）を、今度は保存物まで通して検査する `core/Schema5SaveTest.kt`: `burst137SavesAtMostWindow`、`burst49SavesAtMostWindow`、`adbStallKeepsDeviceOrder`、`hostSuspendIsMissingInAllStreams`、`hostWallStepChangesNothing`、`deviceWallStepSplitsOnlyLogRegion`、`deviceRebootRowsAreNotMeasured`、`shortCaptureWindowIsElapsedSinceStart`、`unknownClockAtSaveNoLongerFallsBackToLatestRecord`、`everyRowHasGrade`、`coverageSumsToWindowInManifest`。
   - 境界: `rowAtExactlyZeroIsInside`、`rowAtExactlyWindowIsInside`、`hostCutoffUsesRhoTimesWindow`（ρ=0・500ppm、W=1秒・900秒）、`replaySecondsOneAndNineHundred`。
   - `core/ReplayTrimSchema5Test.kt`: `trimCoverageSumsToRange`、`trimKeepsGrades`、`trimShiftsEarliestAndLatest`、`trimOfSchema4StillUsesFrozenFormula`。
   - 切替前後の比較 `core/Schema4To5ParityTest.kt`: 同じ入力をT4bの前の計算（T2の影の比較で既に検査済みの値）と比べ、(a)を検査。
   - 更新する既存テスト: `CaptureStoreSaveTest`、`SingleMp4SaveTest`、`LogsOnlySaveTest`、`SaveLayoutTest`、`ReplayToolWindowModelTest`、`ReplayCliTest`、`ReplayMcpServerTest`、T1の `TimeDefinitionBaselineTest`（**消さずに「schema 4 の定義。T4bで置き換えた」と注釈して、置き換えた後の期待値に直すか、schema 4 トリムに関係するものだけ残す。どのテストをどう変えたかをPR本文に一覧で示す**）。
   - 3 OS CI。`plugin_check.py`（プラグインZIPと構造の検査を含む）。
8. **受入条件**: (a) 上のテストが3 OS CIで通る。(b) 5の(a)〜(g)が検査されている。(c) T4aの固定データ（`schema5-*`）と、T4bの書き手が出した保存物が、同じ読み手で開ける。(d) schema 4 の既存の保存物（テスト資源）が開け、トリムでき、結果が切替前とバイト単位で同じ。(e) CLIの契約を変えた場合、4つの文書が同じコミットで更新されている。(f) VMのIDE＋emulatorで、保存→再生→トリム→書き出しが通る（`docs/verification/vm-acceptance.md` の手順。実施した範囲を明記）。
9. **移行と互換**: 書き手は schema 5 だけを書く。読み手は4と5（T4a）。schema 4 の保存物は書き換えない・変換しない。schema 4 のトリムは旧式（#460 `ReplayTrim.kt:908-922` を切り出した `schema4TrimCoverage`）のまま、結果も schema 4。古い版のプラグインは schema 5 を開けない（リリースノートに書く）。U10（外部の利用者）が「いる」なら、リリース前に告知する。
10. **依存と順序**: T0判定（U1・U2）、T2、T3b-2、T4a、オーナー判断（U5・U6・U7・U10）のすべての後。**T3aには依存しない。** このTrailの後は `capture()`・`SaveWriter` の時間まわりを触るTrailを置かない（T5は文書と受入だけ）。
11. **危険と停止条件**: オーナー判断（U5・U6・U7）が出ていない → 着手しない。T0判定でρが仮の値の4倍を超えた、または片方向制約の全面採用が決まった → 本節の式が変わるので、本書を改訂してから着手。T2の影の比較で `rowsNewlyInside > 0` が残っている → 着手しない。5の(a)が成り立たない入力が見つかった → 停止。(h)に該当 → 停止。
12. **規模**: 本番約900行、テスト約1500行、文書約300行。コミット8（窓と所属、配置と等級の書き出し、3区分の書き出し、`prune`、トリム、UI、CLI/MCPとdocs、テストの更新）。難しさ: **高**（出力が変わる唯一のTrail。レビューの負担が最も大きい）。1つのTrailに保つ理由は、schema 5 の意味を2回変えないため。コミットは上の順に分け、各コミットでテストが通るようにする。

### 6.10 T5 受入

1. **目的**: 実機・VMで、仕様どおりに動くことを確かめ、結果を記録する。
2. **範囲**: 受入手順の文書化と実施、結果の記録、READMEへの限界の記載（U9）。**やらないこと**: 本番コードの変更（不具合が見つかったら別のTrailを切る）、録画・ログ本文の公開。
3. **変えるファイル**: 新規 `docs/verification/time-definition-acceptance.md`。`docs/verification/vm-acceptance.md` に項目を足す。`README.md` または `docs/project.md` に「確認済みの意味と限界」を足す。
4. **変更の中身**（受入の項目）:

   | # | 操作 | 合格の条件 |
   | --- | --- | --- |
   | A1 | N=30秒で、負荷なしで保存を10回 | 10回とも `window_ns == 30e9`、MP4の再生時間 `≤ 30.0秒`、3区分の和 `== window_ns`、`window_end_grade == measured` |
   | A2 | N=30秒で、PCに高負荷（全コアを使う処理）をかけて保存を10回 | 10回とも MP4 `≤ 30.0秒`、和が一致。`estimated`・`arrival` があってよい |
   | A3 | ADBを詰まらせる（端末側で `adb` の転送を一時止める、またはUSBを一瞬抜く）→ 復帰直後に保存、を5回 | 5回とも MP4 `≤ 30.0秒`。137秒型・49秒型が再現しない |
   | A4 | 取得中にPCを30秒休止 → 復帰後10秒で保存（3 OSそれぞれ。**macOS・Windowsはオーナーの実機で行う。手順は7.6の手順3と同じ**。Linuxは休止できるVMまたは実機） | 欠落に理由「PC休止」があり、その長さが「実際の休止 − 2秒」以上「実際の休止 + 2秒 + `SUSPEND_SPAN_SLACK`」以下（30秒の休止なら28〜34秒。休止の前後の起きていた時間を含んでよい、4.1・I3b）。確認済みに含まれない。`host_clock.includes_suspend == true` |
   | A5 | 取得中にPCの日時を1時間進める → 保存 | A1と同じ結果。`host_suspends` が空 |
   | A6 | 取得中に端末の日時を変える → 保存 | 現行と同じく、ログ系の時計区間が切り替わる。和が一致 |
   | A7 | 取得中に端末（emulator）を再起動 → 保存 | 和が一致。別bootの行は `measured` でない |
   | A8 | 取得開始から10秒で保存（N=30秒） | `window_ns` が約10秒（±1秒）。`≤ 30e9` |
   | A9 | N=900秒で保存を1回 | MP4 `≤ 900.0秒`、和が一致、保存にかかる時間がT4bの前の版の1.1倍以内 |
   | A10 | クラッシュ自動保存を、時計が確定している状態と不明な状態で各3回 | 確定のとき保存される。不明のとき保存されない（現行と同じ） |
   | A11 | schema 4 の既存の保存物を開く・再生・トリム | 開ける。トリムの結果は schema 4 で、`availableSeconds` が旧版と同じ |
   | A12 | schema 5 の保存物をトリム → 再生 | 和が `b − a`。表示が「確認済み X秒 / 窓 W秒」 |
   | A13 | GUI・CLI・MCPで同じ保存の秒数を読む | 3つとも同じ値（U6が「出す」の場合） |

5. **不変条件**: A1〜A13の合否が、保存物の `replay.json` とMP4の長さから機械的に決まる（集計スクリプト `scripts/analysis/acceptance_check.py` を足し、保存フォルダを渡すと表を出す）。
6. **OS固有の部分**: A4は3 OSそれぞれで行う。**macOS・Windowsの実際の休止を確かめる正式な場はここである**（T3b-2の着手条件からは外した、6.5の10）。子セッションはクラウドのLinuxで動くので、macOS・WindowsのA4はオーナーが実機で行い、保存フォルダを `acceptance_check.py` にかけた結果（合否の表だけ）を渡す。T0-b（6.4b）を先に済ませていても、A4はT4bの後の版でもう一度行う（T0-bは診断記録で時計の動きを見るもの、A4は保存物の欠落・3区分・`host_clock` を見るもの）。A1〜A3、A5〜A13はLinuxのVM＋emulatorで実施し、macOS・Windowsは可能な範囲で実施する。実施できなかったOSは「未実施」と明記する（`AGENTS.md` の「何を確かめ、何を確かめていないかを報告する」）。
7. **試験計画**: 集計スクリプトの単体テスト（固定の `replay.json` に対する合否）。受入そのものは手順書に従って実施。
8. **受入条件**: (a) A1〜A13の表が埋まっている（合格・不合格・未実施）。(b) 不合格が0。(c) 未実施には理由がある。(d) 公開物に録画・ログ本文が無い。
9. **移行と互換**: A11・A12が互換の確認。
10. **依存と順序**: T4b、T3aの後。最後。
11. **危険と停止条件**: 不合格が出た → 直さずに報告し、別のTrailを切る。macOS・WindowsのA4が未実施のまま → 受入を「未完了」として報告する（そのOSでは実際の休止が一度も確かめられていないことになる。リリース判断はオーナー）。LinuxのA4が実施できない → 未実施として報告。
12. **規模**: 文書約250行、スクリプト約200行、テスト約100行。コミット2。難しさ: 低（A4のmacOS・Windowsにオーナーの実機作業が要る。1 OSあたり約15分）。

---

## 7. T0の計測手順と判定基準

T0は3つに分かれる。**T0-a**（6.4のコードを入れ、子セッションがクラウドのLinux VM＋emulatorと3 OSのCIで取れる数値を取る）、**T0-b**（オーナーが手元のmacOS・Windowsの実機とUSB実機で、7.6の手順書どおりに取る。コードは変えない）、**T0判定**（T2の影の記録も合わせて、U1・U2・U3・U11の案を出す。文書だけ）。CIでは休止・日時の段差・端末の飛びを起こせないので、CIは `FakeHostClock` と偽のプロセスで**規則**を検査し、T0-a・T0-bは**前提**（時計が実際にそう動くか）を確かめる。

### 7.1 環境

| 記号 | PC | 端末 | 担当 | 必須か |
| --- | --- | --- | --- | --- |
| E0 | 3 OSのCIのランナー（`test`、`os-parity`） | なし（`FakeHostClock`・偽のプロセス・固定データ） | T0-a | 必須 |
| E1 | Linux（子セッションのクラウドのVM） | emulator | T0-a | 必須 |
| E1b | Linux（実機、または休止できるVM） | emulator | T0-b | 任意（T0-aで(e)(f)が実施できなかったときだけ） |
| E2 | macOS（Apple Silicon の実機） | emulator | T0-b | T5のA4までに必須 |
| E3 | Windows 11（実機） | emulator | T0-b | T5のA4までに必須 |
| E4 | E1b〜E3のどれか1つ | USB接続の実機 | T0-b | ρの確定（U1）に必須（1台以上。2機種あれば両方） |
| E5 | E1b〜E3のどれか1つ | Wi-Fi ADBの実機 | T0-b | 任意 |
| E6 | Windowsで、休止の方式が違う機体（S3とModern Standby。`powercfg /a` で分かる） | emulator | T0-b | 任意（(e)だけ） |

- **T0-a（E0・E1）だけでT3b-2に進める**（6.5の10）。E0で確かめるのは、3 OSでAPIが呼べること・単調であること・最小の増分が1ms未満であること（`HostPlatformClockTest`）と、休止・日時の段差・端末の飛び・ADBの詰まり・まとめて届く記録に対する**規則**（`FakeHostClock` と偽のプロセス、固定データ）。E1で確かめるのは、Linuxでの実際の値。
- T0-b（E1b〜E6）は、用意できた環境だけ行う。用意できなかった環境は「未実施」と書く（どのTrailも止めない）。E2・E3の実際の休止は、遅くともT5の受入（A4）で確かめる。
- E4が1台も無いままT0判定に進む場合、ρは「emulatorだけで求めた仮の値」と明記し、確定はオーナーが決める（U1）。


### 7.2 手順

診断を有効にして取得を始め、各項目を実施し、診断記録を集計スクリプトにかける。保存物（`replay.json`）が要る項目では、N=900秒で保存する。

| 項目 | 何をするか | T0-a（E1。子セッション） | T0-b（オーナー。7.6） | 回数 |
| --- | --- | --- | --- | --- |
| (a) 基準 | 負荷なしで30分取得する。途中で5分ごとに保存 | 実施 | E2〜E4 | 各3回 |
| (b) 高負荷 | PCの全コアに負荷をかけて10分。別に、端末に負荷（動画再生＋画面を動かし続ける）をかけて10分 | 実施 | E4（任意でE2・E3） | 各3回ずつ |
| (c) 長時間 | 2時間取得し続ける（途中で端末を充電・負荷で温める） | 実施（emulator） | E4 | 各1回 |
| (d) ADBの詰まり | adbサーバのプロセスを5秒・10秒・20秒止めて再開する（Linux/macOSは `pkill -STOP adb` / `pkill -CONT adb`、Windowsはリソースモニターの「プロセスの中断」）。USBの実機ではケーブルを抜いて10秒後に挿す | 実施 | E4（ケーブル） | 長さごとに5回 |
| (e) PC休止 | 取得中にPCを10秒・60秒・10分休止させ、復帰後60秒取得を続けて保存。Linuxは `sudo rtcwake -m mem -s 秒`、macOSは `pmset sleepnow`、Windowsはスリープと休止状態の両方 | **VMが休止できる場合だけ**（6.4の11）。できなければ未実施 | E2・E3（E6、E1b） | 長さごとに3回 |
| (f) PCの日時の段差 | 取得中にPCの日時を手で+1時間、−1時間。別に、時刻同期をやり直す | VMで日時を変えられる場合だけ | E2・E3（E1b） | 各3回 |
| (g) 端末だけの飛び | 端末の日時を変える。emulatorを一時停止して30秒後に再開（`adb emu avd pause` / `resume`）。emulatorのスナップショットを復元。端末を再起動。画面を消して30分放置 | 実施（emulatorでできるもの） | E4（日時の変更、再起動、画面消灯） | 各3回 |
| (h) まとめて届く記録 | アプリから毎秒約1万行のログを1分出す。動きの多い画面を1分映す | 実施 | E4（任意） | 各3回 |
| (i) 規則の再現（実機なし） | `FakeHostClock`・偽のプロセス・固定データで、PC休止、PC日時の段差、端末だけの飛び、ADBの詰まり、まとめて届く記録（137秒型・49秒型）を入力として与え、診断の行が期待どおりに出ることを検査する（`TimingDiagnosticsTest`） | 実施（E0。3 OSのCI） | — | CIのたび |

- T0-aの(b)(h)で使った負荷のかけ方（コマンドまたは小さなアプリ）は、`docs/verification/time-definition-t0.md` に書き、7.6から参照できるようにする。
- (i)は、各Trailの試験計画（6.5・6.6・6.7の7）にある同じ入力の、診断側の検査である。規則そのものの検査は各Trailで行う。


(e)で記録すること（診断の `"k":"host"` の行と手元の時計から）: 休止の前後での `H`・`A`・`System.nanoTime()`・PCの日時の増分、実際に休止していた長さ（手元の時計）、休止をまたいでADB接続が続いたか切れたか、復帰後に最初の往復測定が成功するまでの時間。

### 7.3 出す値

| # | 値 | 求め方 | 源 |
| --- | --- | --- | --- |
| 1 | 往復時間（`received − sent`）の p50・p95・p99・最大 | サンプルごと | **既存の `timebase.samples` から出せる**（`host_sent_ns`・`host_received_ns`、`SaveWriter.kt:862-867`） |
| 2 | 端末内の読取幅（`after − before`）の分布、2ms超の割合 | 同上 | 既存（`elapsed_before_ns`・`elapsed_after_ns`・`valid`） |
| 3 | 往復40ms超の割合 | 同上 | 既存（`host_anchored`） |
| 4 | 進み方の差（ppm）の上限 | 有効なサンプルの組 `(i, j)`（`i` が先）で、θの範囲を `[lo, hi] = [before − received, after − sent]` とすると、差は `[(lo_j − hi_i) / Δ, (hi_j − lo_i) / Δ]`（`Δ = received_j − received_i`）。`Δ ≥ 10分` かつ `(幅_i + 幅_j) / Δ ≤ 50ppm` の組だけ使い、区間の外側の端の絶対値の最大を `d_max` とする | (a)は既存の保存物（N=900秒なので Δ ≤ 15分）。(c)の2時間の基線は**診断記録が要る** |
| 5 | 3秒を超えた測定の回数、失敗の内訳（3秒超・形式不正・プロセス終了） | `"k":"probe_fail"` | **計測用コードが要る**（現行は残らない、`DeviceCapture.kt:346-353`） |
| 6 | 有効なサンプルが途切れた長さの分布、5秒を超えた回数（1時間あたり） | `"k":"probe"` の受信の間隔 | 診断記録（保存物からは窓の中だけ分かる） |
| 7 | ストリーム別の遅延（受信H − 記録の端末時刻をHに直した値）の p50・p95・p99・最大 | `"k":"rx"` と直近の有効なサンプル | **計測用コードが要る**（行ごとの受信時刻は schema 4 に無い） |
| 8 | まとめて届いた記録の大きさ（100ms以内に届いた記録の端末時刻の広がり）の最大と頻度 | `"k":"rx"` | 計測用コードが要る |
| 9 | 通常の動作中の `H − A` の増分（1時間あたり、最大の1回分） | `"k":"host"` | 計測用コードが要る（T3b-1の取得口） |
| 10 | 休止1回ごとの `H`・`A`・`nanoTime`・日時の増分と、実際の長さとの差 | 同上＋手元の時計 | 実機が要る（T0-b。LinuxはVMが休止できればT0-a） |
| 11 | 日時の段差の前後での `H`・`A`・`H − A` の変化 | 同上 | 実機が要る（T0-b。LinuxはVMで日時を変えられればT0-a） |
| 12 | 休止をまたいだADB接続の状態、復帰後の最初の往復成功までの時間 | `"k":"probe"`・`"k":"gap"` | 実機が要る（T0-b。同上） |
| 13 | 端末の飛びごとに、時計区間の切り替えが検出されたか、検出までの時間 | 保存物の `timebase` と診断記録 | 既存＋診断記録 |
| 14 | `HostClock.read()` 1回の所要時間（平均・p99） | 100万回読む小さな計測 | T3b-1の取得口 |
| 15 | 影の比較（T2の後）: `rowsNewlyOutside`・`rowsNewlyInside`・`fullIntersectionEmpty`・`fullVsMinimalMaxWidthGainNs`・等級ごとの行の割合・保存時間の増分 | T2の診断の行 | T2が要る（**判定はT2の後に、(a)(b)(d)(h)をもう一度実施して取る**） |
| 16 | 1秒周期の診断の行（`"k":"host"`）の間隔の最大値（休止なし） | `"k":"host"` の `h` の差 | T3b-1の取得口。周期の読みの遅れの実測で、4.1の `SUSPEND_SPAN_SLACK`（仮2秒）を確定するのに使う |

### 7.4 判定基準

**ρ（U1）**

| `d_max`（全環境の最大） | 決め方 |
| --- | --- |
| 250ppm以下 | ρ = 500ppm（仮の値をそのまま確定） |
| 250ppmを超え1000ppm以下 | ρ = `d_max × 2` を100ppm単位で切り上げた値 |
| 1000ppmを超える | 停止（7.5） |

- 2倍にする理由: 計測していない端末・温度に対する余裕。この係数は判断であり、根拠となる計測は無い。
- emulatorだけが基準を超えた場合も停止して報告する（emulatorだけ別の値にするかはオーナーが決める。本書はρを1つの定数として書いている）。
- (c)が実施できず基線が15分までしか無い場合、「15分までの基線で求めた」と明記し、確定ではなく仮のままにする。
- T0-b（USB実機、E4）の結果が無い場合、「emulatorだけで求めた」と明記し、確定ではなく仮のままにする（確定するか、T0-bを待つかはオーナー、U1）。T0-bの結果が後から届いたら、同じ表でもう一度判定する。

**PC時計の手段（U3）** — OSごとに、次の1〜6で決める。**3〜6はT0-a（とT3b-1のCI）で、1・2は実際の休止（T0-b、遅くともT5のA4）で確かめる。**

| # | 条件 | 確かめる場 |
| --- | --- | --- |
| 1 | (e)の全回で、`H` の増分と実際の経過（手元の時計）の差が1秒以内 | 実際の休止: LinuxはT0-a（VMが休止できれば）またはT0-b、macOS・WindowsはT0-b。遅くともT5のA4 |
| 2 | (e)の全回で、検出された休止の長さ（`H − A` の増分）と実際に休止していた長さの差が2秒以内 | 同上 |
| 3 | (f)の全回で、`H`・`A` が段差の影響を受けない（`H − A` の変化がしきい値未満で、休止として検出されない） | LinuxはT0-a（できなければT0-b）。macOS・WindowsはT0-b。3 OS共通の規則としては、CIで `FakeHostClock` の日時だけを動かして検査する |
| 4 | (a)(b)で、休止が1回も検出されない | T0-a（Linux）。macOS・Windowsは、CIの `HostPlatformClockTest` の(c)（`H − A` の変化が50ms未満）で代える |
| 5 | #14 の平均が1µs以下、p99が20µs以下（受信のたびに読むため） | T3b-1のCI（3 OSのランナーで測って記録に出す）とT0-a（Linux） |
| 6 | #16 の最大値が2秒以下。満たせば `SUSPEND_SPAN_SLACK = 2秒` で確定する。超えたら、最大値を0.5秒単位で切り上げた値を案として報告する（5秒を超えるなら停止、7.5） | T0-a（Linux）。T0-bの結果があれば加える |

- **T3b-2の着手条件**は「T3b-1のCIで3 OSのAPIが動いている（`HostPlatformClockTest` が3 OSで通る）」＋「T0-aで3〜6を満たす」（6.5の10）。1・2は着手条件にしない。
- **休止検出のしきい値**: #9 の「最大の1回分」の10倍と、100msの大きい方。ただし1秒を超えるなら停止（7.5）。
- 3〜6のどれかを満たさないOSは、原因を書いて停止する（5は下の代案を添えて報告する）。1・2を満たさないことが後から（T0-b・T5で）分かったOSは、その時点で停止して報告する（7.5のS5・S6）。**黙って退避（`nanotime_fallback`）に落とさない。** そのOSを退避で出すかどうかはU4（オーナー）。
- あわせて、OSごとに「`System.nanoTime()` が休止中に進むか」を表に書く（退避時に窓がどうなるかの説明に使う。判定には使わない）。


**5（所要時間）を満たさなかったときの代案** — OSごとに判断する。**子セッションは切り替えずに停止し、数値を報告する。** 採用は親セッションとオーナーが決める。

- まず切り分ける。#14 を「時計のAPIだけ（`read()`）」と「`HostTime.now()` 全体」に分けて測り、(i) 直接マッピングになっているか、(ii) `read()` のたびにメモリを確保していないか、を確かめる（6.3の「呼び出し方」）。Windowsの `QueryInterruptTimePrecise` は刻み単位の版より遅いと見込まれるが、所要時間は未計測。
- 基準の意味: 読む回数が多いのはログのまとめ配送だけである（毎秒約1万行で、1回1µsなら1コアの約1%、5µsなら約5%）。動画・音声・タップ・往復測定は合わせても毎秒数百回以下。**基準値（1µs・20µs）は本書が置いた判断で、根拠となる計測は無い。**
- **代案1（意味を変えない。先に検討する）: 既に届いている記録は同じHを共有する。** ログの読み取り（`DeviceCapture.kt:312` の周り）で、1件読み終えたら「入力にまだ未読のデータがあるか」（`available() > 0`）を**先に**確かめ、その**後で**Hを読む。未読があったなら、次の1件はそのHをそのまま使う（読み直さない）。次の記録の先頭はそのHを読む前にPCに届いていたので、「記録はHより後には起きていない」は保たれる。読む回数は「まとめて届いた塊の数」まで減る。Hは `hostTime.now()` だけから得るので、混ぜない規則（D13）に触れない。変わるのは6.5の「1件につき1回読む」の1文だけ。
- **代案2: 補間する時計。** `HostClock` の実装を `InterpolatedHostClock(native)` に替える。内部に基準点 `(n0, h0, a0)`（`n0` は `System.nanoTime()`、`h0`・`a0` はネイティブの組）を持ち、`read()` は `d = System.nanoTime() − n0` として `HostReading(h0 + d, a0 + d)` を返す。基準点は、250ms（仮）ごとの周期の読み・保存要求のS・往復測定の送信と受信、の時点でネイティブの組を読んで更新し、`H − A` の増分で休止を検出する（4.1と同じ式）。返す `h` は前回より小さくしない。
  - **混ぜない規則（D13）との関係**: `System.nanoTime()` を使うのは `HostClock` の実装の内側だけで、外へ出る値はHである。呼び出し側は今までどおり `hostTime.now()` だけを使い、Hと `System.nanoTime()` を同じ式に入れない。位置づけは退避の `NanoTimeHostClock`（6.3）と同じ。`TimeAxisSourceTest`（6.5）には「時計の実装」のファイルとして1つ足すだけで、3.1・3.2の分類は変わらない。
  - **代償（採用には4.1と6.5の改訂が要る）**: 休止をまたいだ2回のネイティブの読みの間（起きていた時間で最大250ms）に受信した記録は、補間したHを持つ。`System.nanoTime()` が休止中に進むOSと進まないOSで、そのHは休止の区間 `[fromH, toH)` の中の違う位置になる。したがって「休止の区間にHを付けられた記録は無い」（4.1、6.5の5の(c)）は成り立たなくなり、代わりに「**休止の区間の中のHを持つ記録は、上下限の制約にも受信（4.5）にも使わない**」という規則が要る。これで3 OSの結果は揃う（`FakeHostClock` に「nanoTimeが休止中に進む／進まない」の2通りを足して検査する）。休止の区間そのもの（I3・I3b）は変わらない。
- 代案1でも2でも足りない、またはどちらも採らない場合、そのOSの扱いはU4（オーナー）と同じ場で決める。黙って退避（`nanotime_fallback`）や刻み単位の時計に落とさない。

**片方向制約の全面採用（U2）** — T2の影の記録（#15）で決める。

| 結果 | 決定 |
| --- | --- |
| 既知の不連続（端末の飛び・PC休止・再起動）を含まない計測で `fullIntersectionEmpty > 0` | 停止（7.5）。前提（ρ、または「記録は受信より後には起きていない」）が破れている |
| 上が0回で、往復が使えない間（最後の有効なサンプルから5秒超）の保存のうち、全面採用で誤差幅が半分以下になるものが2割以上あり、保存時間の増分が5%未満 | 全面採用する。本書の4.3・4.4を改訂してからT4bに着手する |
| それ以外 | 採用しない（影のまま。影のコードを残すか消すかは親セッションが決める） |

- 「誤差幅が半分以下」は、保存ごとの影の記録で `fullVsMinimalMaxWidthGainNs ≥ (tSHi − tSLo) / 2` と判定する（`tSHi − tSLo` は必須の形での終点の幅、6.6の `ShadowWindow`）。終点の等級が `measured` の保存は母数に入れない。
- 2割・5%という基準値は本書が置いた判断で、根拠となる計測は無い。
- 破断の検出（上下限が交わらなくなること）については、(g)の各回で「交差が空になった時点」と「実際に飛びを起こした時点」が合っていたかを表にして報告する。必須に上げるかどうかは、この表を見てオーナーが決める（本計画の範囲では影のまま）。

**時計helperの自走（U11）**

- #6 の「5秒を超えた回数」が (b)(d) で0なら、T3aは効果が小さいと報告する（実施するかは親セッション・オーナー）。
- 自走の間隔は1秒で確定する（現行の往復の間隔と同じ、`DeviceCapture.kt:354-355`。5秒の規則に対して4回分の余裕）。端末の負荷への影響はT3aの受入で確かめる。

### 7.5 停止して報告する値

次のどれかに当てはまったら、後続のTrailに進まず親セッションへ報告する。

| # | 条件 | 崩れる前提 |
| --- | --- | --- |
| S1 | `d_max > 1000ppm`（ρが仮の値の4倍を超える） | 境界の余裕 `ρ×W` が900秒の窓で1.8秒を超え、所属の判定が粗くなる |
| S2 | (a)で、読取幅が2msを超えるサンプルが半分以上 | 現行の `valid` の条件（`CaptureClock.kt:22`）が実環境で成り立たない |
| S3 | (a)で、往復時間のp50が40msを超える | 現行の `hostAnchored` の条件（`:23`）が成り立たない |
| S4 | (a)(b)で休止が誤って検出される、またはしきい値が1秒を超える | PC休止の検出 |
| S5 | あるOSで、候補の「休止を含む時計」が休止を含まない、または候補のAPIが呼べない | U3・U4。「APIが呼べない」はT3b-1のCIで、「休止を含まない」は実際の休止（T0-a・T0-b・T5のA4）で分かる |
| S6 | 休止をまたいでADB接続が続き、かつ休止が検出されない組み合わせがある | 「休止を確認済みに数えない」（I3）。実際の休止（T0-a・T0-b・T5のA4）で分かる |
| S7 | (a)で、動画の遅延（#7）のp50が1秒を超える | 末尾未確認が常に1秒を超え、表示（U7）の意味が変わる |
| S8 | 既知の不連続なしで `fullIntersectionEmpty > 0`、または `rowsNewlyInside > 0` | 上下限の前提、または137秒型・49秒型の再発 |

---

### 7.6 T0-bの手順書（オーナー向け）

この節だけを見て実行できるように書いた。**コマンドは本書の時点では実行して確かめていない。** 診断を有効にする方法と診断記録の場所は、T0-aが実物に合わせてこの節を書き直す（6.4の2・8の(e)）。T0-aが済む前は実行できない。

**用意するもの**

- T0-aのCIが出した診断入りのプラグインZIP（Android Studioに導入する）。
- Android Studio と、開いておくproject（何でもよい）。emulator 1台（USB実機の項目ではUSB接続の実機）。
- リポジトリの `scripts/analysis/timing_report.py` と Python 3。
- 手元の時計（スマホのストップウォッチ）。
- `replay` コマンドの場所（Settings > Tools > Android Replay Buffer の「CLIの場所」。`docs/cli.md`）。以下では `replay`、projectのフォルダを `P` と書く。

**手順1: 準備（1回だけ）**

1. 診断を有効にしてAndroid Studioを起動する（環境変数 `REPLAY_TIMING_DIAGNOSTICS=1` をIDEのプロセスに渡す）。

   | OS | やり方（案） |
   | --- | --- |
   | macOS | ターミナルで `launchctl setenv REPLAY_TIMING_DIAGNOSTICS 1` を実行してから、Android Studioを起動し直す |
   | Windows | PowerShellで `setx REPLAY_TIMING_DIAGNOSTICS 1` を実行してから、Android Studioを起動し直す |
   | Linux | ターミナルで `REPLAY_TIMING_DIAGNOSTICS=1 <Android Studioの場所>/bin/studio.sh` |

2. Windowsだけ: `powercfg /a` を実行し、使えるスリープの種類（「スタンバイ (S3)」か「スタンバイ (S0 低電力アイドル)」＝Modern Standby）を記録する。
3. `replay --project P settings set replay_seconds=900`
4. `replay --project P start` を実行し、`replay --project P status` で取得中であることを確かめる。

**手順2: 基準（10分）** — 何も触らずに10分待ち、`replay --project P save` を実行する。

**手順3: PC休止（60秒を3回。最小の組）**

1. ストップウォッチを用意する。PCを休止させると同時に計り始める。

   | OS | 休止のさせ方 |
   | --- | --- |
   | macOS | `pmset sleepnow`（または蓋を閉じる） |
   | Windows | スタートメニュー → 電源 → スリープ。休止状態は `shutdown /h`（別の回として記録する） |
   | Linux | `sudo rtcwake -m mem -s 60`（60秒後に自動で復帰）、または `systemctl suspend` |

2. 約60秒後にキーを押して復帰させ、復帰した時点のストップウォッチの値を記録する（＝実際の休止の長さ）。
3. 復帰後、何もせずに60秒待つ。
4. `replay --project P save` を実行し、出力されたフォルダのパスを控える。
5. 1〜4を3回くり返す。余裕があれば10秒と10分でも3回ずつ行う。

**手順4: PCの日時の段差（各1回。最小の組）** — 取得を続けたまま、日時を+1時間にして30秒待ち、−1時間に戻して30秒待ち、時刻同期をやり直す。最後に `replay --project P save`。

| OS | +1時間 | −1時間 | 時刻同期をやり直す |
| --- | --- | --- | --- |
| macOS | `sudo systemsetup -setusingnetworktime off` の後、`sudo date -v+1H` | `sudo date -v-1H` | `sudo systemsetup -setusingnetworktime on` |
| Windows（管理者のPowerShell） | `Set-Date -Adjust 01:00:00` | `Set-Date -Adjust -01:00:00` | `w32tm /resync` |
| Linux | `sudo timedatectl set-ntp false` の後、`sudo date -s '+1 hour'` | `sudo date -s '-1 hour'` | `sudo timedatectl set-ntp true` |

**手順5: 集計** — `python3 scripts/analysis/timing_report.py --diagnostics <診断記録の場所> --saves <手順2〜4で控えたフォルダ>`（引数の名前は案）。出力の表のうち、下の「記録する表」に当たる行を書き写す。

**手順6: 後始末** — `replay --project P stop`。環境変数を消す（macOS: `launchctl unsetenv REPLAY_TIMING_DIAGNOSTICS`、Windows: `reg delete HKCU\Environment /v REPLAY_TIMING_DIAGNOSTICS /f`）。Android Studioを起動し直す。日時が正しいことを確かめる。

**最小の組**は手順1〜6（1 OSあたり約40分）。T5のA4に必要なのは手順3だけである。

**追加の項目（任意。7.2の欄のとおり）**

| 項目 | やること | 時間の目安 |
| --- | --- | --- |
| (a) 基準の全量 | 30分の取得を3回。5分ごとに `save` | 90分 |
| (c) 長時間（USB実機） | 2時間取得し続ける。途中で端末を充電し、動画を再生して温める。最後に `save` | 2時間 |
| (d) ADBの詰まり | macOS/Linux: `pkill -STOP adb; sleep 5; pkill -CONT adb`（5・10・20秒で各5回）。Windows: リソースモニターで `adb.exe` を「プロセスの中断」→「再開」。USB実機: ケーブルを抜いて10秒後に挿す | 20分 |
| (g) 端末だけの飛び | 端末の設定で日時を手で+1時間にして戻す。`adb reboot`。画面を消して30分放置 | 45分 |
| (b)(h) 負荷 | T0-aが `docs/verification/time-definition-t0.md` に書いたやり方で行う | 各30分 |

**記録する表**（OS・機体ごとに1枚。数値だけ。録画・ログ・診断記録そのものは渡さない）

| 項目 | 値 |
| --- | --- |
| OSと版、機体、スリープの種類（Windowsは `powercfg /a` の結果） | |
| 端末（emulator／USB実機の機種とAndroidの版） | |
| 使ったPC時計の手段（診断の `method`） | |
| 基準10分で検出された休止の回数（0のはず） | |
| 基準10分での `H − A` の増分の最大（#9）、診断の行の間隔の最大（#16） | |
| `HostClock.read()` の所要時間の平均・p99（#14） | |
| 休止ごと（回ごとに1行）: 実際の休止の長さ（ストップウォッチ）／`H` の増分／`A` の増分／`System.nanoTime()` の増分／日時の増分／検出された休止の長さ（#10） | |
| 休止ごと: ADB接続が続いたか切れたか／復帰後、最初の往復測定が成功するまでの秒数（#12） | |
| 休止ごと（T3b-2の後の版だけ）: 保存した `replay.json` の `gaps` に理由「PC休止」があったか、その長さ | |
| 日時の段差: +1時間・−1時間・時刻同期のそれぞれで、休止として検出されたか（されないはず）、`H − A` の変化（#11） | |
| 実施しなかった項目と理由 | |

**結果の見方と連絡**

- 次のどれかに当たったら、その行を添えて親セッションに伝える（7.5のS5・S6・S4）: 休止したのに `H` の増分が実際の休止より1秒以上短い／休止が検出されない／日時の段差や基準の10分で休止が検出された。
- 当たらなければ、表をそのまま親セッションに渡す。親セッションが `docs/verification/time-definition-t0.md` の「T0-b」の欄に書く。

---


## 8. 費用の比較

**前提**: 工数はDevinのセッション数で書く（1 Trail = 1セッションが基本）。数字は6章の行数の目安から出した見積りで、実測ではない。オーナーの実機作業（T0-b、T5のA4）はセッション数に含めていない（作業時間の目安は6.4bの12と6.10の12）。

- **(A)**: このセッション（設計を書いたセッション）が実装する。
- **(B)**: 別の子セッションが、方針書と本書から実装する。

### 8.1 Trailごとの見積り

| Trail | 行数の目安（本番/テスト/文書ほか） | 難しさ | (A) | (B) | 推奨 |
| --- | --- | --- | --- | --- | --- |
| T1 | 0 / 350 / 200 | 低 | 0.5 | 0.5〜1 | どちらでもよい（下の注） |
| T3b-1 | 220 / 250 / 0 | 中 | 1 | 1 | (B) |
| T0-a（計測のコードと、Linux VMでの計測） | 250 / 200 / 450 | 中 | 1 | 1 | (B) |
| T0-b（オーナーの実機作業） | 0 / 0 / 0 | 低 | — | — | オーナー（セッションは使わない。6.4bの12） |
| T3b-2 | 400 / 900 / 0 | 中〜高 | 1〜1.5 | 1〜1.5 | (B) |
| T2 | 700 / 1300 / 0 | 高 | 1.5〜2 | 2〜3 | (B)＋設計側のレビュー |
| T3a | 350 / 700 / 0 | 高 | 1〜1.5 | 1〜1.5 | (B) |
| T4a | 300 / 600 / 固定データ5組 | 中 | 1 | 1 | (B) |
| T4b | 900 / 1500 / 300 | 高 | 2 | 2〜3 | (B)＋設計側のレビュー |
| T5 | 0 / 100 / 450 | 低 | 0.5 | 0.5 | (B)（A4のmacOS・Windowsはオーナーの実機） |
| 合計 | 約3100 / 5900 / 1400 | | 9.5〜11 | 10〜13.5 | |

TRは本書の範囲外なので含めていない。

### 8.2 危険とレビューの負担

| 観点 | (A) このセッション | (B) 子セッション |
| --- | --- | --- |
| 読む文書の量 | 追加なし。ただし行番号は #460・TR の後に引き直す（(B)と同じ） | Trailごとに `AGENTS.md`、方針書4章（約180行）、本書0〜5章（約420行）、担当の節（30〜110行）。合計で約650〜750行 |
| 引き継ぎで失われる文脈 | 少ない。ただしこのセッションは既に長く、古い調査の細部は要約とメモ（方針書・本書）に置き換わっている。**実質的に頼れるのは文書に書いたことだけ**で、(B)との差は見かけより小さい | 不採用にした案の理由（方針書3章）、レビューでの議論（方針書8章）。本書に書いていない判断は伝わらない |
| 誤実装しやすい箇所 | 自分の書いた式の誤りに気づきにくい（書いた本人が読むので、曖昧さが表に出ない） | (1) HとAと `System.nanoTime()` を1つの式で混ぜる（3章）。(2) PC休止で `startRegion()` を呼んでしまう（4.3）。(3) 5秒・20msの境界の `>` と `≥`（4.4）。(4) 3区分で窓の外の受信を捨ててしまう（4.5の2）。(5) T2で既存の行を書き換えてしまう（6.6）。(6) schema 4 のトリムの式を「ついでに」直す（6.8）。いずれもテスト名を本書に挙げてある |
| 並行 | できない（1セッションが順に進める） | 「TR → T4a」と「T3b-1 → T0-a → T3b-2 → T2」を並行できる（5章） |
| レビューの負担 | 差分と本書の突き合わせは誰かが要る。設計者と実装者が同じなので、本書の誤りはレビューでしか見つからない | 子セッションが本書を読んで実装すること自体が、本書の検査になる。曖昧な箇所は停止条件（0章の6）で早く表に出る。差分と本書の突き合わせは、設計側のセッションかCodexが行う |
| 運用との整合 | `AGENTS.md` の運用（子セッションが実装・テスト・push、親が `entire trail`）から外れる。このセッションは親（ARB）ではないので、Trailの操作は別に親が行う | 運用どおり |
| 失敗したときの損 | セッションが長くなり、途中で文脈がさらに要約される。T4bのような大きいTrailで起きると手戻りが大きい | 1 Trail分で済む。やり直しは新しい子セッションに同じ文書を渡せばよい |

### 8.3 推奨

**(B) を基本にする。** 理由は3つ。

1. 本書を読んだだけで実装できるかどうかを、T1・T3b-1 という出力の変わらない小さなTrailで先に試せる。本書に穴があれば、危険の小さい段階で見つかる。
2. 並行できる分、全体の日数が短い。セッション数の合計は(A)より1〜2割多い見積りだが、差の大半は読む時間で、T2・T4bに集中している。
3. このセッションの優位（読んだコードの記憶）は、長いセッションの要約で既に薄れている。残っているのは文書に書いた内容で、それは子セッションにも渡る。

**T2 と T4b は、(B) に加えて設計側のレビューを入れる。** 式の境界と既存の確定条件との一致（I6）が要点で、誤りが出力に出るのはT4bだけだからである。レビューでは、差分を本書の4章・6.6・6.9と突き合わせ、6章の不変条件が実際にテストで検査されているかを見る。設計側は、このセッションを続けて使っても、本書を読ませた新しいセッションでもよい（上の3の理由で差は小さい）。

**T1 は (A) でも (B) でもよい。** 中身は方針書4章を `docs/design/timeline.md` に写し、現行の動作を固定するテストを足すことで、引き継ぎの損がほとんど無い。(B)で始めれば、上の1の「本書の試し」を兼ねる。

**全部を (A) にする案は勧めない。** 並行できず、1つの長いセッションに失敗の危険が集まり、本書の誤りを見つける独立の読み手がいなくなる。

---

## 9. 未計測・未検証の一覧

方針書9章と同じ内容に、本書で増えた項目を足した。

**未計測**（T0-a・T0-bで測る。7.3の番号）

| 項目 | 測り方 |
| --- | --- |
| 往復時間と端末内の読取幅の分布 | #1〜#3。既存の保存物から出せる |
| 端末とPCの進み方の差 | #4 |
| 測定失敗の頻度と内訳（3秒超を含む） | #5・#6。計測用コードが要る |
| ストリーム別の遅延と揺らぎ、動画・音声のencoder遅延の実機での値 | #7。計測用コードが要る |
| logcatのまとめ配送の大きさと頻度 | #8 |
| 通常の動作中の `H − A` の揺らぎ（休止検出のしきい値100msの根拠） | #9 |
| `HostClock.read()` の所要時間 | #14 |
| 取得中の周期の読み（1秒ごと）の遅れの最大（`SUSPEND_SPAN_SLACK` の根拠） | #16 |
| Windowsの `QueryInterruptTimePrecise` の所要時間（刻み単位の版より遅いと見込まれる） | #14（T3b-1のCIのWindowsのランナー） |
| 時計helperが応答しない間の、動画・音声・タップ系とログ系のずれ | #6・#13 |
| emulatorの一時停止・スナップショット復元の影響 | 7.2の(g) |
| 影の計算が保存時間に与える影響 | #15 |
| 時計helperの自走が端末の負荷に与える影響 | T3aの受入 |

**未検証**

| 項目 | いつ確かめるか |
| --- | --- |
| `System.nanoTime()` が休止中に進むかどうか（3 OS） | (e)。LinuxはT0-a（VMが休止できれば）、macOS・WindowsはT0-b |
| 6.3の表のOSのAPI（定数の値、DLL名、単位、JNAの宣言）が実際に呼べ、期待どおりの値を返すこと。**とくにWindowsの `QueryInterruptTimePrecise` / `QueryUnbiasedInterruptTimePrecise` のJNAの宣言、輸出しているDLL、単位（100ns）** | T3b-1のCI（3 OS）。呼べなければ停止 |
| 3 OSで、連続して読んだときの最小の増分が1ms未満であること | T3b-1のCI（`HostPlatformClockTest`） |
| IDEに同梱のJNAの版で、直接マッピング（`Native.register`）が使えること | T3b-1 |
| IDEに同梱のJNAの版で、6.3の宣言が通ること | T3b-1 |
| 休止を含む時計が、実際の休止（Windowsのスリープ・休止状態・Modern Standby、macOSの蓋閉じ、Linuxのsuspend）で休止を含むこと | T0-b、遅くともT5のA4（Linuxは、VMが休止できればT0-a） |
| 休止をまたいだときのADB接続の挙動 | T0-b（同上） |
| 退避時の休止の検出（方針書4.6の(b)）が実際の休止で働くこと | T0-b（退避を強制して実施。任意） |
| 「`measured` の行の集合＝現行で `t_ns` が入る行の集合」（I6）が全テスト入力で成り立つこと | T2 |
| 4.4の等級の条件と現行コードの対応（コードを読んで確かめたが、実行はしていない） | T2 |
| 4.4の「`arrival` では `t ≤ W`」の証明が、ρの切り上げ・系ごとの最小形を含めて成り立つこと | T2（`arrivalNeverExceedsWindowEnd`） |
| 時計helperの自走で、往復の応答と自走の行が混ざらずに読めること | T3a |
| 子セッションのクラウドのLinux VMで、休止（`rtcwake`）と日時の変更ができるか | T0-a（できなければ未実施として進める） |
| 生存監視の `LIVENESS_TIMEOUT = 10秒` が、正常な負荷のもとでhelperを止めないこと | T3a（7.2の(b)） |
| 7.6の手順書のコマンド（環境変数をIDEに渡す方法、休止・日時の変更のコマンド） | T0-a（Linux VMで1回通す）。macOS・Windowsの分はオーナーが最初に実行するとき |
| 7.4の代案1・2（所要時間の基準を満たさなかったとき）が実際に基準を満たすか | 採用を検討する時点（親セッション・オーナー） |
| U13が(a)のとき、PC休止のgapを次の往復成功で閉じる処理が、既存の時計のgapの閉じ方（`CaptureStore.kt:344-348`）と干渉しないこと。休止をまたいだ schema 4 の保存物の `availableSeconds`・MP4の区切りが6.5の4のとおりになること | T3b-2（`HostSuspendGapTest`）。実機ではT0-b・T5のA4 |
| 端末側のlogcatの欠落が実際にどの程度起きるか | 本計画の外（U9） |
| リポジトリ外に schema 4 を読む利用者がいるか | オーナー（U10） |
| FFM（`java.lang.foreign`）がIDE上で許可なしに使えるか | 未決（U8）。本計画では使わない |
| 本書の型・疑似コード・JSONの例（コンパイルも実行もしていない） | 各Trail |
| 本書の行数・コミット数・セッション数の見積り（根拠は行数の目安だけ） | — |
| #460 を入れた後、TRの後の行番号（本書の行番号は develop `c0d4336` と #460 `59cc95f` のもの） | 各Trailの着手時に引き直す |
| 既存テストの本文すべて、PR #459/#460 の本文すべて（一部だけ読んだ） | — |
