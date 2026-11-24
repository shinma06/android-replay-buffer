---
id: VER-23
type: verification
name: T-6E Android 9 (API 28) 実機互換性監査・ハードニング・更新版ゼロ zsh 試用プラグイン ZIP の検証記録
verifies:
  - SPEC-01
  - SPEC-02
  - SPEC-03
  - SPEC-04
  - REQ-01
  - REQ-03
  - REQ-04
  - REQ-07
  - REQ-08
  - REQ-09
---

# VER-23: T-6E Android 9 (API 28) 互換性監査・ハードニング検証記録

記録日: 2026-11-24（macOS arm64 / Rust workspace `core/` / `device-agent/` min-api 24 DEX / Gradle 9.8.1 + JDK 27）

## 1. Android 9 (API 28) 互換性監査で検出・修正した項目

1. **Android < 11 (`ro.build.version.sdk < 30`) / < 13 音声キャプチャの事前フォールバック・実行時フォールバック・サポート下限警告 (`sdk < 28`)**
   - `arb-adb` (`adb.rs`, `scrcpy.rs`): `adb -s <serial> shell getprop ro.build.version.sdk` を解析する `parse_sdk_version_output` / `AdbClient::query_sdk_version`、`audio_unavailable_reason_for_api`、および公式サポート下限（Android 9 / API 28）未満向けの非ブロッキング診断 `unsupported_android_api_warning` を追加した。
   - `arbd` (`live_adb.rs`, `capture.rs`, `protocol.rs`): セッション単位で SDK バージョンをキャッシュ取得し、`plan_scrcpy_audio` で `sdk < 30`（Android 9 API 28 / Android 10 API 29）または `sdk < 33` の場合に `audio=false`（映像単一ソケット、音声ソケット待機なし）で事前起動する。`CaptureStatusView` に `audio_state`（`unknown` / `available` / `disabled` / `unavailable`）、`audio_reason`、および `device_api_warning` を追加し、API 28 では `"Audio capture requires Android 11+ (API 30+); detected API 28. Continuing with video + logs + taps."` を正直に報告する。
   - **サポート下限未満（API < 28）の扱い**: ハードブロックせず、`device_api_warning`（`"Android 9 (API 28) 未満は非サポート対象です (API <sdk>)。ベストエフォートで音声なしキャプチャを継続します。"`）を出した上で `audio=false` のベストエフォート取得を継続する。
   - **実行時フォールバック強化**: API >= 33 または SDK 不明で `audio=true` 起動に失敗した場合（起動時ハンドシェイク失敗・音声ソケット即切断・`codec_id = 0 / 1`）、失敗プロセスとポート転送を即時解放して `audio=false` へ自動フォールバックし、映像＋ログ＋タップ取得を継続する。
   - `ide-plugin/client-core` (`ProtocolModels.kt`, `ProtocolJson.kt`, `ArbViewModels.kt`): `AudioCaptureState`・`audioReason`・`deviceApiWarning` を追加（旧 `arbd` JSON とも後方互換）。`UNAVAILABLE` / `deviceApiWarning` 時は録画を正常継続したまま警告バナーで理由を通知する。
2. **`device-agent` DEX / ART 互換性および Android 9 SELinux ハードニング**
   - `device-agent/build.sh` のターゲットは Java 8 (`-source 8 -target 8`) + `d8 --release --min-api 24`。全 15 Java ソースに API 29+ 専用 API・非公開 API 呼び出しがないことを確認した（`SystemClock.elapsedRealtimeNanos`, `LocalServerSocket`, `AudioRecord.getTimestamp` 等はすべて API <= 24）。
   - 生成 DEX ヘッダ検証: `arb-companion.dex.jar` (`classes.dex` = `dex\n037\0`)、`replay-scrcpy-audio.dex.jar` (`classes.dex` = `dex\n037\0`, `classes2.dex` = `dex\n035\0`) — いずれも Android 9 (API 28) ART（DEX `039` まで対応）でネイティブ動作する。
   - `BootIdResolver.java` を純 Java として切り出し、`/proc/sys/kernel/random/boot_id` が SELinux `EACCES` で読めない場合に `settings get global boot_count` ＋ `/proc/stat btime` へ決定的にフォールバックし、さらに両方が読めない場合もクラッシュせず `ro.boot.bootreason` 診断付きの `fallback:random:<uuid>`（`BOOT_ID_FALLBACK_RANDOM_UUID`）で起動継続することをホスト単体テストで検証した。
   - `LogcatWorker.java`: 一部 Android 9 端末で `logcat -b all -B` が制限された場合（0 件のまま非ゼロ終了）に即座に `logcat -b main,system,crash,events -B` へフォールバックし、あわせて stderr を常時ドレインしてバイナリ stdout パイプの詰まりを防止した。`logcat -S` 統計プローブも `-b main,system,crash,events -S` をフォールバックに追加した。
3. **Android 9 (API 28) Event Log タグ（`am_*`）とバイナリ `30007` / `30043` / `30022` の修正**
   - **重大バグ修正（バイナリ `30043` フィールド順）**: AOSP のイベントタグ `30043`（Android 9: `am_focused_activity`、Android 10+: `wm_set_resumed_activity`）のフォーマットは `[User, ComponentName, Reason]` であり、ComponentName は第 2 要素だが、従来コードは `[User, Token, ComponentName]` の第 3 要素しか読んでいなかった。両形式に対応するよう修正した。
   - **Android 9 タグ追加**: `am_focused_activity`、`am_resume_activity` / `wm_resume_activity`（バイナリ `30007`：`[User, Token, TaskId, ComponentName]`）、`am_on_resume_called`（バイナリ `30022`）を `decode_binary_android_event`、`parse_screen_transition_line`、`extract_screen_spans`、`LogPackageResolver` に統合した。
   - **重大バグ修正（`30022` UID 扱いとスプーフィング防止）**: `am_on_resume_called` / `wm_on_resume_called`（バイナリイベント `30022`）はアプリプロセス自身（アプリ UID）が `ActivityThread` から出力するため、`capture.rs` `decode_log_tag_and_message` が `uid != 1000` を理由に `app:` プレフィックスへ隔離していた挙動を修正し、バイナリ `events` バッファの `30022` は保持しつつ、テキストログ偽装と `system_server` 専用タグ（`am_crash`, `am_focused_activity`, `am_resume_activity`, `wm_resumed_activity`, `30007`, `30043`, `ActivityManager`, `ActivityTaskManager`）の `uid != 1000` スプーフィングは引き続き確実に拒否する。
4. **Android 9 `adb shell` ヘルパーおよび `scrcpy-server-v5.0.1` フラグ検証**
   - `AdbClient::query_package_uids`: `cmd package list packages -U` が失敗または空出力の場合に `pm list packages -U` へフォールバックするよう修正した。
   - `wm size`（`Physical size` 優先）、`getevent -lp` / `getevent -lt`、`adb exec-out screencap -p`（API 21+ 互換）、および `scrcpy-server-v5.0.1` の全引数と DEX バージョン（`dex\n035\0`）が Android 9 (API 28) 互換であることを確認した。

## 2. ローカル品質ゲート & 更新版ゼロ zsh 試用 ZIP 検証結果

- `./device-agent/build.sh --with-scrcpy-audio`: **ProtocolTest 21 passed, 0 failed**（`arb-companion.dex.jar` 27,845 B / `replay-scrcpy-audio.dex.jar` 735,101 B）
- `./core/scripts/verify.sh`: **6/6 PASS**（`cargo fmt --check` / `cargo clippy --all-targets -- -D warnings` 警告 0 件 / `cargo test` **363 passed, 0 failed, 3 ignored** / license **GPL・AGPL 0 件** / `sara check --strict`（本書 `VER-23` を含む）/ REQ → SPEC 14/14）
- `RUSTDOCFLAGS="-D warnings" cargo doc --manifest-path core/Cargo.toml --workspace --no-deps`: **PASS（警告 0 件）**
- `(cd ide-plugin && JAVA_HOME=/opt/homebrew/opt/openjdk@27/libexec/openjdk.jdk/Contents/Home ./gradlew test buildPlugin -ParbNativeDir=../core/target/release -PdeviceAgentDir=../device-agent/build --no-daemon)`: **client-core 83 passed / ide-plugin 35 passed, 0 failed**
- 更新版ゼロ zsh macOS arm64 試用プラグイン ZIP（`arb-v2-ide-plugin-macos-arm64-compose-trial.zip`、`6,439,846` bytes、sha256=`8f115a5a35d2a2b153c6d475a92c860547db2a63bca6d43427772120166c9835`）単体展開スモークテスト（`/bin/bash`・リポジトリなし・zsh なし）:
  - `native/macos-arm64/{arbd,arb-cli,arb-tray}` すべて `0755`、`device-agent/{arb-companion,replay-scrcpy-audio}.dex.jar`（DEX `037` / `035`）、旧 `plugin/` 0 件
  - 隔離 `arbd` 起動 → `arb-cli status --json`（`build_id = 0.1.0-7755824-318178800`, `audio_state = "unknown"`）→ `config get --json` → `mcp` 17 ツール（stderr 0 B）→ `daemon stop` → 残存プロセス 0 件

## 3. 未検証項目（Unverified）

- **物理 Android 9 (API 28) 実機での結合確認**: 本セッション環境には物理 Android 9 実機が接続されていないため、実機 USB 接続での `scrcpy-server` H.264 エンコード、`arb-companion` DEX ロード・SELinux 挙動、`am_focused_activity` / `am_resume_activity` / `am_on_resume_called` 実ログ取得、および Android Studio `2026.2.2.8` GUI への ZIP 導入はオーナー実機テストで確認する。
- **Linux / Windows ホスト**: ローカル実行は macOS arm64 のみ。3 OS パリティは GitHub Actions `v2 CI`（7/7）で確認する。
