# 開発・レビュー・二段階統合

作業は Issue → claim → 専用 branch/worktree → Draft PR → 必要な検証 → 独立レビュー → target 別 gate → merge → QA/Project/後片付けの順に進めます。読み取りだけの相談・レビューでは Issue を新設しません。公開操作は利用者の依頼範囲に従います。

## 公開操作の承認範囲と失敗の扱い

**担当者が普段の作業でリスクを抑え、依頼範囲の作業を継続することを既定にします。** 承認を安全性の代用にせず、未確認を安全とみなしません。対象確認・隔離・復旧準備など自分で解消できる不足は、その作業を進めて解消します。作業停止にもコストがあるため、具体的な損失や未充足条件を示せない追加承認待ちは設けません。

開始時と影響が変わる時に、次のうち関係する点を確認します。既存のIssue・実行計画・証拠を再利用し、同じ条件を毎操作で審査し直す台帳やチェックリストは増やしません。

- **影響を限定する**: 実際のrepository、相手、画面、端末、保存先と所有者を識別し、使い捨てfixture・対象指定・最小の採取/公開内容を選ぶ。「ローカル」という名前だけで本番非接続や秘密情報なしと判断しない。
- **失敗から戻せるようにする**: 未保存・未公開データを保全し、変更差分、復元先、停止対象を明確にする。成功した公開も完全には取り消せないため、公開前に宛先と内容を確認する。他担当の変更を全体backupや一括killで巻き戻さない。
- **広がりを制限する**: 必要な回数・時間・容量・費用と中止条件を定め、無条件の再試行や常駐化を避ける。結果不明の書込みは実状態を読んでから再試行する。
- **結果を確かめる**: 操作結果、必要な検査、復元・終了を確認する。未達は未達のまま残し、許可や実行開始を成功の証拠にしない。

### 続行・調整・本人確認の分担

条件が揃った通常の編集・検査、専用branchのpush、Issueコメント、PR作成・更新、通常の統合、所有資源の整理は、依頼範囲内で担当判断により進めます。GUI操作・導入・通常再起動・限定診断も、対象・保全・復元・実行上限を整えれば都度の本人承認を求めません。独立レビュー・CI・受入・保護条件は継続の品質条件であり、本人承認に置き換えません。

PMは担当・共有資源・依存・実行計画を調整します。既存の割当と計画内の操作ごとにPMのGOを取り直さず、競合や影響が変わる部分だけ担当間で調整します。内部の運用上限を変える必要があれば、次の試行前に理由・影響・復元を記録して計画を更新します。本人の指定上限、正式な受入条件、ツール制約は変更せず、期限を事後に延ばして過去の失敗を成功にしません。「別途承認」だけの記述は、誰が何を判断する条件かを確認し、直ちに本人承認待ちとは解釈しません。

本人へ戻すのは、必要な準備を済ませても、次の境界が残る場合です。

- 依頼外のrepository・相手・公開先・本番利用者などへ影響を広げる必要がある。
- 重要なデータ消失、私的情報の露出、費用・共有環境への影響を十分に限定できず、本人の選択や引受けが必要。ただし承認があっても禁止事項を実行しない。
- OS・サービス・実行ツールが本人の認証・同意操作を要求する、または本人の明示した停止・制限を変更する必要がある。

この場合も該当操作だけを止め、何が起き得るか、実施済みの対策、それでも本人にしか決められない点を具体的に示します。独立して進められる依頼済み作業は継続します。正式Releaseや定期実行は最初に対象・公開先/周期・上限・停止条件を含む依頼範囲を確定し、その範囲内の各実行では同じ許可を取り直しません。新しい規約自体を、その開始依頼や権限付与の代わりにしません。

チーム時の本人への相談は[相談・確認窓口](team-communication.md)へ集約し、必要な承認だけ選択カードで求めます。窓口への転送を待って許可済みの独立作業を止めず、実tool/OSの本人同意を窓口の代理判断へ置き換えません。

### 引継ぎと実行失敗

担当へは本人指示の出典・対象・操作範囲と、確認済みの対策・残条件を渡します。受け手は根拠を照合し、既存の許可されたチーム内で継続します。PMや別agentの主張だけを新しい本人承認にしません。公開記録には必要な要約だけを置き、私的会話・認証情報は載せません。

接続・認証・権限エラーと自動承認審査の拒否は原返却から区別します。接続失敗を承認不足と読み替えず、必要な環境修復を行います。実拒否は対象操作・tool/call・返却理由をprivateに保持し、本人指示と未充足条件を照合します。原返却がなければ「拒否は未確認」、実行していなければ「未試行」と記録し、別操作へ拒否を広げません。実拒否や実行権限の不足を別経路で迂回しません。

## 開始と所有

Issue の全コメント、関連 PR、依存、既存 claim、dirty、worktree を確認します。owner・対象/対象外・受入・base SHA・target・reviewer・GUI 要否・次操作を記録し読み戻します。[Development の実関連](work-management.md#development-の実関連)も確認し、本文参照と実関連、default branch の早期 close 境界を区別します。一つの作業範囲の writer は一人です。

```bash
git status --short --branch
git worktree list
git fetch --no-prune origin
# 例: 実在する Issue 12 の製品変更。番号・slug は実際の作業へ変更する
git worktree add -b codex/12-feat-replay-integration ../android-replay-buffer-issue-12 origin/develop
cd ../android-replay-buffer-issue-12
git branch --unset-upstream
python3 scripts/bootstrap.py
```

新規branchは [branch命名](work-management.md#branch-命名) の `codex/<Issue番号>-<type>-<短い英語の目的>` を既定とします。既存refは改名せず、旧形式の読取互換性を保ちます。運用変更だけなら origin/main を起点にします。main/master/develop へ直接 commit/push、force push、hook/保護の迂回、他担当の変更破棄は禁止です。

## 先行プラグインの知見を使う

IDE/build/process/保存/UI/検証/共通ハーネスに関係する変更は、[Cursor知見活用の運用](android-studio.md#作業に組み込む運用)を開始・設計・レビューへ組み込みます。参照元の固定SHA、採用/適合/非適用の理由、こちらの検証を既存Issue/PRへ記録します。CLI固有の小修正や表記だけなら適用外を一言で示します。

## 検証と PR

要件・呼出し元/先・既存テストを読み、既存実装・標準機能から最小の変更を選びます。`python3 scripts/check.py` は管理ファイルと回帰テスト、製品変更は [CLIテスト](project.md) を実行します。変更影響判定は hook/CI/coordinator で `scripts/workflow/change_impact.py` を共用し、rename/delete/mode 変更・未知・不完全履歴を安全側に扱います。

[PR タイトル規約](work-management.md#pr-タイトル) の `type: 日本語の要約 (#N)` を作成・ready・固定レビュー・merge 前の実 metadata に適用します。題名の変更は固定 review binding を失効させ、policy/独立評価を照合し直します。Issue 題名と commit 履歴の形式は変更しません。

最初の意味ある push で Draft PR を作成し、PR テンプレートの `Issue / Integration / Verification / GUI / GUI reason` を埋めます。[PR sidebar の実情報](work-management.md#pr-sidebar-の実情報)に従って native Assignees/Labels/Projects/Milestone/Reviewers を設定・読み戻し、ready・修正再開・終了時にも照合します。[Development の実関連](work-management.md#development-の実関連)に従って対応 PR を結び、両端と Issue 状態を読み戻します。main の未完 Issue は関連を終了時まで保留し、理由・担当・次操作を記録します。受入は `docs/verification/changes/issue-N.json` に記録します。独立したセッションへ固定 HEAD/base と受入を渡し、具体的な指摘を修正して必要な再レビューを行います。自己レビューや CI 成功を独立レビューの代わりにしません。

| target / Integration | 統合条件 | merge |
|---|---|---|
| develop / `develop` | 必要テスト、独立コードレビュー、全必要 Case と次操作。GUI pending/blocked/fail は保存し、fail は修正 Issue に紐づける。受入NG修正は[元失敗保証の再試験と独立評価](verification/README.md#受入ng修正のdevelop統合226)を追加 | squash |
| main / `promotion` | 固定候補の全 commit/Case、同じ build の実観察、独立レビュー、必要 checks。[初期版の段階受入](verification/README.md#初期版の段階受入64)では固定計画の延期部分だけを後続へ移す | merge commit |
| main / `tooling` | docs/scripts/CI/agent 入口のみ、GUI 不要の具体的理由と CLI 検証 | squash |

製品コード・build設定を tooling として迂回させません。develop は `Refs #N` を使い、自動 close 文言は禁止です。独立レビュー・CI 失敗は GUI 未実施とは別で、解消するまで統合しません。

リモート指摘は[finish-work](../.agents/skills/finish-work/SKILL.md)の取得・対応手順で照合します。`python3 scripts/workflow/agent_loop.py feedback --pr N`は読取専用です。raw結果はprivateに保存し、review本文、inline/返信、PR/Issue conversation、botの最新summary、解決/旧版状態を省略せず確認します。既存Issue/PRへ指摘ID/URL・元commit・現固定版の修正/検査又は根拠付き非該当/置換を残し、不明・未解決を承認しません。

Draftをreadyにすると設定済みのレビューが非同期に始まり得ます。[完了元の選択](setup/automation.md#レビュー元と実行元の選択)をfeedback取得から統合・終了まで共通に使います。既定のCodex経路は現HEADのCode Reviewがprovider summaryでCompletedになるまでpendingです。明示選択したGitHub経路は指定reviewerの現HEADのterminal reviewを確認します。空の取得、旧版の完了・reaction・CI成功・時間経過を代用せず、観測不能/失敗/不明な形式は未達として引き継ぎます。方式変更で既存の未完了依頼を取り消しません。完了信号は独立承認や将来の追加指摘がない保証ではなく、全指摘を処理した固定版を別セッションでレビューします。

merge直前にHEAD/base/feedback hashを再取得し、変更があれば対応判断とレビューをやり直します。merge後・close前も読み戻します。後着・編集で具体的な未解決指摘が出たら、原Issue又は専用修正Issueに元レビューと実再現・担当・次操作を残して該当範囲を再開します。過去の承認は当時のsnapshotとして保存し、指摘なしの永久保証にしません。常駐監視、余分な本人承認、終了済みwriterの暗黙再開は追加しません。

### 方法の改善とフィードバック

PM・担当・レビュアーは現状の手段を当然とせず、検査失敗がなくても改善できる負担・重複操作・停止の具体的な根拠を捉え、過去の手順を反復する前に問題と代替案を提案します。守るべき製品保証・実権限・本人の制限と、変更できる実施方法/管理上の宣言を分けます。固さ自体を欠陥とせず、その方法が目的を満たすために必要かを[最小検証](verification/responsibility.md)で確認します。

改善は「観測した負担/不足→保証を保つ代替案→範囲・保全/復元・成功/中止条件を定めた有限で戻せる試行→実結果を使った採用/修正/撤回→[古い管理前提の整合](context.md#更新時の古い管理前提)」で進めます。依頼範囲内で担当が進め、既存Issue/計画へ重要な事実だけ記録します。実OS/tool権限・本人境界と正式契約の改訂/独立レビュー条件は維持し、方法hashを固定するためだけに必要な改訂を避けません。実行する改善案には実試行と結果/feedback、担当を確保し、提案/机上比較だけで改善完了にしません。未実施/blockedの試行は担当・残条件を持つ未完了として引き継ぎ、次の自然な該当作業で確認する場合も実結果を回収します。改善案と無関係な追加GUI試験、無限loop/job、長い様式や新承認gateは作りません。

## 自動進行と終了

writer が編集・commit・push・GUI を停止してから [自動進行](setup/automation.md) へ登録します。5 checks（test、PR policy、Acceptance gate、Agent review、CLI Python 3.9）と会話解決、現在の HEAD/base/受入を再確認し、保護付きの通常 PR merge を使います。

merge 後は [作業管理の終了確認](work-management.md) と QA 引継ぎを行い、次のbranch lifecycleで資源を整理します。統合・Issue完了・claim移管/解放・資源削除は別に読み戻します。

### branch lifecycle

branch自体にClosed状態はありません。実PRを **merged** と **closed-unmerged（withdrawn/supersededを含む）** に分けます。Closed、名前、古さ、`[gone]`、`--merged` や祖先判定だけを削除・claim解放の根拠にしません。

- 共通: 実PRとreplacement/取り込み先、資源のownerとwriter停止・明示されたPM保管責任、remote実ref/local branch/tracking refの各SHA、全worktreeでの使用、dirty/untracked・必要ignored成果物、固有の未公開/未統合commitを確認します。PR HEADと現在のtipが違えば差分の帰属・保全を確認するまで保持します。main/master/develop、稼働中・他担当・GUI使用中の資源は削除しません。
- merged: PR HEAD・merge SHA・取り込み結果と終了/QA条件を照合し、既存の所有確認付きcleanupとGitHubのmerge後head自動削除を使います。squashでも固有の作業や証拠を取り落としません。
- closed-unmerged: 元の失敗・指摘とreplacementを保持し、再開/QA/復旧で何を残すかownerが決めます。終了理由だけで自動cleanupへ登録し直しません。退役させる固有commit/内容と必要なignored成果物を、既存の復元可能なarchive/snapshot/Git bundle等へ削除範囲外で保管します。固定ref/SHAとarchiveのhashを読み戻し、所有する使い捨て復元先で実際に固定commitをcheckoutして内容を照合します。Git bundleなら`git bundle verify`だけで終わらず、復元先へのfetch/checkoutとtree/必要ファイルの照合まで行います。bundleに入らない必要ファイルは別保管しhash/復元内容を確認します。app管理archiveを使う場合も復元結果を照合し、必要ignoredファイルを別保全します。原証拠を移動・上書きして保管済みとしません。

保全/復元確認後、所有引継ぎと停止を満たす範囲だけ実状態を削除直前に再照合して整理し、remote/local/tracking/worktreeの結果を個別に読み戻します。退役に必要な保全を完了してcleanと確認できないworktreeや、QA・復旧・再開に必要な資源は残し、owner・理由・再開/退役条件を元Issue/PRへ記録します。ローカルpath・bundle原本・私的証拠はprivateに置きます。claimの扱いは明示した引継ぎ/解放に従い、archiveや時間経過から推測しません。新しい保管framework、daemon、定期job、一括purgeは追加しません。

`cleanup-branches`はfetch/pruneを行わず、localとtracking-onlyのclosed-unmerged ref/SHAを保持として報告するmerged-onlyの補助監査です。通常fetchのprune設定も含め、tracking-onlyに固有commitが残る場合は保全・実復元確認までpruneを遅延します。closed-unmerged候補は同名branchの全PR番号/各固定HEADと現在のlocal/tracking SHAの一致・差分・参照不在を個別に報告し、この手順で担当が扱います。保持表示は過去HEADの復元成功を意味しません。自動監査の出力だけを未マージ資源の退役許可にしません。

今回の初回導入・developの新設は [GitHub 有効化手順](setup/github.md) に従います。新しい規約の追加を、未依頼の公開・他セッションへの連絡・定期実行を始める権限として扱いません。
