# CIの実行範囲と効率の確認

[CI workflow](../../.github/workflows/ci.yml)は、PRの候補、統合後のmain/develop、明示した手動全量を検証します。Issue branchのpushだけではremote CIを起動しません。最初の意味あるpushでDraft PRを作り、push前のclean HEADは既存hookで検証します。branch単独HEADをremoteで確認する必要がある場合は手動全量を使います。PRではGitHubが作るmerge候補を検証するため、branch HEADの成功と同一視しません。

```bash
gh workflow run ci.yml --repo shinma06/android-replay-buffer --ref codex/281-ci-efficiency
```

`--ref`は実際の所有branchへ置き換えます。手動実行はclassifierの全量指定であり、必要なJDK/Gradle、CLI、Plugin検査も実行します。通常のPR全差分、未知path、履歴取得不能、削除/改名等は[既存classifier](../../scripts/workflow/change_impact.py)で判定します。新しいpath filter、結果cache、skip-success、再試行は追加しません。

## 保持する境界

- 共通の必須checkは `test` / `PR policy` / `Acceptance gate` / `Agent review`。移行中の実server保護には旧`CLI Python 3.9`も残り、[branchごとの置換確認](github.md#runtime検査の共通testへの段階移行)が済むまで外しません。CI成功は独立レビューやGUI受入の代わりになりません。
- 取消しは同じPR番号又は同じrefのCIだけに適用。新規commit後は最新HEADの結果を読み、旧成功・取消し・未完了を採用しません。
- PRの実行はread-only token、checkoutのcredential保存なし。trusted acceptance/policyと製品検査の信頼境界を保持します。
- 製品buildが必要なら既存commandで生成し、`plugin/build/distributions/*.zip` を `plugin-${{ github.sha }}` として保存。保存元run/event/head、実checkout SHAと内容を照合し、別headのZIPへ名前を付け替えません。CDは同一の検証済みartifactを使い、PRで生成したZIPを公開へ自動昇格しません。

Python原型を配布するbranchでは、`test`の先頭でPython 3.9を選び、`product_check.py`と`test_product_check.py`の正常・非対応構文拒否・復旧を実行してから管理Pythonへ戻します。旧jobも同じ実検査を続けるため、移行中はsetupとCLI検査が重複します。#305 ownerが固定base/候補のjob・step時刻と追加コストを記録し、速度改善とは扱いません。イベント、runner、cache、既存classifier・Plugin build・artifact条件は保持します。

旧job/重複stepは、#103 ownerがそのbranchで実CLI内包・互換/明示移行・回復と原型退役を確認した時に除去します。mainがPython原型を配布し続ける間の必要な3.9検査は残します。保護の移行だけを原型退役とみなさず、未実行の検査を旧名のsuccessで代用しません。

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

## 変更時の運用

CIのworkflow・起動/選択・runner/cache、CIで使う依存/toolchain・build/test command・fixture・成果物処理を変えるときに適用します。実行量に影響する製品変更やテスト追加も含みます。作業ownerが計画から結果まで担当し、PMは担当・共有資源を調整します。[start-work](../../.agents/skills/start-work/SKILL.md)から開始し、[finish-work](../../.agents/skills/finish-work/SKILL.md)と[PR template](../../.github/pull_request_template.md)で同じ記録を確認します。

| 時点・担当 | 既存Issue/PRに残す判断と実行 |
|---|---|
| 計画・writer | 影響する起動回数、待ち/準備/検査/保存のコストと守る検証義務を特定し、上の測定方法から必要な指標・条件を選ぶ。実行への影響なし、固定入力/条件が一致する既存証拠の再利用、有限の比較が必要、のどれかを理由付きで記録する。比較前に基準/候補・品質/採否・回数/時間/費用・停止/復元条件を決める。 |
| 実装・writer | 必須検査と対象ID・入力/成果物の同一性・失敗伝播を保つ最小変更を行い、必要な通常検査と計画した比較を実行する。結果・失敗/取消し・測定しない理由・未達を原記録へ追記する。測定中に限度を延ばしたり、成功まで再試行したりしない。 |
| 固定レビュー・独立reviewer | 変更範囲に対する測定/再利用/省略の理由、品質の同等性、性能主張と実条件、劣化時の担当/復旧を確認する。運用自体の変更は文書や欄の存在だけで成立とせず、代表する実変更/レビュー又は有限の可逆試行の結果を読む。未測定・別条件・未解決を採用済みにしない。 |
| 統合・以後の作業owner | 固定版のCIと残条件を読み戻す。新しいCI入力/環境の変更又は実際の遅延/失敗を契機に、関係する判断だけを更新する。統合前は当該Issue/PR、統合後の退行は元変更へ結ぶ既存の不具合管理に担当・調査/切戻し・再確認の条件を残す。 |

記録はPRの`CI impact`から既存Issueの根拠へ結びます。例えばHTTP fixtureの終了待ち変更なら、保証/対象IDを固定して局所時間・CPU・故障/復旧を比較します。CIの入力や処理を変えない文言修正なら、その差分を理由に追加の性能測定を省略できます。runner/cache/toolchainが変わった場合に、異なる条件の過去の短縮率を転用しません。

変更ごとの全48観点の実験、新台帳、定期job、追加の本人承認は求めません。必要な保証やテスト追加に伴う時間増加は理由と許容範囲を評価し、すべての変更が以前より速いことを条件にしません。既存の必須hook/CI/独立レビューは維持し、同じ入力/条件の有効な証拠を理由なく取り直しません。

## 切り戻し

必要eventの欠落、不正な成功、別対象の取消し、識別不一致、説明できない遅延/費用増加があれば、その施策を停止します。既知のworkflow又はfixtureの変更を所有branch上の通常のrevertで戻し、正常入力とそのcheckが検知する失敗入力を隔離して確認します。失敗fixtureを通常の公開artifactへ混ぜません。復元版にも既存hook/CI/独立レビューを適用し、protected branchへ直接pushしません。

triggerだけを戻す場合はpush対象へ旧Issue branch pattern (`codex/**`, `claude/**`, `cursor/**`, `agent/**`)を戻せます。正常/失敗の検査結果と最新PR候補の成功を確認してから採否を更新します。テストの診断表示は外しても検証を省略しません。標準unittestの空suite拒否と必須commandの非zero終了は保持します。
