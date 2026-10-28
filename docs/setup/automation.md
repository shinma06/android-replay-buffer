# 自動レビュー・修正・統合

`scripts/workflow/agent_loop.py` は、明示登録した同一 repository・maintainer の PR を一工程ずつ進めます。参照元の engine を移植し、既存Python CLI tests、共通 GUI lease、private registry に接続しています。スケジューラーへの登録と、実 PR の自動処理開始は別です。

## 起動条件

- [GitHub の有効化](github.md)を完了し、trusted main にこのハーネスを統合する。
- GitHub CLI の正規認証、Codex CLI の正規認証、管理/開発用Python 3.15以上、製品に必要なadb/scrcpy/ffmpeg を用意する。[runtime選択・旧環境の移行](README.md#runtimeと配置)を確認する。token・認証ファイルを複製しない。
- Issue の受入・ラベル・Project・Milestone・担当を確認し、専用 worktree の clean な HEAD と remote PR HEAD を一致させる。
- writer が編集・commit・push・GUI を停止し、scope と次の owner を確定する。
- worker 実行は実環境で許可される場合だけ使う。`codex exec` は coordinator が起動する子プロセスであり、名前を変えても独立 top-level セッションになるわけではない。子エージェント禁止の環境から `tick` を実行して迂回しない。許可された実行者へ packet を引き継ぐ。

## 登録と単発実行

以下は **trusted main checkout** で実行します。PR 側の coordinator を起動しません。値は実際の Issue/PR/worktree に置き換えます。

```bash
python3 scripts/workflow/agent_loop.py enroll \
  --pr 123 --source ../android-replay-buffer-issue-12 --owner implementation-session \
  --scope replay_buffer/ docs/verification/ --writer-stopped
python3 scripts/workflow/agent_loop.py scan
python3 scripts/workflow/agent_loop.py tick --pr 123
```

停止writerの登録とHEAD/checkout/base確認を済ませたDraftは、tickがReadyへ遷移させてからリモート完了を待つ。Ready前の同HEAD旧Completedは新しいrequest時点より古ければ採用しない。Ready時に追加review requestを重ねない。

`scan` は対象一覧を確認し、`tick` はレビュー、修正、再レビュー、CI 待ち、merge、終了処理のうち次の工程を実行します。結果を確認し、必要な次の tick を呼びます。PR を省略した tick は明示登録済みの最大 3 件を交代で処理します。`--parent` は実在する親がある場合だけ、`--close-issue` は main の全受入完了時だけ指定します。

公開 handoff は opaque ID・Issue/PR・HEAD/base/target・scope・停止宣言です。source/host は Git common-dir 配下の `agent-loop/registry` に 0600 で保存します。実行状態は移植元からコピーしません。

## レビュー元と実行元の選択

通常の義務は、期待する現候補の外部レビューが終わり、全feedbackを固定した別セッションの評価があることです。応答停滞時だけは[限定進行](#外部レビュー停滞時の限定進行)を使えます。既定のCodex summary経路を維持し、明示的に採用したGitHub native reviewは `agent_loop.py --github-reviewer <実login> feedback --pr N` で読めます。同じglobal optionをtick・再取得・reconcile-feedbackにも指定します。既定のCLI workerを変更するoptionではなく、worker実行の許可も作りません。

選択元は信頼されたローカル起動設定で、PR本文・報告・未着summaryから自動選択しません。native経路はGitHub APIから取得した指定reviewerの最後のreviewが、対象repo/PR・完全なHEADと一致し、要求境界以降の `APPROVED` または `CHANGES_REQUESTED` で、未処理のreview requestがない場合だけ完了とします。`COMMENTED`・`PENDING`・`DISMISSED`・不明/欠落値は完了にしません。完了したchanges requestも指摘解消と独立評価が必要です。[GitHub review API](https://docs.github.com/en/rest/pulls/reviews)、[review requests](https://docs.github.com/en/rest/pulls/review-requests)の実レコードを使い、追加の外部serviceは呼びません。

秒単位の `submitted_at` が小数秒付き要求境界と同じ秒に入る場合、現レコードだけでは前後関係を証明できずpendingを維持します。同じ値の再取得だけでは解消しません。境界以降と確認できる後続のterminal review、または要求から提出への順序を独立に立証できる根拠が必要です。時刻の丸めや別方式への自動切替で完了にせず、GitHubの精度・丸め規則や実serviceの停止をこの条件から推定しません。

方式・版・reviewer・要求境界・要求中のreviewers/teamsの内容hashと、全review本文を含むfeedback hashをbindingへ含めます。既存Codex依頼/summaryや保存済みの別方式の依頼は、選択を変えてもその方式の完了を確認します。旧bindingを新方式へ補完して承認を再利用せず、元receiptを保持して必要な再評価へ戻します。service側の設定変更や既存義務の廃止をこのoptionで代行しません。

実行元はCodex CLI envelope/`CODEX_THREAD_ID`に加え、信頼されたhostから `agent_worker.bind_native_review(report, bound, storage, issuer=..., observe_origin=..., acceptance=..., repair_evidence=...)` を呼ぶembedding入口を提供します。呼出し元は認証済みruntimeのexecution recordを取得できるhostです。`observe_origin(report, bound)` は、そのレビュー操作の実session、実operationと提出内容をhost記録から照合して、次の値を返します。reportからsessionやcallbackを選ぶこと、渡されたhashを単にechoすること、架空のreceipt JSONで置き換えることは禁止です。

| 値 | hostが取得・確認する事実 |
| --- | --- |
| `session_id` | 実操作を行ったsessionのopaque ID。model/role変更でも同じactorは同じID |
| `source`, `operation_id` | その実操作を再参照できる原記録のlocatorとoperation ID。非公開receiptに保存 |
| `report_sha256`, `binding_sha256` | 原操作に対応する提出内容と完全なbindingを `handoff_registry.digest` と同じJSON規則で照合したhash |

`issuer`はhostコードに固定するruntimeのidentity domainで、局所IDなら実instance scopeを含めます。実originを取得してから共通正規化し、報告内の異なるsession主張を拒否します。native IDの大小文字を保持し、UUIDは旧identityへ統一して別名の独立役を作りません。原observation、除去前の提出内容、完全なreport/bindingは既存private registryへ保持し、旧Codex fingerprint/receiptと入力ファイルは変更しません。

新しいnative origin（UUIDへ正規化されたnativeを含む）と、検証済みの任意native contributor IDを含む新しい組合せは、検証済み機械値・定型status・finding数・次action・opaque参照だけを公開します。adapter種別と実contributorsはsealed receiptから検証し、report内のflag、session文字列、issuerや診断キーの自己申告で公開方式を選びません。任意のfindings・evidence・assessment・拡張フィールドと全診断mapは非公開のままです。GitHubだけの閲覧者はnativeの詳細理由を読めなくなる実際の差を受容します。既存UUIDのみのCodex/manual公開本文と必要なpath/UUID保護は維持します。 phase由来のnextは検証・回復の確認点であり、操作許可や現在実行できる手順の保証ではありません。cleanupでは、まずmerge後の最新review request、feedback/bindingと非公開診断を確認し、providerが未完了なら完了を待ち、feedbackが変わったならfreshな独立評価を得ます。既存finish gateを通るまでIssue変更・resource削除は行わず、通過後もIssueの扱いと所有resourceのcleanupを別々に確認します。

保存は既存の `store_review` / `restore_review` を使い、`private-review-v1` の公開状態と完全な内部状態のhash・bindingを結びます。内部のfixer、feedback照合、受入、修復・certificate評価は復元した原報告を使い、findingsを持たない公開envelopeを承認報告として検証しません。GUI証拠・resume理由もprivateに保持し、review無効化後は検証済みnative出典を公開方針の根拠として残します。その根拠は承認の代用ではなく、公開側はfresh review待ちとします。state/progress、direct reconcile、resume/GUIとCLI出力に同じ方針を適用します。旧native branch cacheは原本・公開履歴・PAUSED・ownerを保持して明示的なowner回復へ戻し、自動移行・再封印・承認再利用はしません。既存mainの通常UUID cacheは維持します。

`private_sessions`・`private_identifiers`・`private_paths`はadapter所有の予約metadataです。提出reportの同名fieldはsession bindと下位origin発行時に除き、除去前の原形を別の非公開receipt項目へ保持します。source/operationはnative observation、contributorsは検証済みbundle、pathはprivate registry/evidence/handoffだけを根拠にし、report/stateやnested diagnostics自身の同名fieldは規則を指定できません。absoluteなsource表記だけでfilesystem出典にせず、sourceをprivate_pathsへ追加しません。sourceをpathへ混在させた旧receipt/cacheは、同じ文字列が保持された実証bundleのfileとして再hash・照合できる場合だけfilesystem出典を維持し、裏付けがなければowner回復待ちです。旧contributor receiptも元reportと保持bundle・原本を再hashしてcanonicalな出典を照合できる場合だけ利用します。保存済みclaimや再wrapperを新たな出典にしません。

新native経路は診断mapを公開へ投影しないため、異なる全キーと値を完全な原本・binding・hashで保持し、envelopeと衝突できないことを確認します。旧native投影の衝突エラー自体を維持するために任意本文parserは残しません。既存公開経路の必要なキー衝突拒否と文字列キー検証は維持します。原本再hash、全contributorsとの独立性、修復・certificateの原assessment、秘匿/保存条件、未検証の予約metadataの拒否は共通validatorで維持します。

`native-session-v1` の報告は通常のfeedback dispositionでも、予約privacy fieldの有無やverdictに関係なく、現report・完全なbindingに一致するnative adapterの実observation付きreceiptを共通validator・保存・復元で検証します。公開cacheやmanual/CLI wrapperはnative出典の代用にしません。UUIDへ正規化されたnative originも、そのnative receiptの検査を維持します。従来の通常UUID manual/CLI契約は変えず、提示されたreceiptや予約metadataは共通の出典検証へ通します。

この入口はhostを信頼する境界であり、ローカルhost自体の改竄への認証基盤ではありません。`issue_review_origin`は取得済み情報を封印する下位APIです。対応するhostの実source取得・認証・operationとの対応づけはhost側でレビューしてください。embeddingの回帰試験は別providerの実接続/実行成功を証明せず、Codexのworkerや現serviceを無効化するものでもありません。

## 状態と制限

```text
queued → reviewing → reviewed
                     ├ changes_requested → fixing → publishing → reviewing
                     ├ blocked / ci-failed → 明示的な復旧待ち
                     ├ acceptance-wait / ci-wait → 条件待ち
                     └ approved → merging → cleanup → done
```

reviewer は read-only、fixer は指定 scope の workspace-write。生成した出力名はsession identityではなく、CLI JSONLの実 `thread.started.thread_id` をprivate報告へ結びます。手動報告も[同じ実origin入口](../verification/README.md#受入ng修正のdevelop統合226)でbindし、全修復contributorsを共通validatorから除外します。修復PRはenrollの `--repair-evidence` で非公開bundleを既存registryへ渡します。workerには再hashした原本pathとorigin対応を限定的に渡し、欠落時は停止します。raw報告は0600 registryへ分離し、公開stateはfingerprintとbinding済みopaque参照だけを持ちます。実adapter発行のorigin_receiptも全報告/現bindingへ結び、手書きJSONの代用を拒否します。enrollは相対bundleを正規化・原本までpreflightしてから公開します。既存停止handoffへのbundle追加/差し替えは `rebind-target --pr <number> --writer-stopped --repair-evidence <private-bundle-json>` で行い、PAUSEDを維持して旧承認を失効させます。実private path集合は非公開registryで保持し、文中も公開から伏せます。旧wrapper approvalは失効してfresh reviewへ戻し、PAUSEDは維持します。GitHub token を環境から除き、個人 config の MCP/plugin 設定を読み込ませません。モデルは利用者の選択を維持します。worker は 10 分、fix は 3 回、review は 8 回、通信等の失敗は 3 回で停止します。終了時に worker の子プロセスも停止します。[Codex 非対話実行](https://learn.chatgpt.com/docs/non-interactive-mode)・[CLI 引数](https://learn.chatgpt.com/docs/developer-commands?surface=cli)に基づく起動です。

原本bundleとmappingを含む全receiptはcleanup範囲外へ置きます。receiptはowned regular fileで、group/other readなし、または実resolved ancestorのownedディレクトリにgroup/other traversalなしを必要とします。0700祖先下の0644ログは保持でき、privateなsymlink別名だけでは公開targetを保護しません。`--repair-evidence`省略時も保持済bundleを公開変更前にpreflightします。検証済bundle/全原本のcanonical pathと固定SHA-256を既存private registryへ累積し、rebind・cached review・premerge・postmerge・closure/cleanupで存在/所有/実private権限/同一内容を検査します。差し替えは新しいpathを使い、旧原本とidentityを保持します。

旧path-only履歴は現在のbytesから自動でhashを作り直しません。明示的な復旧には、既存trusted adapter発行の`origin_receipt`を含む元のprivate報告と完全なbindingを`{"review": <元報告>, "binding": <発行時binding>}`で保持したprivate JSONを、`rebind-target ... --retained-review <private-proof-json>`へ渡します。複数束なら同flagを繰り返せます。既存registryで全報告/bindingを照合し、その保護されたbundle hashと宣言済receipt hashだけから旧identityを復元します。原本の欠落/改変、未結合のpath、発行証拠不足は停止し、再封印・copy・chmod・registry全走査や承認の復元はしません。PAUSEDと所有を維持し、現在候補のfresh独立評価は別途必要です。

base 更新は通常 merge → 必要テスト → 非 force push → 独立再レビュー。HEAD/base/target/Issue/本文/feedback が変わると旧承認を失効させます。共通4 checks、会話解決、strict base、force/削除禁止、bypass なしの実サーバー設定を確認できなければ merge しません。[runtime移行中](github.md#runtime検査の共通testへの段階移行)の旧CLI contextも実保護に従い、test内の実runtime検査を旧名successで代用しません。CI を待つ間の Agent review は独立レビューの記録であり、GUI 受入を作り出しません。受入NG修正では共通validatorの`repair_revision`と独立報告の`repair_assessment`を照合し、全元失敗保証の再試験と実質評価が揃うまで成功status/mergeへ進みません。[修正gate](../verification/README.md#受入ng修正のdevelop統合226)を手動統合にも適用します。

### 外部レビュー停滞時の限定進行

必要な外部レビューの応答停滞がブロッカーと判断された場合、担当は理由を既存Issue/PRに記録し、同じ完全なHEADを明記して**一度だけ明示的に再依頼**します。Codexは通常の `@codex review`、選択したnative reviewerは `@login please review` のPRコメントに「再依頼」と理由を書き、実投稿のID・server `created_at`を読み戻します。既存の再依頼を重ねず、以前の待ち時間、本文中の時刻、edited時刻を起点にしません。

自動レビューのsummaryは同じコメントを上書きします。停滞を確認した時点・再依頼前に `agent_loop.py prepare-review-retry --pr N` をtrusted mainから実行し、現在の実API観測を既存private registryへ1件だけ保存します。返る `review_retry_original` はopaque IDと内容hashの参照で、元の同HEAD/base/選択元と観測時刻・rawコメントを結合します。再依頼や承認を作らず、保存しただけで元義務の適格性を認定しません。別ledgerやjobは作りません。

`agent_loop.py --review-retry-comment <実comment ID> --review-retry-original <保存した参照> feedback --pr N`で実レコードを確認し、同じoptionをtick・再取得・reconcile-feedbackへ使います。これは読取・判定の指定で、再依頼を投稿せず、workerの実行権限も作りません。coordinatorは採用したIDと元観測の参照を保持します。現在も元service記録を直接確認できる場合や先行manual依頼がある場合は、元観測の参照なしでも従来の確認ができます。最新の未編集コメントがmaintainerによる再依頼で、同じHEAD、先行するreview義務、選択元/保存済み義務が一致し、必要な応答がなお欠ける場合だけ `binding.review_retry` に実ID/server時刻を記録します。先行manual依頼はmaintainerの未編集の実コメントに完全な現HEADがあり、その依頼以後の現HEADのCompleted/receiptで充足済みではない場合だけ数えます。旧HEAD・対象不明・完了済みの依頼を元義務にしません。元観測を指定した場合も同じ充足確認を行い、現在のRunningで過去の完了を上書きしません。先行するreview義務は手動依頼コメントに限定せず、同じHEADの自動起動したRunning/Queued service記録でも確認します。その実コメントの作成と表示された開始時刻がともに再依頼より前であることが必要で、古いdashboardの作成時刻だけ、又は再依頼後に始まった記録だけを元レビューの証拠にしません。同じdashboardが再依頼後のRunning/Queuedへ更新された場合は、再依頼前に実観測した保存記録を読み戻し、内容hash・HEAD/base/PR/選択元と観測時刻を照合します。現在の失敗/未知形式/応答/指摘は先に確認し、古い記録で無視しません。元観測なしの同じ最終状態はpendingを維持します。不明な形式・実failure・外部の具体的な応答/指摘・別方式の未完了義務はこの例外で通しません。

**その新しい再依頼から30分**経っても必要な応答がなければ、外部の待機だけを解除できます。`remote_review_complete`はfalseのまま、`review_wait_satisfied`で待機条件を区別します。通常と同じ独立した別sessionが全feedback・固定HEAD/base/title/body/Issueと未解決指摘なしを評価し、approved/scope_complete・一致するfeedback_hashを持つ報告を、そのreviewer自身の既存adapterとprivate registryでorigin結合する必要があります。時間経過だけではAgent review成功・統合にはなりません。通常のCI・strict base・branch protection・Case/受入・所有・実権限は変えません。

非repairの手動評価はreviewer自身のsessionで `agent_worker.py --report <private-report> --binding <private-binding> --storage <既存registry> --output <新private-output>` を使います。元報告は保持し、実sessionと完全bindingへorigin_receiptを発行します。受入NG/certificateでは従来どおり元証拠・acceptanceも必要です。元観測の参照を使う場合は、その実registry記録（CLI workerには`review_original`で渡す）も読んで時系列と出典を評価します。報告を作ったsource writer、設計/証拠の寄与者と最終評価者の独立性は既存契約に従って確認します。

再依頼・HEAD/base/metadata/feedbackや選択元の変更は旧評価を失効させます。30分後も全feedbackを再取得し、応答が来ていれば通常経路で扱います。merge直前、merge後/close前、後着指摘の既存復旧経路も同じ共通判定とorigin評価を使い、provider Completedを偽りません。局所的な待機だけで独立して進められる全作業まで停止したとみなしません。

## 復旧

```bash
python3 scripts/workflow/agent_loop.py resume --pr 123 --reason '停止原因を解消しworker停止と所有を確認'
python3 scripts/workflow/agent_loop.py rebind-target --pr 123 --writer-stopped
python3 scripts/workflow/agent_loop.py cleanup-branches
# 候補の所有・clean・停止・remote ref を確認した場合だけ --apply を付ける
```

このbranch補助監査はmerged-onlyです。closed-unmerged/withdrawn/supersededは保持として報告し、claimを解放せずworkerを再起動しません。[branch lifecycle](../workflow.md#branch-lifecycle)に従ってowner/PMが保全・実復元確認・所有引継ぎ後に整理します。閉じたPRをmerged cleanupへ再登録せず、既存snapshot/bundle等を使い、新job/保管frameworkは追加しません。

resume は停止原因と元 writer/worker の停止を確認してから使います。target 変更は rebind-target で明示し、旧承認を再利用しません。source が消失した場合は旧所有の確認と private 移転記録が必要です。registry 喪失、別 host、外部 push、dirty を reset/stash で隠しません。

develop の実装完了は、Caseがある場合にQAを作成/再利用し、固定契約・merge SHA・Milestone・native sub-issue・双方向リンクのreadback後にcloseします。CaseなしではQAを作らず、既存release追跡 #10 と元Issueへ固定出典を双方向記録してreadbackします。既存QAがある場合やrelease追跡が終了済みの場合は、PMが移管先・残条件を照合するまでcloseしません。Project 表示の同期は PM が [終了確認](../work-management.md) で行います。cleanup は自分の停止済み clean な資源だけが対象で、merge 成功と cleanup 完了は分けます。

## 定期実行

定期実行は明示依頼がある場合に利用環境の正規スケジューラーへ登録します。[coordinator 依頼文](../../prompts/coordinator.md)を使い、trusted directory、対象、worker 許可、時間・回数予算、停止・復旧条件を設定します。変更なしは通知せず、進展・完了・障害・利用者の操作が必要な場合だけ通知します。既存 PAUSED ジョブを再開しません。今回、cron・LaunchAgent・heartbeat は作成していません。

担当間の結果は[チーム連絡](../team-communication.md)の形式でPMへ集約し、人間向けの必要な結果だけを窓口へ渡します。

## 後着指摘後の終了処理再開

merge後にfeedbackが変わるとclosure/cleanupを止めて資源を保持する。指摘を修正先へ移管した根拠や非該当の根拠を既存Issue/PRで固定し、最新terminal（又は上記の有効な停滞例外）と全feedbackを別の許可された独立reviewerへ渡す。通常resumeや対応コメントだけを承認としない。新しい独立報告をprivate JSON `{"binding": <feedback snapshotの全binding>, "review": <validate_review準拠＋reviewerが確認したfeedback_hashを含む実独立報告>}` に保存し、trusted mainで `python3 scripts/workflow/agent_loop.py reconcile-feedback --pr N --review <private-json>` を実行する。古いreviewを新wrapperへ移すだけでは再開できず、report自身もreviewed feedback_hashの一致を要する。approved/scope_completeと全binding一致・remote完了又は有効な停滞例外・停止/所有を検査した後、次のtickがclosure直前に再取得する。再取得後の変化は再停止する。HEAD/base/target/受入本文が変わった場合は旧GUI/acceptanceを捨て、未達のIssueを閉じない。元writerが再開/消失又はworkerが残っていればこの操作では回復できず、既存の所有/移転確認へ戻る。旧承認は履歴で保持し、この操作で新reviewerを起動したり未修正を自動承認したりしない。
