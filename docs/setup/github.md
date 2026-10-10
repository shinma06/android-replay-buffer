# GitHubの有効化

対象は`shinma06/android-replay-buffer`。Issue/PR/Project/Milestone/labels/rulesetsを取得して既存所有を保全します。[Project設計](../../.github/project.json)と[作業管理](../work-management.md)を使い、元プロジェクトのIDを流用しません。

## 初回導入

1. 導入Issue #1とclaim、専用branch/worktree、Project #5、Milestone #1を確認する。
2. `codex/1-adopt-harness`からmain向けPRを作成し、管理テスト・CLI tests・別セッション固定HEAD/baseレビューを行う。
3. validator導入前のmain `e6fb021b144d4f0f6a1f7916e92449610b3ff80c`・target main・このbranchだけAcceptance gateの初回bootstrapとする。独立レビューやtestsを免除せず、GUI passを表さない。初回だけ通常tooling allowlist外の`.gitignore`（private状態のignore追記）と`prompts/`（共通依頼文）を含む。既存CLI source・tests・設定・設計書の差分が空であること、追加対象、全testsを独立レビューで確認する。通常gateのallowlistは拡大しない。
4. PMが実レビューの証拠に基づいてAgent reviewを記録し、4 checksの実成功を確認する。main保護を適用・readbackして通常PRで統合する。
5. 既存developがないことを再確認し、統合後mainの同じSHAからdevelopを新設する。新設は導入の初期化であり、既存保護branchへの直接変更を許可するものではない。develop保護を適用し実効rulesをreadbackする。
6. ローカルmainをclean確認後fast-forwardし、bootstrap/doctor、trusted-main coordinatorのread-only scan、Project表示を確認する。

## 保護

[main](../../.github/main-ruleset.json)・[develop](../../.github/develop-ruleset.json)はPR必須、strict base、4 checks、会話解決、削除/force禁止、bypassなしです。mainはsquash/merge、developはsquash。GitHub approval数0は同一アカウント運用のためであり、別セッションレビューはAgent reviewで要求します。

同名rulesetを調べてから作成/更新し、重複作成しません。APIの成功だけでなく`rulesets`と`rules/branches/main`・`develop`をGETします。権限・プラン・未実行checkを「保護済み」と扱いません。

PR policyはcheckoutせずmetadataをデータとして検査し、Acceptance gateは最新trusted baseのコードを実行します。PRコードのテストはread-only token/credential非保持です。Agent reviewをPR内コードで自己承認しません。

Projectはprivate、Now/Next/Later/Past/全体/QAのViews、Status、LabelsによるPriority、Milestone、Parent、Relationship Statusを設定します。必要な権限が不足する場合は自動拡張せず対象・担当・再試行条件を記録します。

## マージ後のブランチ整理

Repositoryの `delete_branch_on_merge` を有効にし、通常のPR merge後はGitHub標準機能でhead branchを削除します。[公式手順](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-the-automatic-deletion-of-branches)のとおり保護規則等で削除されない場合もあるため、merge成功とcleanup完了は別に読み戻します。既存の残存枝は設定変更だけで整理済みとしません。

ローカルは `git fetch --prune origin` でremote-tracking refsを同期します。このcloneのoriginが標準の `+refs/heads/*:refs/remotes/origin/*` で、tag pruningを使っていないことを確認した場合は `git config --local remote.origin.prune true` で以後のfetchにも適用できます。global設定やtagの削除範囲は変更しません。

残るlocal branch/worktreeは[終了手順](../workflow.md#自動進行と終了)に従い、所有・停止・clean・PRの最終HEAD・現在のremote/local SHA・worktree利用を照合して整理します。squash後の祖先判定や `[gone]` だけでは削除しません。再利用された枝、未統合成果、未解放claim、GUI使用中、私的証拠は保持します。証拠を保持する停止済みclean worktreeは同じSHAのdetached HEADへ移し、ファイルを残したまま枝だけ整理できます。保持理由・owner・再開条件をIssueに残し、実pathはprivateに置きます。既存coordinatorのcleanup/branch auditを利用できる条件では再利用し、別の常駐削除jobは追加しません。
