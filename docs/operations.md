# GUI lease・private引継ぎ・復旧

## GUI lease

Computer Useの操作前に[操作対象と別ウィンドウのナレッジ](computer-use.md)を読む。操作結果が元画面に見えない場合は、別ウィンドウ等への反映と対応する対象切替を確認してから、操作不能や人手必須と判断する。

`scripts/workflow/gui_lease.py`は、同一project内および別project間のComputer Use競合を防ぐ、ホスト/OSユーザーごとの協調予約です。OSの入力自体は遮断しません。承認済み作業では共有予約とGitHub上の担当・待機状態を確認し、他の担当が使っていなければ、本人への枠単位の追加確認なしで取得します。他projectが動いていない場合も、同一project内の競合防止のため共有予約は使います。

競合時の利用順・停止確認・引継ぎは、担当GPTセッション同士が既存の許可された連絡経路で調整します。必要なら進行役が調整し、通常の枠調整を本人への許可依頼に置き換えません。実際の人間操作を認めた場合は競合する操作を止めますが、空き枠の取得ごとに「今使ってよいか」を尋ねる手順は設けません。

予約は排他の仕組みであり、安全性や作業範囲の代用ではありません。[継続判断](workflow.md#公開操作の承認範囲と失敗の扱い)に従い、担当が対象・保存先・保全/復元・回数/時間上限を整えて録画・保存等を進めます。既存の割当と計画内なら、枠ごとの本人承認やPMのGOを取り直しません。競合や計画の影響変更は担当間で調整し、本人にしか解消できない境界だけを戻します。空き予約を未依頼の操作や権限拡張の根拠にはしません。

```bash
python3 scripts/workflow/gui_lease.py status
python3 scripts/workflow/gui_lease.py acquire --owner gui-session --issue 12 --run run-12-a --head FULL_40_CHARACTER_SHA --minutes 45
python3 scripts/workflow/gui_lease.py renew --token PRIVATE_LEASE_TOKEN --minutes 15
python3 scripts/workflow/gui_lease.py release --token PRIVATE_LEASE_TOKEN
```

SHAとtokenは実値へ置き換えます。返却にはtoken・host等が含まれるため、チャット/GitHubにそのまま貼らず、private記録に保存します。1回の期限は1〜45分。renewは試験全体の予算を延長する許可ではありません。

既定保存先は `/tmp/cursor-in-android-studio-gui-<uid>` でCursorプラグインと共有します。namespace名は既存の予約との互換性のため維持します。すべてのproject/clone/worktreeでこの同一予約を使います。`--state-dir`は隔離テスト専用です。projectごとの保存先で同じ画面を並行操作しません。既存ハーネスが別のleaseを使うホストでは、この新leaseだけで操作を始めず、全操作者と旧lease解放・移行先を調整してください。

最初の操作とinstall/起動/再起動/送信/復元の直前にtoken/期限を確認します。期限切れでも別ownerのacquireは拒否します。元ownerまたは調整担当が旧処理停止・画面状態・未保存データを確認し、現在tokenでreleaseしてから次担当へ渡します。PIDや時間だけで自動削除しません。

画面で対象アプリ・fixture・buildを識別し、source SHAと実ロードartifactのhashを照合します。古い画面要素・座標を使い回さず最新状態から操作します。終了時は自分の処理だけ停止/引継ぎ、Case別結果と次操作を残してreleaseします。

## private handoff registry

`scripts/workflow/handoff_registry.py`は公開可能なrecordと、実パス/hostのprivate対応表を結ぶ小さなPython APIです。coordinatorそのものではありません。

- `register(storage, public, source, host)` はopaque IDとversionをpublicに追加し、digestとsource/hostをprivate側へ保存。
- `resolve(storage, public, host)` はID、所有者、0600、host、digestを照合し、不一致なら拒否。
- storageはgit common-dir配下など、同じrepositoryの本人専用の保管先。storage/registry directoryは本人所有の0700を要求し、symlinkや権限不整合を変更せず拒否する。公開ファイルへ置かない。
- publicにはowner/Issue/PR/HEAD/base/target/scope/停止宣言などを含め、パスや秘密情報は入れない。
- `source`は実在するowned worktreeの絶対パスを呼出側で確認する。registryだけではwriter停止・Git clean・remote SHAは保証しない。

public recordの変更は古い登録のdigestと一致しなくなります。registry喪失・別host・改変・権限不整合時に勝手に再生成やowner差替えをせず、元ownerと復旧します。バックアップはprivateに保ち、実行中の登録を別projectへ移しません。

## Git hooks

bootstrapは当該repositoryのcore.hooksPathだけを設定し、既存custom hooksを検出すると停止します。共通git-dirを持つworktreeにも影響するので導入を共有します。guardはmain/master/developへの直接push/削除、Issue番号のないbranch、別branch/別HEAD/dirtyのpush、非fast-forwardを拒否します。branch名が正しくてもIssueの実在やclaimを確認するものではありません。

pre-pushは一時テストrepositoryへのGit環境変数の混入を防いでからchange_impact.py経由で必要な確認を実行します。ローカルhooksは事故防止で、サーバー保護の代用ではありません。ローカル設定を無効にして通す運用を作りません。
