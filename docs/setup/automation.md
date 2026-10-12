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

base 更新は通常 merge → 必要テスト → 非 force push → 独立再レビュー。HEAD/base/target/Issue/本文/feedback が変わると旧承認を失効させます。5 checks、会話解決、strict base、force/削除禁止、bypass なしの実サーバー設定を確認できなければ merge しません。CI を待つ間の Agent review は独立レビューの記録であり、GUI 受入を作り出しません。受入NG修正では共通validatorの`repair_revision`と独立報告の`repair_assessment`を照合し、全元失敗保証の再試験と実質評価が揃うまで成功status/mergeへ進みません。[修正gate](../verification/README.md#受入ng修正のdevelop統合226)を手動統合にも適用します。

## 復旧

```bash
python3 scripts/workflow/agent_loop.py resume --pr 123 --reason '停止原因を解消しworker停止と所有を確認'
python3 scripts/workflow/agent_loop.py rebind-target --pr 123 --writer-stopped
python3 scripts/workflow/agent_loop.py cleanup-branches
# 候補の所有・clean・停止・remote ref を確認した場合だけ --apply を付ける
```

resume は停止原因と元 writer/worker の停止を確認してから使います。target 変更は rebind-target で明示し、旧承認を再利用しません。source が消失した場合は旧所有の確認と private 移転記録が必要です。registry 喪失、別 host、外部 push、dirty を reset/stash で隠しません。

develop の実装完了は、Caseがある場合にQAを作成/再利用し、固定契約・merge SHA・Milestone・native sub-issue・双方向リンクのreadback後にcloseします。CaseなしではQAを作らず、既存release追跡 #10 と元Issueへ固定出典を双方向記録してreadbackします。既存QAがある場合やrelease追跡が終了済みの場合は、PMが移管先・残条件を照合するまでcloseしません。Project 表示の同期は PM が [終了確認](../work-management.md) で行います。cleanup は自分の停止済み clean な資源だけが対象で、merge 成功と cleanup 完了は分けます。

## 定期実行

定期実行は明示依頼がある場合に利用環境の正規スケジューラーへ登録します。[coordinator 依頼文](../../prompts/coordinator.md)を使い、trusted directory、対象、worker 許可、時間・回数予算、停止・復旧条件を設定します。変更なしは通知せず、進展・完了・障害・利用者の操作が必要な場合だけ通知します。既存 PAUSED ジョブを再開しません。今回、cron・LaunchAgent・heartbeat は作成していません。

担当間の結果は[チーム連絡](../team-communication.md)の形式でPMへ集約し、人間向けの必要な結果だけを窓口へ渡します。

## 後着指摘後の終了処理再開

merge後にfeedbackが変わるとclosure/cleanupを止めて資源を保持する。指摘を修正先へ移管した根拠や非該当の根拠を既存Issue/PRで固定し、最新terminalと全feedbackを別の許可された独立reviewerへ渡す。通常resumeや対応コメントだけを承認としない。新しい独立報告をprivate JSON `{"binding": <feedback snapshotの全binding>, "review": <validate_review準拠＋reviewerが確認したfeedback_hashを含む実独立報告>}` に保存し、trusted mainで `python3 scripts/workflow/agent_loop.py reconcile-feedback --pr N --review <private-json>` を実行する。古いreviewを新wrapperへ移すだけでは再開できず、report自身もreviewed feedback_hashの一致を要する。approved/scope_completeと全binding一致・remote完了・停止/所有を検査した後、次のtickがclosure直前に再取得する。再取得後の変化は再停止する。HEAD/base/target/受入本文が変わった場合は旧GUI/acceptanceを捨て、未達のIssueを閉じない。元writerが再開/消失又はworkerが残っていればこの操作では回復できず、既存の所有/移転確認へ戻る。旧承認は履歴で保持し、この操作で新reviewerを起動したり未修正を自動承認したりしない。
