# CIの実行範囲と効率の確認

[CI workflow](../../.github/workflows/ci.yml)は、PRの候補、統合後のmain/develop、明示した手動全量を検証します。Issue branchのpushだけではremote CIを起動しません。最初の意味あるpushでDraft PRを作り、push前のclean HEADは既存hookで検証します。branch単独HEADをremoteで確認する必要がある場合は手動全量を使います。PRではGitHubが作るmerge候補を検証するため、branch HEADの成功と同一視しません。

```bash
gh workflow run ci.yml --repo shinma06/android-replay-buffer --ref codex/281-ci-efficiency
```

`--ref`は実際の所有branchへ置き換えます。手動実行はclassifierの全量指定であり、必要なJDK/Gradle、CLI、Plugin検査も実行します。通常のPR全差分、未知path、履歴取得不能、削除/改名等は[既存classifier](../../scripts/workflow/change_impact.py)で判定します。新しいpath filter、結果cache、skip-success、再試行は追加しません。

## 保持する境界

- 必須checkは `test` / `CLI Python 3.9` / `PR policy` / `Acceptance gate` / `Agent review`。CI成功は独立レビューやGUI受入の代わりになりません。
- 取消しは同じPR番号又は同じrefのCIだけに適用。新規commit後は最新HEADの結果を読み、旧成功・取消し・未完了を採用しません。
- PRの実行はread-only token、checkoutのcredential保存なし。trusted acceptance/policyと製品検査の信頼境界を保持します。
- 製品buildが必要なら既存commandで生成し、`plugin/build/distributions/*.zip` を `plugin-${{ github.sha }}` として保存。保存元run/event/head、実checkout SHAと内容を照合し、別headのZIPへ名前を付け替えません。CDは同一の検証済みartifactを使い、PRで生成したZIPを公開へ自動昇格しません。

## 測定と再評価

最適化の前に、入力source/base、workflow/checkerの版、tool/runner、変更種別、cache条件、採否と打切り条件をIssueへ記録します。参考は[#281の原手順書](https://github.com/shinma06/android-replay-buffer/issues/281#issuecomment-6228550752)。全48観点の評価、採用検証、独立review、CI受入を区別します。

`check.py`は標準[unittestのdurations](https://docs.python.org/3/library/unittest.html#command-line-options)で遅い10 testsを表示します。test ID、全件数、失敗/skipと終了コードはそのまま確認します。個別test時間はqueueやsetupを含まないため、CI全体の短縮として報告しません。短いHTTP故障fixtureはshutdown確認のpollだけを0.01秒とし、request timeout・実socketの故障条件・停止assertは保持します。

Actionsのrun/job/step時刻は既存APIで取得できます。

```bash
gh api repos/shinma06/android-replay-buffer/actions/runs/RUN_ID
gh api --paginate 'repos/shinma06/android-replay-buffer/actions/runs/RUN_ID/jobs?filter=all&per_page=100'
```

run ID、attempt、head、event、job名、結論、時刻を一緒に保持します。attemptを混ぜて合計せず、取消し/失敗も除外しません。job開始から完了までの和は総job実行時間、要求から必要jobの最終完了までは必須検査の待ちです。runの`updated_at`は後処理/更新を含み得るので必須判定時刻の代用にしません。実行可能時刻が取れない場合、作成からjob開始までを純粋なqueue時間と断定しません。小標本のp95は参考値、実課金情報のない秒数は請求額に換算しません。

COLD/WARM/CHANGE/FAILURE/BURST/DEGRADEDを分け、新規runnerとcacheなしを混同しません。stackやrunnerが変わったら別baselineにします。cacheを増やす前に取得/展開/保存とクリーン実行を照合し、同一入力の再現性・鮮度・信頼境界と全量復帰を確認します。試行用の定期jobや常駐監視は作りません。新しいCI/stack変更時と実遅延/失敗の発生時に、そのIssueのownerが必要な範囲を再評価します。

## 切り戻し

必要eventの欠落、不正な成功、別対象の取消し、識別不一致、説明できない遅延/費用増加があれば、その施策を停止します。既知のworkflow又はfixtureの変更を所有branch上の通常のrevertで戻し、正常入力とそのcheckが検知する失敗入力を隔離して確認します。失敗fixtureを通常の公開artifactへ混ぜません。復元版にも既存hook/CI/独立レビューを適用し、protected branchへ直接pushしません。

triggerだけを戻す場合はpush対象へ旧Issue branch pattern (`codex/**`, `claude/**`, `cursor/**`, `agent/**`)を戻せます。正常/失敗の検査結果と最新PR候補の成功を確認してから採否を更新します。テストの診断表示は外しても検証を省略しません。標準unittestの空suite拒否と必須commandの非zero終了は保持します。
