---
id: SPEC-04
type: spec
name: 性質の証明・自動検証と実環境の合格条件
satisfies:
  - REQ-03
  - REQ-04
  - REQ-07
  - REQ-08
  - REQ-11
  - REQ-13
  - REQ-14
---

# SPEC-04 性質の証明・自動検証と実環境の合格条件

## 五層の証拠 [実装済み]

不具合報告には、操作前の状態・入力・観測結果・対象コミットを添え、該当する仕様の条件と照合する。以下の層はそれぞれ検査対象が異なる。証明の対象外の環境を「合格」と報告してはならない。

| 層 | 検査するもの | 観測できる失敗 |
| --- | --- | --- |
| 1 Kani | 記号入力について純粋計算や有界スライスの性質を検査。基準版の通常構成で `cargo kani list -Z stubbing --format json` が列挙するのは **143 本**。内訳は arb-core 101（K14 の 5 本を含む）、arb-media 25（K13 の 5 本を含む）、arbd 10、arb-os 5、arb-adb 2 | 窓の端点、算術のあふれ、誤った状態遷移などを反例で特定。`kani-extended` は別集合 |
| 2 Stateright | 複数主体の操作順序をモデル内で全探索。S1 親と所有、S2 pin／回復／GC、S3 取得世代・圧迫・ドレイン、S4 viewport／出力、S5 クラッシュ、S6 再生・接続、S7 トリム操作 | 保護中の削除、古い応答の適用、復旧・解放の取り残し。S5 の仮定は TCB-16 |
| 3 Proptest・Miri・共有 JSON | G1 カタログ、G2 リングの障害回復、G3 MP4、G4 複数ストリーム、G5 永続ジョブ等の互換、G6 ADB ワイヤ、G7 SHM レイアウト、G8 音声。M1 は SHM のメモリ操作、M2 は純粋ドメインと MP4 パーサ | 生成した入力を縮小して再現する。共有表・トレースで言語間や公開 wire と実際の実行結果のずれを検出 |
| 4 構造ガード | Kotlin の KON-1〜7、Rust の RS-1〜5。依存方向、OS 境界、RPC 呼び出し、共有方針の再解釈を制限 | 個別の画面テストが成功しても、UI が独自に音声非対応・保存成功を判定すれば違反 |
| 5 TCB 公理台帳 | TCB-01〜21。OS の意味、SQLite、実デコーダ、JVM、端末・OEM、モデルと本番実装の対応など、上の証拠が依存する仮定を記録 | 仮定が外れる事例は実機、I/O テスト、実プロセス等で確認し、未解決を残す |

K1・K2 は時間窓と選別、K3 は時計、K4 はリング、K5 は状態遷移、K6・K7 は入力座標と軌跡、K8 は SHM、K9 はワイヤ・MP4、K10 は VFR 時間軸、K11 は音声方針、K12 は GOP 予算、K13 は VFR 実時間トリムの正長・時間保存・各時刻の表示同値・不正入力拒否・ティック量子化、K14 は被覆台帳と静止画面の引き継ぎ（被覆がなければ引き継がない、窓前で最も遅いフレームだけ、boot・セッション・寸法の不一致を選ばない、被覆時間は窓長以下で単調、ビーコンは途絶前だけ延長）を対象とする。実 MP4 の切り出し、timescale 切替、デコード結果は G3 とメディア統合試験で確かめる。検証済み入力幅・スタブ・未証明集合は各 ID の台帳行に従う。[PDD 台帳](../proofs/tcb-axioms.md)・[Kani／Miri ワークフロー](../../.github/workflows/pdd-kani.yml)

### 共有表の読み手と非空虚性

端末選択・音声・操作結果の表は Rust と Kotlin が読み、状態・原因の変更ではまず表の該当行を更新する。MCP ツール表は Rust の公開スキーマ生成・入力検証と stdio 契約テストが読む。S6 の 16 トレースは Stateright と Kotlin の本番時計・viewport セッションの対応を検査する。S7 は 4,656 状態を探索し、再生位置の追従・リリース時の量子化・中断時のプレビュー残留の 3 変異で反例を得る。22 トレース・122 ステップを Kotlin の本番コントローラー・選択処理・描画座標変換で再生する。実画面のイベント配送は実 IDE の受け入れ対象。[S7 モデル](../../core/crates/arbd/tests/pdd_stateright_trim_s7.rs)・[共有トレース](studio-trim-s7-traces.json)・[Kotlin リプレイ](../../ide-plugin/src/test/kotlin/io/github/shinma06/arb/ui/PddS7StudioTrimTraceReplayTest.kt)

条件を取り除くなどの変異を入れ、期待する反例が出ることを確認する。S6 では到着フレーム時刻の書き戻し、失効した失敗応答の受理、再取得中の reader 解放漏れが対象。全状態数だけで非空虚性の証拠としない。Kani の到達性・変異は実施した個々のハーネスの範囲で評価する。[S6 モデル](../../core/crates/arbd/tests/pdd_stateright_playback_s6.rs)・[共有トレース](playback-s6-traces.json)

`check-mcp-trim-schema.py` は実 stdio で 4 版を交渉し、各版の 20 ツール・40 入出力スキーマ、外部参照なしの Draft 2020-12、2 形式の実時間書き出しを検査する。`ArbBrandingGuardTest` は IDE の表示・メタデータに加え `docs/*.md` と `docs/specs/` 以下の全 Markdown を走査する。[スキーマ検査](../../core/scripts/check-mcp-trim-schema.py)・[表示名ガード](../../ide-plugin/src/test/kotlin/io/github/shinma06/arb/ide/ArbBrandingGuardTest.kt)

通常の単体テストは実装の入口、過去データの回帰、OS・IPC・FS 障害注入に使う。実 SQLite、実 IPC、外部プレイヤー、デバイスの動作をモデルだけで代用してはならない。[統合試験](../../core/crates/arbd/tests)・[端末スモーク](../testing/work-device-smoke-checklist.md)

## PR と配布の判定 [実装済み]

`./core/scripts/verify.sh` の 7 段は fmt、clippy、workspace test、ライセンス、構造、`sara check --strict`、REQ 全件の下流 SPEC 照合。Gradle の `PddContractTablesTest`・`PddUiFeedbackContractTest`・Konsist は Kotlin 側。通常 PR の必要結果は `test`、macOS／Windows の `os-parity` と Entire Gates。週次・手動等の Kani／Miri は通常 PR の 4 チェックとは別に記録する。[検証スクリプト](../../core/scripts/verify.sh)・[CI](../../.github/workflows/ci.yml)

| 配布ゲート | 必要な証拠 |
| --- | --- |
| G-M1、G-M2 | OSS 表記・同梱ライセンス、タグ／バイナリ版・Plugin Verifier |
| G-M3、G-M4 | 3 OS のクリーン展開とハッシュ・実行権限、Suite B のプレイヤー／焼き込み実画素・性能 |
| G-M5、G-M6 | Linux の実 IDE と、macOS の実 IDE＋端末またはエミュレータで操作 |
| G-M7 | Windows のパイプ認証・ネイティブトレイ・プレイヤー等の実 GUI |
| G-M8、G-M9 | 異種監査・オーナー承認と、旧配布物からの設定・カタログ・保存物・デーモン更新互換 |

`release-gate.yml` は 3 OS の配布物検査等を実行する。G-M5／6 の実環境記録、G-M8 の承認も含め、同一候補コミットでどのゲートを通ったかを残す。[ゲート別の合格条件](../testing/main-release-gate.md)・[配布ワークフロー](../../.github/workflows/release-gate.yml)・[VER-34](../verifications/VER-34-main-release-gate.md)

## 実エミュレータからのブラックボックス検証

実 AVD を起動し、外部クライアントの `arb-cli mcp` から初期化・ツール列挙 → 端末選択と録画開始 → 画面変化・ログ発生 → 保存と調査 → フレーム照合 → 共有動画・GC プレビューの順に操作する。エージェントが受け取った ID・尺・件数・エラーを、保存ファイルの再生尺、実画素、カタログ行と比較する。タッチはカーネルに届く操作かを記録する。観測した差は仕様番号・前提・再現入力とともに記録し、修正後に同じ入力で再実行する。

過去の API 37 AVD 実走は当時の 17 ツール構成での記録。現行の全 20 ツールに対する実 stdio の合成データ試験や、API 28／33 の USB 実機・OEM・IDE 受け入れとは対象が異なる。[VER-31](../verifications/VER-31-pre-develop-acceptance-e2e.md)・[複数 OS 試験手順](../testing/devin-multi-os-test-plan.md)

## 仕様 vs バグ

| 観測 | 判定 |
| --- | --- |
| Rust が成功し Kotlin の同じ契約行が不一致 | クライアント間の契約違反。片側の期待値だけ変更して合格にはできない |
| S5 は成功、デーモン再起動後に同じクラッシュが再保存 | 既知制約 TCB-16。永続クラッシュ ID はモデルの前提 |
| `verify.sh` 成功、物理端末は未実施 | 自動検証合格。USB 実機の成否は未判定 |
| S6 の遅延フレームで時計が逆戻り | 実装バグ。該当順序の共有トレースを照合する |

起点は Trail #96、配布は #117・#122・#124、五層と証明昇格は #121・#123〜131・#138・#142・#144〜149、MCP 実契約は #153／PR #556。実験 Trail の結果を採用済みと判定するには統合コミットの有無を確認する。
