---
id: VER-32
type: verification
name: develop への正式昇格検証
verifies:
  - SPEC-01
  - SPEC-02
  - SPEC-03
  - SPEC-04
---

# develop への正式昇格検証

2026-11-24、基準コミット `493d06e4`（`origin/v2-develop`）。再構築した Rust コアと Compose/Jewel の IDE プラグインを `develop` へ昇格する前の最終確認の記録。

## 昇格内容

| 項目 | 確認結果 |
| --- | --- |
| 分岐の安全性 | `origin/develop` に `origin/v2-develop` へ含まれないコミットはなく、旧 v1 の `plugin/` を復元せず、そのまま昇格できる |
| 暫定の配布経路 | CI の `v2-develop` ブランチと `v2-early` タグの分岐を削除。ローリング prerelease `early` は `develop` への push だけが更新する |
| 旧設定の移行 | LegacyV1SettingsMigrator.kt は起動時の 1 箇所から呼ぶ分離実装のまま残し、v1 からの `early` 更新を滑らかにする。`main` への統合時に削除する |
| 監査指摘 | Codex 監査の 24 指摘は T-7A / T-7B / T-7C / T-8 で修正済みで、回帰試験の内訳は VER-28、VER-29、VER-30 を参照 |
| 実機相当の受入 | API 37 エミュレータの E2E と、画素レベルのタップ・スワイプ焼き込みの確認は VER-31 に記録 |
| ライセンス | `core/scripts/check-licenses.py` で全 330 クレートに GPL / AGPL は 0 件 |

## 実行したゲート

`core/scripts/verify.sh`（fmt、clippy -D warnings、test、license、sara --strict、REQ カバレッジ）、`cargo doc` の警告ゼロ、`device-agent/build.sh --with-scrcpy-audio`、`ide-plugin` の `test` と `buildPlugin`、`package_distribution.py --self-test`、ci.yml と release.yml の YAML 検証。昇格 PR では `test`、`os-parity (macos-latest)`、`os-parity (windows-latest)`、Entire Gates の 4 件が必須。

## 検証境界

VER-31 と同じく、実機 USB の Android 9 / 13、Windows・Linux のデスクトップ GUI、Android Studio 実画面の確認は対象外。正式版（`main` のタグ）は VM での IDE・エミュレータ受入後に別途切る。実録画、生ログ、端末診断情報は本記録に含めていない。
