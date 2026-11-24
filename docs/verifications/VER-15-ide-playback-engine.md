---
id: VER-15
type: verification
name: ide-plugin/client-core 連続再生・A/V 同期エンジンおよび SHM seqlock 競合テスト安定化検証記録
verifies:
  - SPEC-03
  - REQ-08
  - REQ-09
  - REQ-11
  - REQ-12
---

# VER-15: `ide-plugin/client-core` 連続再生・A/V 同期エンジンおよび SHM seqlock 競合修正検証記録

記録日: 2026-11-23（macOS arm64 / Gradle 9.8.1 / Kotlin 2.4.21 / JDK 21 target (`client-core`)・JBR 25.0.4）

## 1. 実装対象（`ide-plugin/client-core` — GPL/AGPL 新規依存ゼロ）

次段 T-4 の Compose / Jewel 統合ビューア＆トリムスタジオ、および T-2B の `media.decode_audio_pcm` へ摩擦なく接続できる純 Kotlin テスト可能コアを `io.github.shinma06.arb.playback` パッケージへ実装し、あわせて CI Ubuntu ランナーで間欠失敗していた SHM seqlock 競合を根本解消した。

| ファイル | 役割・主な実装内容 |
| --- | --- |
| `playback/PlaybackModels.kt` | `PlaybackMode`（`PAUSED` / `PLAYING` / `SCRUBBING`）、厳密整数比ペーシングの `PlaybackSpeed`（`HALF = 0.5x`, `NORMAL = 1.0x`, `DOUBLE = 2.0x`）、`AudioState`（`UNAVAILABLE` / `MUTED` / `PLAYING` / `PAUSED`）、整数フレーム不変条件 `0 <= inFrameIndex <= outFrameIndex < totalFrames` を強制する不変スナップショット `PlaybackState`、`FrameSeeker` / `ArbdFrameSeeker`、`PlaybackClock` / `SystemPlaybackClock`。 |
| `playback/PlaybackController.kt` | スレッドセーフかつ決定論的な連続再生・トリム制御コントローラ。実フレーム `ptsNs` 差分（未提供時は `fallbackFps`）× `PlaybackSpeed` に従って `[inFrameIndex..=outFrameIndex]` 内を単調進行し、遅延フレームはドリフト蓄積を避けてドロップ。`play` / `pause` / `togglePlayPause`、`stepFrame(-1/+1)`、`seekToFrame`、`beginScrub` / `scrubTo` / `endScrub`（`isApproximate` は `media.scrub` の近似プレビュー時のみ `true`、確定 `media.seek_frame` で必ず `false` へ復帰）、`setInFrame` / `setOutFrame`、実記録データのみに基づく決定論的範囲プリセット（`selectTapRange` / `selectAroundCrash` / `resetFullRange`）、および境界付き `join` によるリークゼロの `close()` を実装。 |
| `playback/PcmAudioSource.kt` | オーバーフロー安全な ns ↔ サンプルフレーム換算を備えた `PcmFormat`（既定 48 kHz ステレオ s16le）、`PcmAudioSource`（`sampleRateHz`, `channels`, `durationNs`, `readPcmSlice(startNs, endNs)`）、`InMemoryPcmSource`、外部依存ゼロの純 Kotlin RIFF/WAV パーサ `WavFilePcmSource`（`WAVE_FORMAT_PCM` / `WAVE_FORMAT_EXTENSIBLE` PCM、`JUNK`/`LIST` 等任意チャンクおよび奇数長ワード境界パディング対応、`FileChannel` ランダムアクセス読取・バイト配列読取・WAV エンコード補助）。 |
| `playback/AudioSyncPlayer.kt` | `AudioSink` 抽象と `javax.sound.sampled.SourceDataLine` ベースのクロス OS 既定実装 `JvmAudioSink`（ヘッドレス環境・ミキサ不在時は例外を投げず `AudioState.UNAVAILABLE` へ安全縮退）。`arb-core` / `arb-media` と同一の符号規約 `audio_pts_ns = video_pts_ns - avOffsetMs * 1_000_000` で `avOffsetMs` を誠実に適用（正のオフセット時は先頭無音パディング、負のオフセット時は先頭スキップ）。`seek(videoPtsNs)` 時の即時 `sink.flush()`、`SCRUBBING` および非 `1.0x` 倍速時（`0.5x` / `2.0x`）の自動ミュート（チップマンク音程歪み防止）、`mute` / `unmute` / `pause` / `resume`、境界付き `close()` を実装。 |
| `shm/ShmFrameRingReader.kt` & `ShmFrameRingReaderTest.kt` | Ubuntu CI 競合フレーク修正（下節 2 参照）。 |

## 2. SHM Seqlock 競合フレークの原因と修正

### 原因
- `ShmFrameRingReader.readLatest()` は全 64 回（`MAX_SEQLOCK_RETRIES = 64`）のリトライで `Thread.onSpinWait()` のみを行っていた。2 vCPU の Ubuntu CI ランナー上でライタースレッドが奇数 `seq` 保持中に OS プリエンプションされるか、無制限スピンループのテストライターが 3 スロットリングを高速周回すると、64 回のスピンが数マイクロ秒で消費し尽くされ、ライターが書き終える前に `ShmRingException("Shared-memory frame seqlock did not stabilize under contention")` が間欠発生していた。
- さらにテスト側の `writerThread` が一切 `Thread.yield()` せず 100% CPU を専有し、壁時計（`System.currentTimeMillis`）ベースで待機していた。

### 修正（ティアリング防止不変条件は一切弱めない）
- `ShmFrameRingReader.readLatest()`:
  - `MAX_SEQLOCK_RETRIES = 64` および `seq1` 偶数確認・`acquireFence`・`seq1 == seq2` 検証の不変条件は完全維持。
  - リトライ間のスクラッチ `ByteArray` を再利用して GC 圧を削減しつつ、段階的バックオフ（`0..15`: `Thread.onSpinWait()`、`16..47`: `Thread.yield()`、`48..63`: `LockSupport.parkNanos(200_000L)`）を導入。プリエンプトされたライターの完了を許容しつつ、異常停止した奇数 `seq` スロットでは約数ミリ秒以内に確実に `ShmRingException` を送出する。
- `ShmFrameRingReaderTest`:
  - `seqlockPreventsTornFramesUnderConcurrentWriter`: 初回フレーム公開を `CountDownLatch` で待機し、ライターが 8 書き込みごとに `Thread.yield()` することで真の並行書き込み競合を維持したまま 2 vCPU 飢餓を防止。単調時計（`System.nanoTime`）で 5 ラウンド × 200 回 = 計 1,000 回の連続非ティアリング読取と複数世代進行を検証。
  - `throwsWhenSeqlockRemainsStuckOdd`: 奇数 `seq` で停止したスロットに対して `ShmRingException` が確実に送出される決定論的テストを追加し、不変条件が弱まっていないことを証明。

## 3. ローカル検証ゲート実行結果

- `ide-plugin` Gradle テスト (`./gradlew test buildPlugin --no-daemon`):
  - `:client-core:test`: **43 件全 PASS（失敗 0 / スキップ 0）**
    - `PlaybackControllerTest`: 10 件（`0.5x`/`1.0x`/`2.0x` 再生・一時停止・選択範囲内ループ、遅延フレームドロップとドリフトゼロ検証、非ループ時の終端一時停止・再再生、PTS 未提供時 `fallbackFps` ペーシング、`-1/+1` コマ送り、`beginScrub`/`scrubTo`/`endScrub` の `isApproximate` 遷移・確定解除、整数トリム境界と `selectTapRange`/`selectAroundCrash`/`resetFullRange` プリセット、`AudioSyncPlayer` 連携ミュート・`avOffsetMs`・リスナ通知、実スレッド起動〜`close()` リークゼロ、空／単一フレームタイムライン境界）
    - `AudioSyncPlayerTest`: 5 件（RIFF/WAV ファイル・メモリ往復＆任意 `JUNK` 奇数長チャンクスキップ、正負 `avOffsetMs` のサンプル単位同期と `seek` 即時フラッシュ、`SCRUBBING`・`0.5x`/`2.0x`・明示ミュート時の無音化、ハードウェア不在／オープン失敗／音声なしバンドル／実 `JvmAudioSink` での `UNAVAILABLE` 安全縮退、実ポンプスレッド配信〜`close()` リークゼロ）
    - `ShmFrameRingReaderTest`: 4 件（空・近似・確定・stride デコード、5 ラウンド計 1,000 回並行 seqlock 非ティアリング、`throwsWhenSeqlockRemainsStuckOdd`、不正マジック・切り詰め拒否）
    - 既存スイート（`ArbdRealIntegrationTest`, `ArbPathsTest`, `ArbdProcessManagerTest`, `ArbdRpcClientTest`, `BundleLeaseControllerTest`, `ProtocolJsonTest`, `ArbViewModelsTest`）: 24 件全 PASS
  - ルートプラグイン `:test`（`ArbUiComponentsTest`）: **3 件全 PASS**
  - **合計 46 件全 PASS**、`:buildPlugin` プラグイン ZIP ビルド成功、Kotlin コンパイル警告 0 件。
- `./core/scripts/verify.sh`:
  - `cargo fmt --check` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace` / license check / `sara check --strict`（`VER-15` 含む） / REQ→SPEC 被覆 14/14 すべて PASS。

## 4. 明示的な未検証項目（Unverified）

- **実スピーカー出力による聴感 A/V 同期体感確認および T-4 Compose/Jewel UI シェル結合**: 本検証はヘッドレス JUnit 5（合成タグ付き PCM / WAV とフェイク `AudioSink`、および実 `JvmAudioSink` の安全な `open`/`close` 契約）までを実施。実端末録画バンドルの `media.decode_audio_pcm`（T-2B）および T-4 統合ビューア UI での実再生体感確認は後続タスクで実施する。
- **Linux / Windows 上での `client-core` 実行**: 本検証は macOS arm64 で実施。Linux（Ubuntu ヘッドレス環境での `JvmAudioSink` 縮退および seqlock 競合テスト）と Windows での実行結果は GitHub Actions `v2 CI`（全 7 ジョブ）で確認する。
