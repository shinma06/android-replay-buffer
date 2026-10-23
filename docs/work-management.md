# Issue・Project・Milestone の管理

本プロジェクトでは次の責務分担で運用します。Issue は具体的な作業と受入、Project は全体の状態・優先順位・ロードマップ、Milestone は到達目標、native Relationship は実際の分解・依存を表します。

## Issue 作成・triage

1. 重複・コメント・関連 PR・既存 claim を確認し、単一の目的・対象外・観察可能な受入・次の操作を書く。
2. Project に登録する。未接続なら未反映の対象、担当、再試行条件を Issue に記録する。
3. 既存の適切な Milestone を選ぶ。未決なら理由・判断担当・判断条件を記載し、架空の版や期日を作らない。
4. 実際の分割なら parent/sub-issue、開始・完了に必要な先行作業なら blocked by/blocking を設定する。分類のための万能親や偽の依存を作らない。
5. native 関係の両端を読み戻す。関係なしは正常な `Standalone`、関係ありは `Has Relationship`。未判定は空欄とし、空欄を Standalone と推測しない。
6. 題名と各軸ちょうど 1 つのラベルを確認する。

| type ラベル | 題名 |
|---|---|
| `type:feature` | `feat: 日本語の要約` |
| `type:bug` | `fix: 日本語の要約` |
| `type:research` | `research: 日本語の要約` |
| `type:qa` | `test: #元Issue番号 日本語の要約` |
| `type:maintenance` | `chore: 日本語の要約` |
| `type:tracking` | `track: 日本語の要約` |

この改訂の採用後に新しく作るIssueと、明示して改名する対象の題名は **`type: 日本語の要約`** とします。typeは上の6ラベルとの一意対応、小文字・scopeなしです。Issue自身の番号や優先度、PR用の`(#N)`末尾は重ねません。QA要約内に追加の`#N`は置かず、QAだけは単一の元Issueを `test: #35 保存失敗を確認する` のように接頭辞直後へ置きます。要約は自然な日本語とし、Kotlin・SDK等の技術名は保持します。

`feat` / `fix` / `test` / `chore`は既存PRの語彙を使い、`research` / `track`は既存の調査/追跡分類を表す本project専用の語です。世界共通のIssue標準やConventional Commitsの必須分類とは呼びません。6つのtypeラベルと実native関係を維持し、新しい分類やnative Issue typeは作りません。PR/commitのtypeは引き続き実変更で選び、Issue分類をそのままコピーしません。

新規作成/指定された改名はこの形式を確認します。共有validatorとCIの実inline copyはラベルとの対応・QA元番号・日本語文字・自身の番号/優先度の重複を確認し、旧角括弧形式は既存/未移行/固定履歴の読取りだけに残します。文字検査は意味の正しさを保証しません。新規自動QAはcanonical形式を公開前に検査し、検索/再利用/読み戻しは新旧どちらの元番号も区切り込みで照合します。#3と#35を混同せず、新旧2件なら勝手に片方を採らずownerへ戻します。重いparser、日付/ID cutoff、registry、新hookは作りません。

自動QAでは数字付き`#`を含む要約を「元Issueの残受入を確認する」にし、色・URL・識別子をIssue参照として書き換えません。元題名全文はHTML escapeして固定のpre要素内へ置き、新規QAの本文（既存QAの再利用時は追記コメント）へ保持します。Case・担当・次操作はその要素の外に置きます。旧形式の余分な空白を整え、日本語のない旧要約には `残りの受入試験: ` を補います。元Issue番号・リンク・Case出典と既存QAの題名/本文は維持し、新規QAのcanonical検証は緩めません。

実Issueの改名は、規約の独立レビュー・main採用後に、PMが固定した当repoのOPEN一覧と単独metadata ownerへ引き継ぎます。prefixだけを変え、要約・本文・state/labels/assignee/Milestone・native関係・Project値を変更前後に照合します。closed/固定履歴は改名せず、稼働中のwriter/review bindingへの影響を先に調整します。タイトル変更だけで内容・受入・所有権を変更しません。

priority は `priority:P0` / `priority:P1` / `priority:P2`、status は `status:ready` / `status:in-progress` / `status:review` / `status:blocked` / `status:deferred` / `status:done`。closed は done、open は done 以外。優先度を題名に重複させません。PR policy が型・番号・ラベルを検査します。

## PR タイトル

PR の題名は **`type: 日本語の要約 (#N)`** に統一します。例: `fix: 再接続後のログ欠落区間を正す (#227)`。接頭辞は [Conventional Commits 1.0.0](https://www.conventionalcommits.org/en/v1.0.0/) の型を採用します。これは commit の慣行であり、世界共通の唯一の PR 題名・Issue 配置規格ではありません。日本語、型の小文字、scope を付けない形式、所有 Issue の末尾配置は本プロジェクトの選択です。

| type | 選ぶ変更の例 |
| --- | --- |
| `feat` / `fix` | 機能追加 / 不具合修正 |
| `docs` / `refactor` / `perf` | 文書のみ / 挙動を変えない整理 / 性能改善 |
| `test` / `build` / `ci` | テスト / build・依存 / CI |
| `chore` / `revert` | その他の運用保守 / 変更の取消し |

対応 Issue の題名分類をコピーせず、PR の実際の変更を表す型を選びます。要約は何が変わるか分かる日本語とし、Kotlin・SDK・Issue などの技術名は保持します。`(#N)` は branch と本文の単一 `Issue: #N` に一致させ、他の関連 Issue は本文や native 関係に記録します。優先度・Issue 番号を接頭辞へ重ねず、scope は付けません。実際の破壊的変更だけ `feat!: 保存形式を更新する (#N)` のように `!` を保持し、互換性への影響と理由を本文に記録します。release 自動化は追加しません。

題名の `fix:` は [Issue 関連/自動 close の keyword](https://docs.github.com/en/issues/tracking-your-work-with-issues/using-issues/linking-a-pull-request-to-an-issue) と別です。`Fixes #N` などを題名へ入れず、Development と close の時期は次節に従います。Issue 自体の題名、commit 履歴、過去の closed/merged PR はこの規約で一括変更しません。

作成・ready・修正再開・固定レビュー・merge 直前に実題名を読み戻します。共有 PR policy は型・空でない要約・日本語文字の有無・単一番号と branch/本文の一致を検査します。文字の有無は自然な日本語や意味の正しさの証明ではなく、要約・型・破壊的変更の妥当性はレビューで確認します。題名変更は同じ HEAD でも固定 review binding を失効させ、最新 metadata の policy と独立評価を照合します。coordinator はタイトルを reviewer へ渡し、merge 直前の既存 readback/binding と実 squash/merge 文へ反映します。GitHub metadata と merge を外部編集に対して原子的には固定できません。

## branch 命名

この改訂を採用した後に新しく作る専用branchは **`codex/<Issue番号>-<type>-<短い英語の目的>`** を既定とします。例: `codex/265-chore-branch-lifecycle`、`codex/12-feat-replay-integration`、`codex/227-fix-log-reconnect`。これは本repositoryの規約で、世界共通のGit標準ではありません。

`type`は上のPRタイトルと同じ語彙・実際の変更目的から選び、所有する実Issue番号は1つにします。suffixは小文字ASCII英数字、単語間は単一ハイフンとし、英語の目的を短く具体的に書きます。日本語・空白・scope・`!`・末尾/連続ハイフンは入れません。破壊的変更はPRの`!`と本文で説明します。必要な別agent prefix（`claude` / `cursor` / `agent`）でも同じsuffixを使います。

作成前に既存ref/所有者との衝突と`git check-ref-format --branch`を確認し、開始・PR作成・固定レビュー時にbranch、PR type/目的、本文の`Issue: #N`の一致を読み戻します。変更の主目的が途中で変わった場合はownerが理由をIssue/PRに残してレビューし、公開済みrefを命名のためだけに改名しません。既存/open/歴史上のrefsは一括rename・履歴改変・force pushせず、そのまま再開/読取します。共有parser/hookは旧`<Issue番号>-<slug>`も引き続き扱い、新形式の意味と適用時期はこの開始/レビュー手順で確認します。将来用の日付registryや追加daemonは作りません。

## commit 命名

この改訂の採用後に作成する作業commitの題名も **`type: 日本語の要約 (#N)`** とします。例: `chore: 未マージ終了PRの保全と命名を統一する (#265)`。PRと同じ小文字typeの語彙・scopeなし・単一の所有Issue末尾・実breakingのみ`!`を使い、必要なら空行の後に理由/検査をbodyへ書きます。関連Issueや優先度を題名へ重ねません。

各commitのtypeはその実変更を表します。文書だけのcommitが`docs`でPR全体が`chore`など、異なる粒度の理由がある場合は妥当性をレビューします。branch/PRと所有Issue・目的の対応を保ち、公開済みbranchをtypeのために改名しません。commit前と固定レビューで今回新しく作成した題名/bodyを読み戻します。既存PR title parserの語彙と形式を基準にし、履歴を一括lintして過去の成果を拒否する新gateは設けません。

既存coordinatorの自動fix commitは、所有Issueとの一致を検査した意味のある現PR題名を使い、staging前にlive PRの題名・HEAD/base/bodyを再取得して元bindingとの一致と既存title parserを確認し、push前も題名の不変を読み戻します。変更された場合は未公開の作業を保持して停止します。

統合用squash/mergeの題名・本文は既存のPRタイトル/readbackとcoordinatorの固定payloadに従います。GitHubが付ける実PR番号suffix、base同期のためGitが生成するmerge題名は例外として保持します。生成mergeのbodyへ不要なclosing keywordを入れず、[Developmentのclose境界](#development-の実関連)を守ります。通常の作業commitを生成mergeと称して規約を外さず、旧commit・公開済み履歴・固定受入成果はamend/rebase/rewrite/force pushで変更しません。

## PR sidebar の実情報

最初の Draft PR 作成時、ready・修正再開・統合/close 時に、本文と別に PR の native sidebar を確認します。対応 Issue の実分類と計画を使い、[公式 CLI の PR 編集](https://cli.github.com/manual/gh_pr_edit)や[公式 Projects API](https://docs.github.com/en/issues/planning-and-tracking-with-projects/automating-your-project/using-the-api-to-manage-projects)で所有範囲だけ補い、実 PR node と関連 Issue を読み戻します。

| 項目 | 設定と確認 |
| --- | --- |
| Assignees | 実在し、依頼範囲の責任を持つ GitHub account を設定する。単独 owner の本 repository では `shinma06` の assign-to-self を使える。内部の sole writer/PM と別に記録し、架空の agent account を作らない |
| Labels | 対応 Issue の `type` / `priority` を各1つ、PR 自身の現在の `status` を1つ設定する。実装中は in-progress、レビュー中は review、必要条件の障害待ちは blocked、延期は deferred。Issue が未完でも merge/closed の PR は done とし、未統合 close はその理由と Issue の残条件を保持する。Issue の status を機械的にコピーしない |
| Projects / Milestone | 対応 Issue の実 Project/Milestone を使う（Project #5、Milestoneは各Issueの実計画を確認）。PR 自体を Project に追加し、PR の Status は下の Project Status 対応へ同期する。Issue 項目を代用しない。別計画なら実在する対象と理由を記録する |
| Reviewers | 実際の review/request と reviewer を読む。既存 Codex service の review を保持し、必要な current-HEAD review は既存の依頼/ready 手順を使う。空欄を埋めるため無関係な人への依頼や自己承認をしない。bot の COMMENTED/Completed、GitHub の request、別 session の独立評価は区別する |
| Development | 次節の関連時期・両端 readback・default branch の close 境界に従う |

空欄・未反映は放置せず、真に対象なし/機能非対応/権限不足なら根拠と担当・次操作を既存 Issue/PR に残します。既存の他担当/分類/Project を不用意に置き換えず、変更前後の PR ID・各項目・Project Status と Issue の状態/分類/Project を確認し、副作用があれば担当が修正して読み戻します。sidebar を埋めても独立承認・受入・統合・Issue 完了を示しません。追加 validator・台帳・監視は不要です。

## Development の実関連

Issue の Development は対応する PR/branch を示します。本文の `Issue: #N` / `Refs #N`、timeline の mention、Project・Milestone・parent/dependency は別の情報で、Development の関連付け成功を意味しません。[GitHub公式の関連付け仕様](https://docs.github.com/en/issues/tracking-your-work-with-issues/using-issues/linking-a-pull-request-to-an-issue)を前提に、claim 時に実関連と未反映を確認し、最初の PR 作成時・base 変更時・終了時に次を適用します。

| 実際の PR target / Issue 状態 | 関連付けの時期と close 境界 |
| --- | --- |
| non-default の `develop` / OPEN | Draft PR 作成後に手動関連を作り、両端を読み戻す。本文の `Refs #N` と closing keyword 禁止は維持する。develop merge 後も受入・QA/release 引継ぎと readback が済むまで Issue を手動 close しない |
| default の `main` / OPEN・残条件あり | closing reference と closing keyword は付けず、本文参照と未反映理由・担当・次の安全な時期を残す。merge 後の受入・必要な引継ぎ・feedback readback を完了し、Issue を手動 close してから、統合済み PR の手動関連を作る |
| 統合済み PR / 完了済み CLOSED | 指定された所有範囲だけ手動関連を補い、元の CLOSED と close 時刻を保持したか読む。広い過去履歴へ遡及しない |

手動関連も closing reference です。default branch への linked PR merge は Issue を自動 close し得るため、「閉じない PR リンク」として使いません。自動 close は scope 完了の証明ではありません。本プロジェクトの finish-work は merge 後・close 前の feedback readback を必須とし、coordinator もその時点の受入本文/feedback binding と実 Issue 状態を照合します。main の終了時は完了可否にかかわらず、entry と終了直前の実 Issue 状態を照合し、未完の外部 close では cleanup/done に進みません。完了済みの所有 close 要求と status:done が一致する復旧は保持します。この後確認が残る main Issue の関連を保留する理由は、この既存完了条件を守るためです。merge 前のレビュー/CI 成功だけでは解除しません。base を default branch へ変更する前に残受入/引継ぎを照合し、未完なら手動リンクを解除し、本文・commit の closing keyword も確認して早期 close を防ぎます。誤って閉じた場合は元の未完条件を保持して再開し、Project/status を読み戻します。default branch・保護・repository の自動 close 設定はこの運用のために変更しません。

書込み権限と現行機能を確認した公式 API または Development sidebar を使います。公開 GraphQL の [addCloseIssueReferences](https://docs.github.com/en/graphql/reference/issues#addcloseissuereferences) は Issue の `issueId` と対象 `pullRequestIds`（最大10件）による手動関連です。API では PR の `closingIssuesReferences` と Issue の `closedByPullRequestsReferences` を両方向で読み、終了済み PR は `includeClosedPrs: true` で照合します。Development 表示を確認する場合は [GUI lease](operations.md#gui-lease) に従います。関連付け後は Issue の OPEN/CLOSED・status ラベルと対応する Project Status も読み戻し、不整合は所有担当が対応表に従って修正・再取得します。関連追加だけで完了状態を変更しません。API/画面の実結果、Issue/Project 状態、target/default branch、未確認を既存 Issue/PR へ短く記録し、別台帳・常駐監視は追加しません。未対応・認証/権限要求・取得失敗は担当と次操作を残し、本文 URL だけで実関連済みにしません。

coordinator は保護付き merge の直前に実 default branch と closing references を取得し、default 向けで OPEN Issue の closing link がある場合や取得不能/部分結果なら merge を拒否します。両方向の接続で `hasNextPage` を確認し、部分結果は所有担当へ戻します。 develop 向けは同一 repository の所有 Issue/PR が両方向に存在することも merge 前に必須とし、片方向・未関連・別 repository の同番号なら owner が手動関連と readback を終えるまで保留します。完了後のペアは repository と番号で照合し、別 repository の同番号を既存関連として扱いません。所有担当が手動/branch 由来の関連を解除、keyword 由来なら本文/commit の該当文言を直し、現状態を再取得して通常の復旧手順へ戻します。coordinator は他 Issue の関連や本文を自動で書き換えません。default 向けの新規 merge/squash commit は PR title と本文を明示して固定し、その実メッセージの closing keyword が参照する Issue/PR の状態を確認します。番号形式と GitHub の完全 URL 形式を repository/番号へ揃えて同じ検査へ通します。promotion の merge は元 commit も統合するため、全 PR commit メッセージも検査します。OPEN・状態不明なら拒否し、CLOSED・nondefault の正当な関連は保持します。squash で破棄する元 commit は新 commit に混入させません。[公式 REST merge API](https://docs.github.com/en/rest/pulls/pulls#merge-a-pull-request) の `commit_title` / `commit_message` を使用し、PR commit 取得の250件上限などで完全な列挙を確認できない promotion は owner が対応するまで保留します。main の merge 後に所有する完了確認・手動 close が済んだ場合だけ自身の PR/Issue を関連付け、両端と CLOSED/status:done/close 時刻を照合します。早期 close や関連失敗は完了・cleanup にせず owner が照合します。既存関連が両端にあれば復旧時に追加を書き込み直さず、Project は PM が上の終了確認で同期します。

branch 関連は既存の専用 branch を適切に結べる場合だけ使い、表示のために余分な branch を作りません。[Issue から作る branch の自動 PR 関連](https://docs.github.com/en/issues/tracking-your-work-with-issues/using-issues/creating-a-branch-for-an-issue)は preview 機能なので、実際の target と PR 作成後の関連を読み戻します。main 向けでは branch からの自動関連にも上の早期 close 対策を適用します。

## Project の表示

Project 名は **Android Replay Buffer — 開発マップ** を使用します。同名・関連付け済み Project を先に確認して再利用し、参照元の Project #2 をこのアプリへ流用しません。設定案は [.github/project.json](../.github/project.json)。実 ID は GitHub で取得して記録します。

### Project項目の更新入口

Project #5のIssue/PR追加・Status/Relationship Status更新・所有範囲の削除は、PM・writerともに [project_item.py](../scripts/workflow/project_item.py) を使います。番号とrepositoryを含む実URLから毎回取得し、Projectのowner/番号/関連repository、contentのrepository/種別/番号/ID、既存itemのProject/contentを変更前に照合します。opaque IDを引数に取りません。取得エラー・未知・不一致・部分応答は書込み前に拒否し、返却itemと変更後の実状態を照合します。既存登録を再利用し、指定したfieldだけを更新します。

```bash
python3 scripts/workflow/project_item.py add https://github.com/shinma06/android-replay-buffer/issues/262
python3 scripts/workflow/project_item.py set https://github.com/shinma06/android-replay-buffer/issues/262 --field Status --value 'In Progress'
python3 scripts/workflow/project_item.py set https://github.com/shinma06/android-replay-buffer/issues/262 --field 'Relationship Status' --value Standalone
# PRは /pull/N。removeは必要性と所有範囲を確認した対象だけ。
```

Standaloneの判定・Issue状態とのStatus対応・操作の所有範囲は従来どおり担当が確認します。アーカイブ状態も照合し、アーカイブ済み項目へのaddは復帰成功とせず拒否します。ownerが復帰の必要性・対象・方法を整合するまで自動unarchiveしません。未知/外部itemは報告してownerと整合し、自動削除しません。拒否や結果不明を別の `gh project`、直接GraphQL、connector、GUIのitem操作で回避せず、実状態を取得して原因を解消します。接続に100件を超える関連Project/field/repositoryがある場合も部分取得として拒否し、必要になった時にページ取得を対応させます。Project概要・View・repository link/defaultの管理はこのitem入口の対象外です。

[GitHubのrepository link/default](https://docs.github.com/en/issues/planning-and-tracking-with-projects/managing-your-project/adding-your-project-to-a-repository)は一覧表示と新Issueの作成先を設定します。[任意organizationの項目追加](https://docs.github.com/en/issues/planning-and-tracking-with-projects/managing-items-in-your-project/adding-items-to-your-project)を禁止するallowlistではありません。上の入口内では不一致を構造的に拒否しますが、直接CLI/API/connector/UIの書込み権限を持つ利用者までは遮断できません。[Projectアクセス権](https://docs.github.com/en/issues/planning-and-tracking-with-projects/managing-your-project/managing-access-to-your-projects)もread/write/admin単位です。任意経路を完全に禁止するにはplatformのadmission機能または書込み権限を独立した検証サービスだけに渡す構成が必要で、現在の管理入口だけを全経路の強制制限と説明しません。認証変更・常駐サービス・自動削除jobは追加しません。

| View | filter | 役割 |
|---|---|---|
| Now — 進行中 | `is:open label:status:in-progress,status:review -label:type:tracking` | 現在の担当と PR |
| Next — 着手候補 | `is:open label:status:ready -label:type:tracking` | 次の候補 |
| Later — 依存待ち・保留 | `is:open label:status:blocked,status:deferred` | 再開条件 |
| Past — 完了履歴 | `is:closed` | 経緯・完了日 |
| 全体 — 親子と全Issue | filter なし | 分解・横断 QA |

Status は `ready/blocked/deferred → Todo`、`in-progress/review → In Progress`、`closed/done → Done`。Priority は Labels で表示し、同じ情報を別の手入力フィールドにしません。Milestone・Parent・Linked pull requests は標準フィールド、`Relationship Status` のみ 2 値の追加フィールドです。概要に過去・現在・次の節目・依存順を短く記載します。

試験も status に応じて Now / Next / Later に表示し、QA 専用 View は設けません。`type:qa`、[試験手順](verification/human-qa.md)、元 Case 契約、人間・REAL の未達は保持します。

全体の試験状況は既存の横断QA #27 を正本とし、開始・停止・結果確定で担当・固定候補・実施済みと未達・停止理由・次操作を更新します。各QAは自分のCase、証拠と未達、担当・再開条件を持ち、全体の数値や計画JSONを複写しません。履歴の候補・結果・担当は上書きせず、現在と区別します。現在の作業状態に合わせて status ラベルを更新し、上の対応で Project Status を反映して読み戻します。停止だけで done にせず、再開可能なら ready、障害待ちは blocked、延期は deferred とし、完了は元の受入・引継ぎ条件で判断します。公開は要約のみとし、媒体・生ログ・秘密情報・ローカル絶対パスを載せません。未反映は Issue に対象・担当・再試行条件を残します。新しい自動化は追加しません。

## 中断・終了

Issue に owner、HEAD/base/target、PR、Development の実関連/未反映、dirty の有無、検証、残条件、次の操作、claim の継続/解放を残します。時間経過や応答なしで担当を奪いません。worktree の絶対パス、host、認証値は private registry だけに保存します。

develop統合後、coordinatorは元Issueの実装受入を照合します。独立した残試験がある場合だけQAを作成/再利用し、全Caseの固定出典へリンクします。QAは実際のsub-issueとしてMilestoneを継承し、双方向リンクを読み戻します。CaseなしはQAを新設せず、固定PR/merge/契約と次操作を既存release追跡 #10・元Issueへ双方向記録します。いずれもreadback後に元実装Issueをcloseします。main反映だけの残件はrelease追跡で管理し、試験完了とは分離します。[完了条件](verification/README.md#試験コストと完了の判断)に従い、QA・親・Milestoneを子PRのmergeだけで完了にしません。

PM は元 Issue・QA の Project 登録と Status、Milestone、native 関係、両端の Relationship Status に加え、Development の PR 関連と Issue 状態を読み戻します。main の関連保留は上の時期に解消し、未反映を完了済みと扱いません。Project の更新は coordinator が自動実行したとみなさず、未反映は担当・次操作を記録します。主要な管理変更、Milestone 完了、10 件の実 merge を目安に `python3 scripts/workflow/governance_audit.py` で監査契機を確認し、必要な範囲だけ見直します。

チームの解体・再編は[連絡規約](team-communication.md#必要時の編成と解体)に従います。本人の明示解体と停止を確認したら、PMが未完scopeの保管責任、後任未配置、固定成果と再開条件を既存の親/関係Issueへ記録します。過去の担当名は履歴として保持し、archiveを実装完了や所有権の自然失効にしません。後任を割り当てる時に実状態を読み戻し、単一writerとQAの双方向引継ぎを維持します。
