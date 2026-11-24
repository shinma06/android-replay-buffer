---
id: VER-11
type: verification
name: v2 3 OS CI・リリースパッケージ・Devin マルチ OS テスト計画・職場実機スモーク検証結果
verifies:
  - SPEC-04
  - REQ-05
  - REQ-06
  - REQ-08
  - REQ-11
  - REQ-14
---

# VER-11: v2 CI と Devin マルチ OS 検証計画

記録日: 2026-11-23

## 1. 成果物

| 成果物 | 内容 |
| --- | --- |
| `.github/workflows/v2-ci.yml` | `rust-3os-parity`（Ubuntu/macOS/Windows。Linux・Windows は `nasm` を導入し openh264 の x86_64 SIMD を有効化。fmt / clippy `-D warnings` / test / `cargo doc` `-D warnings` / ライセンス検査 / リリースバイナリ `arbd` `arb-cli` `arb-tray` を OS・arch 別にアップロード）、`sara-and-traceability`（`sara check --strict` と REQ 14/14 下流被覆）、`device-agent-host-tests`（固定 android-37.2 + `ProtocolTest: ALL 19 TESTS PASSED`）、`ide-plugin-tests`（`ide-plugin/gradlew` が無ければスキップ。ある場合は `arbd` を併せてビルドし `./gradlew test`） |
| `core/scripts/devin_verify_player_matrix.py` | 決定論的 FHD30 フィクスチャ生成、または `arb-cli trim [47..=647]` / 既存 MP4 に対し 601 フレーム・0 `edts`/`elst`・faststart・負 PTS/DTS 0・先頭/末尾 PNG を検査し JSON/Markdown レポート出力。`--self-test` あり |
| `docs/testing/devin-multi-os-test-plan.md` | Devin Pro 向け Suite A（OS 基盤）/ B（性能バー）/ C（プレイヤー・GUI 動画）と提出形式 |
| `docs/testing/work-device-smoke-checklist.md` | 約 2 分の職場実機スモーク 6 項目 |
| `docs/testing/first-run-os-permissions.md` | 有料署名なし方針（REQ-06）に沿った Gatekeeper / SmartScreen 案内 |

## 2. 検証済み・未検証

- 検証済み（macOS arm64、ローカル）: ワークフロー YAML 構文、`devin_verify_player_matrix.py --self-test`、生成フィクスチャに対する `--mp4` 検査（全 check PASS）、`core/scripts/verify.sh` の 6 ゲート。
- 未検証（理由）: GitHub Actions 上での実行（push 禁止のため）、Windows の `choco install nasm` 実走、Linux/Windows でのスクリプト実行、プレイヤー動画・GUI 収録（Devin 実行待ち）、`ide-plugin-tests`（W8 未マージ）、実機スモーク（オーナーが職場で実施）。
