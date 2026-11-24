---
id: REQ-09
type: requirement
name: Android Studio / IntelliJ IDEA 主役と CLI・トレイの基盤統一
---

# REQ-09: Android Studio / IntelliJ IDEA 主役と CLI・トレイの基盤統一

主たる操作入口は Android Studio / IntelliJ IDEA プラグインとし、CLI（`arb-cli`）およびメニューバー／トレイ（`arb-tray`）も同一のローカルデーモン（`arbd`）と IPC 契約で統一する。
IDE プラグインには時刻計算・選別・ファイルロック・MP4 処理・状態遷移を持たせず、薄い Kotlin UI シェルに徹する。
