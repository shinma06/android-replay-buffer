---
id: REQ-14
type: requirement
name: 標準動作確認対象（FHD 30fps 実機および Android エミュレータ）
---

# REQ-14: 標準動作確認対象（FHD 30fps 実機および Android エミュレータ）

一般的な Android 実機（FHD クラス・30fps 程度）と Android エミュレータ（ローカル arm64 FHD+ AVD 等）の両方を標準の動作確認対象とする。
日常開発はローカル Android エミュレータ＋決定論的モック／障害注入ハーネスで完結でき、実機確認は軽量なスモーク手順で再現可能とする。
