---
id: VER-31
type: verification
name: 昇格前の API 37 エミュレータ受入検証
verifies:
  - SPEC-01
  - SPEC-02
  - SPEC-03
  - SPEC-04
---

# API 37 エミュレータ受入検証

2026-11-24、macOS arm64、基準コミット `28d7f26d`。初回はタッチ座標の単位違いで焼き込みに失敗した。修正後の同一 AVD で Tap と Swipe のマーカーを復号画素で確認し、ローカル受入は合格した。

## 初回のライブ取得と出力

release の arbd / arb-cli と当該作業ツリーでビルドした 2 個の DEX JAR を使用した。`arb_api37_arm64` をヘッドレスで起動し、専用の data / runtime / cache ディレクトリで測定した。以下の時間は各操作 1 回のホスト側経過時間。

| 操作 | 観測結果 |
| --- | --- |
| 常時録画 | 端末なしで ON を確認。コールドブート途中に接続した初回は映像 0・音声 unavailable を観測。起動済み端末に対し停止後に config.update で再度 ON にすると、端末指定なしで 1,682 ms 後に映像・音声の両方を確認 |
| 通常保存 | 12 秒窓、481.0 ms。出力先設定を適用。video.mp4、audio.m4a、timeline.sqlite、events.jsonl、logcat.log、Settings の app-logs/*.log の 6 ファイルを manifest の SHA-256 と照合 |
| 画面遷移・入力 | Home → Settings 等、overview に画面区間 5 件。adb shell input の tap 2 回・swipe 1 回のみでは生入力カウントは 0。adb emu event mouse で仮想タッチデバイスを操作すると Tap 2 件・Swipe 1 件。Swipe の軌跡は 4 点、開始 (10012, 8464)、終了 (10012, 2730) |
| ビューポート | 197 フレーム、frame_pts_ns 197 件。隣接 PTS の間隔が複数あることを確認。filmstrip 59.5 ms、scrub 0.1 ms、seek_frame 64.1 ms、decode_audio_pcm 16.0 ms（RIFF を確認） |
| トリム | 入力フレーム 76〜141、両端込み 66 枚。両ジェスチャー種別を含む。共有動画を含む完全なフォルダーを 2 個生成し、各 manifest の 7 ファイルを SHA-256 で照合。焼き込み指定あり 159.0 ms、なし 133.1 ms |
| プレイヤー検査 | share-taps.mp4、トリム video.mp4、share.mp4 とも自動検査 17/17。avc1 が 1 件、avc3・edts・elst が 0 件、全ストリームの ffmpeg 復号エラー 0。17 項目中 6 項目は同梱 audio.m4a の検査 |
| 焼き込み | 画面 1080×2400 に対し evdev の軸範囲は 0〜32767。記録された画面外座標を map_point_to_video が除外。ffmpeg で抽出した share-taps.mp4 と同一フォルダーの video.mp4 の H.264 が全 240,306 バイト一致し、焼き込み不成立。指定なしの share.mp4 は意図通り video.mp4 と映像が一致 |
| ログ専用・スクリーンショット | capture.start 20.1 ms。save.trigger 172.3 ms、logs_only の logcat.log 6,564 B、timeline.sqlite 86,016 B、video.mp4 なし。指定フォルダーの PNG は 1080×2400、65,111 B、168.3 ms |
| MCP | initialize と tools/list が成功、17 tools、標準エラー 0 B。overview に Tap / Swipe と app_logs、inspect_frames の contact_sheet は 1020×588 の image/png |
| 既存の e2e_emulator.sh | 別のクリーン起動で全段階成功。161 フレーム、filmstrip 77.724 ms、scrub 0.083 ms、seek 102.948 ms、81 枚の mid-GOP trim 159.7 ms |

## 共有メモリの回帰試験

T-7C CI の run 45343495540、初回の test ジョブで、並行テストが仕様上許される 64 回の seqlock 再試行切れを致命エラーとして扱った。テストはこの競合理由だけを再試行し、20 世代ごとのチェックポイントで読取側の検証完了を待つ。成功したすべての読取りで世代順と全画素を検査する。強制的に毎回ラップさせる試験では競合理由まで照合した。SHM の 9 テストを 50 回実行し、450 件成功、失敗 0 件。

## 検証境界

実機 USB の Android 9 / 13、Windows・Linux のデスクトップ GUI、Android Studio 実画面、QuickTime・Chrome の目視再生は今回の測定対象外。既存の 24 指摘への対応・回帰試験の内訳は VER-28、VER-29、VER-30 を参照。旧設定移行の本番実装は LegacyV1SettingsMigrator.kt、起動時呼び出しは ArbStartupActivity の 1 箇所。実録画、生ログ、端末診断情報は本記録に含めていない。

## 元監査との突合

原票の 24 件を VER-28〜30 の修正箇所と今回の回帰試験に突合した。以下の判定は個々の指摘に関するもので、ライブ焼き込みの再検証は以下に記載する。

| 指摘 | 今回確認した挙動・試験 |
| --- | --- |
| H1・H2・H3 | 保存中の端末切替で端末情報を保持、IDR 間の窓の両端を復号、設定変更の前後で別寸法の映像を復号 |
| H4・H5 | OpenH264 の疎な seek で対象画像だけを保持。巨大な mdat はヘッダーだけを読み取る |
| H6・H7 | 同一割り当てでの並行読取りと強制ラップ。Windows パイプの所有者 SID 不一致時に書込み 0 B で閉じる Kotlin テスト |
| H8 | 実録画の VFR 配列、Studio モデルの実 PTS とループ後の音声位置 |
| H9・H10 | 旧形式ディレクトリへの出力拒否、現行形式の外部メディア・タイムライン参照拒否 |
| M1・M2 | viewport 開設中のエディタ破棄、映像先行表示と 5,000 行のページング |
| M3・M4 | サムネイル生成中の別 viewport の seek と status 応答、読取り停止時の drain の期限 |
| M5・M6 | クラッシュ窓と連続トリガーの抑制、旧 ADB 転送の serial・port・socket 照合 |
| M7・M8 | 音声キャッシュ更新と実 AAC の PCM 復号、複数 boot epoch の画面区間・フレーム境界 |
| M9・M10 | 同サイズ上書きを SHA-256 で識別、24 バイト旧ヘッダーの UID 不明を保持し自動保存を抑制 |
| M11・M12 | 統計プローブの停止・期限切れ後の子プロセス回収、パッケージ自己試験で正式版・候補タグを分類 |
| M13・L1 | 低速な端末探索を停止で取り消す試験、不正な shutdown 要求後も接続を維持する試験 |

製品名・表示ラベルの点検では旧ブランド表記を検出しなかった。互換用の保存値と wire 値は継続使用する。IDE の Checkbox 呼び出しは ArbLabeledCheckbox 内の 1 箇所。端末のタッチ表示設定を変更する製品コードは検出されなかった。

ローカルでは verify.sh 全 6 ゲート（GPL/AGPL 0、sara strict の Verification 31 件・REQ 14/14）、警告をエラーとした workspace rustdoc、device-agent ビルド・Java 24 試験、JDK 27 の Gradle test/buildPlugin、配布物の自己試験が成功した。Gradle は非推奨機能の使用を通知した。今回起動したエミュレータと専用デーモンの終了、adb の接続端末 0 台を確認した。

## 軸変換修正後の再検証

再接続ごとに入力デバイス別の ABS 軸の最小値・最大値を読み、分類と保存には画面ピクセル座標を使用する。リングには元のイベントと変換値を一緒に記録する。単体試験では 0〜32767 のパネル上の小移動を Tap、大移動を Swipe と分類し、軌跡の座標まで照合した。別デバイスの 4096 段階の軸、等倍の軸、軸情報のない旧イベントの経路も検査した。

| 修正後の操作 | 結果 |
| --- | --- |
| 自動接続 | 起動済み API 37 に対し config.update で ON。端末指定なしで映像・音声を 3,105 ms で受信 |
| 収録・保存 | 288.6 ms。出力先、6 ファイルの SHA-256、画面区間 5 件を確認。VFR 213 フレームと実 PTS 213 件 |
| 座標 | Tap (200,375)、(260,520)。Swipe の軌跡は (330,620) → (330,410) → (330,200) → (330,200)。1080×2400 上で入力位置と一致 |
| 読取り | filmstrip 61.8 ms、scrub 0.1 ms、seek_frame 71.3 ms、PCM 12.8 ms |
| トリム | 92〜157 の 66 枚。焼き込み指定 215.7 ms、指定なし 147.2 ms。両方とも manifest の 7 ファイルが SHA-256 一致。焼き込み動画の H.264 は 290,540 B、同梱の元映像は 233,183 B。指定なしは元映像とバイト一致 |
| 復号・描画 | 各 MP4 を全フレーム RGB24 に復号。344×768 上の第 9・29 フレームで各 Tap 中心、第 47 フレームで Swipe 軌跡中央を照合。21×21 画素内で RGB のいずれかが 30 以上変わった画素は順に 388・372・84 個。各点から右に 35 px 離した同じ広さの対照領域はすべて 0 個。Swipe 中央 (105,131) は RGB (236,236,242) から (237,195,80) に変化 |
| コンテナ | share-taps.mp4、video.mp4、share.mp4 の自動検査は各 17/17。各 avc1 は 1 件、edts / elst / 復号エラーはすべて 0 |
| ログ専用と PNG | capture.start 18.1 ms、保存 221.0 ms。logs_only、logcat.log 69,466 B、timeline.sqlite 241,664 B、video.mp4 なし。独自の出力先に PNG 1080×2400、65,872 B、164.7 ms |
| MCP | 17 ツール、stderr 0 B、overview に両種のジェスチャー・app_logs、PNG コンタクトシート 1020×588 |
| 既存の e2e_emulator.sh | 別起動で 196 枚、filmstrip 52.400 ms、scrub 0.076 ms、seek 95.500 ms、98 枚の trim 148.4 ms。終了後 adb 接続 0 件 |

修正後の release で device-agent、Rust の全 6 ゲート・rustdoc、Gradle test/buildPlugin、配布物自己試験を再実行して成功した。
