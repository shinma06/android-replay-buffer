# Windows独自描画menu・独立prototype（製品ではない）

Windows専用subtree。製品source・macOS/Linuxは変更しない。全commandはdummyで、保存・取得・設定・IDE表示へのroutingはない。
既存PR459 `606a14789afa79dcc6bb0335c9cff51a3d225ad7` の `MenuBarModel.kt` をread-only参照。
複数projectのtitle→separator→project submenu、および各project内のstatus→save→screenshot→結果→toggle→IDE表示→設定の順を使う。
日本語／ASCIIのproject名、disabled screenshot、checked toggle、前回保存結果のdummy状態を明示する。
実製品の全状態生成・非同期routing・保存結果の同等性はこのsurrogateでは受け入れない。

## 必要条件・build / launch

Windows、既存Python 3、指定Studioの同梱javac/java。追加依存・downloadなし。
固定Studio `AI-262.10968.63.2622.16730706`、同梱JBR `25.0.4+-16339643-b508.27` と4入力hashをscriptが照合する。
`STUDIO`、`JBR`、`RUN`は利用者が指定する絶対pathで、sourceには個人pathを埋めない。
RUNは新規private領域を用い、Windows所有者限定ACLは実行者が先に確保する。

```powershell
python .\prototype.py test --studio "$STUDIO" --jbr "$JBR" --run "$TEST_RUN"
python .\prototype.py build --studio "$STUDIO" --jbr "$JBR" --run "$RUN"
python .\prototype.py launch --studio "$STUDIO" --jbr "$JBR" --run "$RUN"
python .\control_server.py --state "$RUN\state.json" --port 8767
```

TEST_RUNも新規private領域を指定する。testは同じ固定SDK/JBRで実row生成とdummy toggleをheadless実行し、
両projectのON/OFF往復・再生成時のroot／submenu／checkboxの一致を検査する。
binding unitはroot／子menuそれぞれのRight／Left／Up／Down／Enter／Esc／Spaceのcallback配送を検査する。
通常のSwing key lookupと、固定SDKが矢印を先に処理してJList標準ActionMap名を直接呼ぶ経路を分けて検査する。
後者が独自InputMapを迂回するため、同じcomponent内の標準action名も同じmenu Actionへ結ぶ。
Window生成・reflection・追加test依存はなく、unitは実windowの開閉やIMEを検証するものではない。
これはGUIのfocus／描画／実高DPIや製品同等性の合格を代用しない。

初回Studioのtrust等を通常UIで処理し、Tools→「Windows独自menu試作を開始」。
Chromeで `http://127.0.0.1:8767/` を開き最大化。IDEを背面に保ち、実trayのWをダブルクリック。
rootは `new JDialog((Window)null, MODELESS)`。submenuだけはその実menuをownerとする。
不透明・内容サイズ・通常のfocus APIだけを用い、補助Frame/隠しowner/IDE owner/強制IDE activationはない。
ArrowUp/Downでenabled行を移動、Enter/Right/clickでsubmenu、Enter/clickでdummy callback、
Escでmenu全体終了、Leftでsubmenu終了。外部Windowへのfocus lossでも全実menuをdisposeする。
既存own TrayIcon限定InvocationEvent source橋渡し以外のglobal listener/hookなし。

control pageはDOMのキー漏れ数、dummy callback、状態を表示するだけで、フォーム／送信／外部リンクはない。
実DPIは通常Windows表示設定とOS/monitor実測で別途確認する。JBR倍率proxyは高DPIの代用にしない。
最初の重要な不一致で停止し、新backendやfocus retryに進まない。

## 終了

Studio内ToolWindowの「試作終了」で自menu／TrayIcon／公開SDK listener lifetimeだけを除去。
IDE本体へのhide/dispose/toFront/requestFocusは行わない。その後Studioを通常終了し、control serverをCtrl+Cで終了する。
RUN内profile／source／記録は保全。raw診断・録画を公開Gitへ追加しない。
親へ渡す機能的sourceはこのsubtreeだけで、profile、PID、rawログ、録画、個人path、秘密を含めない。
