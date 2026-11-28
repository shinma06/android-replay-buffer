---
id: SPEC-07
type: spec
name: 端末選択・音声・ジェスチャー・クラッシュの観測契約
satisfies:
  - REQ-01
  - REQ-02
  - REQ-03
  - REQ-08
  - REQ-09
  - REQ-12
  - REQ-13
  - REQ-14
---

# SPEC-07 端末選択・音声・ジェスチャー・クラッシュの観測契約

明示した serial は接続前の一時ターゲットで、`config.json`・取得要求・稼働中の世代を変えない。その端末が `device` 状態で選ばれ取得を開始できた時点で、`capture_enabled=true` と `device_serial` を 1 回の原子的書き込みで確定する。期限（`wait_connected=false` は即時、true は `wait_timeout_ms`）までに選べなければ `DEVICE_NOT_FOUND`（`details` に `serial`、`selection`、空の `config_changed`、`wait_timed_out`）を返し、設定は変えない。オフラインの端末を事前に録画対象として予約するには `config.update` の `device_serial`（MCP は `arb_config_update`）を使う。録画中は同じ serial（または省略）なら既存の取得を止めない冪等な成功（`already_active: true`）、別の serial は `CONFLICT`（`details` に `active_serial`・`requested_serial`）で既存の取得と設定を変えない。自動接続の探索や別の開始が接続予約を持つ間、開始は予約の解放を待って順番に判定し、黙って上書き・取消しない（5 秒または接続待ちの期限を過ぎれば再試行可能な `CONFLICT`）。停止（`capture.stop`、端末の再選択、容量不足による解除）は、それより前に受け付けた待機中・順番待ちの開始をすべて取り消し（`CONFLICT`）、録画を再開させない。接続待ちの期限は開始が接続予約を得た時点から数え、`wait_timeout_ms=0` を含め ADB の応答停止で期限を越えて応答を保留しない。serial を省略した開始は従来どおり録画の意図を先に保存し、端末を待つ間は `warnings` に `waiting_for_device` を返す。serial を省略した開始および停止は設定済み serial を保持する。再起動後の自動接続は永続化した組に従う。状態の `configured_device_serial` は設定値、`serial_mismatch` は録画接続中の実 serial と設定済み serial がともに存在して異なる場合に真。

## Android と端末選択 [実装済み]

ADB の通常接続で映像・ログ・入力を取得し、root・対象アプリの改修・常駐 APK を要求しない。端末にはセッション用コンパニオンと scrcpy-server を転送し `app_process` で起動する。scrcpy は H.264、B フレームなし・1 秒 GOP を要求する。ビーコンの既定周期は 300 ms。静止による同一フレーム保持とストリームの停止・切断を区別する。[端末プロトコル](../../device-agent/PROTOCOL.md)・[ADB 実装](../../core/crates/arb-adb/src/adb.rs)

| Android API | コア取得の保証 | 既定 `playback_dup` | 明示 `output_compat` |
| --- | --- | --- | --- |
| 27 以下 | サポート外。非ブロッキング警告、ベストエフォート | 無効 | 無効 |
| 28〜29 | Android 9 から映像・ログ等をサポート | 音声無効 | 音声無効 |
| 30〜32 | コアをサポート | 音声無効 | 試行可能、取得中は端末出力が消音 |
| 33 以上 | コアをサポート。音声推奨環境 | スピーカーを維持して音声を試行 | 消音を明示した上で試行可能 |
| API 不明 | 有効な情報だけで状態判定 | 選択方式で試行 | 選択方式で試行 |

有効な ADB serial のみを候補とする。`selectable_online_serials` は `device` 状態だけを採用する。USB、エミュレータ、Wi-Fi のホスト・ポート、IPv6、mDNS を許し、空文字、255 byte 超、不正文字、パストラバーサル等を拒否する。未選択で 0 台は待機、1 台は自動選択、複数は明示選択待ち。指定済み serial が不在なら他に 1 台だけいても切り替えてはならない。明示した対象が戻れば同じものへ接続する。切断時は再試行を行い、標準バックオフは 200、500、1,000、2,000 ms。復帰時間には端末の応答時間も含まれる。[端末選択表](device-selection-policy-table.json)・[Rust の選択と検証](../../core/crates/arb-core/src/device.rs)

## 音声の状態遷移 [実装済み]

`audio_capture_mode` の省略は `playback_dup`。対応 API で `audio_source=playback audio_dup=true` を渡す。`output_compat` は明示選択時だけ `audio_source=output` を渡し `audio_dup` を省く。方式の自動切替は禁止。ログ単体モードまたはユーザーが音声を無効にした場合は音声取得を開始しない。録画中の方式変更は現在の接続を終え、新しい接続で再試行する。

| 観測した条件 | `audio_state`／`audio_cause` と次の動作 |
| --- | --- |
| 音声無効・ログ単体 | `disabled`、`user_disabled` または `logs_only` |
| 既知 API が選択方式の下限未満 | `unavailable`／`api_below_min`。「非対応」と表示できる唯一の原因 |
| 開始前・AAC 確認 | 試行中は `unknown`、AAC ヘッダを確認すれば `available` |
| `codec_id=0` | `device_capture_failed`。音声キャプチャを開始できない非致命経路。映像・ログは継続 |
| `codec_id=1` | `encoder_fatal`。scrcpy の致命エラー。同じ取得セッション・serial・boot_id の再接続は音声を止めて映像・ログを復旧する |
| 音声あり起動だけ失敗 | `launch_failed_audio_true`。音声なしで起動できた場合は同じ組の再接続で `runtime_sticky_disabled` |
| ハンドシェイク・トンネル・両起動が失敗 | `handshake_io`、`tunnel_connect_failed`、`launch_failed_both` を区別。OS の世代不足を推測しない |

再接続時の音声抑止は方式変更、停止→開始、別端末、別 boot で解除する。IDE は `audio_reason` と取得できた stderr の末尾（理由へ含める上限は 600 byte）を詳細として表示する。原因のない旧応答は原因不明の実行時失敗と扱う。API 33 以上でも、実機・OEM・同時ミラーリングにより音声取得は失敗し得る [既知制約 / TCB-17]。[音声方針 86 行](audio-policy-table.json)・[音声ポリシー](../../core/crates/arb-core/src/audio_policy.rs)・[実 ADB セッション](../../core/crates/arbd/src/live_adb.rs)

## タップ・スワイプ・焼き込み [実装済み]

端末の `show_touches` 設定を変更してはならない。コンパニオンは `getevent -lt` でカーネルの入力を読み、ホストは入力デバイスごとの `getevent -lp`（プロパティ表示を含む）の最小値・最大値を用いて物理画面のピクセルへ変換する。軸／寸法が取得できない場合は生座標を利用する場合があり、映像に対応付けられない位置にマーカーを作らない。旧記録で両寸法が欠ける場合は両軸とも 0〜1 の正規化座標だけを映像に対応付ける。API 37 エミュレータでも、校正済みの軸の宣言範囲を超えた入力は最寄りの端に制限してから画面へ写す。校正前の現行形式の保存物は、正の画面寸法があれば読取時に始点・終点・全軌跡点を `[0, width−1] × [0, height−1]` に制限する。元ファイルは不変で、概要、クロップ、Studio、焼き込みが同じ座標を使う。[読取時の座標処理](../../core/crates/arb-storage/src/bundle_io.rs)・[校正](../../core/crates/arbd/src/touch_calibration.rs)・[端末入力ワーカー](../../device-agent/companion/src/io/github/shinma06/arb/agent/InputWorker.java)

指の始点からの最大移動量または終点までの距離が閾値以上なら Swipe。閾値は短辺の 2.5% と 20 px の大きい方、寸法不明時は 24。1 点以下は Tap。Swipe は始点・終点を保持し最大 24 点へ間引く。Studio オーバーレイと共有 MP4 の焼き込みは同じ保存座標を使う。画素の描画はホストのメディア経路で行い、端末にタップ跡を強制表示しない。[K6／K7 の対象](../../core/crates/arb-core/src/gesture.rs)・[VER-27](../verifications/VER-27-t6i-running-devices-and-burn-taps.md)

[観測制約 / TCB-08] `adb shell input tap` は Android Framework へ注入する。エミュレータの `emu event mouse` で位置を試すときは画面内座標を指定する。カーネル入力デバイスから `getevent` が受け取った操作だけがこの記録の対象で、注入で UI が変化してもタップ件数が増える保証はない。実機の物理タッチやエミュレータ画面の入力を使い、イベントの有無と描画位置を別々に照合する。

## ログ・クラッシュ・画面遷移 [実装済み]

- 端末でのログ読取時刻とログヘッダの時刻を区別する。logcat 統計から欠落を裏付けられないときの完全性は `unverified`。ドロップを検知した場合や境界ドレインのタイムアウトは保存結果・マニフェストの証拠を参照する。[ワイヤ](../../core/crates/arb-adb/src/wire.rs)・[保存の証跡](SPEC-02-time-selection-bundle.md)
- `am_crash` 自動保存の入力はバイナリのシステムイベントかつ UID 1000 を要求する。アプリの同名テキストログでは起動しない。設定した自動保存クールダウンの既定は 15 秒。保存ジョブと pin は永続化する。[クラッシュ検出と保存](../../core/crates/arbd/src/capture.rs)
- [既知制約 / TCB-16] クラッシュ ID の重複抑止はデーモン再起動をまたいで永続化されない。同じイベントが再配信されれば二重保存が起こり得る。S5 の再起動後の一意性は設計前提である。[TCB 台帳](../proofs/tcb-axioms.md)
- クラッシュのスタック（`stack_top`・`first_app_frame`・`caused_by`）は、クラッシュした PID 自身が書いた `AndroidRuntime` の `FATAL EXCEPTION` ブロック（同じ boot・前後 64 行・±2 秒）からだけ読取時に導く。別 PID の行やアプリ ID と一致しない Java パッケージの行をアプリ行として作らない。[抽出](../../core/crates/arb-core/src/analytics/crash_detail.rs)
- クラッシュ以外のプロセス終了は、logd が付けた UID でシステムの記録だけを採る。`ActivityManager` のテキストは UID 0 か 1000、`lowmemorykiller`／`lmkd` はそれに加えて lmkd の UID 1069、events の `am_kill`・`am_proc_died`・`am_anr` は UID 0・1000 または未報告。アプリ UID（10xxx）の同名ログは採らない。`Force stopping <pkg>` と後続 16 行以内の `Killing … stop <pkg>`（`am_kill` を含む）は 1 件の `force_stop` に統合し最初の PID を採る。統合した kill と同じ PID・boot の ±5 秒以内の後続の kill 報告（16 行より後の `am_kill` など）も同じ kill として数えない。同じ種類・PID の重複報告（`am_kill` と `Killing`、`am_anr` と `ANR in` など）は ±5 秒で 1 件にする。クラッシュまたは kill（force-stop に統合した全プロセスの kill を含む）と同じ PID・boot の ±5 秒以内の `died` はその結果として出さない。ANR はプロセスを終了させないので、この抑止の原因に数えない。LMK の理由（`lmk`・low memory）の `am_kill` は `lmk_kill`、それ以外は `killed`。書式の API 差は TCB-23。[共有表](crash-detail-table.json)
- `am_*`／`wm_*` などのライフサイクルイベントから前面アプリと画面遷移帯を構築する。アプリが付けた同名テキストを無条件に信頼してはならない。前面／クラッシュ等に対応づけられるログはパッケージ別に抽出する。実ファイルは `app-logs/<安全な名前>.log`。大小文字を無視する FS でも衝突を解消し、Windows 予約名を回避する。古い VER・型コメントの `.jsonl` 表記から拡張子を推定せず、マニフェストの相対パスを使う。[帰属・ファイル名決定](../../core/crates/arb-core/src/analytics.rs)・[保存処理](../../core/crates/arb-storage/src/bundle_io.rs)

## 仕様 vs バグ

| 観測条件 | 正常仕様 | バグ |
| --- | --- | --- |
| API 32 の既定音声 | 映像／ログを継続、音声非対応 | 録画全体が停止する |
| API 33 の音声ソケット失敗 | 実行時の原因を表示 | OS が音声非対応と表示する |
| 互換方式を明示選択 | 端末消音を案内して取得 | 勝手に互換方式に切り替えて消音する |
| 指定端末だけ抜いた | 他端末へ切り替えず待つ | 別端末の証跡を同じ記録に混ぜる |
| アプリが任意に `am_crash` を出力 | 通常ログとして扱う | テキストだけで自動保存する |
| アプリ UID が `ActivityManager` の has died・Force stopping、`am_kill` を出力 | `process_events` に入れない | 他アプリの終了やクラッシュ外終了を偽装できる |
| `am_crash` だけでアプリ側の `AndroidRuntime` ブロックがない | `stack_top`・`first_app_frame`・`caused_by` は null | 例外名からスタックを作る |
| 軸範囲を超えたタッチを記録・閲覧 | 既知寸法では端に描く | タップを落とす、古い保存物を上書きする |
| Framework 注入だけを行った | タップなしでも観測制約内 | 合成のタップを物理観測として加算する |

根拠は Trail #98・#105・#108・#110・#111、#141／PR #544、#143／PR #546、#148／PR #551。S3・S5、K6・K7・K11、G6・G8、KON-6、TCB-08／16／17。[VER-23](../verifications/VER-23-t6e-aos9-compat.md)・[実機スモーク](../testing/work-device-smoke-checklist.md)

## 注入操作の観測

保存ログのタグが厳密に adbd、UID が 0 または 2000 のサービス要求だけを読取時に解析する。座標は Android の input と同じく小数も受け付け、整数はそのまま整数で返す。NaN・無限大になる値は未解析として数える。PATH を変えた後の素の input・cmd は端末側の実体を特定できないため数えない。注入の時計は要求受信時刻。対話シェル内の操作と他の注入元は観測範囲外。複合要求で後続コマンドの実行・成功は保証されない。API 30 以上の 50 ms、それ以外の 500 ms は受信時刻を解釈する目安で、端末が操作を完了するまでの保証時間には使えない。
