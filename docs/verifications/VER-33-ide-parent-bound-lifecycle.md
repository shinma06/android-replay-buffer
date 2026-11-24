---
id: VER-33
type: verification
name: Android Studio を親とする arbd / arb-tray ライフサイクル（--exit-on-stdin-eof）の検証記録
verifies:
  - SPEC-01
  - SPEC-03
  - REQ-08
  - REQ-09
---

# VER-33: Android Studio を親とする arbd / arb-tray ライフサイクル 検証記録

記録日: 2026-11-24（macOS arm64 / JDK 27 / Rust 1.100）

オーナー要件「プラグインのプロセスは Android Studio を親とし、親が生きていれば稼働し、死んでいれば稼働しない」の実装と検証。仕様は SPEC-01 §1、`system.hello` の `parent_bound` は SPEC-03 §2。

## 1. 仕組み

| 対象 | 実装 |
| --- | --- |
| 親の監視（共通） | `arb-os` の `parent_watch`：専用スレッドが標準入力を読み続け、EOF または読み取りエラーで 1 度だけ通知する。親が終了・クラッシュ・`kill -9` されると OS がパイプの書き込み端を閉じるため、3 OS とも同じ仕組みで検知できる。PID の監視はしない |
| `arbd --exit-on-stdin-eof` | `arbd-parent-watch` スレッドが EOF を受けると `DaemonShutdownHandle::trigger()` を呼ぶ。これは `system.shutdown` と同じ処理で、自分自身への接続で `accept` の待機を解除し（Unix ソケット・Windows 名前付きパイプとも同じ）、キャプチャ停止・viewport 解放・ソケットとロックの解放を行い終了コード 0 で終わる。`system.hello` は `parent_bound: true` を返す |
| `arb-tray --exit-on-stdin-eof` | `arb-tray-parent-watch` スレッドが EOF を受けると UI ループへ `TrayUpdate::QuitNow` を送る（「Stop Daemon && Quit」と同じ終了経路）。メニュー表示中などで 1.5 秒以内に終了しなければ自らプロセスを終了し、OS がロックを解放する。`--headless` はトレイアイコンなしで同じプロセス寿命を動かす診断・CI 用オプション |
| IDE（`ArbdProcessManager`） | `parentBound = true` のとき `arbd` に `--exit-on-stdin-eof` を付け、`Process` と標準入力の書き込み端を強参照で保持する。稼働中の `arbd` が `parent_bound == false`（旧プラグイン・`arb-cli --auto-start`・手動起動）なら保存と排他的な `system.shutdown_for_handover` に成功した時点で親付きの `arbd` に置き換える。`parent_bound == true` の場合、自分の子は保存と排他的な引継ぎで更新できる。別の IDE の子はそのまま使う。`stopManagedDaemon()` は自分が起動した `arbd` だけに、PID を確認したうえで `system.shutdown`（1.5 秒）を送り、標準入力を閉じ、2 秒待ってから `destroy` → `destroyForcibly` の順に回収する |
| IDE（`ArbdDaemonService` / `ArbStartupActivity`） | 最初のプロジェクトを開いた時点で、Tool Window を開かなくても EDT 外で `arbd` を起動し、その後 `trayEnabled == true` なら `arb-tray --exit-on-stdin-eof` を起動する（標準入力は閉じずに保持）。`arbd` は IDE が所有するので `--auto-start` は付けない（親の死と同時にトレイが親なしの `arbd` を再起動する競合を避けるため）。トレイ OFF では `arb-tray` だけを止める。約 10 秒ごとに `arbd` の稼働を確認し、止まっていれば親付きで起動し直す。`dispose()`（IDE 終了・プラグインのアンロード時のみ）は `arb-tray` → `arbd` → IPC 切断の順に止める |
| 後方互換 | フラグは既定で OFF。単体の CLI / MCP / `start-arb.sh` / `start-arb.ps1` / `arb-tray` は従来どおり。IDE 同梱のバイナリは常に同じリリースなのでフラグを解釈できる。設定画面で指定した古い `arbd` がフラグを拒否して異常終了した場合、同じ実行ファイルが旧版の引数拒否を返すことを確認し、その起動だけフラグなしで管理する。既存のフェイク `arbd` / `arb-tray` を使う試験は `parentBound` が既定の `false` のため引数の変化を受けない |

## 2. 回帰試験

| 試験 | 内容 |
| --- | --- |
| `arb-os` `parent_watch` 単体（2 件） | 実際の匿名パイプで、書き込み端が生きている間（データ書き込み中を含む）は通知せず、書き込み端を破棄すると 2 秒以内に `Eof` を通知する。読み取りエラーも親の消滅として扱う |
| `arbd` 実バイナリ結合 `parent_bound_lifecycle_e2e`（3 件） | `--exit-on-stdin-eof` で起動し、パイプ保持中は `system.status` に応答し `system.hello.parent_bound == true`。書き込み端を破棄すると 2 秒以内に終了コード 0 で終了し、`arbd.lock` を取得でき、Unix では `arbd.sock` が消え、すぐ次のインスタンスを起動できる。パイプ保持中でも `system.shutdown` で止まる。フラグなしでは EOF で止まらず、`parent_bound == false` |
| `arb-tray` 単体（3 件追加） | EOF で `QuitNow` が届き終了コード 0 になること、UI が応答しない場合は猶予後に強制終了が呼ばれること、`--headless` のレンダリング、引数解析 |
| `arb-tray` 実バイナリ結合 `parent_bound_lifecycle`（2 件） | `--headless --exit-on-stdin-eof` で起動し、パイプ保持中は `arb-tray.lock` を保持して生存、書き込み端の破棄で 2 秒以内に終了コード 0、ロック解放、次のトレイがすぐ起動できる。フラグなしでは EOF で止まらない |
| `ArbdParentBoundLifecycleTest`（client-core・実 `arbd`、3 件） | `system.shutdown` も `destroy()` も呼ばず、保持していた標準入力を閉じるだけで（IDE の `kill -9` と同じ）`arbd` が 2 秒以内に終了コード 0 で終わり、次の起動が成功する。親なしの `arbd` は親付きへ置き換えられ、`stopManagedDaemon()` で終了コード 0 で止まる。別の親に紐付いた `arbd` は再利用され、所有者でない側は止めない |
| `ArbParentBoundLifecycleTest`（IDE・実 `arbd` と実 `arb-tray`、3 件） | `trayEnabled == false` の起動で親付き `arbd` だけが起動する。トレイ ON → OFF で `arb-tray` だけが終了コード 0 で止まり `arbd` は稼働を続ける。`dispose()` で `arbd` と `arb-tray` の両方が 3 秒以内に終了コード 0 で止まり、エンドポイントは応答しない。稼働中の親なし `arbd` は起動時に置き換えられ、`dispose()` で止まる。`ArbStartupActivity` がトレイ設定に関係なく先に `arbd` を起動すること |

## 3. 実測（macOS arm64、ローカル）

| 場面 | 結果 |
| --- | --- |
| `arbd --exit-on-stdin-eof`、親側でパイプを閉じる（親の死の再現） | 24 ms で終了コード 0。`arbd.sock` が消え、ロックを取得可能 |
| `arb-tray --exit-on-stdin-eof`（トレイアイコンあり、`tao` のイベントループ）、親側でパイプを閉じる | 71 ms で終了コード 0。`arb-tray.lock` を取得可能 |
| IDE の `dispose()`（`ArbParentBoundLifecycleTest`） | `arb-tray`・`arbd` とも終了コード 0。試験全体 1.0 秒 |

## 4. 未確認事項

- 実機の Android Studio（macOS / Windows / Linux）での IDE 終了・強制終了の目視確認、USB 実機・エミュレータでの録画中の親終了は未実施。stable リリース前の VM 受け入れで確認する。
- Windows の `arb-tray`（トレイアイコンあり）の実測は未実施。Windows では `--headless` の結合試験と、`ArbdParentBoundLifecycleTest`（client-core の Windows ジョブ）で確認する。

## 5. Windows のハンドル継承と引継ぎの追加監査

- OpenJDK 25 の Windows 実装は `CreatePipe(..., NULL, ...)` で両端を継承不可として生成し、stdin の子側の読み取り端だけを `SetHandleInformation` で継承可能にする。親側の書き込み端が継承可能になる区間はない。この実装は `CreateProcessW` のハンドル継承を有効にし、標準ハンドルを個別に設定する。[OpenJDK 25 の実装](https://github.com/openjdk/jdk/blob/jdk-25-ga/src/java.base/windows/native/libjava/ProcessImpl_md.c)・[CreatePipe の仕様](https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-createpipe)
- Rust 1.100 の `Command` が孫へ渡せるのは、自プロセスにあるハンドルである。arbd は IDE の書き込み端を受け取らず、adb や scrcpy はその端を継承できない。`CREATE_NO_WINDOW` はコンソールの作成を制御する。Rust は読み取り時の `ERROR_BROKEN_PIPE` を EOF として扱い、ARB は 0 バイトと、`ERROR_PIPE_NOT_CONNECTED` を含む非 Interrupted エラーのいずれでも終了を開始する。[Rust のプロセス生成](https://github.com/rust-lang/rust/blob/1.100.0/library/std/src/sys/process/windows.rs)・[ハンドル読み取り](https://github.com/rust-lang/rust/blob/1.100.0/library/std/src/sys/pal/windows/handle.rs)
- client-core は同じ JVM から実 daemon と tray を起動し、tray が生きたまま daemon の stdin を閉じて 1 秒以内の正常終了を確認する。配布ゲートも展開先の両バイナリで個別に EOF を検証する。1 秒の試験条件はアイドル状態で、保存やキャプチャの終了処理の時間は含まない。
- 保存処理と競合する自動引継ぎは次の監視周期まで延期する。引継ぎの要求と検出は同一接続を使い、終了待ちがタイムアウトしても未管理プロセスを強制停止しない。原子的な引継ぎに対応しないカスタム旧版は明示的な再起動まで使い続ける。G-M9 の同梱版更新は同一接続で保存状況を確認してから従来の停止 RPC を使う。旧版ではこの確認と停止の間の保存開始を排除できない。
