# GUI・ブラウザー・リモート環境

GUIを操作する作業だけに適用します。通常GUIと今後のGUI試験は、agentを問わず`agent-desktop`を観察・入力の第一選択にします。コード編集や通常のCLI検証にデスクトップ権限は不要です。製品・CIの必須依存には追加しません。

Plugin検証版の通常導入は[固定ZIPのターミナル配置](../operations.md#固定plugin-zipのターミナル導入)に従います。配置/通常終了・起動も共有leaseと保全・復元を必要とし、Settings → Pluginsの導入/版探索をGUI準備の必須操作にしません。自製品SettingsとAbout/actionの必要な実UI確認は継続します。

## セットアップ

1. [公式配布元vercel-labs/agent-desktop](https://github.com/vercel-labs/agent-desktop)から、そのhostのOS/architectureに対応するCLIを導入します。旧`lahfir/agent-desktop` URLは同repoへ転送されます。macOS/LinuxのHomebrew、Node環境の公式npm package、または公式release archiveを選びます。既存の共有導入を無断で置換せず、archiveでは同releaseの実行ファイルとplatform helperを一緒に配置し、checksum/attestationを確認します。古いcustom tap/bucketや別repoのhelperを追加しません。
2. OSの画面収録・アクセシビリティ等は、既存権限と実際の最小操作で利用可否を確認します。更新後もまず同じ確認を行い、動作する権限を取り直しません。OSが本人による新しい同意操作を要求する場合だけ、必要な対象と操作を伝えます。権限DBやtrust記録を直接変更して回避しません。
3. ブラウザーは連携先の対応browser/profileを選び、対象tabだけを指定します。個人profile/cookieをtemplateへ移しません。
4. 指定担当は[資源影響と共通lease](../operations.md#gui-lease)を確認します。対象/結果先を独立所有する非競合のbackground操作は並行可能とし、共有状態・証拠へ影響する区間だけ空きleaseを追加の本人確認なしで取得します。activeな全体排他/範囲不明の予約はownerと範囲を合意するまで尊重します。実競合は現ownerの安全な通常復帰後、準備済みARBを次に優先します。使い捨てfixtureで必要な画面取得・対象表示・最小操作を確認し、対象アプリ、project、build SHA/hashを記録します。
5. `agent-desktop`で対象の観察・入力・結果確認を行い、[操作正本](../computer-use.md)に従って別ウィンドウ等への反映を確認します。主経路の具体的制約時は、用途に合うPeekabooまたは利用agent純正Computer Useを同じ資源範囲・必要なlease・保全・上限内で使います。fallback間の固定順位は設けず、切替先で同じ対象を新たに識別し、副作用の範囲も確認します。未試行・機能未対応・接続失敗・対象検証失敗・dispatch後未確認・実成功・実拒否を区別し、結果不明の入力を再送しません。認証要求・実拒否は該当境界で停止します。残る障害と再開条件を記録し、CLIの正常終了だけをGUI passにしません。

### 導入・診断の入口

次は公式CLI v1.1.0で構文を確認した例です。Homebrewの[公式formula](https://formulae.brew.sh/formula/agent-desktop)がstable 1.1.0を提供することも確認しています。catalogや導入版が進んだ場合は、実際の`version`・対応OS・helpを記録して照合します。Homebrewとnpmを重ねて導入する必要はありません。

macOS/LinuxのHomebrew環境では次を使います。

```bash
brew install agent-desktop
```

Node 18以上の環境でnpmを選ぶ場合の固定版例は`npm install -g agent-desktop@1.1.0`です。導入後は共通の入口を確認します。

```bash
command -v agent-desktop
agent-desktop version
agent-desktop --help
agent-desktop skills get desktop
agent-desktop skills get platform
agent-desktop snapshot --help
agent-desktop click --help
```

PowerShellではcommandの解決に`Get-Command agent-desktop`を使います。GUIクライアントから起動するshellのPATHも確認し、別hostの絶対パスをコピーしません。インストールだけでは利用可能・受入済みとはしません。CLI内のguideはAPI/機能を確認する資料で、本人の既存許可・clientの実権限・本projectのleaseやCase契約を増減しません。MCP登録やAIプロバイダー/APIキー、全agent設定へのskill一括導入は通常利用に不要です。

導入・接続・観察は元の許可範囲で行い、上記の資源影響判定で共有状態に影響する部分へleaseを適用します。`agent-desktop status`でplatform・permissions・`supported_surfaces`を確認し、`agent-desktop permissions`で不足を切り分けます。macOSではAXにAccessibility、画像取得にScreen Recording等が必要です。本人の同意が実際に不足するときだけ`agent-desktop permissions --guide`等の正式手順で必要操作を伝え、動いている権限を取り直しません。Linuxではuserのgraphical session/AT-SPIへの接続も必要です。未対応・PATH・接続失敗を実拒否と混同せず、OS別の`skills get platform`と各commandのhelpへ進みます。

スナップショットのJSON、画像、clipboard、debug/traceには私的情報が含まれ得ます。対象を限定しGit外へ保管し、画像が必要な保証ではモデルが実画像を確認します。部分treeや入力dispatchを成功にせず、使い捨てfixtureで新しい観測→最小入力→結果の再観察と復元を行い、利用版・実経路・制約・実成功/未達を既存Issue/QAへ残します。

CLI例の固定確認元は[v1.1.0 / d3dc190の導入手順](https://github.com/vercel-labs/agent-desktop/blob/d3dc190afbf34cd2377252398ddd5c30efbd093a/docs/install.md)、[標準ループ](https://github.com/vercel-labs/agent-desktop/blob/d3dc190afbf34cd2377252398ddd5c30efbd093a/skills/agent-desktop/SKILL.md)、[観察command](https://github.com/vercel-labs/agent-desktop/blob/d3dc190afbf34cd2377252398ddd5c30efbd093a/skills/agent-desktop/references/commands-observation.md)です。公式mainと導入版の機能が同じとは仮定しません。host導入・smokeの実結果は#396と既存QAで別に記録します。

純正Computer Use経路そのものを明示要求する受入条件はagent-desktop・他の補助入力・人間操作で代替せず、実際の実行経路と未達を記録します。通常GUIへ一律に純正限定を追加せず、人間に引き継ぐ場合は許された確認経路を明示します。fallbackとして純正/browser連携を導入する場合も正式手順を使い、Codex Desktop内部のNode REPL・helper・socket・module path等のsource configを複製しません。ブラウザーとIDEを別worktreeから操作してもフォーカスは共有されます。

## リモートhost

元の会話環境にはローカルと別OSホストへの接続機能が提供されていましたが、遠隔機の設定・認証・実プロセスは今回調査していません。接続先一覧にあるだけでは操作可能としません。

導入先の公式remote connection手順で登録し、接続するユーザー・root・branch・worktree・runtime・対象画面を再照合します。ローカルの権限・PATH・MCP・trust・registryを遠隔機へ暗黙継承しません。同じホストの共有資源へ影響する複数クライアントは1つのlease運用で調整し、非競合の対象限定操作は上記の条件で並行可能とします。この環境のARB優先を別ホストへ暗黙継承しません。host名や実パスは非公開の運用記録に残します。

## テスト環境

使い捨てfixtureにはテスト用データだけを用意し、本番の個人会話や実tokenを採取しません。実modelへ送信する試験と、fake ACP/MCP serverを使う合成試験を分けます。合成fixtureは通信処理の確認であり、providerの実能力やGUI観察の代替にはなりません。
