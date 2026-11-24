---
id: SPEC-02
type: spec
name: 二層時刻モデル・3値選別エンジン・WORM バンドル v2 と旧形式読取互換
satisfies:
  - REQ-01
  - REQ-02
  - REQ-03
  - REQ-04
  - REQ-07
  - REQ-08
  - REQ-10
  - REQ-12
---

# SPEC-02: 二層時刻モデル・3値選別エンジン・WORM バンドル v2 と旧形式読取互換

## 1. 二層時刻モデル（`arb-core::time` / `arb-os::clock`）

- **生観測と導出アライメントの分離**: 端末ネイティブ時刻・時計種別（`ClockKind`）・意味ラベル（`TimestampSemantics`）・PC 受信時刻は決して上書きしない。
- **OS 二重時計（`arb-os::clock`）**:
  - スリープ中停止する `PcActiveMono` とスリープ中も進行する `PcContinuousMono`、および表示用 `PcWallEpoch` を提供する。
  - **macOS 実装契約（スパイク S2 確定）**:
    - `PcActiveMono` → `clock_gettime_nsec_np(CLOCK_UPTIME_RAW)`（42 ns 分解能、`mach_absolute_time` と一致）
    - `PcContinuousMono` → `clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW)`（42 ns 分解能、`mach_continuous_time` と一致）
    - `CLOCK_MONOTONIC` は NTP/adjtime 周波数調整（実測 `-4.365 s` 乖離）と 1000 ns 粒度を持つため `CLOCK_UPTIME_RAW` とのスリープ差分計算に **使用禁止** とする。
- **時計アンカーと不連続検出**:
  - 端末内 3 時計トリプル `{ boot_id, boottime_ns, monotonic_ns, realtime_ns }` により `DeviceReboot`（`boot_id` 変化）、`DeviceSleep`（`boottime - monotonic` 差分 > 50 ms）、`DeviceWallJump`（`|Δ(realtime - boottime)|` > 50 ms）を検出する。
  - PC↔端末同期プローブ（1〜2 秒周期）によりオフセットと誤差半幅 `rtt_ns / 2`、および `PcSleep` を検出する。
- **3 段階の時刻確度（`TimeAlignment`）**: 同一 `boot_id` 内の `DeviceBoottime` を基準軸とし、`Anchored { boottime_ns, half_width_ns, semantics }` / `Bounded { min_boottime_ns, max_boottime_ns, reason }` / `Unanchored { pc_ingest_cont_ns, reason }` のいずれかに写像する。

## 2. 保存・トリム共通の 3 値選別エンジン（`arb-core::selection`）

- **保存ウィンドウ**: トリガ時刻 `T_trigger` から実経過時間 `D` 秒前の `[W_start, W_end]` を対象とし、途中のスリープ・切断・再起動は欠損／不連続区間としてそのまま保持する。
- **トリムウィンドウ**: 浮動小数秒ではなく必ず整数 `[in_frame_index, out_frame_index]` から元バンドルの正確な `boottime_ns` 区間を引く。
- **共通分類関数 `classify_record(record, window) -> Membership`**:
  - `InRange`: `Anchored` 誤差区間がウィンドウと交差、または `Bounded` 区間が完全包含。
  - `OutOfRange`: 完全に範囲外（遅延到着の古いレコードを確実に除外）。
  - `UncertainTime`: `Bounded` の部分交差、または `Unanchored` / 別 `boot_id`。主タイムラインへは絶対に混ぜず、PC 受信近傍 `[W_ingest_start - margin, W_ingest_end + margin]` 内のものを件数・容量上限付きで `unanchored` サイドカーへ隔離収録する（上限超過は `unanchored_truncated_count` に明記）。トリム時も元バンドルの `unanchored` を無断削除せず引き継ぐ。
- **境界ドレイン**: 保存トリガ後、最大 `drain_timeout_ms`（初期 500 ms）または各アクティブストリームの `T_trigger` 以降ハートビート通過まで待機し、タイムアウト時は `tail_may_be_incomplete = true` を明記する。

## 3. 3 ゾーンストレージと WORM バンドル v2（`arb-storage`）

- **Zone 1（Live Ring Store）**: 追記専用チャンク（デフォルト上限 `1.5 GiB` / 目安 `10 min`）。ホスト空き容量が `3 GiB` を下回ったら `StoragePressureStop` を記録して新規取得のみ停止し、Pin 済みチャンク・確定バンドルは一切削除しない。保存要求時は即座に対象チャンクを `save_jobs/<job-id>.json` に Phase-1 Pin する。
- **Zone 2（WORM Bundle v2）**: 同一ファイルシステム上の `.staging-<job-id>/` に全ファイル書き出し・`fsync` → `manifest.json`（`schema_version: 2`, `files` SHA-256, `epochs`, `streams`, `unanchored_summary`）書込 → 原子リネーム（Windows では一時ロックへの指数バックオフリトライ＋コミットマーカー検査）で確定。確定後は完全不変（`manifest.json`, `video.mp4`, 任意の `audio.m4a`, 読取専用 `timeline.sqlite`）。
- **Zone 3（Catalog / Lease / GC）**: `catalog.sqlite`（WAL）で `bundles` / `leases` / `save_jobs` / `edit_sessions` を管理。削除は論理退避（`trashed`）のみ行い、物理 GC は `active leases == 0` かつ猶予経過時のみ実行する。

## 4. 旧保存物の読取専用互換（`LegacyBundleReader`）

- 共通トレイト `BundleReader` を `V2BundleReader` と `LegacyBundleReader` で実装する。
- `LegacyBundleReader` は元フォルダを一切変更せずに以下を読み取り、共通の `TimelineView` へ投影（および必要に応じ v2 への非破壊トリム書き出し）する:
  - **Schema 4（`develop` 系統）**: `replay.json`（`schema: 4`）、`video.mp4`、`video-frames.jsonl`、`logcat-device.jsonl`、`logcat-app.jsonl`、任意の `touches.jsonl`、および **ログ単体保存（`-logs`）**。
  - **Schema 3（`main` / `v0.2.0` 系統）**: `session.json`、`video-001.mp4`、`frames.jsonl`、`logcat-device.jsonl`、`logcat-app.jsonl`。

## 4. トリムバンドルの共有用ファイル

`export.trim` は明示された出力先、設定済みの保存先、管理バンドルディレクトリの順に保存先を選ぶ。`logcat.log` と `events.jsonl` は選択区間が空でも作成し、元の記録からアプリを特定できるログは `app-logs/<package>.log` にも出力する。`unanchored.log` は対象行がある場合に作成する。すべてのサイドカーはマニフェストに SHA-256 を記録する。

`include_share_mp4` は音声付きの共有動画（音声欠落時は映像のみ）を追加する。`burn_taps` が有効な場合は `share-taps.mp4` にタップ・スワイプを描画する。元のバンドルは読み取り専用として扱い、`trash_original` は書き出し成功後に管理対象の元バンドルを論理ゴミ箱へ移す。旧形式の元バンドルにこの指定を行うと書き出し前にエラーを返す。
