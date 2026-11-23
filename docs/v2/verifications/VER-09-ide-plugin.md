---
id: VER-09
type: verification
name: 薄い Android Studio / IntelliJ IDEA プラグイン（ide-plugin/）IPC・SHM 結合およびヘッドレス JUnit 検証結果
verifies:
  - SPEC-01
  - SPEC-03
  - REQ-01
  - REQ-02
  - REQ-03
  - REQ-07
  - REQ-09
  - REQ-11
  - REQ-12
---

# VER-09: `ide-plugin/` 薄い IDE プラグイン IPC / SHM 結合検証結果

記録日: 2026-11-23（macOS arm64 / Gradle 9.8.1 / Kotlin 2.4.21 / Android Studio SDK `2026.2.2.8` / JBR 25.0.4 / 実 `arbd` バイナリ結合）

## 1. 実装対象とアーキテクチャ境界（`SPEC-01`, `SPEC-03`）

旧 `plugin/` は一切変更せず参照専用として残し、新規ディレクトリ `ide-plugin/` に **純 Kotlin のテスト可能コア（`:client-core`）** と **薄い IntelliJ Platform / Android Studio UI シェル（ルートプラグイン）** を分離構築した。時刻計算・3 値選別・リング退避・ロック・MP4 インデックス／トリム・カタログ GC 等の業務ロジックは一切重複実装せず、すべてローカル IPC / SHM 経由で `arbd` へ委譲する。

| モジュール / パッケージ | 役割・主な実装内容 |
| --- | --- |
| `ide-plugin/client-core` (`model/`) | `core/crates/arbd/src/protocol.rs` と完全一致する全 28 JSON-RPC 2.0 DTO・タグ付き列挙（`Provenance`, `TimeAlignment`, `AdbDeviceState`, `DiscontinuityKind`, `LogCompleteness`, `AvOffsetStatus` 等）と Gson `LOWER_CASE_WITH_UNDERSCORES` + 階層アダプタ (`ProtocolJson`)。 |
| `ide-plugin/client-core` (`ipc/`) | 単一 OS 検出点 `HostOs`、`arb-os/src/paths.rs` と一致する `ArbPaths`（`ARB_*_DIR` 優先・macOS/Linux UDS・Windows SID バイナリ変換 + FNV-1a Named Pipe）、外部 JNI ゼロの `PosixUnixSocketTransport`（Java 16+ `UnixDomainSocketAddress` + `SocketChannel`）と `WindowsNamedPipeTransport`（`RandomAccessFile` + busy リトライ）、4 MiB 上限 NDJSON フレーミング、`ArbdRpcClient`（全 `system.*`, `device.*`, `capture.*`, `save.*`, `bundle.*`, `timeline.*`, `edit.*`, `export.*`, `media.*`）、`ArbdProcessManager`（`ARB_DAEMON_PATH` → `bin/<os>-<arch>/arbd` → `core/target/{debug,release}/arbd` → `PATH` + Lazy Spawn + `system.hello` 照合）、`BundleLeaseController`（タブ開放中の定期 `bundle.lease_renew` ハートビート + 閉鎖時の `media.close_viewport` / `bundle.close` 冪等解放）。 |
| `ide-plugin/client-core` (`shm/`) | `core/crates/arb-os/src/shm.rs` とバイト単位で一致する `ShmFrameRingReader`（`MappedByteBuffer` Little Endian + `VarHandle` Acquire セマンティクスによる 64 リトライ seqlock、`ARBSHM01` マジック・バージョン・スロット数検証、`FLAG_IS_APPROXIMATE` 抽出、`RGBA8`/`BGRA8`/`RGB24`/`NV12` → `BufferedImage` 変換）。 |
| `ide-plugin/client-core` (`state/`) | 純 Kotlin ビューモデルとレデューサ（`CaptureBarReducer` による `STORAGE_PRESSURE_STOP` と `CONNECTION_LOST` の厳密分離、`FailedSaveRecoveryReducer` による再試行／保存先変更／破棄、`BundleCatalogState` の `Legacy (RO)` バッジとリース保護表示、`TimelineViewerReducer` の `近似プレビュー（位置確定前）` バッジ制御・整数フレーム範囲クランプ・`main`/`unanchored` 厳密分離と `truncated_count` 通知、`DiscontinuityFormatter` / `ProvenanceFormatter`）。 |
| `ide-plugin` (`ide/`, `ui/`, `META-INF/plugin.xml`) | Android Studio `2026.2.2.8`（`sinceBuild = "262.9437.185"`, `untilBuild = "262.*"`）向け薄い UI シェル。`ArbdDaemonService`、`ARB v2` ToolWindow（`CaptureBarPanel` / `FailedSaveRecoveryPanel` / `BundleCatalogPanel`）、`ArbBundleFileEditor`（タブ開閉連動の Viewer Lease・SHM ビューポート・編集セッション永続化）、`TimelineViewerPanel`（`VideoViewportCanvas` 近似プレビューバッジ・フィルムストリップ・`-1`/`+1` ステップと整数 In/Out トリム・PNG 出力・`DiscontinuityTimelineBar`・誠実な来歴パネル・主タイムラインと「時刻不明の記録 (Unanchored)」タブ分離）。 |

## 2. Gradle テストおよび実 `arbd` UDS + SHM 結合検証結果

実行コマンド: `JAVA_HOME="/Applications/Android Studio.app/Contents/jbr/Contents/Home" ./gradlew test buildPlugin --no-daemon`（`ide-plugin/` 配下）

- **全 24 テスト PASS（失敗 0 / スキップ 0）**
- **`:buildPlugin` PASS**（`ide-plugin/build/distributions/arb-ide-plugin-2.0.0-SNAPSHOT.zip` 生成成功）

| テストスイート | 件数 | 主な検証内容 |
| --- | ---: | --- |
| `ProtocolJsonTest` | 5 | `Provenance` / `TimeAlignment` / `AdbDeviceState` の Rust `serde` 互換、`ArbdRpcClient` の JSON-RPC 2.0 成功・エラー・プロトコルバージョン不一致検出、NDJSON 改行混入・超過長フレーム拒否とストリーム再同期。 |
| `ArbPathsTest` | 4 | `ARB_*_DIR` 優先、macOS / Linux XDG 既定パス一致、Windows テキスト SID → Win32 バイナリ変換と FNV-1a 64-bit Named Pipe 決定性。 |
| `ArbdProcessManagerTest` | 1 | `ARB_DAEMON_PATH` > バンドル `bin/<os>-<arch>/arbd` > 開発 `core/target/debug/arbd` の解決優先順位。 |
| `BundleLeaseControllerTest` | 1 | 定期 `bundle.lease_renew` ハートビート送信と、`close()` 時の冪等な `media.close_viewport` → `bundle.close` 解放および以降のタイマー停止。 |
| `ShmFrameRingReaderTest` | 3 | 空リング `null`、`RGBA8`/`BGRA8` パディング付き `stride` デコードと `isApproximate` ビット判定、並行ライター下での seqlock 非ティアリング（200 回連続一貫読取）、不正マジック・切り詰めファイル拒否。 |
| `ArbViewModelsTest` | 6 | スクラブ時 `近似プレビュー（位置確定前）` バッジ点灯 → `seek_frame` 確定時のみ消灯、`STORAGE_PRESSURE_STOP` と `CONNECTION_LOST` の分離、`main`/`unanchored` ログ分離と `truncated_count` 通知、誠実な来歴・不連続フォーマット、失敗保存リカバリと `Legacy (RO)` バッジ、整数フレーム範囲クランプ。 |
| `ArbdRealIntegrationTest` | 1 | **実 `arbd` バイナリ（`core/target/debug/arbd`）を隔離 `ARB_DATA_DIR` / `ARB_CACHE_DIR` / `ARB_RUNTIME_DIR` で Lazy Spawn し、実 Unix Domain Socket + 実 SHM mmap 経由でフルラウンドトリップ検証**: `system.hello` / `system.status` → 実リポジトリ Legacy Schema-4 フィクスチャ（`replay-20240101-000000-manual-feedface`）の `bundle.open` (`is_legacy=true`)・`timeline.summary`・`timeline.query_logs`・SHM `scrub` (`isApproximate=true`) → `seek_frame` (`isApproximate=false`) → 実 WORM v2 バンドルの `bundle.open` (`is_legacy=false`)・`timeline.discontinuities`・`main`/`unanchored` ログ分離・`edit.update_session`/`get_session`・SHM `scrub` (`isApproximate=true`) → `seek_frame` (`isApproximate=false`, `frameIndex=7`, 非空 RGBA8 → `BufferedImage`) → 中間 GOP `export.trim` (`[2..18]`, `outputFrameCount=17`, `edts=0`, `elst=0`, `unanchored` 引継ぎ) → `export.png` (PNG シグネチャ確認) → `bundle.lease_renew` + `bundle.trash` + `bundle.gc`（アクティブ Lease 保護確認）+ `bundle.restore` → `BundleLeaseController.close()` による自動ビューポート／リース解放 → `system.shutdown` 正常終了。 |
| `ArbUiComponentsTest` | 3 | ヘッドレス Swing での `VideoViewportCanvas` 近似プレビューバッジ切替、`TimelineViewerPanel` の主タイムライン / 「時刻不明の記録 (Unanchored)」タブ分離・省略バナー表示、`CaptureBarPanel` / `FailedSaveRecoveryPanel` / `BundleCatalogPanel`（`Legacy (RO)` 表示）の描画検証。 |

## 3. 未検証事項（Gate G3 残件）

- **実 Android Studio GUI 上での手動 / Devin 画面受入検証（Gate G3）**: 本検証はヘッドレス JUnit 5 および実 `arbd` IPC / SHM 結合・`buildPlugin` パッケージ生成までを実施済み。実 Android Studio `2026.2.2.8` プロセスへプラグイン ZIP をインストールしたライブ GUI 操作（ToolWindow ドッキング、マウスドラッグによる連続スクラブ体感、実端末・エミュレータ接続時の録画〜保存〜トリム描画）は Gate G3 の残件として記録する。
- **Windows Named Pipe（`\\.\pipe\arb-daemon-*`）および Linux UDS 実機動作**: 単体テスト（パス・SID・FNV-1a・フレーム検証）までは本 macOS arm64 環境で実施済み。Windows / Linux ネイティブランタイムでの実結合は CI `os-parity` / クロス OS 検証で実施する。
