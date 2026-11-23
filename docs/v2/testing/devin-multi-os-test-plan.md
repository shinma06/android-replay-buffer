# Devin Pro マルチ OS テスト & 動画収録 実行計画

対象: Linux (Ubuntu) / Windows 11 / macOS。目的は **3 OS の結果同一性・性能バー・プレイヤー互換・GUI 挙動** を動画付きで証明すること。判断が要らないよう、以下を上から順に実行して結果を返す。

## 0. 共通セットアップ（各 OS 1 回）

```bash
# Entire（Devin セッション記録）: 各 worktree で必ず最初に
entire-agent-devin bootstrap --checkpoint-repo shinma06/entire-checkpoints
git clone <repo> && cd android-replay-buffer && git checkout <対象ブランチ>
# Rust stable、ffmpeg/ffprobe、Python 3 が必要
#  Linux:   sudo apt-get install -y nasm ffmpeg vlc
#  Windows: choco install nasm ffmpeg vlc -y   （PowerShell 管理者）
#  macOS:   brew install ffmpeg vlc            （x86_64 のみ nasm も）
cargo build --manifest-path core/Cargo.toml --release -p arbd -p arb-cli -p arb-tray
```

録画は各 OS 標準（Linux: `ffmpeg -f x11grab` / OBS、Windows: Win+Alt+R、macOS: Cmd+Shift+5）。チェックポイントの push は `checkpoints` リモートのみ（`origin` へは push しない）。

## Suite A: OS ネイティブ基盤（動画不要、ログ提出）

| ID | 手順 | 合格 |
| --- | --- | --- |
| A1 | `cargo test --manifest-path core/Cargo.toml -p arb-os -- --nocapture` | 全テスト pass。OS 単調時計と wall clock の差が `arb-os` 期待範囲内 |
| A2 | `cargo test -p arb-os ipc`（Linux/macOS=UDS、Windows=Named Pipe） | 接続・複数クライアント・切断復帰 pass |
| A3 | `cargo test -p arb-storage -- --nocapture`（Windows では `MoveFileExW` 原子コミットと AV ロック再試行を含む） | pass。Windows はテスト出力に rename retry 経路が出てもよいが最終 pass |
| A4 | `cargo test -p arb-core -p arbd shm` / `ShmFrameRing` クロスプロセステスト | 別プロセス reader がトーンなしで全フレーム整合 |
| A5 | `cargo test --manifest-path core/Cargo.toml --workspace` | 3 OS すべて 0 failed |

## Suite B: OpenH264 + nasm FHD30 性能（REQ-11 ライン）

1. nasm 有効確認: Linux/Windows x86_64 は `nasm -v` が出ること。macOS arm64 は不要。
2. フィクスチャ生成: `python3 core/scripts/devin_verify_player_matrix.py --generate-fixture fx1s.mp4 --gop-seconds 1` と `--gop-seconds 5`。
3. `arbd` を起動した実バンドル（`core/scripts/e2e_emulator.sh` もしくは arbd テストが作るバンドル）に対し:
   ```bash
   arb-cli --json scrub-bench <BUNDLE>
   python3 core/scripts/devin_verify_player_matrix.py --bundle <BUNDLE> --arb-cli arb-cli --out-dir out-<os>
   ```
   （trim は `[47..=647]` の 601 フレーム、GOP 途中開始）
4. 合格バー（オーナー確認済み）: **open ≤ 1 s / seek ≤ 0.2–0.5 s / trim ≤ 数秒**。OS ごとの数値を表にして提出。バー超過は FAIL ではなく「性能指摘」として値付きで報告（OS 間差も記載）。

## Suite C: 動画収録 GUI / プレイヤー検証

### C1 プレイヤー再生（各 OS、各プレイヤーごとに画面録画）

入力: `out-<os>/` の trim 済み MP4（単一 `stsd` `avc1` + in-band SPS/PPS）。`devin-report.json` が PASS であることが前提。

| OS | プレイヤー |
| --- | --- |
| Linux | Chrome、VLC |
| Windows | Chrome、VLC、Windows Media Player（なければ「映画 & テレビ」） |
| macOS | Chrome、VLC、QuickTime Player |

各動画で確認: ① 先頭フレームの焼き込み番号 = 47（`frame-0.png` と一致）② 開始前フレームが **1 枚も無い**（47 より前の番号・黒/緑フレームなし）③ スプライス IDR 位置（GOP 境界）を跨いでも滑らか（コマ飛び・フリーズ・色化けなし）④ 最終フレーム = 647。1 本 30 秒以内で、先頭・スプライス・末尾が映るようにする。

### C2 IDE プラグイン + トレイ GUI（Android Studio がある環境、`ide-plugin/` が存在する場合）

`ide-plugin/` が未マージなら「N/A（W8 未マージ）」と記載して C1 のみ提出。存在する場合は `arbd` を起動し、プラグインを入れた Android Studio と `arb-tray` を開いて 1 本の動画で:

1. タイムラインのドラッグ中に **近似プレビューのバッジ** が出る
2. 離して **正確フレームに確定** するとバッジが消える
3. 不連続区間が表示される
4. 「Unanchored」ログが **別タブ** に分かれている
5. バンドルを Trash → Restore できる。**元ファイルは削除されない**（ファイルブラウザで存在確認を映す）

## 提出フォーマット

```
devin-results/<os>/
  suiteA.log  suiteB.md（数値表）  devin-report.json  devin-report.md
  frame-0.png frame-last.png
  C1-<player>.mp4（プレイヤーごと） C2-gui.mp4
  SUMMARY.md   # 下記チェックリスト、各行 PASS/FAIL/N-A + 一言
```

チェックリスト（SUMMARY.md にコピー）:

- [ ] A1–A5 pass（3 OS）
- [ ] B: open/seek/trim の数値を記載、バー内か明記
- [ ] `devin-report.json` の全 check が PASS（601 フレーム、0 edts/elst、faststart、負 PTS/DTS 0）
- [ ] C1 各プレイヤー: 開始前フレーム 0、スプライス IDR で滑らか
- [ ] C2: 5 項目（または N/A 理由）
- [ ] 失敗は再現手順・ログ・動画の該当秒を添付

秘密情報・実機録画・logcat を含む成果物は公開しない。
