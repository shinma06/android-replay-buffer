---
id: SPEC-01
type: spec
name: 責務境界・親プロセスと配布
satisfies:
  - REQ-05
  - REQ-06
  - REQ-08
  - REQ-09
---

# SPEC-01 責務境界・親プロセスと配布

## 誰が記録を書き込むか [実装済み]

Android Replay Buffer（ARB）は Android Studio、CLI、MCP、トレイから同じユーザーの `arbd` に接続する。コアの公開・退避・回復を要求する入口が違っても、保存窓と確定済みファイルの意味は共通である。デーモンはランタイムの `arbd.lock` とデータ領域の `.arbd-data.lock` を取得してから回復する。ランタイムの場所だけを変えて同じデータを二重に書き込んではならない（MUST NOT）。

| 境界 | 担当と検査対象 |
| --- | --- |
| `arb-core` | 時計、選別、状態遷移、端末・音声ポリシー。通常依存は serde、serde_json、thiserror に限定 |
| `arb-os` | 連続・活動時計、パス、IPC、ロック、共有メモリ、原子的な公開 |
| `arb-adb` と `device-agent` | ホスト側の ADB 通信・ワイヤ解析と、端末側のビーコン・ログ・入力取得 |
| `arb-media` | MP4・AAC、映像コーデック境界、トリム、フレーム復号 |
| `arb-storage` | リング・WORM バンドル・SQLite WAL カタログ・リース・GC・旧保存形式 schema 3/4 の読み取り |
| `arbd` | 上記を統合する単一ライター。IDE が SQLite や動画を直接編集してはならない |
| `arb-cli`、`arb-tray`、IDE | 前二者は Rust のクライアント。MCP stdio は CLI の入口。IDE は純 Kotlin の client-core と Android Studio の Compose/Jewel 表示を分離 |

### 変更時の構造契約

- **RS-1〜3** は純粋コアの依存、ストレージ・ADB・メディア間の横依存、CLI／トレイのエンジン依存を検査する。OS 分岐は OS 層・コーデック・ネイティブトレイ境界に限定。unsafe の許容範囲をクレート別に固定する。
- **RS-4〜5** は直接の `fs::rename` を OS 層に限定し、ライブラリ本番の `unwrap`／`expect` と RPC の呼び出し先を検査する。全 45 RPC に少なくとも一つのクライアント経路が必要で、GC と forget は GUI・CLI・MCP の各入口で必要。基準版の RS-5 の名前抽出は英小文字と `_` に限られ、報告値は 44。数字を含む `export.share_mp4` はディスパッチと CLI／MCP 呼び出しも照合する。[全 RPC 一覧](SPEC-03-protocol-media.md)
- **KON-1〜3** は UI のファイル書込み・プロセス起動・DB 操作、アクションの更新や Compose 本文のブロッキング操作、本番の Swing ウィジェット／UI DSL を制限する。IDE の子プロセス所有はアプリケーションサービスとプロセスマネージャーに集約する。
- **KON-4〜7** は OS 判定を `HostOs` に集約し、client-core を IDE/UI 非依存に保つ。旧設定移行の呼び出し元を限定し、音声ラベルと構造化操作結果の解釈を共有ポリシーに委ねる。[Rust 構造ガード](../../core/scripts/check-architecture.py)・[Kotlin 構造ガード](../../ide-plugin/src/test/kotlin/io/github/shinma06/arb/ArchitectureKonsistTest.kt)

同じ RPC を使う入口は、同じ引数に対して同じ結果を解釈する。Studio の選択範囲書き出しはカタログに残す `export.trim`、CLI／MCP の単独動画は `export.share_mp4` を選ぶ。公開・カタログ登録の所有者はいずれも `arbd`。[責務所有権と変更影響](README.md)

## IDE と常駐プロセス [実装済み]

| 操作・前提 | 必須の挙動（MUST） |
| --- | --- |
| 最初のプロジェクトを開く | Tool Window を開く前から監視を開始。親付き起動では `arbd --exit-on-stdin-eof` の stdin 書込み端を IDE が保持する |
| トレイを有効・無効にする | `trayEnabled` の初期値は false。有効時だけ親付き `arb-tray` を起動する。無効化で録画用デーモンを停止しない。macOS のトレイは `ActivationPolicy::Accessory` を使う |
| 同じ IDE の窓を一つ閉じる | 残るプロジェクトが使うプロセスを維持する。IDE 通常終了・プラグイン解除で所有するトレイ、デーモンの順に回収する |
| IDE がクラッシュまたは `kill -9` で終了 | 親付き子の stdin に EOF が届く。デーモンは受付・キャプチャを停止しロックを解放する。トレイも終了する。PID の再利用に依存しない |
| CLI 単体起動 | stdin EOF 終了は明示フラグのときだけ有効。単体で起動した常駐デーモンは CLI 終了後も使える |
| 別の IDE が所有する親付きデーモンに接続 | `system.hello.parent_bound` を確認し共有する。自分が所有しない親付きプロセスを停止してはならない |
| 親なしデーモンから引継ぎ | `system.shutdown_for_handover` が保存等の処理中・viewport 使用中なら拒否。親付きデーモンの更新には所有確認値も必要。未対応の旧版では所有者の終了を待つ |

`system.hello` はプロトコル版と `build_id` を返す。監視は停止した子を再起動する。古い独自バイナリが EOF フラグを拒否した場合は通常終了時の回収に限られる。[デーモンの実装](../../core/crates/arbd/src/lib.rs)・[IDE の所有者](../../ide-plugin/src/main/kotlin/io/github/shinma06/arb/ide/ArbdDaemonService.kt)・[VER-33](../verifications/VER-33-ide-parent-bound-lifecycle.md)

## 配布・更新 [実装済み]

Linux x86_64、macOS arm64、Windows x86_64 で保存結果と元データ保護は共通。正式配布のユニバーサルプラグイン ZIP は各 OS のネイティブバイナリと端末資材を同梱し、zsh を必須にしない。Linux ELF の要求シンボルは GLIBC 2.35 以下、GLIBCXX 3.4.30 以下。IDE 対象は Android Studio Rabbit、IntelliJ ビルド `262.9437.185`〜`262.*`。対象外の IDE でも CLI・トレイ・MCP を使える。[配布検査](../../core/scripts/verify_release_bundle.py)・[配布ゲート](../testing/main-release-gate.md)

本体のライセンスは `MIT OR Apache-2.0`。配布に GPL／AGPL／LGPL の義務を持ち込んではならない。複数選択可能な SPDX 式で許容的ライセンスを選べる場合と、禁止ライセンスしか選べない場合を区別する。有料の Apple 公証・Windows コード署名を前提にせず、初回許可手順を提供する。[ライセンス検査](../../core/scripts/check-licenses.py)・[サードパーティ表記](../../THIRD_PARTY_NOTICES.md)・[OS 初回許可](../testing/first-run-os-permissions.md)

チェックポイントは専用の非公開リモートにだけ同期する。録画・logcat・生の診断・認証情報を成果物や公開リポジトリへ含めてはならない。

[移行方針] 現行の IDE は旧設定を一度だけ読み込む。正式版 v1.0.0 への昇格時に移行器と起動時呼び出しを撤去する方針。設定移行器の撤去後も旧保存形式 schema 3/4 の閲覧は REQ-07 の対象である。[移行器](../../ide-plugin/src/main/kotlin/io/github/shinma06/arb/ide/LegacyV1SettingsMigrator.kt)・[保存物の互換](SPEC-02-time-selection-bundle.md)

## 仕様 vs バグ

| 観測条件 | 正常仕様 | バグ |
| --- | --- | --- |
| トレイ初期 OFF | アイコンなしで記録できる | アイコンを出さないとデーモンも起動しない |
| 窓を二つ開き一つ閉じた | 他方から保存を継続できる | 最後の IDE 終了と扱って親付き子を回収する |
| 親が消えた | 親付き子が EOF で終了 | 対応版の子がそのまま常駐し続ける |
| Linux 配布物 | 基準環境で起動する | より新しい GLIBC の要求で起動できない |

責務の決定は Trail #96・#116、親紐付きは Trail #118／PR #523、配布は Trail #117・#122・#124。S1 と TCB-01／02 は所有と EOF、TCB-10／12 は実 IDE と配布物の信頼境界を扱う。[TCB 台帳](../proofs/tcb-axioms.md)
