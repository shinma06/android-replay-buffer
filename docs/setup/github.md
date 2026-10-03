# GitHubの有効化

対象は`shinma06/android-replay-buffer`。Issue/PR/Project/Milestone/labels/rulesetsを取得して既存所有を保全します。[Project設計](../../.github/project.json)と[作業管理](../work-management.md)を使い、元プロジェクトのIDを流用しません。

## 初回導入

1. 導入Issue #1とclaim、専用branch/worktree、Project #5、Milestone #1を確認する。
2. `codex/1-adopt-harness`からmain向けPRを作成し、管理テスト・CLI tests・別セッション固定HEAD/baseレビューを行う。
3. validator導入前のmain `e6fb021b144d4f0f6a1f7916e92449610b3ff80c`・target main・このbranchだけAcceptance gateの初回bootstrapとする。独立レビューやtestsを免除せず、GUI passを表さない。通常のtooling scopeを満たす差分だけで導入する。
4. PMが実レビューの証拠に基づいてAgent reviewを記録し、4 checksの実成功を確認する。main保護を適用・readbackして通常PRで統合する。
5. 既存developがないことを再確認し、統合後mainの同じSHAからdevelopを新設する。新設は導入の初期化であり、既存保護branchへの直接変更を許可するものではない。develop保護を適用し実効rulesをreadbackする。
6. ローカルmainをclean確認後fast-forwardし、bootstrap/doctor、trusted-main coordinatorのread-only scan、Project表示を確認する。

## 保護

[main](../../.github/main-ruleset.json)・[develop](../../.github/develop-ruleset.json)はPR必須、strict base、4 checks、会話解決、削除/force禁止、bypassなしです。mainはsquash/merge、developはsquash。GitHub approval数0は同一アカウント運用のためであり、別セッションレビューはAgent reviewで要求します。

同名rulesetを調べてから作成/更新し、重複作成しません。APIの成功だけでなく`rulesets`と`rules/branches/main`・`develop`をGETします。権限・プラン・未実行checkを「保護済み」と扱いません。

PR policyはcheckoutせずmetadataをデータとして検査し、Acceptance gateは最新trusted baseのコードを実行します。PRコードのテストはread-only token/credential非保持です。Agent reviewをPR内コードで自己承認しません。

Projectはprivate、Now/Next/Later/Past/全体/QAのViews、Status、LabelsによるPriority、Milestone、Parent、Relationship Statusを設定します。必要な権限が不足する場合は自動拡張せず対象・担当・再試行条件を記録します。
