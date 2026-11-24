---
id: VER-26
type: verification
name: T-6H オーナー実機試用フィードバック対応（チェックボックス整列・Tap/Swipe 判別・ループ再生・ログ自動追従）の検証記録
verifies:
  - SPEC-01
  - SPEC-02
  - SPEC-03
  - SPEC-04
  - REQ-07
  - REQ-08
  - REQ-09
  - REQ-11
  - REQ-12
---

# VER-26: T-6H Studio UX 仕上げ（チェックボックス整列ヘルパー・Tap/Swipe 判別・ループ再生トグル・ビデオログタイムライン自動追従同期） 検証記録

記録日: 2026-11-24（macOS arm64 / JDK 27 / Rust workspace `core/`）

## 1. 目的

オーナーの実機試用フィードバック 4 件を 1 つの Entire Trail で解消する。

1. Jewel `CheckboxRow`/`Checkbox` の光学的な垂直ずれ（上流 JEWEL-1073 相当）を 1 箇所の共有コンポーネントで恒久修正し、`ide-plugin/src/main/**` 内の生の `CheckboxRow`/アドホックな `Checkbox`+`Text` 組み合わせを全廃する。
2. `getevent` 由来の生タッチサンプルを PC 側で指単位の `DOWN -> MOVE* -> UP/CANCEL` ストロークへ集約し、解像度に応じたタッチスロップでストロークごとに決定論的に Tap / Swipe を分類する（Android 実機側の追加負荷ゼロ）。
3. Studio のメディア・トランスポートバーにループ再生トグルを追加する（デフォルト OFF）。
4. 右側 Event/Log パネルに再生位置自動追従（`followPlayhead`）トグルを追加する（デフォルト ON）。

## 2. 変更内容

### Item 1: `ArbLabeledCheckbox` 共有コンポーネント

- `ide-plugin/src/main/kotlin/io/github/shinma06/arb/ui/ArbJewelTheme.kt` に `ArbLabeledCheckbox` を追加。Jewel `Checkbox` を 1px 下にオフセットし、ラベル側は `LineHeightStyle` で行ボックスを中央トリムして視覚的に整列させる。クリック/ホバー/アクセシビリティは外側の `Row.toggleable` が担い、内側の `Checkbox` は同じ `interactionSource` を共有する「見た目だけ」の描画（二重トグルなし）。
- `ArbSettingsConfigurable.kt`（3 箇所）・`ArbStudioContent.kt`（トラッシュ原本トグル、ループトグル、自動追従トグル）の生 `CheckboxRow` 呼び出しをすべて置換。
- 新規ガードテスト `ArbCheckboxAlignmentGuardTest`（`ide-plugin/src/test/.../ui/`）: `ide-plugin/src/main` 全体を走査し、`ArbJewelTheme.kt` 内の定義を除いて `CheckboxRow(` / アドホックな `Checkbox(` が 0 件であることを常時検査。

### Item 2: Tap/Swipe ジェスチャー判別

- **`core/crates/arb-core/src/gesture.rs`（新規）**: `GestureKind`（`#[non_exhaustive]`, デフォルト `Tap`）、`TouchPoint`、`touch_slop()`（`max(20.0, 0.025 * min(display_w, display_h))`、解像度不明時フォールバック `24.0`）、`classify_stroke()`（DOWN からの直線距離と最大逸脱の両方でスワイプ&復帰も正しく Swipe 判定、パスは先頭/末尾を必ず保持しつつ 24 点まで間引き）。単体テスト 9 件。
- **`arb_core::bundle::TapRecord`**: `gesture_kind` / `end_x` / `end_y` / `end_t_offset_ns` / `path` を `#[serde(default)]` で追加し、既存バンドル・テストの JSON は変更なしで読み込み可能。
- **`arb-storage`（`bundle_io.rs`）**: `taps` テーブルに同 5 列を追加。既存の `logs.uid`/`logs.package_name` と同じ `sqlite_column_exists` 分岐パターンで、列が存在しない旧 v2 バンドルも例外なく読み込める。`v2_bundle_write_read_roundtrip_and_tamper_detection` に Swipe（非空 `path`・`end_x`/`end_y`/`end_t_offset_ns`）の往復ケースを追加。
- **`arbd`（`capture.rs`）**: `ActiveStroke` が指（pointer_id）ごとに `DOWN -> MOVE* -> UP/CANCEL` を集約し、`UP`/`CANCEL` で `classify_stroke` を 1 回だけ呼んでサンプル数に関係なく 1 ストローク = 1 イベントを生成。`display_width`/`display_height` から保存ジョブ開始時に 1 回だけ解像度対応スロップを算出。`build_draft` 終了時に未完了ストロークを必ずフラッシュ（切断・停止時のジェスチャー消失防止）。新規単体テスト 5 件（短押しは常に 1 Tap・長いドラッグは 1 Swipe・切断時フラッシュ・マルチタッチ独立分類・UP 欠落後の新規 DOWN での安全なフラッシュ）。
- **`arbd`（`protocol.rs` / `inspect_export.rs`）**: `OverviewTapItem` に `gesture_kind`/`end_x`/`end_y`/`end_t_ms` を `#[serde(default)]` で追加し、`timeline.overview` RPC が Tap/Swipe を区別して公開。
- **`ide-plugin/client-core`**: `ProtocolModels.kt` に `GestureKind`・`TouchPoint` と `TapRecord` の新規フィールドを追加。Gson は「全コンストラクタ引数にデフォルト値がある場合のみ」Kotlin のデフォルト値を尊重する制約があるため（`TapRecord` は `seq` 等の必須フィールドを持つ）、専用 `JsonDeserializer`（`TapRecordAdapter`）を実装し、旧バンドル（フィールド欠落）・現行バンドルの空 `path`（`skip_serializing_if = "Vec::is_empty"` で省略）の両方を安全に `Tap`/`null`/空リストへ縮退させる。JSON 往復・旧バンドル互換のテストを追加。
- **Viewer UI（`ArbStudioContent.kt` ほか）**: 動画キャンバスオーバーレイは Tap をリップル、Swipe をポリライン + 矢印 + 始点から終点へ往復するアニメーション点で描画。Event/Log 行は `ArbFormatters.formatTapDisplay` で `Tap #N (x, y)` / `Swipe #N (x1, y1) -> (x2, y2) (上/下/左/右スワイプ)` を生成（絵文字ゼロ）。マルチトラックタイムラインのマーカーレーンは Tap を点マーカー、Swipe を区間バー + 終点ピンで描画。

### Item 3: ループ再生トグル

- `PlaybackState.loopInSelection` のデフォルトを `true` → `false` に変更（既存のテストはすべて `initialLoopInSelection` を明示していたため停止挙動テストは無傷）。`PlaybackController` に `loopEnabled` / `setLoopEnabled` / `toggleLoop` を追加。
- `ArbStudioController` に `isLoopEnabled` / `loopTooltip`（「ループ再生 ON」/「ループ再生 OFF」、絵文字・`v2` 表記ゼロ）/ `toggleLoop` を追加し、トランスポートバーにコンパクトな Jewel トグルボタンを配置。
- 新規テスト 3 件: フルレンジ無トリムでのループ OFF 停止、フルレンジ・トリムレンジでのループ ON ラップアラウンド、`toggleLoop` の反転。

### Item 4: ログ自動追従

- `activeTimelineItemIndex(rows, playheadFrameIndex, toleranceFrames)` を純粋関数として `client-core` に追加（再生位置以前で最後の行、なければ許容範囲内の直後の行を返す）。単体テスト 8 件（空リスト・先頭行より前・許容範囲内外・完全一致・行間・フィルタ後サブセット・トグル OFF→ON 再スナップ）。
- `ArbStudioController` に `followPlayhead`（デフォルト ON）・`activeTimelineItemIndex` ゲッター・`selectFollowPlayhead`/`toggleFollowPlayhead` を追加。
- 右パネルのタブ帯に「自動追従」トグルを追加し、ON 時は `LazyListState.animateScrollToItem` で上から 1/3 オフセットへスムーズスクロール、OFF でユーザースクロール位置を保持、再度 ON で即座にスナップ。アクティブ行は控えめなハイライト（アクセント色の帯）で強調。

## 3. 検証結果

- **`cargo fmt --check` / `cargo clippy --workspace --all-targets -- -D warnings`**: PASS（警告・エラー 0）。
- **`cargo test --workspace`**（`core/`）: 全件 PASS（`arb-core` gesture 単体テスト 9 件、`arbd::capture` T-6H ジェスチャー統合テスト 5 件、`arb-storage` Swipe 往復ケースを含む全テスト、`arbd` 統合テスト群、MCP `timeline.overview` の `gesture_kind`/`end_x` 検証を含む）。
- **`(cd ide-plugin && ./gradlew test --no-daemon)`**（JDK 27）: 全件 PASS（`ArbCheckboxAlignmentGuardTest`・`ArbBrandingGuardTest`・`PlaybackControllerTest`（ループ 3 件含む）・`ActiveTimelineItemIndexTest`（8 件）・`ArbFormattersTest`（Tap/Swipe 表示・方向ラベル 4 方位・旧バンドル互換）・`ArbT4StudioModelTest`（Swipe オーバーレイ・行表示連携）・`DeviceSelectionPolicyTest`（`TapRecord`/`GestureKind` JSON 往復・旧バンドル縮退）を含む）。

## 4. 未検証

- 実機（USB 接続の Android 端末）での実際のスワイプジェスチャー録画・Studio でのビジュアル確認は本自動化の範囲外のため未実施（単体・統合テストによるロジック検証のみ）。
- Android Studio GUI 上でのチェックボックス整列・ループボタン・自動追従トグルの目視確認は未実施。
- `./device-agent/build.sh --with-scrcpy-audio` は Android SDK 依存のためこの検証セッションでは未実行（コード変更なし、影響範囲外）。
