---
id: VER-29
type: verification
name: T-7B 3 OS の IPC/SHM メモリ安全・Windows 名前付きパイプ所有者 SID 検証・期限付き drain・内容ダイジェストによる arbd 自動更新・リリース候補タグ分離・メニューバー改善の検証記録
verifies:
  - SPEC-01
  - SPEC-03
  - SPEC-04
  - REQ-05
  - REQ-06
  - REQ-08
  - REQ-09
  - REQ-11
---

# VER-29: T-7B IPC/SHM セキュリティ・期限付き drain・内容ダイジェスト自動更新・RC タグ分離・メニューバー改善 検証記録

記録日: 2026-11-24（macOS arm64 / JDK 27 / Rust workspace `core/`、Windows・Linux は `x86_64-pc-windows-msvc` / `x86_64-unknown-linux-gnu` 向け `cargo clippy` と GitHub Actions `os-parity` で確認）

対象は昇格前監査の H6・H7・M4・M9・M12 と、それに付随する H8（VFR 再生）の wire モデル、常時録画（`capture_enabled`）の wire フィールド、macOS メニューバー（`arb-tray`）の実利用不具合 2 件、Dock 非表示、トレイの多重起動防止。

## 1. 修正と回帰試験

| 項目 | 修正 | 回帰試験（失敗先行の根拠） |
| --- | --- | --- |
| H6 SHM の同時読み書き | `arb-os/src/shm.rs` のスロットメタデータを `AtomicU64` / `AtomicI64` / `AtomicU32` に、ペイロードを `AtomicU64` ワード + `AtomicU8` 端数バイトの `Relaxed` アクセスに置換。既存の `Acquire` / `Release` 世代シーケンスロックで囲む。バイト配置は不変（オフセットをコンパイル時 assert で固定）なので Kotlin の `ShmFrameRingReader` と互換。発行・読取の本体を `RingView` に集約し、同一割当で検査できるようにした | `same_allocation_concurrent_publish_and_read_is_race_free_and_untorn`: 同じ割当上で writer と reader を並行実行。Miri（`cargo +nightly miri test`、データ競合検出 + Stacked Borrows）で修正前の非 atomic コピーを再現すると **Data race detected**、修正後は **PASS**（111 秒）。`writer_wrapping_whole_ring_mid_copy_never_returns_torn_frame`: コピー途中で writer がリング全体を一周 → 1 回だけ再試行し最新フレームを無破損で返す。`writer_lapping_every_attempt_yields_contention_error_not_torn_frame`: 毎回追い越されると破損フレームではなくエラーを返す。`atomic_payload_round_trips_all_tail_lengths` |
| H7 Windows パイプ所有者検証 | `ide-plugin/client-core` の Windows トランスポートを `RandomAccessFile` から JDK 標準 FFM（`java.lang.foreign`）の Win32 呼出しに変更。`CreateFileW(SECURITY_SQOS_PRESENT \| SECURITY_IDENTIFICATION)` で開き、`GetKernelObjectSecurity` + `GetSecurityDescriptorOwner` の所有者 SID を `TokenUser` SID と照合してから最初の RPC を送る（`arb-os/src/ipc.rs` と同じ規則）。Win32 呼出しは `WindowsPipeOpener` / `WindowsPipeHandle` / `WindowsPipeSecurityVerifier` に抽象化。JNI・JNA・第三者コードなし | `WindowsPipeSecurityTest`（全 OS）: 所有者 SID 不一致でハンドルを即時クローズし **送信バイト 0**、検証不能時も送信 0 で失敗、一致時のみ NDJSON 送受信、`ERROR_PIPE_BUSY` 再試行。Windows 実機 CI（`os-parity (windows-latest)` の `:client-core:test`）で `ArbdRealIntegrationTest` が実 `arbd` の名前付きパイプへ所有者検証付きで接続し、`win32CurrentUserSidIsAValidSidOnWindows` が実 FFM 経路を検査 |
| M4 Windows drain の無期限待ち | `LocalIpcStream::drain()` を既定 2 秒（`DEFAULT_IPC_DRAIN_TIMEOUT`）の期限付きにし、`drain_with_timeout(Option<Duration>)` を追加。Windows では `FlushFileBuffers` を複製ハンドル上の補助スレッドで実行し、期限で `CancelSynchronousIo`。解除できない場合は切り離し、次回の drain か切断時に回収する。完了済みの OVERLAPPED 書込みは drain を必要としない | `drain_returns_within_deadline_when_peer_stops_reading`（3 OS 共通）: 読まないクライアントを残しても drain が期限内に戻り（監視スレッド付きで、修正前の Windows では無期限待ちが失敗として検出される）、応答は失われず、クライアントの読取り再開後の drain は成功する |
| M9 同サイズ・同時刻の更新 | `ArbdProcessManager` が `system.hello` の `build_id` 末尾 `:sha256:<hex>`（実行中 `arbd` の SHA-256）を解析し、同梱 `arbd` の SHA-256（パス・サイズ・mtime・ファイルキーで一度だけ計算してキャッシュ）と照合。ダイジェストを返さない旧 `arbd` に限りサイズ・mtime 比較へ縮退。1 セッション 1 回の再起動制限は維持。`package_distribution.py --self-test` に、固定時刻 ZIP では同サイズの別ビルドがサイズ・時刻で区別できず SHA256SUMS では区別できることの検査を追加 | `samePathSameSizeSameMtimeDifferentBytesTriggersExactlyOneRestart`: 同一パス・同サイズ・同 mtime・内容違いの 2 ビルドで、従来のサイズ・mtime 判定は再起動しない（前提を assert）一方、ダイジェスト照合で **再起動ちょうど 1 回**。`digestMismatchThatSurvivesRestartIsStillLimitedToOneRestart`、`differentPathWithIdenticalDigestNeverRestartsEvenIfBundledIsNewer`、`parsesContentDigestBuildIdWithAndWithoutLegacyPrefix` |
| M12 リリース候補タグ | `release.yml` のタグ判定を `package_distribution.py --classify-tag` に移し、`vX.Y.Z` は `--latest`、`vX.Y.Z-alpha.N` / `-beta.N` / `-rc.N` は `--prerelease --latest=false` で公開。それ以外（`-dev.1`、`+build`、先頭ゼロなど）は公開前に失敗 | `package_distribution.py --self-test`: `v2.0.0` → 正式版、`v2.1.0-rc.1` → プレリリース、不正タグ 9 種を拒否、`GITHUB_OUTPUT` 書式を検査。修正前の正規表現は `v2.1.0-rc.1` を正式版として `gh release create` に渡していた |
| H8 VFR の wire モデル | `MediaOpenViewportResponse.framePtsNs: List<Long>`（`frame_pts_ns`、旧 `arbd` では空リスト）。必須項目を持つ data class では Gson が Kotlin 既定値を無視して `null` を入れるため、専用デシリアライザで解析 | `mediaOpenViewportParsesFramePtsNsForVariableFrameRate`、`mediaOpenViewportWithoutFramePtsNsDefaultsToEmptyListForOlderDaemons` |
| 常時録画の wire フィールド | `ArbConfig.captureEnabled`、`ArbConfigPatch.captureEnabled`、`SystemStatusResponse.captureEnabled`（いずれも旧 `arbd` 互換の既定値） | `captureEnabledRoundTripsInConfigPatchAndStatus` |
| メニューバーの緑の四角 | `badge_icon` を 44 px（Retina `@2x`）の透過背景・アンチエイリアス付き円形リプレイ矢印に変更。状態ドット: 録画中 緑、再接続中 橙、保存要対応 黄、容量逼迫 赤。待機中・デーモン停止は macOS のテンプレート画像（明暗メニューバーに自動追従、停止時は淡色） | `badge_icons_are_transparent_replay_glyphs_not_solid_squares`（四隅とリング内側が `alpha == 0`、前景画素あり、塗りつぶしでない、輪郭がアンチエイリアス）、`each_badge_renders_a_distinct_icon_with_template_only_for_neutral_states` |
| メニューが勝手に閉じる | `TrayUi` が表示中のメニュー・アイコン・ツールチップを保持し、変化がなければ呼ばない。構造が同じ更新（2 秒ごとの秒数など）は既存項目の `set_text` / `set_enabled` で反映。ルートメニューは作成時に 1 度だけ設定し、以後は `set_menu` を呼ばない。構造変化・アイコン・ツールチップの更新は、macOS でメニュー表示中（`NSEventTrackingRunLoopMode`）なら閉じるまで延期 | `repeated_polls_with_ticking_counters_never_swap_the_menu_or_icon`、`structural_changes_rebuild_items_but_are_deferred_while_menu_is_open`、`menu_plan_classifies_label_enabled_and_structure_changes` |
| 常時録画の表示 | `arb-tray` のメニューに `常時録画: ON・REC` / `ON・待機中` / `OFF` を表示（`system.status.capture_enabled`、なければ `config.capture_enabled`。どちらもない旧 `arbd` では行を出さない） | `capture_enabled_is_parsed_from_status_with_config_fallback_and_rendered` |
| Dock 非表示 | macOS で `ActivationPolicy::Accessory` を設定し、Dock と Cmd+Tab に出さない（メニューバーのアイコンとメニューは操作可能） | macOS 実機 GUI での目視は未実施（§3） |
| トレイの多重起動 | `arb-tray` が `<runtime_dir>/arb-tray.lock` を `arbd.lock` と同じ `SingleInstanceLock` で保持し、2 つ目は終了コード 0 で終了 | `second_tray_instance_is_rejected_until_the_first_exits` |

## 2. ローカル検証結果

- `./core/scripts/verify.sh`: PASS（fmt / clippy `-D warnings` / test / GPL・AGPL ゼロ / `sara check --strict` / REQ カバレッジ）。
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`: PASS。
- `cargo clippy -p arb-os -p arb-tray --all-targets --target x86_64-pc-windows-msvc -- -D warnings` と `--target x86_64-unknown-linux-gnu`: PASS（Windows 専用の drain 実装と各 OS のトレイ実装を型検査）。
- Miri（nightly、`shm::tests::same_allocation_concurrent_publish_and_read_is_race_free_and_untorn`）: 修正後 PASS。修正前の非 atomic コピーを同テストで実行すると Data race detected。
- `(cd ide-plugin && ./gradlew test buildPlugin --no-daemon)`（JDK 27）: PASS。
- `python3 core/scripts/package_distribution.py --self-test`: PASS。
- `.github/workflows/release.yml`: YAML として読み込めることを確認。

## 3. 未検証

- Windows 実機での名前付きパイプ所有者検証・期限付き drain は GitHub Actions `os-parity (windows-latest)` の結果で確認する（ローカルは macOS のみ）。
- macOS 実機メニューバーでの目視（アイコンの見え方、メニューを開いたまま 2 秒以上経っても閉じないこと、Dock 非表示）は、GUI 操作ができないこの環境では未実施。判定ロジックはヘッドレスの単体試験で検証した。
- `arbd` 側の `build_id` の `:sha256:<hex>` 付与、`frame_pts_ns`、`capture_enabled` は並行タスク T-7A の担当。本変更は旧 `arbd` 互換の既定値で受ける。
- `arbd` の接続ループは応答ごとに `drain()` を呼んでいる（T-7A の担当範囲）。本変更で drain が期限付きになったため、読まないクライアントがいても期限で先へ進む。
- 実 Android 端末・Android Studio GUI・エミュレータでの受け入れは未実施。
