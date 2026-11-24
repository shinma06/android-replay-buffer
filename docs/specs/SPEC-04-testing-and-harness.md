---
id: SPEC-04
type: spec
name: 検証ハーネス・ローカルエミュレータ・マルチ OS 検証および品質ゲート
satisfies:
  - REQ-08
  - REQ-14
---

# SPEC-04: 検証ハーネス・ローカルエミュレータ・マルチ OS 検証および品質ゲート

## 1. ローカル Android エミュレータと決定論的障害注入ハーネス

- 日常開発では自宅に実機がない前提でも完結できるよう、ローカル arm64 FHD+ Android エミュレータ AVD **`arb_api37_arm64`**（`system-images;android-37.0;google_apis;arm64-v8a`、デバイスプロファイル `medium_phone` `1080x2400` `420dpi`、Apple Silicon Hypervisor.Framework + Metal GPU）を標準ターゲットとする。
- スクリプトはユーザーのシェル dotfiles を書き換えず `${ANDROID_HOME:-$HOME/Library/Android/sdk}` を参照する。
- `arb-core` / `arb-storage` / `arb-adb` / `arb-media` に対し、端末スリープ・PC スリープ・壁時計巻き戻り・ADB 切断・`boot_id` 変化・ディスク容量逼迫・旧形式（schema 3 / 4）を再現する決定論的モック／障害注入テストを常時実行する。
- 実機 Android（FHD 30fps）での確認は、業務環境で短時間に回せる軽量スモーク手順に分離し、未確認項目を正直に記録する。

## 2. マルチ OS 検証戦略（CI + Devin Testing & Video Recordings）

- GitHub Actions（`ubuntu-latest` / `macos-latest` / `windows-latest`）で Rust ワークスペースのビルド・単体テスト・統合テスト・ライセンス検査・`sara` 検証を 3 OS 同等に実行する。
- 3 OS での GUI 動作、IDE プラグイン表示（近似バッジ・欠損区間表示・スクラブ体感）、および主要プレイヤー（QuickTime / Windows Media Player / VLC / Chrome）でのトリム済み MP4 再生検証は、詳細なテスト仕様・検証スクリプトを用意して **Devin のマルチ OS Testing & Video Recordings** へ集約実行する。

## 3. BeautifulRust 品質ゲートと `sara` 3-Type トレースゲート

- **Rust 品質ゲート**: `morganlinton/BeautifulRust` 由来の厳格な `[workspace.lints.rust]` / `[workspace.lints.clippy]` と `core/clippy.toml`（`max-fn-params-bools = 0`、テスト内のみ `unwrap`/`expect`/`print` 許容）、および `.agents/skills/rust-best-practices` / `rust-review` をリポジトリ内へ同梱する。
- **ライセンスゲート**: `core/scripts/check-licenses.py` により全クレートの SPDX 式を検証し、GPL / AGPL 違反を機械的に遮断する。
- **軽量 3-Type 要件トレース**: `docs/sara.toml` と `docs/model.yaml`（`requirement` ← `spec` ← `verification` の 3 型のみ）を `sara check --strict -c docs/sara.toml` で検証し、下流 `SPEC-*` を持たない孤児 `REQ-*` が存在しないことも合わせて検査する。
- **統合検証コマンド**: `core/scripts/verify.sh` が `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`、ライセンス検査、`sara check --strict`、および孤児 `REQ-*` 検査を一括実行し、全項目 exit 0 を必須とする。
