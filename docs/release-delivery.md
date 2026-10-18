# GitHubへのZIP配布

配布先はこのrepositoryのGitHub Releasesです。Marketplace、別サービス、定期jobは使いません。計画上の第1版・第2版とは別に、正式な配布版は `major.minor.patch` で採番します。第2版の正式候補は `0.2.0` を選び、**受入前のbuild時に確定**します。公開のためにZIP内のversionを書き換えたり再buildしたりしません。

| 経路 | 起点 | 公開物・判定 |
|---|---|---|
| early | developへのpushのCI成功 | 現在developから生成省略を確認できた履歴だけ遡った最新製品ZIP、単一の`early` prerelease。対象履歴のmerged PRの共通4 checksも確認 |
| 正式候補 | main上の`Release candidate`を固定develop SHA/versionで手動実行 | check/buildPlugin/verifyPluginStructureを実行したZIPを`candidate-<ZIP SHA-256>` Artifactへ保存。ここではReleaseを公開しない |
| 正式 | main promotion mergeのCI成功 | 既存promotion validatorで受入と固定hashを再確認し、保存済み候補の同じbytesを`v<version>`へ公開 |
| 復旧 | main上の`Release delivery`へpromotion PR番号を指定 | 同じ入力を再照合。新しいversion・別ZIP・tag移動による修復はしない |

`test`、`PR policy`、`Acceptance gate`、`Agent review`を成功条件に含めます。Python原型を配布するbranchでは実3.9検査を必須`test`内で実行し、[段階移行](setup/github.md#runtime検査の共通testへの段階移行)中の旧jobも実検査を保持します。publisherはcoordinatorの`CI_CHECKS`を共有し、元CI全体の成功・成果物同一性の確認も維持します。元のCase結果、未実施、失敗、人間/REAL延期は変更しません。初期Agent段階のpromotionを正式公開する場合にも、段階受入であることとQA #65の残条件を本文に示します。正式という表示は未実施の完全GUI受入を意味しません。第2版の必要受入は第2版の固定計画で確認します。

## 固定候補から正式公開まで

1. 必要な製品変更をdevelopへ統合し、cleanなsource SHAと正式versionを固定する。buildの `-PpluginVersion` とidentityのversion入力対応は #206で導入する。未対応sourceは候補検査で拒否される。
2. `Release candidate`を**main上で**実行する。sourceはdevelopに含まれる40桁SHA、versionは`0.2.0`のような確定値。build権限はreadのみで、公開権限を渡さない。通常の開発版と正式版は別のbuild入力なので同一成果物扱いにしない。
3. 作られたArtifactのZIPをそのまま保存し、SHA-256・source・内部version・dirty=falseを照合する。Artifactの保持は90日であり、原本の長期保管を代替しない。受入担当は必要な固定ZIPをprivateに保持する。
4. このZIPのまま必要な受入・独立評価を行い、既存`docs/verification/promotion.json`へcandidateと`artifacts.plugin`のhashを記録する。実環境・保証が異なる過去のPASSを転記しない。
5. 通常のpromotion PRをmerge commitでmainへ統合する。配布処理はPRの実merge親、mainへの包含、全必要Caseと共通4 checksを既存validatorで再確認する。正式tagはmainのpromotion merge、ZIPのsourceは受入済みcandidateへ対応付ける。
6. 保存されたArtifactをtransport digestとZIP hashで照合し、再圧縮せず公開する。公開後にassetを再downloadし、実bytes・本文・tagを読み戻す。不確定な応答や取得失敗を完了扱いにしない。

CIが完了しても、対象commitがpromotionでなければ正式Releaseを増やしません。別候補、未知のworkflow、欠落/期限切れArtifact、失敗・中断・未完check、内部source/versionの不一致は公開を止めます。Artifact digestの警告だけに頼らず、publisherが不一致を明示的に拒否します。

## earlyの更新と途中失敗

1つのReleaseページを維持し、tagをforce更新しません。`early` tagは最初の保管場所を指す固定識別子です。**製品sourceはRelease本文・ZIP内部のSHA**であり、GitHub自動生成のSource code ZIPはインストール用ではありません。

公開処理は全channelを同じconcurrency groupで直列化し、`queue: max` と `cancel-in-progress: false` を使います。実行中A・待機中の正式配布Bへearly Cが後着しても、BをCで置き換えず両方を待機させます。待機の上限は100 runで、それを超える追加runは取消しになります。待機を開始した順の処理で、event発生順そのものは保証しません。取消し/失敗した正式配布は対象promotion PR番号の手動復旧、earlyは後続の正常CI event又は元runの再実行で現在HEADを再解決します。上限なし/取消しなしを保証せず、残件は#271 ownerがrunと配布先を照合します。[GitHub concurrency](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/control-workflow-concurrency)遅れて届いたrunも現在のdevelop HEADを固定して解決し、旧sourceへ巻き戻しません。現在HEADの最新CIが未完了/失敗なら公開しません。開始時・履歴照合中・本文切替直前に固定したdevelop HEADを再確認します。

文書・管理だけの変更では既存classifierがPlugin buildを省くため、CIの成功jobでArtifact upload stepが明示的にskippedだったcommitだけ、developの第一親を遡ります。各commitの最新push CIとmerged PRの共通4 checksを確認し、最初に見つかった製品ZIPを同じbytesで公開します。製品commit Aの配布より先に文書commit BがHEADになっても、Bまたは遅着したAのrunからAのZIPを回収できます。BのCIがAより先に完了してAが未完了なら失敗を保ち、Aの完了eventで現在HEADを再解決します。本文は製品sourceと追従確認したdevelop HEADを分けます。CI履歴不明、未完了/失敗、生成されるはずのZIPの欠落、期限切れは、それより古い製品へ飛ばさず失敗にします。CI側のstep名/構成変更ではこの接点も更新・検証します。

新しいhash名のZIPを先にupload・再download検証し、本文を切り替えて読み戻した後だけ、旧本文で識別済みの旧assetを削除します。途中失敗時は旧本文と旧ZIPを残し、既存の同じbytesを確認して再開します。公開前の失敗ならdraftのまま残します。tag未作成の既存draftは保存済みの40桁`target_commitish`を保持し、earlyではmain履歴への包含、正式では元promotion mergeとの一致を確認します。現在mainが進んでも新しいtargetへ付け替えず、保存済みtargetで公開後を照合します。準備中のdraft target変更も公開前に拒否します。不明なassetや所有markerのないReleaseは削除/上書きしません。未知の中断で残った追加assetは担当が現状態と所有を確認し、後片付けを別に記録します。

正式版の既存tag/内容は不変です。同じ入力の再実行は再照合だけを行い、異なるbytesや不完全な公開済み正式版を自動置換しません。Release説明の編集も現在のpublisherの所有recordと一致しなければ止まるため、変更前に担当が方針を決めます。

古い正式版を初めて公開・復旧する場合、公開済み正式版のversionを数値で比較し、より新しい版があればlatestを変更しません。既存の正式tagのversionが判定できない場合もlatestを維持します。draft/prereleaseは比較から除きます。APIの`make_latest: true`は指定Releaseをlatestへ設定するため、復旧時に無条件で使いません。[GitHub Releases API](https://docs.github.com/en/rest/releases/releases#update-a-release)

publisherのGitHub CLI呼出しはworkflowの`GH_TOKEN`を保持します。agent worker用の認証情報を除去する共通実行関数とは分離し、workerの制限は変更しません。read/list/check/uploadを同じ認証経路で実行し、tokenを引数や公開記録へ出しません。[GitHub CLIの認証環境変数](https://cli.github.com/manual/gh_help_environment)

## 復旧と停止

- まず該当run/Release/assetの実状態を読み、失敗した段階を特定する。結果不明のupload/PATCHを盲目的に繰り返さない。
- 同じ入力なら`Release delivery`の手動実行で再照合する。ただし次のworkflow権限境界では同じjobの再実行を復旧策にしない。新しいcandidateは元失敗を残し、別の固定source/入力/受入として扱う。
- Artifact期限切れの場合は、保持してある**同一ZIP**をcleanなtrusted mainから公開できる。秘密情報や生診断を公開しない。

```bash
python3 scripts/workflow/release_delivery.py stable \
  --archive "$ACCEPTED_ZIP" --promotion-pr "$PROMOTION_PR" --version 0.2.0
```

ここで変数は実在する原本とmerged PRへ設定します。CLI実行も同じReleaseのworkflowと重ねず、実runを終了確認してから担当1名で行います。CLIにはGitHub Actionsのconcurrency lockは自動適用されません。受入を省く引数、force更新、保護無効化はありません。

### 自動公開のworkflow権限境界

固定targetと現在のdefault branchで`.github/workflows/`が異なるRelease作成/更新には、Contents writeに加えてWorkflows writeが必要です。`GITHUB_TOKEN`には後者を付与できません。[GitHub Release API](https://docs.github.com/en/rest/releases/releases#create-a-release)

自動publisherはRelease作成前と本文/公開状態の切替前に現在default branchを取得し、固定targetとのworkflow差分を確認します。差分・取得不能は失敗として残し、targetとZIPを維持します。これは事前検査であり、検査直後のdefault更新やその他のAPI権限拒否も成功へ読み替えません。既存assetとdraftは保全し、同じGITHUB_TOKENの再実行では不足権限が増えないことをエラーへ明記します。既に公開・一致しているReleaseの読み戻しは新しい公開操作とは分けます。

担当は#271の配布owner（GitHub assignee shinma06）です。復旧時は次の手順を使います。

1. 独立review・固定候補の受入・公開対象と権限範囲を確認し、Release deliveryの実runが終了していることを読み戻す。同時実行を止め、担当1名にする。
2. cleanな採用済みmainで `gh auth status --active` を読み、既存の許可されたローカル認証が対象repositoryへpush可能で、classic/OAuthなら`repo`と`workflow`、fine-grained/AppならContents writeとWorkflows writeを持つことを確認する。認証確認は公開の成功証明ではない。tokenを取り出したり新しいsecretを作ったりしない。
3. 正式版は上の`stable --archive ... --promotion-pr ... --version ...`を実行する。Artifactが有効なら`--archive`を省略できる。earlyの権限復旧は、同じtrusted mainから `python3 scripts/workflow/release_delivery.py early --run "$PRODUCER_RUN" --source "$PRODUCER_SOURCE"` で正常CIのrun/sourceを指定し、現在developと有効な同一成果物を通常gateで再照合する。
4. publisherの実tag・本文・ZIP再download照合が成功した後だけ#271へ完了を記録する。復旧が失敗したらdraft/tag/assetを保全する。許可された既存認証がなければこの経路は未完了とし、ownerが具体的な権限境界を扱う。別SHAへのtag付替え・権限拡大・保護の迂回で通さない。

このローカル経路はGitHub Actionsのlockを共有しません。既存認証のscopeとownerを読み戻してから使い、実公開の前提を満たさない段階では実行しません。

運用を止める場合はGitHubのworkflow無効化で新しい実行を止め、進行中の副作用を確認します。ZIPやtagを削除して失敗を隠しません。元の手動ZIP保管・受入は残り、配布workflowの停止で製品データを変更しません。復帰後は固定候補と正常/拒否経路を再確認してから次の公開を行います。

実試行・切戻し・公開結果と残条件は [#271](https://github.com/shinma06/android-replay-buffer/issues/271) に記録します。この文書やunit testの成功だけでは、実CI・公開の成功を証明しません。

仕様根拠は[GitHub Releases API](https://docs.github.com/en/rest/releases/releases)、[Artifact API](https://docs.github.com/en/rest/actions/artifacts)、[workflow_runの信頼境界](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#workflow_run)、[concurrency](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/control-workflow-concurrency)です。参考実装の固定版と採否はIssueに記録し、他製品の受入・version・公開結果は継承しません。
