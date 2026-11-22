---
id: REQ-07
type: requirement
name: 旧保存物の読み取り専用互換（schema 3 / schema 4）
---

# REQ-07: 旧保存物の読み取り専用互換（schema 3 / schema 4）

ARB v2 の保存フォルダ構造・内部仕様は新規設計としてよいが、過去の保存物は新しい ARB でも閲覧・再生できること。
具体的には schema 4（`develop` 系統、`replay.json`、ログ単体保存を含む）および schema 3（`v0.2.0` 系統、`session.json` / `video-001.mp4`）の両方を元のファイルを一切書き換えずに読み取り専用で開ける互換アダプタ（`LegacyBundleReader`）を提供する。
