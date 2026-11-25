---
id: SPEC-03
type: spec
name: Device Agent・ローカル IPC・GPL 非依存メディアおよびフレーム単位トリム仕様
satisfies:
  - REQ-03
  - REQ-08
  - REQ-09
  - REQ-11
  - REQ-12
  - REQ-13
  - REQ-14
---

# SPEC-03: Device Agent・ローカル IPC・GPL 非依存メディアおよびフレーム単位トリム仕様

## 1. Device Agent と ADB ワイヤプロトコル（`arb-adb` / `device-agent`）

- **実績ある映像・音声取得の再利用**: `scrcpy-server-v5.0.1`（Apache 2.0）と初フレーム PTS 修正済み `AudioRecordReader` を中核として再利用し、`app_process` によりセッション単位で ADB 転送起動する（root・APK インストール・対象アプリ改修不要）。
- **キャプチャ推奨設定**: `video_codec=h264`, `video_codec_options=max-bframes:int=0,i-frame-interval:int=1`, `max_size=1920|1024`, `max_fps=30|15`（1 秒 GOP を明示要求しシーク・先頭 GOP 再エンコードを高速に保つ）。音声再生キャプチャ（`audio_source=playback audio_dup=true`）は API >= 33 対象とし、未校正オフセットを正直に記録する。
- **統合時計・ビーコン・logd コンパニオン**:
  - 20 byte 固定長 Little-Endian ヘッダのバイナリフレーミングで ADB forward 通信を行う。
  - `beacon`（250〜500 ms 周期）が `boot_id`, `boottime_ns`, `monotonic_ns`, `realtime_ns` と各ストリームの生存状態・連番を送信し、静止画面（`STATIC_HOLD`）と切断／欠損（`CONNECTION_LOST` / `STREAM_GAP`）を分離する。
  - `logcat` は端末読取時の `device_read_boottime_ns` を打刻し、`logcat -S` / `-g` 統計監視によりドロップを検知する。裏付け不可の端末では `completeness = "unverified"` を明示する。
  - `video` / `audio` / `logcat` / `input` / `beacon` は独立エラー境界で障害隔離する。
- 入力取得時は接続ごとにホストが `getevent -lp` のデバイス別・軸別の最小値と最大値を読み、絶対座標を物理画面のピクセル位置に変換する。Tap / Swipe の移動量判定、保存する軌跡、Studio・共有動画の表示は同じ座標を使う。軸情報や寸法を取得できない場合は従来の座標を使い、表示範囲外のマーカーは描画しない。
- **自動再接続**: ADB デバイストラッカーを常時監視し、切断検知後は指数バックオフ（200 ms → 500 ms → 1 s → 2 s）で数秒以内に Device Agent を自動再起動する。再接続時の `boot_id` 照合で `CONNECTION_RESTORED` と `DEVICE_REBOOT` を厳密区別する。

## 2. `arbd` ↔ クライアント ローカル IPC と共有メモリフレームリング

- **制御プレーン**: バージョン付き JSON-RPC 2.0 / NDJSON（Linux: `$XDG_RUNTIME_DIR/arb/arbd.sock`、macOS: `~/Library/Caches/arb/arbd.sock`、Windows: `\\.\pipe\arb-daemon-<sid>`）。`system.*`, `device.*`, `capture.*`, `save.*`, `bundle.*`, `timeline.*`, `edit.*`, `export.*`, `media.*` を提供する。
- `system.hello` は `parent_bound`（真偽値）を返す。`--exit-on-stdin-eof` で起動され、親の終了とともに終了する `arbd` だけが `true`。古い `arbd` は返さず、クライアントは `false` とみなす。
- `system.shutdown_for_handover` は非 system RPC・クラッシュ自動保存と排他的に引き継ぐ。親付き daemon は、起動した IDE が子の環境変数 `ARB_HANDOVER_OWNER` に設定したランダム値と RPC 引数 `owner` の一致も要求する。処理中、開いている映像 viewport（Studio のタブ）がある場合、または所有確認に失敗した場合は `{"ok":false}`（タブを閉じると受理される）。受理時は次の保存の受付を閉じて通常終了し、`{"ok":true}` を返す。旧版の method not found では、所有者が停止するまで更新を延期する。
- `timeline.query_logs` と MCP の `arb_query_logs` は省略可能な `after_seq` を受け付ける。指定時は同じ条件のうち、指定した番号より後の行を `seq` 順に返す。これにより同一時刻のログも分割して取得できる。
- `bundle.list`（および `bundle.trash` / `bundle.restore`）は各 `BundleEntryView` に実ディスク状態 `disk_state`（`present` / `missing` / `storage_unavailable`、旧 `arbd` 省略時は `present` 扱い）を非破壊の純粋読取で付与する。`bundle.forget`（CLI: `bundles forget <ID>|--all-missing`、MCP: `arb_forget_bundle`）はディスクを一切触らず `missing` かつリース・保存ジョブ 0 件の行のみカタログから解除する。`bundle.gc`（CLI: `bundles gc [--now]`、MCP: `arb_gc_bundles`）は省略可能な `ignore_grace`（既定 `false`）を受け付け、`true` 指定時のみ `gc_grace_secs` を 0 として管理下 v2 の即時物理削除を行う（リース・保存ジョブ・管理域外・旧形式保護は維持）。
- **高帯域データプレーン**: ビューポート単位のトリプルバッファ共有メモリ（mmap）フレームリングで 1080p30 RGBA/BGRA を配信する。スロットヘッダには `generation`, `frame_index`, `is_approximate`, `width`, `height`, `stride`, `pixel_format`, `boottime_ns` を持ち、`is_approximate == true` の間は UI に近似プレビューバッジを必ず表示する。

## 3. GPL 非依存メディア処理とフレーム単位トリム（`arb-media`）

- **純 Rust ISO-BMFF コンテナ層（スパイク S1 確定）**:
  - 異種プロファイル結合（例: OpenH264 Constrained Baseline 先頭 GOP ＋ ストリームコピー High/CABAC 後続 GOP）における全デコーダ互換のため、**単一 `stsd` `avc1` ＋ IDR 境界でのインバンド長さプレフィックス付き SPS/PPS 挿入** を採用する（二重 `stsd` は VideoToolbox で失敗、`avc3` は Apple `AVAssetReader` で失敗することを S1 で確認済み）。
  - `edts` / `elst`（Edit List）に一切依存せず、開始前フレームを符号化データ自体から完全除去（0 枚）し、faststart 配置（`moov` 前置）で出力する。
- **`VideoCodecBackend` 抽象とバックエンド構成（スパイク S1 確定）**:
  - **ユニバーサル 3 OS ベースライン**: `openh264`（`BSD-2-Clause`、1 フレーム内部 DPB 遅延は `flush_remaining()` / 1 パケット先読みで補正）。
  - **macOS デフォルト HW バックエンド**: `VideoToolbox` / `CoreMedia` / `CoreVideo` 直接 C FFI（外部クレート 0、ゼロコピー `CVPixelBuffer` 復号→再エンコード、障害時は OpenH264 へ自動フォールバック）。
  - Windows / Linux x86_64 向け HW バックエンド（Media Foundation / VA-API）は OpenH264（`nasm` 有効）実測後に必要時のみ追加する。
- **体感性能設計**:
  - **外部フィルムストリップキャッシュ**（`cache/<bundle-id>/filmstrip.bin`）は MP4 全体の SHA-256 を照合する。形式が古い場合や内容が変わった場合はキーフレームのサムネイルを再生成する。全体のハッシュはストリーミングで計算し、ファイル全体をメモリに載せないが、キャッシュ命中時も MP4 全体の読み取りが要る（およそ 0.5 GB/s 以上の実測。数百 MB の保存物で最大 1 秒前後、ストレージの読み取り速度に依存）。並列復号によるオープンの目標は 1 秒以内で、大きな保存物では実機計測で確認する。
  - **GOP-LRU 復号済みフレームキャッシュ**＋最新優先キャンセルで任意シーク 0.2〜0.5 秒以内、前後 1 フレームコマ送りの遅延ゼロを達成。
  - **フレーム単位トリム**: `[in_frame_index, out_frame_index]` 契約で先頭が非キーフレーム時のみ先頭 GOP 残余を新規 IDR から再エンコードし、後続 GOP は無劣化ストリームコピー。関連トラックは `arb-core::selection` の同一 3 値選別エンジンで抽出し元バンドルを一切変更しない。


### Studio の連続再生と接続復旧

- 表示用時計は単調時計で進み、静止区間は次の PTS まで同じフレームを表示する。`media.open_viewport` の任意フィールド `last_frame_duration_ns` は MP4 の最終サンプル長を返す。旧デーモンでは直前のフレーム間隔を使用する。
- 再生タブごとに専用 IPC 接続と SHM viewport を保持する。切断または viewport 失効時は一度だけ開き直して同じ位置をシークする。復旧後も選択範囲と速度を保持する。再生継続が困難なときはフレーム番号と理由を表示する。
- 解像度と GOP 長から復号画像がキャッシュ上限を超える場合は、先頭 IDR から順次復号し、上限内の直近フレームを再利用する。
