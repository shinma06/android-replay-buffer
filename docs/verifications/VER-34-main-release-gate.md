---
id: VER-34
type: verification
name: main 昇格前の正式配布ゲート（G-M1〜G-M9）検証記録
verifies:
  - SPEC-01
  - SPEC-02
  - SPEC-03
  - SPEC-04
  - REQ-05
  - REQ-06
  - REQ-07
  - REQ-08
  - REQ-09
  - REQ-11
  - REQ-14
---

# VER-34: main 昇格前の正式配布ゲート（G-M1〜G-M9）検証記録

記録日: 2026-11-25。ゲートの定義と合格基準は `docs/testing/main-release-gate.md`。この記録は Phase 10（Stage 1〜3）で実測した一次証跡の集約で、**`main` へのマージ・タグ作成・`LegacyV1SettingsMigrator.kt` の撤去は含まない**。それらはオーナーの最終 GO 後の専用 PR だけで行う。

## 1. 対象コミット

| 区分 | コミット | 備考 |
| --- | --- | --- |
| 製品コード・5 層 PDD の凍結点 | `b9d6300029e8e7f9e702969320d4bf7b7e82909f` | Stage 3 の macOS 実機検証と Linux Devin 初回検証の対象 |
| 最終配布物・可搬 Linux ビルド | `b17df28099173a282268a36ca5e2f3cf7fc90dae`（`origin/develop`） | `b9d63000` との差分は CI・ビルド補助 5 ファイルのみ（`ci.yml` / `release-gate.yml` / `release.yml` / `core/scripts/build-linux-release-container.sh` / `core/scripts/verify_release_bundle.py`）。本番の Rust・Kotlin・device-agent は 0 変更。`early` の 4 ZIP（`dev.b17df28`）は 2026-11-24T23:02:37〜39Z に公開 |
| `origin/main` | `6c84530262692b0384f4cb3ff0399d0a68a8d81c` | 未変更。オーナーの最終 GO と、`LegacyV1SettingsMigrator` の撤去（`docs/testing/main-release-gate.md` の昇格 PR チェックリスト 1）を待つ |

GitHub Actions の `release-gate.yml`（CI 経由）run **`45506749411`** は `b17df280` で `conclusion: success`（`build` 3 OS、`plugin-verifier`、`build-and-assemble (release.yml path)`、`gate-linux`、`gate-macos`、`gate-windows`、`gate-summary` がすべて成功）。以下「GHA」はこの run を指す。

## 2. ゲート判定（G-M1〜G-M9）

| ゲート | 判定 | 要点 |
| --- | --- | --- |
| G-M1 法務・ライセンス・表記 | 合格 | 4 ZIP すべてに `LICENSE-MIT`・`LICENSE-APACHE`・`THIRD_PARTY_NOTICES.md`（Cisco OpenH264 と開発専用の PDD ツールの節を含む）を同梱。`check-licenses.py` は 351 パッケージで GPL / AGPL / LGPL 0 件。`ArbBrandingGuardTest` で利用者向け表示に旧版ラベル `"v2"` が 0 件。`core/Cargo.toml`（`2.0.0-dev`）と `release.yml` のバージョン検査が一致。`sara check --strict` 合格（REQ 14 件） |
| G-M2 Plugin Verifier・ユニバーサル ZIP | 合格 | GHA の `plugin-verifier`（Android Studio `2026.2.2.8` / `AI-262.10968.63.2622.16730706`）は **Compatible**。互換性エラー 0、内部 API 使用 0、非推奨 API 0、実験的 API 2 件は既知の意図的な `VirtualFilePreCloseCheck`。ユニバーサルプラグイン ZIP に `konsist` と テスト専用 JAR は 0、3 OS のネイティブ一式と `device-agent/*.dex.jar` を確認。Windows の Gradle テストは Named Pipe 関連の skip 0（`WindowsPipeSecurityTest`、`ArbdParentBoundLifecycleTest` を含む） |
| G-M3 3 OS の配布 ZIP コールドインストール | 合格 | 下表 §3 |
| G-M4 3 OS のメディア正しさ・REQ-11・焼き込み実画素 | 合格（性能は値つきで記録） | §4 |
| G-M5 Linux 実 GUI | 合格 | §5 |
| G-M6 macOS 実機デスクトップ | 条件付き（受入条件の相違 3 件を明記） | §6 |
| G-M7 Windows 実 GUI | 合格 | §7 |
| G-M8 `release.yml` ドライラン・クロス監査・オーナー GO | **保留**（準備項目は完了、オーナーの最終 GO は未取得） | §8 |
| G-M9 v2 → v2 アップグレード・親付きライフサイクル | 合格 | §9 |

## 3. G-M3: 3 OS の配布 ZIP コールドインストールとクリーン実行

GHA の `gate-linux` / `gate-macos` / `gate-windows`（いずれも `verify_release_bundle.py` が PASS）。

| OS | セクション | チェック | 補足 |
| --- | --- | ---: | --- |
| Linux（`ubuntu:22.04` の可搬ビルド） | 12/12 | 261/261 | ELF ガード: `arbd` / `arb-tray` は最大 `GLIBC_2.35`、`arb-cli` は `2.34`、`GLIBCXX` は 3.4 以下 |
| macOS（arm64） | 12/12 | 263/263 | |
| Windows（x86_64） | 12/12 | 256/256 | |

3 OS とも、旧形式 schema 3 / 4 の読取専用（操作前後の SHA-256 が全ファイル一致）、MCP 17 ツール（標準エラー 0 B）、OS ごとのランチャー、残留プロセス 0 を確認した。

## 4. G-M4: Player Matrix・REQ-11 の体感ライン・焼き込み実画素

3 OS とも FHD30 の 1 s GOP と 5 s GOP で、フレーム範囲 `[47..=647]`（両端を含む 601 枚、GOP 途中スプライス）の Player Matrix が **17/17**。`video.mp4`、`share.mp4`、`share-taps.mp4` の各版も 17/17（`avc1=1`、`avc3=0`、`edts=0`、`elst=0`、faststart、デコードエラー 0）。`share-taps.mp4` は元の `video.mp4` と比べてタップ・スワイプの座標で画素が変化し、対照窓は変化しないことを確認した。親バンドルの SHA-256 は不変。

FHD30 の性能（単位 ms。`open` + `filmstrip` が REQ-11 のサムネイル目安 1 秒、`seek` が正確な seek の目安 0.2〜0.5 秒、`trim` が数秒の目安に対応）。

| 環境 | GOP | open | filmstrip | scrub | seek | trim | trim+burn |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Linux（GHA） | 1s | 7.7 | 78.7 | 0.49 | 35.3 | 314.8 | 9,338 |
| Linux（GHA） | 5s | 7.8 | 31.0 | 0.48 | 137.0 | 1,819 | 11,392 |
| macOS（GHA） | 1s | 15.6 | 88.7 | 0.59 | 32.4 | 275.0 | 3,354 |
| macOS（GHA） | 5s | 9.5 | 39.3 | 0.66 | 206.0 | 727.9 | 3,910 |
| macOS（ローカル Apple Silicon、`b9d6300`） | 1s | 4.4 | 40.8 | 0.16 | 12.9 | 172.5 | 2,377 |
| macOS（ローカル Apple Silicon、`b9d6300`） | 5s | 5.7 | 20.0 | 0.16 | 57.8 | 525.3 | 2,704 |
| Windows（GHA） | 1s | 14.4 | 71.7 | 0.71 | 36.0 | 307.7 | 9,169 |
| Windows（GHA） | 5s | 15.3 | 26.9 | 0.86 | 123.1 | 1,669 | 10,460 |

全環境で open + filmstrip は 1 秒以内、seek は目安内、trim は数秒以内。焼き込み付き書き出し（20 秒）は GHA の Linux・Windows で 9〜11 秒台と、macOS の 3〜4 秒台より遅い。「数秒」に厳密な上限値はなく、性能指摘として値を記録する。計測は CLI 経由のバックエンドで、各値は 1 回の実測。Studio UI の表示遅延とパーセンタイルは測っていない。

## 5. G-M5: Devin Cloud Linux の実 GUI

- 初回（`b9d63000`、[Devin セッション](https://app.devin.ai/sessions/7712a51be0e647b397ca9537736d4df4)）: Ubuntu 22.04 で `GLIBC_2.43` 依存の非互換を検出し、Trail M-3（PR #527）で可搬ビルドに修正した。あわせて実 Chrome（1920×1080、ドロップ 0、seek 0.8〜14.5 ms）、VLC 3.0.16（`rc=0`、エラー 0）、D-Bus `StatusNotifierItem`、Android Studio `2026.2.2.8` の Xvfb 実 GUI（Tool Window、旧形式の読取専用、Viewer、設定。ARB のエラー 0、通常終了 0.58 秒、`kill -9` 105.6 ms）を確認し、動画 3 本（`chrome-playback.mp4`、`vlc-playback.mp4`、`as-gui-session.mp4`）と PNG 14 枚を残した。
- 素の Ubuntu 22.04（`glibc 2.35`、Docker なし）での再検証（`b17df280`、[Devin セッション](https://app.devin.ai/sessions/3b51bbd68c9e465d97fb5055a92672bb)）: `verify_release_bundle.py` が 12/12 セクション・261/261 合格、`GLIBC_2.35` 以下を確認。実 Android Studio `2026.2.2.8` で `GLIBC_2.43` エラー 0、ARB エラー 0、`arbd --exit-on-stdin-eof` の自動起動、通常終了と `kill -9`（99.3 ms、孤児プロセス 0）を確認し、`u22-barehost-as.png` を残した。

## 6. G-M6: macOS arm64 実機デスクトップ（ローカル、`b9d63000`）

`/tmp/arb-stage3-macos/report.md` を直接読んで転記した。ローカル作業ツリーは指定コミットへ detach し、検証後の `git status --porcelain` は 0 バイト、コミット・push・リリース変更は 0。

- 配布検証 `verify_release_bundle.py`: 12/12 セクション・**263/263 合格**。実際の `24d4702 → b9d6300` のアップグレードと pin 済みジョブの retry / retarget を含む。
- API 37 エミュレータのライブ E2E: 440 Hz の AAC（48 kHz・2ch）が有音の 4/4 区間で復号一致。`getevent -p` で確認した実入力で Tap は 716 画素、Swipe は 967 画素が焼き込まれ、対照領域は 0 画素。元の `video.mp4` は検証前後で SHA-256 が一致。`logs_only` 保存、1080×2400 の PNG、MCP 17 ツール（標準エラー 0 B）を確認し、終了後の残留プロセスは 0。
- 実 `arb-tray`: `lsappinfo` で `UIElement`。透明な 44 px のリプレイ矢印で緑の四角は 0 画素。メニューを開いたまま 3 回（2 秒間隔）のアクセシビリティツリーが一致。親 `SIGKILL` からの終了は `arb-tray` 12.9 ms、`arbd` 53.5 ms。
- 再生: QuickTime 6/6、AVFoundation 6/6、隔離 Chrome 6/6。
- 隔離した Android Studio `2026.2`（JBR 25.0.4）: ARB の例外 0、親付き `arbd` の自動起動と正常終了を確認。所有者の Android Studio のディレクトリ 3 件は前後で mtime が不変。

**受入条件の相違（同レポートの「条件付き不合格」の根拠）**。いずれも製品の欠陥ではなく、依頼時の想定と実物のずれとして記録する。

| 条件 | 実測 |
| --- | --- |
| 両 ZIP に `SHA256SUMS.txt` | macOS ZIP のみ（同梱のプラグイン ZIP の分を含む）。プラグイン ZIP 単体には無い |
| 通常保存に `logs.jsonl` | 通常保存には無く、`logcat.log` と `timeline.sqlite` に保存。`logs_only` 保存には有る |
| 親 `SIGKILL` 後にソケットとロックファイルを削除 | ソケットは消えるが、ロックファイルは 2/2 残る（排他は解放され、再起動と再取得は成功。残留プロセス 0） |

## 7. G-M7: Windows x86_64 実 GUI（GHA `gate-windows`）

- Gradle の全テストが実行され、Named Pipe の所有者 SID を確認する `WindowsPipeSecurityTest` を含めて skip 0。
- `verify_release_bundle.py` は **256/256 合格**。
- `windows_gui_evidence.py` は **17/17 合格**: `arb-tray.exe` の生存と単一インスタンス、Edge での `share.mp4`・`share-taps.mp4` と `win-playback.mp4`、VLC での `share-taps.mp4`、Media Foundation（1920×1080、長さ 20 秒、再生位置 1.08 秒）。PNG 4 枚はアーキテクトが目視で確認した。

## 8. G-M8: `release.yml` ドライラン・クロス監査・`main` 準備

G-M8 の合格基準は、他ゲートが同一コミットで緑であることとオーナーの明示的な GO までを含む。**オーナーの GO は未取得で、G-M6 も条件付きのため、G-M8 は合格にしない。** 完了した準備項目と保留を分けて記す。

完了した準備項目:

- `package_distribution.py --classify-tag` は 3/3 合格（`v2.0.0`、`v2.0.0-rc.1`、不正タグの拒否）。公開操作は 0。`release.yml` 経路の `build-and-assemble` は GHA で成功。
- Phase 10 の Trail（M-1、M-2、P-1、P-2、P-3、M-3）はいずれも実装とは別のプロバイダーで監査し、未解決の High / Medium は 0。
- `LegacyV1SettingsMigrator` は Konsist KON-5 で分離を保っている（撤去の手順は下記）。

保留:

- オーナーの最終 GO。
- `main` 昇格 PR での G-M1〜G-M9 の再実行（昇格コミットでの結果と、版の引き上げ後の VER 更新）。
- **`LegacyV1SettingsMigrator` の撤去は 1 ファイルの削除では済まない。** 昇格 PR で、`LegacyV1SettingsMigrator.kt` と `LegacyV1SettingsMigratorTest.kt` の削除、`ArbStartupActivity.kt` の呼び出し 1 行と KDoc の削除、`ArbLocalSettingsState.kt` の `migratedFromV1` の削除、VER / SPEC の記述更新に加えて、KON-5 が 3 クラスの存在を固定集合で検査しているため、その更新が必要になる。
- **推奨**: `main` の ruleset の必須チェックに `os-parity (macos-latest)` と `os-parity (windows-latest)` を加える（`develop` の必須チェックと揃える）。ruleset の変更は公開設定にあたるため、この PR では行わない。

## 9. G-M9: v2 → v2 アップグレードと親付きライフサイクル

- 3 OS の GHA で U1〜U5 を確認: 異なる `arbd` ハッシュへのホットスワップ、pin 済みの失敗ジョブの retry / retarget、T-6H 以前の 5 カラム `timeline.sqlite`、`arbd` の再起動を跨ぐ `arb-tray` の再接続（3 OS の `upgrade` セクションは Linux・macOS・Windows とも 52 チェックで合格）。
- 実際の履歴リリース `24d4702 → b9d6300` のアップグレードを macOS で確認。
- 親 JVM の `SIGKILL` に相当する終了を macOS（`arbd` 53.5 ms、`arb-tray` 12.9 ms）と素の Ubuntu 22.04（99.3 ms）で実測した。詳細は VER-33。

## 10. 5 層 PDD の概要

詳細は `docs/proofs/tcb-axioms.md`、CI は `pdd-kani` run `45484610918`。

| 層 | 結果 |
| --- | --- |
| 1 Kani | 必須集合 28/28 成功（`arb-core` 16、`arb-os` 5、`arb-adb` 2、`arb-media` 5）。拡張集合 25 本は 5 分内に終わらず**未証明**と明記 |
| 2 Stateright | S1 / S2 / S3 の一意状態は合計 1,104,258、2.6 秒。`always` 性質に反例なし |
| 3 Proptest / Miri | G1〜G7 は Rust 491 テストの一部として成功。Miri 77/77（M1 5、M2 70、`mp4::` 2） |
| 4 Konsist / 構造ガード | Konsist 6/6、`check-architecture.py` は RS-1〜RS-5 違反 0、RPC 41 件がクライアントと一致 |
| 5 TCB 台帳 | 外部公理 12 件（TCB-01〜TCB-12）と、各公理の未検証境界を記録 |

## 11. 未確認の境界（任意の人手・実機での追加確認）

- 物理 USB 端末（Android 9 / API 28、Android 13 以上の OEM `MediaCodec`、実機の画面パネル・音声、Running Devices の物理ミラーリング）。手順は `docs/testing/work-device-smoke-checklist.md`。
- 署名なし配布の初回起動時の OS 警告（macOS Gatekeeper、Windows SmartScreen）。手順は `docs/testing/first-run-os-permissions.md`。
- 物理的な電源断でのディスク fsync の耐久性と、複数 IDE を同時に使う実 GUI の負荷試験。
- 本記録のとおり、Windows と Linux の GUI は CI と Devin の VM 上での確認であり、実機デスクトップではない。macOS 実機の確認は `b9d63000` で、`b17df280` ではローカルで再実行していない（差分は CI・ビルド補助のみ）。

録画、生ログ、端末診断情報は本記録に含めていない。
