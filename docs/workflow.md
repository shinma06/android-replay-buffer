# 開発・レビュー・二段階統合

作業は Issue → claim → 専用 branch/worktree → Draft PR → 必要な検証 → 独立レビュー → target 別 gate → merge → QA/Project/後片付けの順に進めます。読み取りだけの相談・レビューでは Issue を新設しません。公開操作は利用者の依頼範囲に従います。

## 公開操作の承認範囲と失敗の扱い

利用者の既存の依頼・承認は、その対象と操作範囲内で継続します。依頼された作業に含まれる専用branchのpush、Issueコメント、PR作成・更新を、追加承認が必要という想定だけで止めません。利用者が範囲を限定・撤回した場合はその指示を優先し、別repository、秘密情報の公開、正式Release等へ許可を広げません。実行環境の権限、通常の検証・独立レビュー・統合条件は維持します。

- 担当へ引き継ぐ際は、本人指示の出典と該当内容、対象Issue/repository、許可された操作・制限を渡します。受け手は本人指示を根拠として照合し、PMや別agentの判断だけを新しい本人承認にしません。公開記録には必要な範囲だけ要約し、私的な会話や認証情報は載せません。
- 接続・認証・権限エラーと、自動承認審査による拒否を返却結果に従って区別します。接続失敗を承認不足と読み替えません。書込みの成否が不明なら、再試行前に投稿・ref・PR等の実状態を読み戻し、重複を防ぎます。
- 実際に拒否された場合は、対象操作・tool/call・返却理由を私的な作業記録に保持し、本人指示と未充足条件を照合します。原返却が確認できなければ「拒否は未確認」と記録し、未試行のpushやPRまで拒否されたと扱いません。実拒否の別経路での迂回は禁止し、独立して進められる承認済み作業は継続します。
- 不足する本人承認が実際に必要な場合だけ、既存の承認で足りない対象・操作と根拠を示して確認します。停止報告では、確認済みの失敗と未試行の保留を分け、解除条件を具体的に記します。

## 開始と所有

Issue の全コメント、関連 PR、依存、既存 claim、dirty、worktree を確認します。owner・対象/対象外・受入・base SHA・target・reviewer・GUI 要否・次操作を記録し読み戻します。一つの作業範囲の writer は一人です。

```bash
git status --short --branch
git worktree list
git fetch --prune origin
# 例: 実在する Issue 12 の製品変更。番号・slug は実際の作業へ変更する
git worktree add -b codex/12-replay-integration ../android-replay-buffer-issue-12 origin/develop
cd ../android-replay-buffer-issue-12
git branch --unset-upstream
python3 scripts/bootstrap.py
```

branch は `codex|claude|cursor|agent/<Issue番号>-<slug>`。運用変更だけなら origin/main を起点にします。main/master/develop へ直接 commit/push、force push、hook/保護の迂回、他担当の変更破棄は禁止です。

## 先行プラグインの知見を使う

IDE/build/process/保存/UI/検証/共通ハーネスに関係する変更は、[Cursor知見活用の運用](android-studio.md#作業に組み込む運用)を開始・設計・レビューへ組み込みます。参照元の固定SHA、採用/適合/非適用の理由、こちらの検証を既存Issue/PRへ記録します。CLI固有の小修正や表記だけなら適用外を一言で示します。

## 検証と PR

要件・呼出し元/先・既存テストを読み、既存実装・標準機能から最小の変更を選びます。`python3 scripts/check.py` は管理ファイルと回帰テスト、製品変更は [CLIテスト](project.md) を実行します。変更影響判定は hook/CI/coordinator で `scripts/workflow/change_impact.py` を共用し、rename/delete/mode 変更・未知・不完全履歴を安全側に扱います。

最初の意味ある push で Draft PR を作成し、PR テンプレートの `Issue / Integration / Verification / GUI / GUI reason` を埋めます。受入は `docs/verification/changes/issue-N.json` に記録します。独立したセッションへ固定 HEAD/base と受入を渡し、具体的な指摘を修正して必要な再レビューを行います。自己レビューや CI 成功を独立レビューの代わりにしません。

| target / Integration | 統合条件 | merge |
|---|---|---|
| develop / `develop` | 必要テスト、独立コードレビュー、全必要 Case と次操作。GUI pending/blocked/fail は保存し、fail は修正 Issue に紐づける | squash |
| main / `promotion` | 固定候補の全 commit/Case、同じ build の実観察、独立レビュー、必要 checks。[初期版の段階受入](verification/README.md#初期版の段階受入64)では固定計画の延期部分だけを後続へ移す | merge commit |
| main / `tooling` | docs/scripts/CI/agent 入口のみ、GUI 不要の具体的理由と CLI 検証 | squash |

製品コード・build設定を tooling として迂回させません。develop は `Refs #N` を使い、自動 close 文言は禁止です。独立レビュー・CI 失敗は GUI 未実施とは別で、解消するまで統合しません。

## 自動進行と終了

writer が編集・commit・push・GUI を停止してから [自動進行](setup/automation.md) へ登録します。4 checks（test、PR policy、Acceptance gate、Agent review）と会話解決、現在の HEAD/base/受入を再確認し、保護付きの通常 PR merge を使います。

merge 後は [作業管理の終了確認](work-management.md) と QA 引継ぎを行います。remote 実 ref、local branch、remote-tracking ref、worktree を照合し、自分の停止済み・clean な資源だけを整理します。squash 後の `--merged` や `[gone]` だけを削除根拠にせず、PR HEAD と統合結果を確認します。main/master/develop、他担当、GUI 使用中、未公開成果物は保全します。

今回の初回導入・developの新設は [GitHub 有効化手順](setup/github.md) に従います。新しい規約の追加を、未依頼の公開・他セッションへの連絡・定期実行を始める権限として扱いません。
