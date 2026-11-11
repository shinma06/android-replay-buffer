# 初期版プラグインの設計

[製品要件](https://github.com/shinma06/android-replay-buffer/blob/4d278e65dec0c55e71d584ff24c7f6fa407586cb/docs/requirements.md)を実装するための設計です。設計の採用と実装・実機受入の完了は区別します。UI配置は [採用済みのIDE UI設計](https://github.com/shinma06/android-replay-buffer/blob/4d278e65dec0c55e71d584ff24c7f6fa407586cb/docs/design/ide-ui.md)（[#12](https://github.com/shinma06/android-replay-buffer/issues/12)）を参照してください。時刻・録画方式・出力形式は [採用済みの同期設計](https://github.com/shinma06/android-replay-buffer/blob/4d278e65dec0c55e71d584ff24c7f6fa407586cb/docs/design/timeline.md)（[#13](https://github.com/shinma06/android-replay-buffer/issues/13)）を参照してください。同梱CLIは同じproject service・取得コア・保存writerを使います。[CLI](cli.md)と[原型の復元](cli-origin.md)を参照してください。

[固定develop原設計](https://github.com/shinma06/android-replay-buffer/blob/4d278e65dec0c55e71d584ff24c7f6fa407586cb/docs/plugin-design.md)を管理側へ接続し、保存attempt保全を#201、取得モードと正常0行の保存条件を#274で改訂します。#284の既定OFFの[クラッシュ報告による自動保存](crash-autosave.md)は同じ単一pin/writerへ接続します。製品のmain promotionや受入完了を意味しません。

冒頭の固定リンクは設計採用時の出典です。改訂後の時刻・保存窓・出力形式は[現行の同期設計](design/timeline.md)、UIは[現行のIDE UI設計](design/ide-ui.md)、実装と受入の状態は[Case台帳](verification/README.md)を照合します。末尾の初期Case表や当時の未実施記述を、現在の一律pending・実行済みPASSへ読み替えません。

## 保存物の構成

保存1件は1件の不具合の再現記録（replay）です。手動保存・クラッシュ時の自動保存・ログのみ保存・ログ専用モードの保存は、どれも同じ`SaveWriter`で次の構成になります。旧形式（`session.json` / `README.txt`）のフォルダは移行も削除もしません。

フォルダ名は`replay-<yyyyMMdd-HHmmss>-<manual|crash>[-logs]-<ID>`です（例: `replay-20261111-140312-manual-a1b2c3d4`）。日時は保存を始めたローカル時刻、`crash`はクラッシュ時の自動保存、`-logs`は動画を含まない保存（ログのみ保存・ログ専用モード）、IDは保存試行ごとの8桁の16進数です。使う文字は`[a-z0-9-]`だけで、Linux・macOS・Windowsで同じ名前になり、同じ種類の中ではローカル時刻順に並びます。作成中は`.<フォルダ名>.partial`で、既存のフォルダを上書きしません。同じ記録の再試行は`save_id`が同じで、フォルダ名のIDは別になります。

| ファイル | 内容 | 作る条件 |
|---|---|---|
| `README.md` | 人とAIが最初に読む要約。見出しは「概要」「ファイル」「記録の状態」「クラッシュ」（クラッシュ時だけ）「動画とログの照合方法」「注意」の固定構成で、値は`replay.json`と同じ事実から書きます | 常に |
| `replay.json` | 機械向けの記録（`schema` 4）。取得状態・時計・gap・アプリ履歴・各ファイルのSHA-256 | 常に |
| `video.mp4` | 画面の動画（MP4 1本） | 動画がある保存 |
| `video-frames.jsonl` | 動画の各frameの時刻索引（1行1frame） | 動画がある保存 |
| `video-source.h264` | 再エンコード前の元映像（H.264 Annex B） | サイズ変更を含み再エンコードした保存 |
| `logcat-device.jsonl` | 端末全体のログ（1行1レコード） | 常に（0行もある） |
| `logcat-app.jsonl` | 対象アプリのログ | 常に（0行もある） |
| `touches.jsonl` | タップの座標と時刻 | 動画を要求した保存 |

`replay.json`のschema 4では、schema 3の内容に次を加えました。`folder_name`、`trigger`（`manual` / `crash`）、`created_at`（UTC）と`created_at_local`（時差付きのローカル時刻）、`files`（`readme / record / video / video_frames / video_source / logcat_device / logcat_app / touches`。値はファイル名、作らなかったものはnull）、`counts`（`device_log_rows / app_log_rows / video_frames / touches / touches_in_movie`）。schema 3の`video_file` / `touch_file` / `video_source_file`は`files`へまとめました。各ログ・frame・タップの行の形式は変えていません。

スクショは保存フォルダを作らず、`screenshot-<yyyyMMdd-HHmmss>-<ID>.png`の1枚です（日時・IDの形式は保存フォルダと同じ）。

MCPの`save_replay`と`get_status`の`last_save`は、完成したフォルダの`directory`に加えて`README.md`のパスを`summary_path`で返します。CLIの`save`のstdoutは従来どおり完成したフォルダのパスだけです。

## タップ位置の記録と表示（Trail #22）

動画＋ログの取得中は、タップ座標と時刻を自動で記録します（専用の設定はありません）。ログ専用では記録せず、`touches.jsonl`も作りません。端末へは`adb shell getevent -lp`で`ABS_MT_SLOT`と`ABS_MT_POSITION_X/Y`を持つmulti-touch入力device（protocol B）と軸範囲を、`wm size`で画面px（Override sizeを優先）を取得し、`getevent -lt`の各SYN_REPORTでslotごとの`down / move / up`を作ります。座標は軸範囲から画面pxへ換算した、画面の自然な向きの値です（`x / y`には回転を適用しません）。画面の回転は同じ`adb shell`呼び出しで`dumpsys input`の内蔵display（`displayId=0`）のviewportの`orientation`（0〜3、90度単位）から読み、`display_rotation`に記録し、回転後の画面（動画に写る向き）のpxを`screen_x / screen_y`に併記します（読めなければnull）。取得中の回転、`wm size`のOverride変更やemulatorの画面サイズ変更に追従するため、前回の読み取りから1秒以上たって入力が届いたとき、および動画streamが新しい画面サイズを報告した直後の入力で`wm size`と回転を読み直し、以後のsampleを新しい値で換算します（読み取りに失敗したときは直前の値のまま）。各行の`display_width / display_height`は、その行を換算したサイズです。入力が続いている間にサイズが変わった場合は、最大1秒は古いサイズで換算されることがあります。protocol Aやmulti-touch軸のない入力deviceは対象外で、その場合は動画取得を止めずに状態を`UNAVAILABLE`として保存します。

時刻はkernelの入力時刻（`getevent -lt`の端末CLOCK_MONOTONIC）を正本とし、動画PTSと同じ`CaptureClock`の時計対応でsequenceへ写します。host受信時刻で置き換えません。保存窓・固定・pruneの規則は両ログと同じで、保存対象の固定後に新しいclock sampleで再解釈しません。保持件数の上限（200,000件）を超えた古いsampleは`dropped_count`へ数えます。

`touches.jsonl`は1行1sampleで、`clock_epoch / generation / device_mono_ns / elapsed_ns / window_ns / uncertainty_ns / movie_us / action / pointer / x / y / display_width / display_height / display_rotation / screen_x / screen_y / input_device`を持ちます。`movie_us`は保存MP4で、その時刻に表示中のframe（`video-frames.jsonl`の`movie_pts_us〜movie_end_us`）上の位置です。表示区間外、時計対応が不確定、clock gapに重なる行はnullで、同期済みと扱いません。`replay.json`は`files.touches`と`touches`（`state / reason / count / in_movie_count / dropped_count / show_touches / show_touches_state / time_source / coordinate_space / screen_coordinate_space / gaps`）を持ちます。`show_touches`は設定値で、`show_touches_state`は実際の状態（`applied`＝点を表示、`already_on`＝端末で既にON（設定OFFでも端末の値を読み、開発者向けオプションなどで`1`なら`already_on`。点は動画に映ります）、`failed`＝端末設定を変更できず点なし、`off`＝設定OFF、null＝未確定）です。保持数の上限（200,000件）を超えた古い行は捨て、捨てた範囲が保存窓に重なる場合は`gaps`に記録します。端末の入力bufferがあふれた（`SYN_DROPPED`）ときは、そのdeviceで押されていたpointerをすべてdrop時刻の`up`で終え、次のSYN_REPORTまでの入力を捨てます。evdevは変化した軸だけを送るので、各slotの最後のX/Yは消さずに残し、現在のslotとそのX/Yは`getevent -lp`の現在値で読み直します。そのあと、新しい`ABS_MT_TRACKING_ID`が届くまでは指の同一性が分からないため、そのslotからは行を作りません。drop時刻から、最初に新しい`ABS_MT_TRACKING_ID`（0以上。dropをまたいだ指を離す`-1`は含めません）を含むSYN_REPORTまで（端末の入力時刻を時計対応で写した範囲）を`gaps`に記録し、再同期する前に読み取りが止まった場合は止まった時刻で閉じます。保存時点でまだ再同期していなければ、gapの終わりはnull（保存窓の終わりまで不明）です。evdevは読み始めたreaderにも現在値を送り直さないため、readerを開始するたび（取得開始・再接続後の再開）にも、取得前に読む`getevent -lp`の現在値（現在のslotとそのX/Y）でslotと座標を初期化し、最初のタップで変化しなかった軸やslotの指定を補います。ほかのslotには初期値がなく、最初のタップでX/Yの両方が届くまで行を作りません。制約として、dropをまたいで押されたままの指は、離して押し直すまで記録されません。`getevent -lp`の値はdrop時点より少し後の状態なので、その間にslotが変わるとpointer番号や座標がずれることがあります。読み直せるのは現在のslotのX/Yだけで、ほかのslotはdrop前の最後の値を使います。reader開始時の値も`getevent -lt`の開始より少し前の状態です。`dropped_count`は取得開始からの累計です。schema 3で追加したfieldです。

動画上の点表示は設定「取得中は端末画面にタップ位置を表示する」（既定OFF）で切り替えます。ONでは取得開始時に端末の`settings get system show_touches`を記録し、`1`でなければ取得中だけ`1`にして、取得終了（OFF・切断による世代終了・project close）時に元の値へ戻します（未設定だった場合は削除）。取得OFFかつ反映完了後だけ変更できます。切断中で戻せなかった元の値はproject内で端末serialごとに保持し、同じ端末の次の取得では端末の現在値（自分が設定した`1`）を元の値と取り違えずに、その取得の終了時に戻します。IDE異常終了やproject closeまで端末が戻らなかった場合は、端末側の設定が残ることがあります。点は端末の画面描画として動画に焼き込まれ、`touches.jsonl`の記録とは独立です。

エミュレータでの確認には実際のevdev入力が必要です。`adb shell input tap`や`adb emu finger`はevdevを通らないため記録されません。VMではstandalone emulator windowへの`xdotool`のclickで実イベントを作れます。

## 動画品質の選択（#282）

設定画面の動画品質は、標準（長辺最大1920px・最大30fps・指定8Mbps、従来の既定値）と軽量（長辺最大1024px・最大15fps・指定2Mbps）です。未指定の旧XMLは標準で読み、既存の保持時間・保存先・アプリ・スクショ等の値と旧CLI importを維持します。品質は取得OFFかつ設定/操作反映後に変更し、Apply/OKだけで保存します。LOG_ONLYでは選択欄を無効にし、値を保持したまま動画を開始しません。

軽量は文字の細部、短い変化、滑らかさと圧縮品質を優先しない選択です。scrcpy [v5.0.1の公式例](https://github.com/Genymobile/scrcpy/blob/a60891aea193d92e7e5c3942700eca63f9d19a5f/doc/video.md)の各値を組み合わせた初期候補であり、最適な組合せ・CPU/RAM/電力の削減率を測定したものではありません。長辺・fpsは上限、bitrateはencoderへの指定値で、実解像度・実fps・実bitrateとは区別します。H.264・B-frameなし・IDR interval指定1秒・既存encoder選択を維持します。[serverのdownsize_on_error](https://github.com/Genymobile/scrcpy/blob/a60891aea193d92e7e5c3942700eca63f9d19a5f/server/src/main/java/com/genymobile/scrcpy/Options.java)はfalseを明示し、encode失敗で黙って解像度を下げず、既存の取得中断・復旧/gap表示へ返します。

保存時は受理したimmutable settingsの品質を固定し、`replay.json`の`settings.video`の既存形式を値に合わせて出力、追加の`video_request`へquality/max_size/max_fps/video_bit_rate/downsize_on_errorを記録します。これらは要求値で、実際の寸法・PTS・内容は既存のparts/frame/config記録を使います。LOG_ONLYの両動画設定はnullです。設定の現在値で失敗pin/既存保存物を付け替えません。品質の選択自体では時計・窓・ログ・欠落表示・pin/再試行・保存失敗保全を変更しません。性能の完了条件は[現行方針](verification/performance.md)です。

## 保存失敗とデータ保全

設計対象: [#11](https://github.com/shinma06/android-replay-buffer/issues/11)。SAVE-05の未決事項について、次の比較から**失敗した保存対象を1件だけ固定して再試行する方式**を採用します。以前の会話の案を合意済みとみなしたものではなく、今回の設計判断です。

| 方式 | 利点 | 問題・判断 |
| --- | --- | --- |
| 現在のバッファから保存をやり直す | 追加の保持が不要 | 復旧までに現象が保持窓から消える。今回の用途には不採用 |
| 失敗対象を1件固定し、再試行・保存先変更・破棄を選べる | 同じ現象を保存し直せる。保持量を制限できる | 解決するまで次の保存要求を受け付けない。初期版で採用 |
| 失敗要求を無制限に蓄積する | 複数の失敗を後から回収できる | 容量不足を悪化させる。初期版では不採用 |

### 保存要求の境界

保存要求を受けた時点で、要求ID、シーケンス、有効な取得モード、保持時間、開始・終了時刻、端末、アプリ選択履歴、取得済み動画と両ログへの参照を固定します。GUIとCLIは同じcommand queueからこの保存処理へ入り、以後の取得・設定変更・再接続で保存対象を変えません。バッファの削除処理は保存中・失敗保持中の参照を削除しません。動画の未確定部分の扱いと切り出し精度は[同期設計](design/timeline.md)に従います。

動画のある保存はMP4を1件にまとめ、再接続・config切替・時計不明を同じファイル内の再生区間として保存します。元frameとPTS、復号prefix、gapの既知/不明は[同期設計](design/timeline.md)に従います。必要な全対象を保存できない場合は成功を公開せず固定要求を保ちます。

保存処理は同時に1件です。処理中は保存ボタンを「保存中」にし、重複要求を起動しません。取得は継続します。成功後はすぐ次を保存でき、保存区間の重複を許容します。動画＋ログモードで全種類0件なら、空の成功フォルダを作らず「保存できるデータがありません」と表示します。1種類でもあれば、取得できた内容を保存します。ログ専用は例外として、必要readerが正常headerを確認し、同じ取得世代の有効な時計・現在epochの対象アプリ・正の取得窓が揃った場合だけ両0行のmetadata保存を受理します。取得開始前のtailはreader確認にのみ使い、保存行にはしません。未開始、reader障害、不明時計、対象未確定、完全切断中の両0行は新規保存不可です。正常0行のcoverage秒数は0のままで、窓・clock/app履歴・gapを別に保存します。再試行は受理時のモードと同じ固定対象を使います。

ログ専用は動画経路と動画ringを開始せず、MP4と動画sourceを生成しません。`replay.json`（schema 3で追加）に`capture_mode`、`settings.video_requested`、`coverage.*.requested`を加え、動画stateを`NOT_REQUESTED`とします。`video-frames.jsonl`は作らず、動画欠落範囲・動画clock scope・partsは空、video file/tailはnullです。共有clockのgapは両ログの不足へ反映し、意図した動画なしをmissingKindsへ加えません。従来schema 2までの保存物を変更せず、通常モードの動画欠落・保存失敗は従来どおりです。

### 成功・一部取得・書込み失敗

- 保存先の下に日時とランダムな要求IDを含む新規フォルダを作り、既存フォルダを再利用しません。生成中は `.partial` を末尾に付けます。ファイルを閉じ、必要な出力と同期情報の整合を確認した後、同じ親フォルダ内で完成名へ移動します。既存ファイルを置換しません。完成できなければ成功通知を出しません。
- 取得時点で動画や片方のログが不足・未確認でも、利用できる内容の書込みがすべて完了すれば「保存しました（記録の不足・未確認があります）」と通知します。対象種類・時間範囲・理由を出力の同期情報へ記録し、ログ0行の正常な空期間と取得失敗を区別します。通知からフォルダを開けます。現在窓から外れたと証明できる履歴の扱いと元履歴の保全は[同期設計](design/timeline.md)に従い、固定した失敗対象を現在窓へ差し替えません。
- 書込み・動画出力・最終移動の失敗は保存失敗です。「保存先に書き込めません」「空き容量が不足しています」などの分類と、保持中の対象時刻を常時表示します。原因を特定できなければ「保存に失敗しました」とし、推測を事実として表示しません。ログ本文や生のprocess出力を通知・IDEログへ埋め込みません。
- 失敗時には「同じ内容を再試行」「保存先を変更して再試行」「保持した保存対象を破棄」を表示します。再試行は自動ループにせず利用者が行います。保存先変更は明示的な適用後に、その同じ対象へ反映します。取得条件まで変更した場合も失敗対象の時刻・内容は変更しません。
- 失敗対象を解決するまで新しい保存要求は受け付けず、その理由と上の操作を表示します。取得は継続し、破棄後は現在のバッファを直ちに保存できます。破棄は保存用の固定参照を解放する操作で、現在の取得バッファや完成済み出力を消しません。
- 失敗・取消しした保存試行のfile/directoryは自動削除せず保全し、`.partial` を完成エビデンスと表示しません。path確認と削除を同じ実体に結び付けられないため、今回の作成物も残します。作成済みsessionは保持descriptorで `complete:false` へ戻すbest effortを行いますが、書込み不能時は保証しません。同identityの実在する未完成フォルダの場所をbest effortで既存エラー経路へ伝え、確認できない場所を実在と断定しません。取消しmessageにも場所を保持しますが、OFF/project closeの既存UI抑止は維持します。残留による容量増加があり、自動回収は提供しません。再試行は同じ固定対象を別の新規フォルダへ出力し、成功時のatomic非上書きと完成済みエビデンス・所有不明物の保全を維持します。原cleanup契約と旧観察は履歴として保持し、削除成功へ読み替えません。

### 容量と寿命

通常の実時間窓に加え、保存対象の固定は最大1窓です。参照共有できるデータは複製しません。保持時間だけでは大量ログの容量を制限できないため、実装は動画・ログ別のバイト上限と空き容量の下限も設定し、固定画質・最大保持時間と合わせて実測根拠を記録します。具体値は録画方式の選定後、実装PRで確定し、未設定のまま初期版を完成扱いにしません。

保存先の障害と取得用一時領域の障害を区別します。保存先だけが書込み不可なら取得を継続します。一時領域が不足した場合は未参照の期限切れデータから解放し、保持中の保存対象は勝手に消しません。それでも保存できない種類は取得失敗として表示し、可能な種類を継続します。書込み成功や復旧を検出できるまで新しいデータを捨てた区間を欠落として記録します。復旧を直ちに試み、継続失敗時は待機を入れてCPUやログの暴走を防ぎます。

機能の無効化では保存中処理も取消し、終了を確認してから固定参照を含む未保存データを破棄します。保存完了の確定と無効化が競合した場合は、確定済みの完成フォルダは残し、未確定処理だけ取消します。画面の無効化説明に「未保存のデータと再試行待ちの内容を破棄します」を含めます。

初期版の再試行保持は現在のIDEセッション内です。終了・project closeでは処理を停止し、未保存データを破棄します。失敗表示にこの寿命を明記します。IDE再起動後は有効状態と設定を復元して新しく取得し、以前の未保存データが復元できるとは案内しません。異常終了後の取得用一時rootは所有と旧process終了を確認してから限定回収し、時間経過だけで他の取得処理や不明ファイルを削除しません。この取得root回収は、上記の保存attempt残留を回収する機能ではありません。通常取得・remote cleanupは維持します。

### 切断と設定適用

保持時間をN、保存基準時刻をTとすると、対象窓は `[max(sequence開始, T-N), T]` です。接続中は保存要求の受付時刻をTとします。完全に切断して全取得が止まった場合は、切断を確定した時刻でTを凍結します。長い切断中もその窓を保存でき、待ち時間だけを理由に切断前データを消しません。取得できた長さをNにするために開始を過去へ広げません。

再接続後は現在時刻へTを戻し、元のシーケンスIDのまま窓を更新します。古い区間が現在の窓外なら通常どおり期限切れにし、既に保存対象へ固定した参照は解決まで残します。片方の取得だけが失敗した状態ではTを凍結せず、正常な取得と同じ実時間窓に欠落を記録します。時刻源、切断確定の精度、時計補正、動画PTSとの対応は[同期設計](design/timeline.md)に従います。

設定編集は下書きです。適用時だけ取得条件を変更します。保存先だけの変更では取得を再起動しません。保持時間の変更は新しい時間窓へ更新し、固定した保存対象を変更しません。録画の再起動が必要な適用でもシーケンスと既存の窓内データを保持し、停止区間を記録します。対象アプリ切替の区間表現と自動選択の優先規則は[同期設計](design/timeline.md)・[IDE UI設計](design/ide-ui.md)に従います。

### 実装で確認する受入Case

ここは試験計画であり、pass記録ではありません。#14 のCaseへ対応づけ、固定ZIPで実機・Emulatorの観察を記録します。

| Case | 操作 | 期待値 |
| --- | --- | --- |
| SAVE-FAIL-01 | 書込み不可の保存先へ保存し、保持時間経過後に別の保存先へ再試行 | 同じ要求区間を保存。待機中も取得が続き、新しい区間へすり替わらない |
| SAVE-FAIL-02 | 完成前の各出力段階と最終移動で失敗させる | 成功通知なし。未完成と識別できる。元の完成済み出力は不変 |
| SAVE-FAIL-03 | 動画のみ、ログのみ、対象アプリログのみ欠落させて保存 | 取得できた内容を保存し、不足種類と期間を通知・同期情報で確認できる |
| SAVE-FAIL-04 | 全種類0件、短時間取得、同じ時刻で連続保存 | 0件は明示、短時間は取得済み分、各成功フォルダは一意。取得は止まらない |
| SAVE-FAIL-05 | 失敗保持中に再試行連打・設定適用・破棄を行う | 二重出力や固定対象の書換えなし。破棄後は新しい保存が可能 |
| SAVE-FAIL-06 | 保存中・失敗保持中・保存確定直後に無効化/project close | 未保存参照と所有processが解放され、完成済み出力は残る。dispose後にUI更新しない |
| SAVE-FAIL-07 | 保存先と一時領域の容量不足を別々に起こす | 前者で取得継続、後者は上限内で可能な種類を継続し欠落を明示。固定対象を消さない |
| SAVE-FAIL-08 | Nを超える切断後に保存し、再接続して再保存 | 切断中は凍結窓、再接続後は現在の窓。シーケンスは同じで空白を詰めない |
| SAVE-FAIL-09 | 異常終了後の自分の一時領域、生存owner、所有不明ファイルを用意 | 停止・所有確認済みだけ回収。生存中や不明データ、完成出力は保持 |

同期精度の目標とアプリ選択方式は採用済み設計へ接続しました。固定画質と容量値の確定、同期精度・IDE/端末動作の実測は #31/#32/#27 で確認します。設計の採用だけで検証済みとは扱いません。
