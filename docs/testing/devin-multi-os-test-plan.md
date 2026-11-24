# Devin マルチ OS テスト & 動画収録 実行計画

対象: Linux（Ubuntu）/ Windows 11 / macOS。目的は **3 OS の結果同一性・性能バー・プレイヤー互換・GUI 挙動・アップグレード** を動画つきで証明すること。判断が要らないよう、以下を上から順に実行して結果を返す。ゲートの定義と合格基準は [`main-release-gate.md`](main-release-gate.md)（G-M1〜G-M9）が正本で、この計画はその実行手順。

自動で通る部分（配布 ZIP の検査、Suite B、焼き込みの実画素、アップグレード）は `core/scripts/verify_release_bundle.py` が 1 コマンドで行う。Devin は同じ手順を各 OS で実行して出力を提出し、そのうえで人の目が要る GUI・プレイヤー再生を動画に撮る。

## 0. 共通セットアップ（各 OS 1 回）

```bash
# Entire（Devin セッション記録）: 各 worktree で必ず最初に
entire-agent-devin bootstrap --checkpoint-repo shinma06/entire-checkpoints
git clone <repo> && cd android-replay-buffer && git checkout <対象ブランチまたは develop>
# Rust stable、Python 3、ffmpeg/ffprobe が必要
#  Linux:   sudo apt-get install -y nasm ffmpeg vlc xvfb dbus-x11
#  Windows: choco install nasm ffmpeg vlc -y   （PowerShell 管理者）
#  macOS:   brew install ffmpeg vlc            （x86_64 のみ nasm も）
cargo build --manifest-path core/Cargo.toml --release -p arbd -p arb-cli -p arb-tray
cargo build --manifest-path core/Cargo.toml --release -p arbd --example arb_gate_bundle
```

録画は各 OS 標準（Linux: `ffmpeg -f x11grab` / OBS、Windows: Win+Alt+R または `ffmpeg -f gdigrab`、macOS: Cmd+Shift+5）。チェックポイントの push は `checkpoints` リモートのみ（`origin` へは push しない）。

配布物は CI の `Release Gate`（`release-gate.yml`）のアーティファクト `release-gate-dist`（`arb-<os>-<arch>.zip` 3 個 + `arb-ide-plugin-<版>.zip`）を使う。ローカルで組み立てる場合は `main-release-gate.md` の「ローカルでの実行」に従う。

## Suite A: OS ネイティブ基盤（動画不要、ログ提出）

| ID | 手順 | 合格 |
| --- | --- | --- |
| A1 | `cargo test --manifest-path core/Cargo.toml -p arb-os -- --nocapture` | 全テスト pass。OS 単調時計と wall clock の差が `arb-os` 期待範囲内 |
| A2 | `cargo test -p arb-os ipc`（Linux/macOS=UDS、Windows=Named Pipe） | 接続・複数クライアント・切断復帰 pass |
| A3 | `cargo test -p arb-storage -- --nocapture`（Windows では `MoveFileExW` 原子コミットと AV ロック再試行を含む） | pass。Windows はテスト出力に rename retry 経路が出てもよいが最終 pass |
| A4 | `cargo test -p arb-core -p arbd shm` / `ShmFrameRing` クロスプロセステスト | 別プロセス reader がトーンなしで全フレーム整合 |
| A5 | `cargo test --manifest-path core/Cargo.toml --workspace` | 3 OS すべて 0 failed |
| A6 | `python3 core/scripts/package_distribution.py --self-test` と `python3 core/scripts/verify_release_bundle.py --self-test` | OK（Windows では PowerShell の判定ケースも走る） |

## Suite B: 配布 ZIP の実走と性能（OpenH264 + nasm、FHD30）

1. nasm 有効確認: Linux/Windows x86_64 は `nasm -v` が出ること。macOS arm64 は不要。
2. 組み立て済みの自 OS 向け ZIP を、リポジトリの外のクリーンなディレクトリで検査する（展開・`arbd` 起動・MCP・旧形式・Suite B・焼き込み・`arb-tray` を 1 回で実行）:
   ```bash
   python3 core/scripts/verify_release_bundle.py \
     --bundle-zip arb-<os>-<arch>.zip --plugin-zip arb-ide-plugin-<版>.zip \
     --fixtures-dir <arb_gate_bundle の出力> --check-tray --out-dir out-<os>
   ```
   G-M3 の `legacy-read-only` で、schema 3 の `session.json` と schema 4 の `replay.json` が読取専用で開き、操作前後に全ファイルの SHA-256 が一致することを確認する。
   Linux のヘッドレス環境では `xvfb-run -a dbus-run-session -- python3 ...` で包む。
3. `out-<os>/release-bundle-report.md` の「性能 (Suite B)」表（open / filmstrip / scrub / seek / trim / trim+burn）を 3 OS 分まとめる。トリムは `[47..=647]` の 601 フレーム、GOP 途中開始。
4. 合格バー（オーナー確認済み）: **open ≤ 1 s / seek ≤ 0.2–0.5 s / trim ≤ 数秒**。超過は FAIL ではなく「性能指摘」として値付きで報告し、OS 間差も記載する。
5. 実バンドル（実機・エミュレータで録画したもの）に対する従来の手順も残す: `arb-cli --json scrub-bench <BUNDLE>` と `python3 core/scripts/devin_verify_player_matrix.py --bundle <BUNDLE> --arb-cli arb-cli --out-dir out-<os>`。

## Suite C: プレイヤー互換と焼き込みの目視

自動検査は `share.mp4`（`burn_taps=false`）、`share-taps.mp4`（Tap と Swipe を焼き込み）、トリム済み `video.mp4` のそれぞれで **17/17**（映像 11 + 同梱 `audio.m4a` 6）が通ることを要求する。`devin-report.json` が PASS であることが前提。

### C1 プレイヤー再生（各 OS、各プレイヤーごとに画面録画）

入力: `verify_release_bundle.py --keep-work --work-dir <dir>` が残す `<dir>/burn-plain-1s/bundle-*/share.mp4` と `<dir>/burn-taps-1s/bundle-*/share-taps.mp4`（単一 `stsd` `avc1` + in-band SPS/PPS）。

| OS | プレイヤー |
| --- | --- |
| Linux | Chrome、VLC |
| Windows | Edge、Chrome、VLC、Windows Media Player（なければ「映画 & テレビ」）。Edge・VLC・Media Foundation は CI の `gate-windows` が `windows_gui_evidence.py` で自動撮影する |
| macOS | Chrome、VLC、QuickTime Player |

各動画で確認: ① 先頭フレームの焼き込み番号 = 47（`frame-0.png` と一致）② 開始前フレームが **1 枚も無い** ③ スプライス IDR 位置（GOP 境界）を跨いでも滑らか（コマ飛び・フリーズ・色化けなし）④ 最終フレーム = 647 ⑤ **`share-taps.mp4` だけ**に Tap（3 フレーム目付近）と Swipe の軌跡が見え、`share.mp4` には出ない。1 本 30 秒以内で、先頭・スプライス・末尾・ジェスチャーが映るようにする。

## Suite D: IDE プラグインの実 GUI（Android Studio 2026.2.2.8）

対象は実際の Compose / Jewel の UI。`ide-plugin` の ZIP は `arb-ide-plugin-<版>.zip`（3 OS のネイティブを含むユニバーサル版）を `Settings → Plugins → 歯車 → Install Plugin from Disk...` で導入する。

**Linux（Devin Cloud）**: Android Studio `2026.2.2.8` の公式 Linux アーカイブを展開し、Xvfb 上で動かす。

```bash
Xvfb :99 -screen 0 1920x1080x24 &  export DISPLAY=:99
dbus-run-session -- ./android-studio/bin/studio.sh &
ffmpeg -f x11grab -framerate 15 -video_size 1920x1080 -i :99 -c:v libx264 -pix_fmt yuv420p c2-gui.mp4 &   # 録画
```

プロジェクトは任意の空の Android プロジェクトでよい。`arbd` はプラグインが同梱版を自動起動する。1 本の動画で次を順に示す。

1. `Settings → Tools → Android Replay Buffer` を開く。**メニューバー（トレイ）は初期状態で OFF**（`trayEnabled` の既定値 false）。ON にするとトレイが起動し、OFF に戻すと止まる
2. ARB ツールウィンドウの常時録画（OFF / ON で端末待ち / 録画中の 3 状態）。端末なしで ON にしても落ちず、端末待ちを表示する
3. 実際に保存した（または用意したフィクスチャの）バンドルを開く。タイムラインのドラッグ中に **近似プレビューのバッジ** が出て、離して **正確なフレームに確定** するとバッジが消える
4. 不連続区間が表示される。「Unanchored」ログが **別タブ** に分かれている
5. 区間を指定してトリムし、**完全なフォルダー**（`video.mp4`、`audio.m4a`、`manifest.json` ほか）として保存する。「タップを焼き込む」の既定は ON で、`share-taps.mp4` が出る。OFF にすると `share.mp4` になる
6. バンドルを Trash → Restore できる。**元ファイルは削除されない**（ファイルブラウザで存在確認を映す）
7. Tools メニューの各アクション（直前の記録を保存、キャプチャ開始 / 停止、スクリーンショット、ログのみ保存）が動く。Running Devices のツールバーに ARB のボタンが出る（エミュレータ接続時）

**macOS / Windows**: 同じ手順を各 OS の Android Studio で行う。macOS ではエミュレータ（API 37）または USB 実機で常時録画・保存・トリムまで通す。

## Suite E: Windows 実 GUI（G-M7）

CI の `gate-windows` が自動で行い、アーティファクト `release-gate-windows` に残す。Devin は同じ成果物を実機の Windows 11 で再確認する。

- `arb-tray.exe` の実起動・単一インスタンス、デスクトップのスクリーンショット（`win-tray.png`）
- Edge（`--autoplay-policy=no-user-gesture-required`）で `share.mp4` / `share-taps.mp4`（`win-edge-share.png`、`win-edge-taps.png`）、VLC で `share-taps.mp4`（`win-vlc-taps.png`）、Media Foundation（WPF `MediaPlayer`）での再生
- `ffmpeg -f gdigrab` の画面録画（`win-playback.mp4`）。ウィンドウを前面に出し、画面と録画フレームが黒でないことを確認済み
- ide-plugin の全 Gradle テストが Windows で走り、`WindowsPipeSecurityTest.win32CurrentUserSidIsAValidSidOnWindows` が skip されず成功

## Suite F: v2 → v2 アップグレード（G-M9 の U3〜U5）

| ID | 内容 | 手順 |
| --- | --- | --- |
| U3 | v2-old → v2-new（稼働中 `arbd`、状態保持） | `verify_release_bundle.py --upgrade-from-zip <前リリースの ZIP>` の `upgrade` セクションが PASS。下記「前 `early` → 新候補プラグイン」で IDE 側も確認 |
| U4 | `arb-tray` の再起動跨ぎ | `--check-tray --upgrade-from-zip ...` の `upgrade` セクション。トレイの標準エラーに「arbd に再接続しました」が出る |
| U5 | 公開済み `early` からの実アップグレード | `gh release download early --pattern arb-<os>-<arch>.zip` で取得し、`--upgrade-from-zip` に渡す。CI は `release-gate.yml` が 3 OS で実施 |

### 前 `early` → 新候補プラグイン（Devin Cloud Linux / macOS）

1. 前回の `early` のプラグイン ZIP を取得: `gh release download early --pattern 'arb-ide-plugin-*.zip'`。IDE に導入して案件を開き、常時録画とトレイ（Settings で ON）を有効にして `arbd` を起動した状態にする。`Settings → Tools → Android Replay Buffer` で保持秒数・保存先を変え、タップ焼き込みを OFF にしておく
2. IDE を閉じずに、または閉じてから、新候補の `arb-ide-plugin-<版>.zip` を上書きして再起動する。ZIP のエントリ時刻は固定のため、時刻とサイズでは新旧を判別できない
3. 合格基準: 旧 `arbd` が `system.shutdown` で止まり、新しい `arbd` が起動する（`arb-cli --json status` の `build_id` が新プラグインの `native/<os>-<arch>/arbd` の SHA-256 と一致、旧プロセス 0）。設定（保持秒数・保存先・タップ焼き込み・トレイの ON）が保たれる。保存済みのバンドルとゴミ箱が残り、Restore できる。トレイが二重に出ない

## 提出フォーマット

```
devin-results/<os>/
  suiteA.log  suiteB.md（数値表）  release-bundle-report.json  release-bundle-report.md
  devin-report.json  devin-report.md  frame-0.png  frame-last.png
  C1-<player>.mp4（プレイヤーごと） D-gui.mp4  F-upgrade-early.mp4
  SUMMARY.md   # 下記チェックリスト、各行 PASS/FAIL/N-A + 一言
```

チェックリスト（SUMMARY.md にコピー）:

- [ ] A1–A6 pass（3 OS）
- [ ] B: `verify_release_bundle.py` が全セクション PASS。open/seek/trim の数値を記載し、バー内か明記
- [ ] `devin-report.json` の全 check が PASS（601 フレーム、0 edts/elst、faststart、負 PTS/DTS 0、`audio.m4a` の 6 項目を含めて 17/17）
- [ ] C1 各プレイヤー: 開始前フレーム 0、スプライス IDR で滑らか、焼き込みは `share-taps.mp4` だけ
- [ ] D: 7 項目（または N/A 理由）
- [ ] E（Windows）: 画面と録画が黒でない、`win32CurrentUserSidIsAValidSidOnWindows` が実行された
- [ ] F: U3〜U5（前 `early` ZIP の上書き、実 `early` からの `--upgrade-from-zip`）
- [ ] 失敗は再現手順・ログ・動画の該当秒を添付

秘密情報・実機録画・logcat を含む成果物は公開しない。
