# 時間の定義・正誤表（T1）

<a id="scope"></a>
## 適用範囲と優先順位

本書は、承認済み原本を保持して、E1〜E9の9点だけを訂正する規範文書である。後続の子セッションは、原本に定めた着手前の文書に加えて本書も読む。該当する式・型・試験・保存例については、**本書を原本とtimelineの転載より優先する**。ほかの事項、別途提示されたオーナー判断、各Trailの順序と関門は変更しない。

| 固定原本 | SHA-256 |
| --- | --- |
| [方針書・改訂4](time-definition-policy.md) | `3e29f94cad5b55beec7adceafbfe3450266aa8312da5aaaa12c0c0bcb7268e20` |
| [実装仕様書・第2版](time-definition-impl-spec.md) | `b70707e8fcb8161f035a8ecd340552d36aeae62b624def77a2911c267ab9d7e7` |

以下の行番号はこのSHAの原本に対するもの。原本のバイトは変更しない。本T1で追加するのは本書と参照リンクだけで、本番コード、既存・新規ベースライン、schema 4の動作・出力・数え方、自動保存の受付は変更しない。以下の型・保存形・試験は後続Trailの実装契約であり、本T1の実装済み機能を意味しない。

時刻・区間長はすべて整数ns。例の「秒」は説明用の換算であり、JSONのnsは十進文字列とする。位置の誤差範囲は閉区間、coverageの長さを数える区間は半開区間。`null`の下限は負の無限大、`null`の上限は正の無限大を意味し、未知を0に置換しない。`0 ≤ ρ < 1`を前提に、ρによる誤差の拡大は外向きに切り上げる。

射影・包絡・除外の証明は整数または正確な有理数で計算し、下限を切り下げ、上限を切り上げる。中間の加減乗除が固定長整数に収まらないことを、wrapや狭い範囲への飽和で隠さない。多倍長で計算するか、その端を未知へ広げる。未知の端から有限な除外根拠は作らない。有限の代表点自体が表現不能なら、正常な時刻を捏造せず原本0.6の停止・報告対象とする。

<a id="e1-candidates"></a>
## E1 推定ログの時計区間の候補をすべて含める

対象: findings `01M7MA4YJ3PBCA9XNDPMBRCRRB`・`01M7MEK2XAF6N8KB6BX9DVBPCG`。実装仕様書4.4の317行の単一offset・単一範囲の規則、および本正誤表の旧式`e_j = readError_j + ceil(ρ × |受信H − received_j|)`を、以下で置き換える。関連箇所は同4.2（260〜263行）、4.3（267〜290行）、4.4（314〜324行）、方針書4.3の推定等級（205行）。

### 反例

epochログの元時刻がwall=90秒、測定済みの段差の前後で`D − wall`が10秒と20秒なら、Dの候補は100秒と110秒になる。段差をD=105秒で既に観測し、最新の有効な測定がH=108秒・D=108秒・wall=88秒、受信H=114秒なら、現行の5秒規則では不明となり、将来の等級は推定になる。受信区間のoffsetだけを選ぶと、読取誤差1msと500ppm×6秒で110秒±4msとなり、100秒を除いてしまう。両候補は受信より前なので、因果関係の上限だけでは一方に決められない。これは324行の「まだ測定されていない段差」の制限とは別である。

### 候補と原点

段差が無くても、受信の古さだけで誤差を広げてはならない。Hとepochが同じ速さで進む例で、`ρ=1/2000`、Dが500ppm速く、測定H=`100000000000`、D=`100050000000`、イベントH=epoch=`10000000000`、受信H=`106000000000`、読取誤差=`1000000`とする。旧式は`q=10050000000`と±`4000000`から`[10046000000,10054000000]`を作るが、真のD=`10005000000`を含まない。後で錨と交差したり、同じ誤った範囲の包絡を取ったりしても真値は戻らない。

保存要求Sで凍結した測定・錨を用い、**記録自身の等級がestimatedの場合だけ**次の候補列挙を行う。現行の`sequence`が非nullの記録の換算、等級、値、誤差をこの列挙で置き換えない。追加候補を理由にmeasuredへ昇格させたり、現行のmeasuredを降格させたりしない。終点の等級と最終行等級の悪い方を取る規則も変更しない（I6）。

候補は少なくとも次をひとまとまりで持つ。単位や原点の識別子は数値と分離しない。

```text
Candidate = (sourceDomain, bootSegment, AnchorId, segmentOffset,
             sourceSample, F_A, D_lo, D_hi, h_lo, h_hi)
AnchorId = (region, suspendIndex)                 // 原本4.3
sourceDomain = epoch または monotonic             // 系の生時刻を混ぜない
D = bootSegment固有のBOOTTIME                    // seqではない
seq = D + その候補のsegmentOffset
```

1. epoch時刻`v`について、同じ取得接続であること等からbootを特定できるならそのbootだけ、特定できないなら否定できないbootごとに列挙する。そのbootの**既知の全時計区間**の有効なサンプルを候補元とする。MONOTONIC系にも同じ原点管理を使う。受信時のregionと異なること、古いregionであること、現行の換算がstale・境界・曖昧さによりnullであることは、候補を捨てる理由にしない。
2. 各サンプルと、そのregionの各既知AnchorIdを候補元として残す（PC休止はregionを変えずAnchorIdを変える）。`q_j = v + elapsed_j − source_j`は名目のoffset換算値であり、真のDの上下限ではない。**ρはDとHの進み方の差の仮定であって、D−epoch、D−MONOTONIC、epoch−Hの誤差を同じρで抑える追加の仮定ではない。** この最小契約では、名目値±ρから候補を切らない。sourceから発生Hへの独立した包含保証が無い場合、発生Hの範囲は`(−∞, 受信H]`とする。sourceの数値だけから発生Hを得たことにせず、遅延の最大値も仮定しない。行の根拠となる測定・錨・元時刻は保持中の行とともに保持するか、同じ範囲を復元できる要約を残す。
3. **受信Hをイベント発生Hと同一視しない。** 区間Aの錨が発生H=`x`について返すelapsed系の範囲を`[L_A(x), U_A(x)] = [lo'_A(x), hi'_A(x)]`とする。原本4.3のサンプル選択（そのxで最小幅の1件、同幅なら新しい方）を保ち、サンプル同士の交差は取らない。floorとの合成・矛盾時の退避にはE4の安全な入力を用いる。候補の可能な組と、その射影を次で定める。

   ```text
   F_A = { (x, D) | x ≤ 受信H,
                     0 ≤ D,
                     L_A(x) ≤ D − x ≤ U_A(x) }
   [D_lo, D_hi] = hull(projection_D(F_A))
   [h_lo, h_hi] = hull(projection_H(F_A))
   ```

   `U_A(x)`が無い場合は右側の制約を置かず、elapsed系の`L_A(x)`も持たなければ左側の制約も置かない。生epoch系のfloorをelapsed系のθに代用しない。sourceの読取誤差や、同じ系について既に包含保証のある発生Hの範囲を使うときも、その保証された範囲だけをxへ課す。追加のsourceの速度上限を置いて狭めてはならない。橋渡しを保証できない候補は上の広い集合のまま残す。段差の発生Hや錨の有効期間を推測した時間制約も置かない（段差の最初の検出Hは発生Hそのものではない）。錨の連続性を確認できない期間を外挿するときはその側を未知とし、未検証の区間へ有限なθ範囲を延長しない。`F_A`が空と証明できた候補だけを除く。

   この定義は、元時計とDの関係の変化幅が分からないときには意図的に広い。読取誤差だけでは、その変化幅の上限にはならない。計算は絶対値の折点、サンプルの選択が変わる点、既知のxの端で分割した一次不等式の射影として行える。丸めたρ項を、有理数の式に加えて外側1nsへ広げて解く方法もよい（包含する方向にだけ近似する）。未知・非有界の場合は端をnullとし、探索打切りを候補の不可能性とみなさない。
4. 原本317行の因果上限`受信H + hi'(受信H)`を使う場合も**候補自身のelapsed系の錨**を使い、発生から受信までその錨の連続性を保証できる場合に限る。そこへ別候補のsegmentOffsetを混ぜない。最終的には3の`x ≤ 受信H`を満たす組を含む範囲を残す。補助候補の代表値を「既知のelapsed」とみなして最小形へ戻してはならない（自分で選んだoffsetを根拠に他の候補を消してしまうため）。

単一の同時刻測定`(H_j,D_j)`に±εの読取誤差がある例では、上の錨はイベント時刻xで`D_j + (x−H_j) ± (ε + ceil(ρ × |x−H_j|))`を含む。測定H自体に幅があれば原本4.3の`sent`・`received`による外側の上下限を用い、`received`を測定の正確な瞬間とみなさない。遅延反例の真の発生H=`10000000000`では、測定との距離は90秒で、幅は`1000000 + 45000000 = 46000000`。Dの断面は`[10004000000,10096000000]`で、真の`10005000000`を含む。Dが500ppm遅い符号では、測定D=`99950000000`、真のイベントD=`9995000000`、断面は`[9904000000,9996000000]`となり、これも含む。これらのxは反例の検査用真値であり、実装がepochの値をそのままHとみなして使うものではない。xが不明なら断面1つを選ばず、可能な全xの射影を残す。

この列挙は、既知の対応から否定できない候補をすべて包むためのもの。真の候補以外も残り得る。受信区間の1候補に狭めた確度を表示してはならない。候補集合が空になった場合は点や原点を捏造せず、原本0.6に従って停止・報告する。

### 各候補をtへ変換してから包絡する

先に異なるseqやθの数値をmin/maxしてはならない。候補ごとに原本4.2の同じ錨／別の錨の分岐を守る。

- **保存終点と同じAnchorId**: `F_A`のD射影にそのbootのsegmentOffsetを加え、`[d_lo, d_hi]`を作る。終点も同じseq原点であることを確認し、候補自身のelapsed系の錨から`J_A(S) = [S + L_A(S), S + U_A(S)] + segmentOffset`を求める。記録自身がestimatedの包含計算では、`E_A = hull(T_Sの原本の範囲 ∪ J_A(S))`を補助の終点範囲とし、`t_lo = W − E_A.hi + d_lo`、`t_hi = W − E_A.lo + d_hi`とする。終点の既存範囲だけを真値保証とみなさない。いずれかの上端が未知なら`E_A.hi = null`かつ`t_lo = null`、下端が未知なら`E_A.lo = null`かつ`t_hi = null`。錨をSまで連続に延長できない側も未知とする。同一の組から得たDとHの相関は、この包絡で失ってよい（範囲が広がるだけである）。これはestimated記録の包含の拡大だけであり、`T_S`の既存点・等級・`window_end_uncertainty_ns`や、記録自身がmeasuredの行の点・誤差（I6）は変更しない。
- **異なるAnchorId**（同bootの段差・休止を含む）: `F_A`のH射影`[h_lo, h_hi]`を用いて、`[t_lo, t_hi] = [W − S + h_lo, W − S + h_hi]`。ここには終点の別錨のθやsegmentOffsetを足さない。旧bootのseqを新bootの`T_S`から直接引かない。
- 同じAnchorIdでも、終点が別の生時刻系のままで共通のseq原点を保証できなければ、数値を直接引かず、その候補のH射影による上の変換を使う。AnchorIdの一致だけで時刻系の一致を代用しない。
- 真値の包含範囲`C_i`は、全候補のt範囲の**包絡**`[min(t_lo), max(t_hi)]`とする。どれかの下限／上限が無ければその端はnull。候補間の交差は取らない。包含範囲と名目上の配置は分け、各行の中点・有限端・0を個別に代表点にする旧規則は撤回する。点と最終的に宣言する範囲は次による。measured記録の点・誤差は原本どおりであり、この選び直しの対象外である。

反例でDとHの中心の差を0、S=114秒、W=30秒とすれば、候補の中心位置は16秒と26秒であり、最終範囲は両方を含む。異なるbootのsegmentOffsetが数千秒違っても、各候補を正しい錨でHへ戻してからtに置くので、その差を遅延や誤差幅として足さない。

終点についても外挿の幅が必要になる。段差・休止なし、`D(H)=MONOTONIC(H)=1.0005H`、ρ=500ppm、唯一のサンプルの送受信H=100秒、D=MONOTONIC=100.05秒、読取幅0（既存readError=1ms）とする。S=104秒、W=6秒では終点はmeasuredだが、原本のBOOTTIME上の点104.05秒・範囲`[104.049秒,104.051秒]`は真の終点104.052秒を含まない。同じbootの生時刻0の遅延行は現行換算範囲より古いためestimatedであり、E1のD射影下限は0。旧変換のC_i下限−98.051秒と名目点−98.050秒の包絡では、真の位置−98.052秒が1ms外れる。原本4.3の錨は読取区間からθ=0.05秒、4秒の外挿幅2msを得るので、J_AのBOOTTIME範囲は`[104.048秒,104.052秒]`であり、E_Aを使うとC_i下限は−98.052秒まで広がって真値を閉区間の端として含む。既存readErrorの1msを錨の幅へもう一度加算しない。両seq値へ同じsegmentOffsetを加えても差は同じ。この補助範囲はestimated行専用の包含計算であり、measured終点の既存フィールドやmeasured行の誤差を広げる変更ではない。

### 包含とは別に、固定原点から端末時刻の差で点を置く

有効サンプルがH=D=MONOTONIC=100秒の1件だけ、ρ=0、受信H=S=106秒、W=6秒とする。読取幅0・錨θ=0が連続するモデルで、PTS=103秒と104秒のframeはともにサンプルから5秒超後に受信したestimatedである。source→発生Hの独立した保証が無ければ、両行のD射影は`[0,106秒]`、`C_i=[−100秒,6秒]`となる。個別の中点はともに−47秒で、1秒の端末内間隔を失う。包含範囲を狭めてこれを直してはならない。

以下は**記録自身がestimatedの行の名目配置**だけに適用する。記録自身がmeasuredなら、終点との組で最終等級がarrivalとなっても、既存の点・誤差を変更しない（I6）。記録自身がarrivalの既存の生時刻構成も変更しない。

1. 保存要求Sの状態を凍結した時点で、ストリームの取得世代・boot・sourceDomain・連続した生時刻の区間を識別した`LayoutSegment`を作る。原点は窓との所属判定より前に、その区間について凍結した有効サンプルから1回だけ選び、`(referenceCandidateId, v_ref, u_ref)`を保持する。これは計算用の内部状態であり、新しい保存フィールドは要求しない。`v_ref`はその参照サンプルの生時刻。`u`は`u=t+S−W`という**名目配置用の座標**であり、真の発生Hという保証は持たない。
2. 参照候補は、当該区間の候補元の中から`(受信H, 送信H, 安定したsample識別子, 候補識別子)`の昇順で先頭を選ぶ。候補識別子はboot・AnchorId・sourceDomainを含み、凍結内で全順序を持つものとする。行ごとの近いサンプル、行の受信順、窓内に残った先頭行で選び直さない。参照候補と終点が同じAnchorIdかつ共通のseq系なら、`seq_ref=参照sampleのelapsed+そのsegmentOffset`、`u_ref=S−T_S.point+seq_ref`。差を取る前に両者の共通seq原点を確認する。別AnchorIdまたは共通seqを保証できない場合は、その参照サンプル自身の送信H・受信Hによる読取区間の名目中点`u_ref=sentH+floor((receivedH−sentH)/2)`を使う。別bootのDや他候補のθをここへ加えない。この中点はその読取区間の代表であって、各frameの真の発生Hを確定しない。
3. 同じLayoutSegmentの各行iについて、整数nsで`p_i=W−S+u_ref+(v_i−v_ref)`を計算し、`t_ns=p_i`とする。したがって同じ区間の2行では、正負のどちらも`p_j−p_i=v_j−v_i`となる。端末時刻の差を名目尺に使う規則であり、D−sourceやsource−Hの新しい速度上限を主張するものではない。SやWを使って各行を個別にclampしたり、中点へ戻したりしない。上の反例では共通seqの参照から`u_ref=100秒`となり、PTS=103秒／104秒の点は3秒／4秒である。両端未知でも1行ずつ0へ戻さず、同じ固定原点を使う。
4. 包含包絡`C_i=[c_lo,c_hi]`を保ったまま、点も含む**計算上の位置範囲**`R_i=hull(C_i ∪ {p_i})`を作る。有限な端は`r_lo=min(c_lo,p_i)`、`r_hi=max(c_hi,p_i)`、未知の端は未知のままにする。p_iがC_iの外ならR_iを外側に広げ、点を内側へ動かさない。E3の「計算上の範囲」はこのR_iを指す。宣言・保存はR_iにE3を適用し、最終arrivalの下限は必ずnull、ほかの端はR_iを保持する。全点が保存範囲に入り、C_iを狭めず、真値包含も失わない。例えば同じ反例でPTS=210秒も同時受信したなら名目点110秒を置き、上限を6秒から110秒へ広げる。この広がりは名目配置と包含の違いを示すもので、110秒を真の発生位置と証明したことにはならない。
5. 複数の候補・錨があっても、C_iは全候補を各々の原点で変換して包絡したものを残す。連続した同じsource区間に属することを確認できる行は、錨が変わっても同じ名目参照を使う。名目参照の選択は真の候補を選別したことではなく、行ごとに候補を切り替えて端末内間隔を変えてはならない。boot・sourceDomainが異なる場合、既知のsourceの段差・PTSのreset・ストリーム再開始では区間を分け、参照を接続しない。どの区間かを特定できない行は、否定できない候補集合を保持し、その集合と取得世代をキーとする別の名目区間で1つの参照を固定する。区間をまたぐ差を確かな経過時間と呼ばない。未観測段差についての原本の限界は残す。
6. R_iの拡大・名目点をθ、Dの換算範囲、floorやF_Aへ戻し、新しい時計の証拠にしない。所属・trim選択は**最終的な宣言／保存範囲**を使い、C_iだけや内部有限下限で追加除外しない。E5のhiも拡大後の宣言上限である。先行除外のBは補助終点範囲E_Aで広げたC_iを包む必要があり、元のT_SだけによるBは再利用しない。BがC_iだけを包んでいる場合は、名目点も包む`max(B,p_i)`へ広げるか、その最適化を省く。これによりE5の証明付き除外は選択を変えない。

参照候補の送信Hがnullの場合は、ソートでは既知の送信Hより後に置く。共通seqの式を使えず、読取の有限なH区間も無い自走サンプルでは、`u_ref=receivedH`を名目原点にだけ使う。これはsource時刻とHの対応の保証ではない。送信Hを捏造して往復測定に見せたり、包含範囲を狭めたりしない。未知の包含端は4のとおり未知のまま保つ。

凍結後の新測定・窓の再選択で同じ保存物の参照を更新しない。trimでは既存の点と有限端からaを引き、nullは保持する。参照サンプルや名目原点を選び直さない。直接のtrimと入れ子のtrimは同じ点・範囲・間隔を得る。MP4はこの点の並びを使用して表示部分だけを`[0,W]`に切り、負の点は既存のpreroll／起点内容、Wを超える点は窓後として扱う。各点を0やWへ移して元の行の位置を書き換えたり、窓外の再生時間を作ったりしない。多倍長の差・加算で計算し、必要な有限点を保存型で表現できない場合は原本0.6の停止・報告とし、wrap・各点別の飽和で間隔を変えない。

**変更しない限界**: サンプルでまだ観測していない端末時計の段差に対して、真値包含を保証するものではない（原本324行）。ρの仮定と測定済み対応に基づく範囲である。凍結後の新測定で、候補・位置・等級を変更しない。

### 後続Trailと試験

- T2: 影の候補列挙・原点を持つ範囲・tでの包絡を実装する。`estimatedBackwardWallStepKeepsBothCandidates`（上の100/110秒と4msの反例）、`candidateCausalityUsesItsOwnAnchor`、`differentAnchorsConvertThroughHostBeforeHull`（異なるsegmentOffsetも使用）、`estimatedPointIsInsideHull`、`knownCandidatesDoNotChangeMeasuredRows`を加える。5秒ちょうど／+1ns、20msちょうど／+1nsとI6の既存比較は保持する。
- T4a: T2と同じ計算規則に従う候補の広いt範囲を持つfixtureを読み、点と範囲を変更せず保持する。読み手がoffsetを選び直さない。
- T2の追加回帰: `delayedEventUsesFeasibleEventTime`（±500ppmの両符号、上の旧式の除外を再現）、`unknownSourceRateDoesNotInventFiniteConversionError`、`eventAtSampleAndReceiptBoundaries`（同時・直前／直後1ns、遅延0／90秒以上、x=受信H／+1ns）、`unboundedCandidateIsNotDropped`、`allKnownBootRegionSourceCandidatesSurvive`。各断面の真値包含、射影の外向き丸め、候補自身の錨、包絡内の有限点、measured/I6不変を検査する。
- T2の終点包含回帰: `estimatedRowUsesAnchorEndpointEnvelope`で上の1ms除外を再現し、ρ=0／500ppmと真のドリフトの両符号、S=測定H／+1ns／5秒ちょうど、未知の錨端、共通segmentOffsetの相殺を検査する。E_Aは元の終点範囲を狭めず、最終arrivalの下限null・点と保存範囲・E5の拡大後の上限証明を維持し、measured行と終点の既存点・等級・誤差フィールドは変わらない。T4a/T4bでも広げたestimated行の範囲を読取・保存・繰り返しtrimで保持する。
- T4b: T2の凍結した点・範囲・等級を保存する。推定位置を再計算して狭めず、trimでは各位置・有限な範囲の両端を同じ量だけ移す。
- T2の代表点試験: `estimatedBatchKeepsSourceSpacing`（100秒の単一sample、同時受信106秒、PTS103秒／104秒で3秒／4秒、旧中点は両方−47秒）、`estimatedLayoutUsesFrozenSegmentOrigin`、`nominalPointWidensDeclaredRangeWithoutNarrowingCandidates`、`unknownBoundsDoNotCollapseNominalSpacing`、`multipleAnchorsKeepCandidateHullAndOneLayoutOrigin`、`sourceStepStartsSeparateLayoutSegment`、`layoutOverflowStopsInsteadOfClamping`。`estimatedPointIsInsideHull`はR_iと最終宣言範囲への点の包含を検査し、C_iへの個別clampの試験にはしない。正負のPTS差・±1ns、有限端と点の等号／±1ns、片側／両側未知、複数候補の順序、異なるsegmentOffset、同じsourceの錨変更、bootとsourceの段差、最終arrivalのnull下限、measured/I6不変を含める。T4a/T4bは`trimKeepsFrozenLayoutAndNullBounds`・`mp4ClipsDisplayWithoutRetimingRows`で、読取不変・入れ子trimの点／範囲同値・端末間隔とMP4の上限を検査する。

<a id="e2-graded-coverage"></a>
## E2 確認済みの小区間に等級を保存する

対象: finding `01M7MA51VCW6MH5WKK7NVQJWXF`。実装仕様書4.5の型（336〜337行）・手順4（352行）・I4（373行）、6.9(4)(c)の例（855〜859行）と同(g)（881行）を訂正する。方針書4.7の等級別内訳・trim（306〜308行）を実現するための情報保持であり、3区分の意味を変更しない。

### 反例

確認済みが`[0,10秒)`で、measuredとestimatedが各5秒という合計だけでは、先頭5秒をtrimしたときにどちらが5秒なのか分からない。音声にはpacket別等級のJSONLもなく、保存された行からの復元を前提にできない。

### 型と数え方の置換

```kotlin
internal data class ConfirmedSpan(val span: Span, val grade: TimeGrade)

// Coverageの該当部分。ほかのフィールドは原本4.5のまま。
val confirmed: List<ConfirmedSpan>
val confirmedNs get() = confirmed.sumOf { it.span.to - it.span.from }
val confirmedByGrade get() = TimeGrade.entries.associateWith { grade ->
    confirmed.filter { it.grade == grade }.sumOf { it.span.to - it.span.from }
}
```

原本の手順1〜3で定まる集合`C = C0 − M`を変えず、手順4で得る「隣り合う受信の両端のうち悪い等級」を**各小区間と組にして残す**。同じ接続・同じ位置の複数受信は、その位置で最も悪い等級を使う。異なる接続の区間が重なる場合は端点でさらに分割し、その小区間を覆う等級の悪い方を使って1回だけ数える。欠落Mで必ず切り、長さ0を除き、位置順に並べる。隣接して等級も同じ小区間だけは併合してよい。等級境界や欠落を越えて併合しない。

`confirmedByGrade`はこの一覧から導く集計値で、独立の入力として保持・更新しない。従ってI4は整数nsで常に成立する。I1〜I3の検査に使う確認済み集合は、この一覧からgradeを除いた区間の和集合とする。`missing`の理由別一覧の重複は原本どおり許し、`missingNs`にはその和集合Mの長さを使う。

### schema 5の形

`coverage.<種類>.confirmed`の**各要素に必須の`time_grade`**を加える。位置と等級を別配列にしない。`confirmed_by_grade`は表示・検査用の派生値として引き続き保存する。音声を含む全種類に同じ形を使い、packet別JSONLの存在に依存しない。原本6.9(4)(c)のcoverageの例を次に置き換える（関係部分の抜粋。ほかのtimebase等の項目は原本のまま）。

```json
{
  "schema": 5,
  "timebase": { "window_ns": "30000000000" },
  "coverage": {
    "video": {
      "state": "CAPTURING", "requested": true, "reason": null,
      "confirmed_ns": "27000000000",
      "unconfirmed_ns": "1000000000",
      "missing_ns": "2000000000",
      "confirmed_by_grade": {
        "measured": "25000000000", "estimated": "2000000000", "arrival": "0"
      },
      "confirmed": [
        { "from_t_ns": "0", "to_t_ns": "12000000000", "time_grade": "measured" },
        { "from_t_ns": "14000000000", "to_t_ns": "27000000000", "time_grade": "measured" },
        { "from_t_ns": "27000000000", "to_t_ns": "29000000000", "time_grade": "estimated" }
      ],
      "unconfirmed": [ { "from_t_ns": "29000000000", "to_t_ns": "30000000000" } ],
      "missing": [ { "from_t_ns": "12000000000", "to_t_ns": "14000000000", "reason": "PC休止" } ]
    }
  }
}
```

T4aのfixtureと読み手をこの形で先に揃え、T4bの書き手は同じ形を出す。既知3等級では、小区間の長さ・等級から各`confirmed_by_grade`、`confirmed_ns`を再計算し、一致を検査する。等級のないconfirmed要素は不正なschema 5として拒否し、合計の比率や周辺行で境界を推測してはならない。未知の等級を読み手が「不明」として扱う原本6.8の方針は維持し、既知等級へ推測置換しない。`EXCLUDED`で3区分がnullになる原本の扱いも維持する。

### trimの置換規則

`0 ≤ a < b ≤ W`に対するschema 5のtrimは、等級付きconfirmedの各`[u,v)`を`[max(u,a), min(v,b))`に切る。空なら捨て、残った両端からaを引き、**gradeをそのまま保持する**。unconfirmed・理由付きmissingも同じ切り方・移動を行う。新しい`window_ns = b − a`とし、confirmedの長さと全等級の内訳を一覧から整数nsで再計算する。`missing_ns`は切った理由別一覧の和集合から、`unconfirmed_ns`は切ったunconfirmedの長さから再計算する。割合配分・丸め・音声の行からの推測は禁止する。

通常保存もtrimも同じ小区間の正規化・集計を使う。通常保存の`splitCoverage`で保持した等級境界を使うので、全受信（窓の外の前後の受信も含む）から同じ範囲のcoverageを計算した結果と一致する。`[a,b)`でtrim後に`[c,d)`をtrimしたcoverageは、元を`[a+c,a+d)`で直接trimしたcoverageと一致する（`0 ≤ c < d ≤ b−a`）。

上の30秒の例を`[26秒,30秒)`でtrimすると、confirmedは`[0,1秒)` measuredと`[1,3秒)` estimated、unconfirmedは`[3,4秒)`、missingは空。内訳はmeasured=1秒、estimated=2秒、arrival=0、確認済み3秒で、3+1+0=4秒になる。さらに`[0.5秒,3.5秒)`をtrimするとmeasured=0.5秒、estimated=2秒、unconfirmed=0.5秒で、元の`[26.5秒,29.5秒)`からの直接trimと一致する。

### 後続Trailと試験

- T2: `ConfirmedSpan`を持つ`Coverage`と正規化・集計を影の計算に用いる。`confirmedRetainsGradeBoundaries`、`overlappingRunsCountOnceWithWorseGrade`、`gradeTotalsAreExactNs`とI1〜I4を検査する。
- T4a: grade付きconfirmedを保持できる読み手と`schema5-*` fixtureを揃える。等級のないconfirmedからは推測復元せず、既知3等級の集計不一致を拒否するfixtureを加える。同じ合計5秒ずつでも順序が逆の2つのfixture、packet別JSONLのない音声のfixtureを用意する。T4aの時点ではschema 5 trimの拒否を継続する。
- T4b: この一覧と派生合計をそのまま保存し、trimはこの一覧を入力とする。`trimKeepsGrades`に、先頭5秒で異なる結果となる2つのfixture、上の30秒の例、音声のtrim、等級境界のちょうど／1ns前後、直接trimと繰り返しtrimの一致を加える。schema 4のtrimは変更しない。

<a id="e3-position-bounds"></a>
## E3 到着基準の証明と一般の誤差範囲を区別する

対象: finding `01M7MA54TD38XG1QW6X6SH4DBJ`。実装仕様書6.6(5)(e)（670行）の「どの行も`t_latest`はWを超えない」を撤回し、以下に置き換える。関連する4.4の321行の証明は下の前提がある場合に限定する。**6.9(5)(c)（888行）・方針書4.8（319行）の、最終`time_grade=arrival`の全行で`t_earliest_ns=null`とする保存規約は変更しない。** オーナー回答7章の2「到着基準の行は t_earliest_ns を null」に従う。方針書4.4(5)（218行）の窓外の誤差を切り詰めない規則と、この下限を主張しない保存規約を両立させる。

### 反例と一般規則

同じ錨・seq原点で`d = T_S`、それぞれの誤差が±1msなら、原本320行から`t = W`、`t_earliest = W − 2ms`、`t_latest = W + 2ms`となる。両方measuredでも成立する。一般の独立な誤差範囲には`t_latest ≤ W`を課さないし、成立させるために誤差幅を切らない。

最終行等級は`worst(記録の等級, 終点の等級)`であり、**最終行等級がarrivalであることだけでは証明の前提にならない**。記録がmeasured／estimatedで有限の幅を持ち、終点がarrivalとなる混合ケースも含むためである。同じ軸で`t = W`、`T_S_lo = dの点`、`d_hi = dの点 + 1ms`、`T_S_hi`なしを与える式の試験では、最終行等級がarrivalでも`t_latest = W + 1ms`、`t_earliest = null`になる。

混合ケースも、同じAnchorIdなら原本320行の上下限の引き算、別AnchorIdなら4.2のその区間の錨からHを経由する変換で**内部の位置範囲**を求める。例えば別錨のmeasured記録をHへ写した範囲が`[S−1ms,S+1ms]`、点がSなら、内部のt範囲は`[W−1ms,W+1ms]`、点はWである。終点の等級から最終行等級がarrivalとなる場合も、この有限下限を内部で知り得る。しかし保存行は`time_grade=arrival`、`t_ns=W`、`t_earliest_ns=null`、`t_latest_ns=W+1ms`とする。有限な内部下限を保存しないことは出力範囲を`(−∞,W+1ms]`へ保守的に広げる規約であり、データの可能な位置を狭めない。内部でも有限下限を知り得ないという主張ではなく、nullを0で埋めない。上限をWに切らない。これは誤差範囲であり、発生が受信より後だったと断定するものでもない。E1のestimated候補の因果関係による除外と、measured記録の誤差幅の保存を混同しない。

値の対応は次のとおりとする。`lo`・`hi`が不明ならその端はnullであり、JSONのnsの表記は原本どおりとする。

```text
内部の位置 = (lo, point, hi)                    // 同じ錨／別錨の変換結果
最終行等級 = worst(記録の等級, 終点の等級)
t_ns = point
t_earliest_ns = (最終行等級 == arrival) ? null : lo
t_latest_ns = hi
```

この保存規約はE1のestimated記録が終点との組合せで最終arrivalになる場合にも適用する。候補列挙を変えず、下限の宣言・出力をnullにする。所属・保存／trimの選択は、宣言・保存済みの行範囲を使う原本T2／T4bの契約のままとする。計算上知り得る有限下限は数学的説明のために区別するものであり、追加の所属判定・trim選択には使わない。旧版で維持した原本6.6の独立した受信H cutoffは、[E5](#e5-host-cutoff)で置き換える。保存されたnullから「内部にも有限下限が無かった」と逆推定せず、読み手がその下限を推測復元することもない。新しい保存項目や選択条件は追加しない。

全等級について、**有限の点が計算上の範囲および宣言した出力範囲の中にあること**を検査する。最終arrivalは下限を常にnullとし、点・上限は計算上の値を保持する。ほかの等級は計算上の上下限をそのまま出す。measuredの点・誤差とI6、MP4の表示範囲を`[0,W]`に切る規則、coverage区間を`[0,W)`に切る規則は変更しない。下限を主張しない宣言・保存規約と、表示・長さの切り取りは別の操作である。

### 上限を証明できる前提

321行の`t = t_latest ≤ W`は、次の条件を**すべて**満たす生時刻系のarrivalの構成について検査する。

1. 記録と終点が同じAnchorId、同じboot、同じ生時刻系に属し、両方ともその系の到着基準の構成を使う。別錨・別系・別bootの数値を引かない。まだ測定されていない段差等で同じ系の連続性の仮定が破れていない。
2. 記録`i`の生時刻`v_i`の換算幅は0で、独立に拡大したmeasured／estimatedの`d_hi`を流用しない。記録の受信Hを`h_i`とし、凍結した対象の全行で`h_i ≤ S`。
3. 同じ系の最小形`f(S)`が、その記録による下限を保っていること、すなわち`f(S) ≥ v_i − h_i − ceil(ρ × (S − h_i))`を満たす。終点の点と下限にはともに`S + f(S)`を用い、上限は無い。別の錨やサンプルのoffsetをこの式へ足さない。

このとき、整数nsで`(S + f(S)) − v_i ≥ (S − h_i) − ceil(ρ × (S − h_i)) ≥ 0`なので、`t_i = t_latest_i = W − ((S + f(S)) − v_i) ≤ W`、`t_earliest_i = null`となる。受信がSちょうどなら上限の等号も許す。この証明の条件を持たない行に、ラベルarrivalだけを根拠にこの上限を適用しない。

### 後続Trailと試験

- T2: 670行(e)を「上の前提を持つarrival構成で`t == t_latest ≤ W`。一般の行では点が内部・出力範囲内」に置換する。`measuredAtEndKeepsLatestBeyondWindow`（±1msずつ、W+2ms）、`mixedArrivalGradeDoesNotImplyLatestWithinWindow`（記録measured/estimatedの両方）、`otherAnchorUsesHostBounds`、`rawArrivalBoundRequiresSameSourceAndFloor`を追加する。`arrivalHasNullEarliest`は維持し、`everyFinalArrivalSerializesNullEarliest`で記録／終点の等級全9組、同じ錨／別錨、内部下限が有限／不明の場合を検査する。最終arrivalなら例外なく出力下限はnullであり、内部の有限下限・点・上限は変わらない。
- 境界は整数nsで検査する。生時刻arrivalは`h_i = S`／`S−1ns`、ρ=0／500ppm、同じ系のfloorへの自己の寄与を保持した場合の等号を含める。一般範囲は`t_latest = W`／`W+1ns`／`W+2ms`、計算上の下限0／−1ns、点と両端の等号、片側nullを含める。原本の宣言範囲による所属判定（上限0は窓内、−1nsは窓外、有限な宣言下限Wは窓内、W+1nsは窓外）も保持する。計算上の下限がW+1nsでも、最終arrivalでは宣言下限がnullなので、その有限値を追加の窓外除外に使わない。受信H cutoffの境界試験はE5の証明条件とちょうど／±1nsへ置き換える。
- T4a: 窓外にはみ出す誤差と混合arrivalのfixtureを読む。measuredのW+2ms、最終arrivalのW+1msの上限を保持し、arrivalのfixtureは内部で有限下限を知り得るケースも含めて必ず`t_earliest_ns=null`とする。読み手はそのnullを保持し、0や推測値を入れない。
- T4b: JSONLは上の保存規約に従い、最終arrivalの全行で`t_earliest_ns=null`を検査する。trimでは点と有限な上下限からaを引き、arrivalのnull下限はそのまま保持する。窓外への上限のはみ出しや、ほかの等級の有限下限は新しい窓でも切り詰めない。原本の宣言・保存範囲による選択を維持し、内部の有限下限を追加の除外条件にしないこと、直接保存・読み取り・繰り返しtrimでのarrival下限nullの維持を試験する。MP4・coverageの窓内切り取りとは分ける。

<a id="e4-conservative-floor"></a>
## E4 換算点を真のelapsedとして片方向floorへ戻さない

対象: finding `01M7MEDX70YFVF1WABP80NKW64`。実装仕様書4.3の`OneWayFloor`（276行）と更新・合成（287〜289行）、用語の`d − h`（66〜67行）、6.6の`anchors.observe`の入力を次で具体化する。`MappedTime.elapsed`が非nullというだけで、幅0のDとして使う解釈を撤回する。原本4.4のmeasuredの集合・点・誤差、5秒／20msの境界、I6は変えない。

### 反例

`ρ=0`、真の`θ=D−H=0`、有効なサンプル`before=mono=sent=0`、`after=received=2000000`とする。名目offsetは`1000000`、RTTのθ範囲は`[−2000000,2000000]`。H=`3000000`で即時受信した生mono=`3000000`は名目elapsed=`4000000`へ写る。これを真値扱いすると`cand=1000000`となり、範囲を`[1000000,2000000]`に狭めて真の0を失う。`floor > upper`ではないので、矛盾時の退避だけでは直らない。

### 入力・状態・更新

floorのキーは`(bootSegment, AnchorId, valueDomain)`とする。`valueDomain`はBOOTTIME elapsed、生MONOTONIC、生epochを区別する。seqは受け取らず、segmentOffsetを加えた値はelapsedとして使わない。型の最小契約は次のとおり（名称は案）。

```text
ConversionBounds = (bootSegment, AnchorId, domain,
                    point, lower, upper, provenance)
OneWayFloor = (bootSegment, AnchorId, domain, thetaLower, atH)
// point は表示・既存換算用。floorの証拠は lower とその包含保証。
// lower=null は有限下限なし。OneWayFloor自体が無い状態も許す。
```

1. BOOTTIME系floorには、**真のイベントDを含むと保証できる換算範囲の下限**`d_lower`だけを入力する。点から直接`cand`を作らない。`MappedTime`の点・誤差を残したまま、floor用の包含範囲を別に持つ。既存の誤差幅を包含保証として使えるかは発生時刻と時刻系の根拠によるもので、点が非nullなことだけでは保証にならない。遅延・候補不明の換算はE1のイベント時刻の集合を用いる。未知なら`lower=null`として、この系のfloorを更新しない。
2. 発生から受信H=`h`まで同じboot・AnchorId・系の連続性を保証できる場合だけ、`cand = d_lower − h`をその系の下限候補とする。複数候補が同じキーであり、そのいずれかが真である場合は下限の最小値を使う。候補のキー自体が複数なら、共通のelapsedを知ったとみなす更新はしない。各候補の名目点で複数のfloorを強化することもない。
3. 生の値が得られる間は、その生時刻系のfloorを独立に保つ。生値v自体に読取幅があればその下限を用い、幅0ならvを用いる。生系での`v−h`はその系−Hの下限であり、D−Hの下限ではない。生系の既定の時間変化の仮定とD−Hのρを、D−sourceの新しい速度上限へ合成・流用しない。生のfloorをDへ移す場合にも、上の独立した換算範囲とキーの根拠が必要である。
4. 同じキーの旧floorを`f`とすると、`cand > f.thetaLower − ceil(ρ × (h−f.atH))`のときだけ`(cand,h)`へ更新する（Hは非減少）。初回の有限候補はそのまま採用する。任意のxで`f(x) = f.thetaLower − ceil(ρ × |x−f.atH|)`。使うρはその系−Hについて既に宣言された仮定のものだけとする。系の速度上限を保証できない区間には、有限のfloorを外挿しない。根拠となった同じ行を含む点・推定offset・そのfloorで狭めた範囲を循環させて、新しい証拠を作ってはならない。
5. RTTの`lo(x)`・`hi(x)`との合成は同じelapsedキーだけで`max(lo(x),f(x))`とする。`max == hi`は有効。`max > hi`ならその評価ではfloorを使わず、元のRTT下限へ戻し、矛盾を数える。重なっていること自体を入力の正しさの証明にはしない。サンプル同士の逐次交差、矛盾による新しい錨の切断は引き続き影運用に限る。

真のイベントD≥`d_lower`、発生x≤h、Dが同じ連続区間で非減少なので、`D(h)−h ≥ d_lower−h`となる。この下限だけをhからρで広げるのが上の更新である。複数候補の「代表点」はこの証明に現れない。反例で、端末内読取の包含区間からmono→Dを`[3000000,5000000]`と保証できれば`cand=0`となる。元の`readError=2000000`を名目点の両側へ置いた、さらに広い包含区間`[2000000,6000000]`なら`cand=−1000000`である。いずれも真のθ=0を残す。保証のない区間はこれらの有限区間を仮定せず、更新しない。

### 後続Trailと試験

- T2: 既存案の`minimalFloorKeepsLeastDelayedRecord`・`minimalFloorDecaysByRho`は、正確な生値の試験として残す。換算済み入力には`mappedPointCannotTightenAsExactElapsed`（上の反例）、`mappedFloorUsesConservativeLower`、`ambiguousOriginDoesNotUpdateElapsedFloor`、`rawFloorsRemainSeparate`を加える。`floorAboveUpperBoundIsDroppedAndCounted`は、正しい包含保証が別に必要であることも検査する。RTT下限との交差／等号／非交差、ρ=0／500ppm、h=x／遅延、null下限、両端±1nsを含める。
- E1で用いるfloorもこの規則に従う。E3の幅0・同じ生系のarrival証明とその前提は変更しない。`mappedUncertaintyDoesNotSatisfyRawArrivalProof`と全9等級組のE3試験、`knownCandidatesDoNotChangeMeasuredRows`・I6を合わせて検査する。T4aは点と宣言範囲を再換算せず読み、T4bはT2の凍結結果を保存する。

<a id="e5-host-cutoff"></a>
## E5 受信Hだけの一律cutoffを宣言範囲の除外証明へ置き換える

対象: finding `01M7MEE1F549XTZS673PWG2FZ4`。実装仕様書2.1 D7、6.6(4)（666〜667行）、6.9の変更表の`hostStart`・`prune`、方針書4.4(2)の`受信H < S − W − ceil(ρW)`という等級共通の独立除外と、別錨なら常に余裕0という適用を撤回する。E3旧版の同cutoffを維持する記述も本節が置き換える。原本4.2の`ceil(ρW)`はD側での起点のずれの表示としては残すが、H単位の除外の余裕として流用しない。`ShadowWindow.slack`も一律の行除外閾値にはしない。

### ドリフトだけの反例と単位

`W=900000000000`、`ρ=1/2000`、Dが`(1−ρ)H`で進む。同じ錨の幅0の行が即時受信され、`S−受信H=900450002000`なら、`T_S−d=(1−ρ)×900450002000=899999776999`、`t=223001`で窓内である。旧余裕`450000000`では、受信Hが旧閾値より`2000`ns古いため誤って除外する。

この幅0・同一錨・連続という限定条件でも、D上の長さWに対応し得るHの長さは`W/(1−ρ)`である。H側の整数余裕は少なくとも`ceil(ρW/(1−ρ)) = 450225113`。`ρ=0`なら0。これは丸め前のドリフトから必要な幅を示す式であり、一般の宣言範囲の十分な除外条件ではない。実際の整数nsの丸め、行・終点の不確かさ、別錨については次のBの証明を使う。いずれの証明でも閾値の等号では除外しない。

### 一般の除外規則と証明を持つ最適化

規範となる選択は、E3の保存規約を適用した**宣言する行の閉区間**`[lo,hi]`と`[0,W]`の交差である。`hi < 0`、または有限な`lo > W`のときだけ窓外とする。上限0／下限Wの等号は窓内候補である。最終arrivalのloは常にnullであり、内部の有限下限から追加除外しない。範囲が窓と交わるのに「真のイベントは古いはず」という別の理由で落としてはならない。

受信Hによる先行除外を実装する場合は、**真の位置だけでなく宣言上限hiそのものを上から包む**有限な証明値Bを求め、`hi ≤ B < 0`を保証できるときに限る。これは上記と同じ判定の最適化である。Bとhiを交差させて宣言範囲を狭める操作ではない。証明のない候補、未知の上限、未知の誤差、originの不一致が1つでもあれば先行除外しない。最適化を省き、常に宣言範囲を計算して判定する実装もこの契約を満たす。

具体的には受信Hをh、`Δ=S−h≥0`とし、候補ごとに次を用いる。次の同一錨の式の`d_hi`と`T_S_lo`は、両方から共通のsegmentOffsetを引いたBOOTTIME値を指す。D(h)の上下限とseqの上下限を、そのまま比較してはならない。共通のBOOTTIME系にそろえられない場合は別錨と同じH射影による証明を用いる。

- **同じAnchorId・boot・seq原点**で、x≤hからSまでD−Hの速度の仮定が連続して成り立つ場合: 宣言に使う`d_hi`・`T_S_lo`について`d_hi ≤ D(h)+E_d`、`T_S_lo ≥ D(S)−E_S`を包含保証し、`E=E_d+E_S≥0`（整数ns）とする。外向きの丸め込みで`hi = W−T_S_lo+d_hi ≤ W−Δ+ceil(ρΔ)+E = B`。有限なEを保証できなければ使わない。例えば同じ錨のD(h)の包含下限`D_h_lo`とD(S)の包含上限`D_S_hi`が得られれば、`E_d=max(0,d_hi−D_h_lo)`、`E_S=max(0,D_S_hi−T_S_lo)`が安全な選択である。名目点をこれらの真値の下限・上限に代用しない。Eは全てDのnsである。丸め前のH側の必要余裕は`ceil((ρW+E)/(1−ρ))`だが、整数のBに対する十分な余裕は`hostSlack = ceil((ρ×(W+1ns)+E)/(1−ρ))`。`Δ > W+hostSlack`ならB<0となる。例えばW=1ns・ρ=1/2・E=0では旧い丸め前の余裕1nsだけだとΔ=3nsでもB=0であり、宣言上限0の行は残す。整数の余裕2nsなら、この等号を落とさない。900秒・500ppm・E=0の例では整数の式でも450225113nsである。
- **別AnchorId**の候補は、E1と原本4.2に従ってHを経由する。その宣言上限の元となる`h_pos_hi`が有限なら、`hi = W−S+h_pos_hi`。`E_H=max(0,h_pos_hi−h)`とすると`hi ≤ W−Δ+E_H = B`であり、余裕を無条件に0にはしない。PCで直接観測した幅0の出来事なら`h_pos_hi=h`で余裕0となる。別bootのseqを引いたり、他候補のoffsetでE_Hを作ったりしない。
- 候補が複数なら**全候補**の証明値の最大をBとする（宣言hiも候補の最大である）。候補の1つだけを使って除外しない。混合等級でも同じで、最終arrivalだからというだけで誤差Eを0にしない。どの側でもnull上限は正の無限大で、有限なcutoffを導かない。下限nullはhiの証明を妨げないが、有限下限へ戻す理由にもならない。

θ・行・終点の保証は同じ凍結状態と原点から得る。後から得た測定で宣言範囲や選択を狭め直さない。`ρ=1`や負のρは前提外として拒否し、分母0・負数を計算しない。閾値計算やBは外向きに丸める。`ceil((ρW+E)/(1−ρ))`が大きくなったときも固定長へのwrap、余裕の切捨て、上限の偽造をしない。

### 保存・trim・後続Trailと試験

T2は原本6.6の影の所属と受信H境界試験を上の宣言範囲による選択に置き換え、T4bは通常保存に同じ規則を使う。原本6.9の変更表の`hostStart`・`prune`に残る一律の`ceil(ρN)`による除外も撤回する。リングの時間による除外には、原本の「どの将来のSに対しても窓外と言えるものだけ」という追加条件を保つ。最大窓N、候補・等級・終点の不確かさと錨の変化を含めて証明し、現在の小さいWで窓外というだけでは捨てない。将来について証明できなければ時間を理由に捨てない。バイト上限等の既存の別制限は変更しない。

T4aは保存済みの広い範囲をそのまま読む。新しい保存フィールドは追加しない。schema 5 trimは保存済み範囲と対象`[a,b]`の交差で選び、点と有限な端からaを引く。受信Hの証明用状態を推測復元せず、元のH cutoffをtrim窓へ再適用しない。直接trimと入れ子のtrimで同じ行・null・範囲を得ることを検査する。これはE2の半開coverageの切取りと両立する。既知のgap不確かさの問題のT3b-2／T4bへの後送は変えない。

追加試験は`slowDeviceDriftDoesNotLoseWindowStartRow`（上の223001ns）、`hostSlackDividesByOneMinusRho`、`hostExclusionRequiresDeclaredUpperProof`、`uncertaintyAndMixedGradesDisableUnprovenCutoff`、`otherAnchorCutoffUsesHostUpper`、`unknownUpperRetainsCandidate`、`trimSelectionUsesPersistedBoundsRepeatedly`。余裕の閾値ちょうど／±1ns、宣言hi=0／−1ns、lo=W／W+1ns、ρ=0／500ppm／1に近い有効値／範囲外、最終arrivalのnull下限、W+1ns／W+1ms／W+2msの上限、片側・両側未知、複数候補の1つだけが窓内、異なるsegmentOffset、大きなWと整数限界を含める。最適化の有無で選択が一致することを検査する。I6・measuredの値・誤差、schema 4の保持・保存・trimは変更しない。

<a id="e6-fallback-pair"></a>
## E6 成功した読取時のHとnanoの組で退避を継続する

対象: finding `01M7MEE4EY41S1BAVAXBE03REE`。実装仕様書6.3(4)（492〜507行）の`GuardedHostClock`の例と「最後のH − 失敗直後のnano」というoffset規則、4.1の`HostReading`の退避時の注釈と`HostTime`のlast更新を次で置き換える。Hとnanoの直接の差は、ここで定める参照対による明示的な原点の接続だけに隔離する。他の呼出元は従来どおりHと締切時計を混ぜない。

### 反例

最後の成功でH=`100000000000`、それと組にしたnano=`50000000000`、失敗時nano=`51000000000`とする。旧規則ではoffset=`49000000000`、最初の退避H=`100000000000`となり、休止なしでも1秒失う。成功時の組を保つならoffset=`50000000000`、退避H=`101000000000`となり、観測済みのnanoの1秒を運べる。

### 読取順・状態・公表値

参照は`SuccessRef(hPublished, nanoBefore, nanoAfter)`とし、成功した読取の公表Hと、その読取を挟むnanoの両端を保持する。これは論理的な組であり、毎回オブジェクトを追加確保せずprivateなプリミティブの状態として保持する。GuardedHostClockの読取・参照更新・failed・最後の公表値・methodの切替は同じロックで直列化する。先にロックを得た呼出から公表し、成功refの途中更新や失敗との競合を許さない。

1. nativeを試す間の順序は`n0=nano → native awake → native h → n1=nano`とする。nativeの両方が成功し、値が有効なときだけ公表し、`SuccessRef(公表h,n0,n1)`を置き換える。部分成功・例外では参照を更新しない。失敗後はnativeへ戻らない。nativeが前回公表Hより小さいという不正な読みも有効な成功refにせず、同じ退避へ進む。
2. 成功refでは**n1をnano側の参照時刻**とする。native hの観測は`[n0,n1]`の中であり、n1で同時に測った真のHだとは主張しない。n1を使うのは読取にかかったnanoの時間を二重に加えないための原点の約束である。読取区間とその幅は診断で保持する。休止も速度差も無い理想例でも、この選び方による遅れは成功読取区間内に残り得る。読取中の未観測休止を、そのnanoの幅で上から抑えたことにはしない。
3. 失敗を受けた後に`nf=nano`を読み、成功refがあれば`h_raw = ref.hPublished + elapsedNano(ref.nanoAfter,nf)`とする。成功refが無ければ`ref=(nf,nf,nf)`を退避専用の基準として初期化し、最初の`h_raw=nf`（offset 0）とする。いずれも`failed=true`を確定し、理由の記録は1回だけ。失敗時のnanoで、過去の成功Hを新しく組み直してはならない。
4. 以後は同じ固定refと各回のnanoを使って`h_raw`を求める。失敗・呼出のたびにoffsetを作り直さない。退避の公表値は`h = max(lastPublishedH, h_raw)`、`awake = h`、`includesSuspend = false`、`method = nanotime_fallback`とする。初回でlastが無ければh_rawを使う。Hは非減少でよく、同値を許す。nano差は経過値として計算し、符号付きnanoのwrapを絶対値の大小比較と混同しない（差を一意に取れる`2^63`ns未満の観測間隔に限る）。その前提を満たさない・加算結果が表現不能なら、wrapしたHを返して正常扱いしない。
5. `HostTime`が覚えるlastのHは実際に返した公表Hであり、clamp前の小さい候補値で上書きしない。clampを重ねる実装でも公表状態は1つとし、退避ではawakeも公表Hにそろえる。原本の休止検出は、連続する2回がともに有効なnative対で`includesSuspend=true`の場合にだけ行う。native→退避の境界と以後の退避対は比較しない。awakeは退避開始でnativeのAからHと同じ原点へ変わるが、これを休止の増分として数えない。

これにより原本6.3(5)(a)のH非減少、(c)の退避中`h==awake`かつ`includesSuspend=false`を守る。(b)の休止なしのh−awakeの安定性は同じ手段の連続対、(e)の刻みは同じ手段の通常進行で検査し、原点を接続する1回やclampで0増分となった回に、nativeのAの連続性や正の増分を要求しない。nativeでの`awake→h`順序、しきい値ちょうどでは休止にしない規則は残す。

退避が継ぐのは**成功ref以後に観測できたnanoの進行**である。nanoが進まなかった未観測休止は復元できず、Hが真の休止込み時間だと保証しない。`includesSuspend=false`を保存に渡す原本方針書4.6(b)の限界と、独立した2証拠による後続の補正の条件は変更しない。原本6.5の`FallbackSuspendDetector`が認めた補正は、初期値0の別状態`verifiedSuspendCorrectionNs`へ加え、上のh_rawに足す。この更新も同じロックで行い、固定refを失敗時刻に付け替えない。補正後も退避の印と`h==awake`を保つ。凍結後の保存物を後から修正しない。

### 後続Trailと試験

- T3b-1: `fallbackCarriesElapsedFromSuccessfulPair`（上の101秒）、`fallbackUsesSuccessfulAfterNano`（例: n0=50秒、h=100秒、n1=50.002秒、nf=51秒なら100.998秒）、`partialReadFailureKeepsPreviousRef`、`repeatedFallbackDoesNotRebase`、`firstReadFailureUsesZeroOffset`、`concurrentReadsPublishOneMonotoneState`、`clampKeepsPublishedLastAndFallbackAwakeEqual`、`nanoWrapUsesElapsedDifference`を追加する。成功／失敗の呼出順、Hとnanoの異なる原点、同値／1ns進行、逆行した無効なnative値、整数限界を含む。
- T3b-2／T0-a: 成功区間の割込み、失敗中のnanoの停止、native→退避でAの原点だけが変わる場合に、偽の休止を発火せず`includesSuspend=false`を保持することを検査する。退避の実休止込みの正確さはPASSとしない。T4bの保存印と凍結の規則は原本どおり。T1ではnative実装・テストを変更しない。

<a id="e7-acquisition-start-host"></a>
## E7 時計の確定を待たず取得開始Hを記録する

対象: finding `01M7MH9RY5PJTHD69FCVWZAKC6`。実装仕様書4.2（258〜259行）・6.6(3)（642行）の開始Hを既存の`started`・`logsStarted`と同時設定する規則を置き換える。方針書4.2（170〜172行）のWとSの定義を維持する。

### 開始と選択

- `startedHost`は、この取得ストアが有効な取得元から最初の動画frameまたは端末全体ログ行を受け入れたとき、その行の受信Hで設定する。正しい取得元の識別と受け入れ条件は従来どおり。受け入れた行の`sequence`・等級・時計の確かさや、直後の保持上限による破棄は設定条件にしない。音声・タップ・時計問い合わせだけでは設定しない。
- `logsStartedHost`は、必要な端末全体ログの読み手がそろって取得中であることを最初に確認した通知のHで設定する。どの読み手が必要かは分割の有無を含め取得側の集約状態で決める。時計、アプリログの時刻付き対象確認、出力対象となる行の有無を待たない。これは窓の起点であり、静かな窓の保存受付やアプリログの取得開始・確認済みcoverageを証明するものではない。
- いずれもT3b-2で導入した同一のH系を使い、生成済みのストアを開いた時や接続試行を始めた時には設定しない。有効な世代の通知・受信だけを採用し、ストア内の同期下で最初の値を一度だけ確定する。同じストアを保つ再接続・時計復旧・凍結解除では上書きせず、新しいストアでは未設定から始める。
- 動画を含む保存では`startedHost`、動画を含まない保存では設定済みの`startedHost`と`logsStartedHost`のうち早い値を採る。候補が未設定なら取得開始を証明できない。S、現在のHや最新レコードを代入して窓を作らない。選んだ値がある場合は`W = min(N × 1_000_000_000, S − 取得開始のH)`を使う。Sと開始Hが同じ、またはSのほうが前なら`W ≤ 0`であり保存不可。凍結中のSは`fixedHost`とする。ストリームごとの開始が遅い場合の欠落は4.5で別に扱う。

### 反例・後続Trail

N=30秒、最初の受け入れがH=100秒で、その後すべてarrival、S=110秒ならW=10秒。時計がH=112秒で確定してもS=115秒のWは15秒であり、復旧からの3秒にはならない。動画も保存対象ログ行もなく、ログの読み手がH=99秒で取得中になった場合、ログのみの窓の起点にも99秒を使える。既存の保存受付条件は独立に検査する。

原点差は比較可能な両窓の起点から求める。旧窓の起点を`oldStart`とすると、同じseqを使える測定行について`originShiftNs = (T_S − W) − oldStart`であり、新位置は旧位置からこの値を引いたものになる。例えばN=30秒、H=D、開始H=100秒、時計確定後の最初の旧`started`=125秒、S=`T_S`=130秒では、新しいW=30秒、旧窓長=5秒、原点差は−25秒となる。新しいWが満杯でも旧窓が短いときは、原本6.9(5)(a)（886行）の原点差上限に抵触し得る。上限やU5を本節で変更しない。この入力に当たる場合は同6.9(11)の停止条件に従い、T4bの本採用前に両起点と差を報告する。上限に合わせるため開始Hを付け替えない。

T2では影で計算し、T4bで採用する。既存の`started`・`logsStarted`、schema 4保存と自動保存の受付、I6・測定行間隔は維持する。現行の受付が保存を返さない入力でも、開始HとWの計算は影の単体試験で検査できる。T2の`shortCapture`に加え、`allArrivalWindowStartsAtFirstAcceptedReceipt`、`clockRecoveryKeepsHostStart`、`quietLogReaderStartDoesNotWaitForClock`、`hostStartSelectionMatchesSaveContents`、`freezeAndReconnectKeepHostStart`、`hostFullLegacyShortReportsOriginShift`を検査する。未設定、Sとの差0／1ns、窓の設定長ちょうど、無効世代からの通知、新しいストアへの切り替えも含める。

<a id="e8-retention-and-source-runs"></a>
## E8 保持破棄で取得の接続番号を進めない

対象: finding `01M7MKVXFAKR4EMEB7EQJY4DFH`。実装仕様書4.5（358〜359行）・6.6(3)（642〜643行）の接続番号と保持から外した受信の扱いを訂正する。方針書4.7（296〜305行）の「同じ連続取得」を`Receipt.run`で表す。

- 接続番号はストリームごとに持つ。取得の中断（接続が切れた、取得元が停止・交代して継続性を失った等）を観測したときに、境界の後の受信へ新しい番号を割り当てる。同じ中断の追加通知・gapの更新・併合で二重に進めない。異なる実中断がgapの一覧で併合されても、受信が持つ境界は保持する。別のストリーム、`"clock"`のgap、接続継続中のPC休止は、それだけで番号を進めない。PC休止は原本どおり全ストリームの欠落として確認済み区間から引く。
- 保持期間による`prune`と、byte／packet／config等の上限で受信済みデータを捨てる処理は、取得の中断と区別する。例えば`enforceVideoLimit`が`addGap(lostVideo=entry)`を呼ぶ場合やログのbyte上限処理では番号を進めない。理由文字列や`addGap`の呼出回数から推定せず、発生箇所で中断と保持破棄を区別する。保持された受信と、原本の「最後に保持から外した受信」には受信時点の番号を保持し、排出時の番号を付け直さない。
- 保持破棄による検出済みの欠落は、原本4.5の規則で破棄されたデータの区間をMに入れる。破棄を実行した時刻を現在の取得の切れ目にしない。通常の窓外`prune`は、その処理だけで窓内の欠落を新設しない。先に同一runの受信の位置でC0を作り、欠落Mを引き、E2で等級付き区間にする。保持破棄の記録や、欠落の長さを削るために接続番号を使わない。

反例: W=10秒、位置2、4、6、8秒の動画受信が同じ取得元から続き、各受信時に古いframeの保持上限による破棄が起きる。全`addGap`で番号を進めると各runに受信1件だけが残り、C0が空になる。番号を保てばC0は`[2,8秒)`。検出された欠落が`[3,4秒)`なら確認済みは`[2,3秒)`と`[4,8秒)`の計5秒。対して4秒と6秒の受信の間に実中断があれば、C0は`[2,4秒)`と`[6,8秒)`で、4〜6秒を前後の受信でつながない。

T2で`retentionEvictionKeepsReceiptRun`、`retentionLossStillSubtractsMissing`、`evictedReceiptKeepsOriginalRun`、`sourceBreakSplitsRunEvenWhenGapsCoalesce`を検査し、T4bの保存とtrimでI1〜I4、等級と欠落境界の保持を検査する。動画とログ、他ストリームの継続、重複通知、PC休止、窓外の保持境界の受信を含める。既存の`decodeRun`とschema 4の保持・gap表現は変更しない。

<a id="e9-reply-deadline-clock"></a>
## E9 応答を読み取った時点と保存した締切を比較する

対象: finding `01M7MKVYQ7QHBQRE0BTR0EHJFS`。実装仕様書6.7(4)（705〜706、712、727〜735行）の応答の分類・状態遷移と幅の注釈を訂正する。締切と生存監視の役割・値（714〜724行）は維持する。

1. 送信時にnonce、往復式用の`sentH`、締切時計`System.nanoTime()`の送信値`sentNano`と締切`deadlineNano = sentNano + 3_000_000_000`を1つの未応答状態に保存する。読み手は完全な1行を読んだ直後に`readNano`と`receivedH`を採取し、解析やタイマー処理待ちの後に取り直さない。
2. 有効で一致するnonceの応答は、保存した同じ締切時計で`readNano ≤ deadlineNano`なら`ROUND_TRIP`、`readNano > deadlineNano`なら`LATE`。差`readNano − sentNano`と3秒を比較する等、`2^63`ns未満の比較可能な経過時間として扱い、wrapする絶対値の大小で比べない。3秒ちょうどは`ROUND_TRIP`。同じ比較は`WAITING`と`OVERDUE`のどちらでも成り立つ。nonceなしは従来どおり`FREE_RUN`、未知nonce・不正形式は従来の異常経路。
3. タイマーは未応答のnonceに紐付けた期限経過の通知だけを渡す。期限ちょうどまたはそれより前の通知で期限経過を確定しない。読み手は未応答状態を管理し、`OVERDUE`は未応答の管理にだけ使う。古いnonceの通知を次の問い合わせに適用しない。期限を過ぎたnonceでは`probe_fail(timeout_3s)`を1回記録する。完了済みの読取が処理待ちの可能性がある間、タイマー通知だけで失敗を不可逆に公表せず、読み手が読取時刻と照合する。未応答のまま締切後の行を処理するか期限後に終了する場合にも通知を照合し、締切後の一致応答ならタイマー未実行でも`probe_fail`を先に確定して`probe_late`を記録する。期限内に読んだ一致応答には`probe_fail`を残さない。
4. 終了や一致応答までは新しいnonceを送らない。3秒の期限でhelperを止めない。生存監視は引き続き独立に動かす。往復の幅とR4の上下限には実際の`sentH`・`receivedH`を渡す。分類の3秒は締切時計で測るため、異なるHと締切時計の原点・進み方・読取順を根拠に、Hの幅が必ず3秒超と仮定したり3秒へ置換したりしない。

反例: `readNano − sentNano = 3_000_000_001`nsでタイマーが遅れ、まだ`WAITING`でも結果は`LATE`。逆に読み取りが3秒ちょうどで、行の処理までに期限経過の通知が見えても`ROUND_TRIP`。R1/R3に使ってよい列はこの規則の`ROUND_TRIP`だけで、R4、D11、起動時のheader期限と生存監視は維持する。

T3aの`ClockLateReplyTest`に`replyBeforeDeadlineIgnoresTimeoutFlag`、`replyAfterDeadlineIsLateBeforeTimerFires`、`deadlineRaceRecordsOnlyOneTimeoutForItsNonce`、`deadlineUsesReadNanoAndKeepsActualHostWidth`を追加する。締切前1ns・等号・後1nsを両方のflag、読取から解析までの遅延、古いnonceの通知、締切時計のwrap、Hだけが乖離する場合、無応答中の自走行・終了と組み合わせる。R1/R3の往復列と補助列を検査し、D11の比較を維持する。

## 既存契約の読み合わせ

以下は原本とE1〜E9を併読するときの検査対象と試験入力を明確にするもので、保存形式、合否条件、実施するTrailはそれぞれの既存契約に従う。

### 除外ストリームのcoverage（T4a）

原本6.8(4)（788行）の和の検査は、3区分を数値で計上するストリームに適用する。`EXCLUDED`は原本6.9(4)(c)（873行）とE2が定める未計上の形を使う。読み手はその状態と3区分がnullであることを検査し、nullを0秒に換算しない。T4aの固定入力には、正規の`EXCLUDED`音声を含む保存物が開ける例と、計上対象の3区分の和が`window_ns`に一致しない`schema5-bad-sum`が拒否される例を置く。

### 保存する時刻の半幅（T4b）

原本6.9(3)（820行）の半幅式は、6.9(4)(c)（865〜870行）の保存例とE3に従い、非測定行の**保存する両端が有限**な場合に使う。`t_uncertainty_ns = (t_latest_ns − t_earliest_ns) / 2`。片端でも未知ならnullであり、最終arrival行では有限の上限があっても下限はnullなので半幅もnullとなる。例えば保存範囲が`[8,12]`nsなら半幅2ns、`[8,8]`nsなら0ns、`[null,12]`・`[8,null]`・`[null,null]`ならnullを検査する。時刻の点・両端・包含、measuredの既存の誤差値には元の規則を適用する。

### schema 4互換試験の固定入力（T4a・T4b）

原本6.8(5)(a)・(7)（790、794〜796行）と6.9(8)(d)（901行）の互換回帰には、schema 4の固定保存物と切替前の期待値を使う。`ReplayTrimSchema4FrozenTest`はT1の固定テストと同じ入力条件・期待値を使い、schema 4を開けること、トリム後もschema 4であることと旧式のcoverageを検査する。固定保存物は試験資源またはschema 4に固定したfixture生成で用意し、既定schemaに切り替わる`SaveWriter`でテスト実行時に作らない。現行の書き手を通す試験は、そのTrailの書き手と出力を検査する。T1のベースラインは今回の読み合わせの追加では編集しない。

### 保存物と操作結果の検査（T5）

原本6.10(4)(5)（925、928、930行）のA10・A13では、保存物の`replay.json`とMP4だけで検査できる項目と、操作結果を記録して照合する項目を区別する。

- A10は時計が確定／不明の状態で自動保存を各3回試み、試行ごとに時計状態と実際の保存の有無を対応づける。確定時は保存、不明時は不保存を検査する。保存フォルダの一覧だけでは、不保存が試行された結果かどうかを判定できない。
- A13はU6が「出す」の場合、同じ保存に対するGUI・CLI・MCPそれぞれの秒数を取得し、3面が同じ値であることを検査する。1つの保存物に記録された値だけでは各面の結果を確認できない。

集計では保存物から求めた判定と、これらの観測結果に基づく判定を区別する。公開するのは必要最小限の合格・不合格・未実施と必要な理由に限り、録画・ログ本文や生の診断を含めない。

### クラッシュ自動保存の受付と保存内容（T4b）

6.9(4)(a)・(5)(d)の受付互換は、E7を適用しても、切替前と同じ凍結入力に対して自動保存の可否が一致することを指す。受付には既存の`autosave.admissionProblem`、凍結中・`clock.certain(host)`の判定に加え、旧窓の起点・終点・誤差と、旧選別で残ったログを使う`frozenAutoTrigger`の判定を含む。platform報告が旧選別結果に存在することと、報告の誤差を含む範囲が旧窓の安全側の境界内に収まることは別の条件で、時計の確定や等級だけで代用しない。対象の照合や、Java報告とplatform報告に対する従来の条件はそのまま適用する。

例えば、秒単位でN=30、S=110、取得開始H=100、旧窓が[105,110]、終点誤差が0.001、platform報告の誤差が0.002とする。ここでは時刻軸が揃い、報告の時刻と存在以外の受付条件は満たすとする。

- 報告の中心が104なら旧選別から外れ、旧受付は範囲外で拒否する。新窓[100,110]で選別と境界判定を置き換えるとこの報告を誤って受け付ける。
- 中心が105.002なら旧選別に残っても、報告の下限105.000が旧窓の安全側の下限105.001を下回るので受付は偽。中心が105.003なら境界と等しく、報告が残っていれば受付は真。同じ境界内の時刻でも、保持破棄でplatform報告が旧候補から消えた入力では受付は偽。

ケースごとに同じ凍結入力を切替前後で使い、旧候補に報告が存在するかと旧境界の誤差判定を個別に照合して、最終的な受付が各々一致することを検査する。

保存する窓と行の位置はE7と既存の保存規則に従う。旧窓での受付に使った`auto_trigger`の値を新しい保存物へ転記せず、保存側の`frozen_t_ns`・`arming_t_ns`は保存物の時刻軸と照合し、各報告の`in_output`は実際の保存行の有無と照合する。旧受付で残っていた証拠行が新しい保持で失われる場合や、保存行とメタデータの整合を保てない場合は、受付条件や保存の窓を付け替えず、6.9(11)に従いT4bを止めて差分を報告する。

### schema 4固定入力への移行（T4b）

6.9(7)の試験計画では、書き手の切替までにT1の`ReplayTrimCoverageBaselineTest`が使うschema 4入力の準備を、T4aの`ReplayTrimSchema4FrozenTest`と同じschema 4専用の固定入力に移す。切替後に既定の`SaveWriter`を通してschema 4を要求する経路を残さない。移すのは入力の準備だけで、元の各ケースのトリム範囲、schema、coverage、保存元のバイト一致の期待値を維持する。この移行は後続の試験計画であり、T1のテストはここでは編集しない。

### A4の休止時間とA9の保存時間（T5）

- A4の「7.6の手順3と同じ」は、休止・復帰・保存の操作と、PCから独立したストップウォッチで休止開始から復帰までの実時間を測る方法を指す。A4は30秒休止し、復帰後10秒で保存する。欠落の長さは実測の休止時間を基準に、原本どおり実測値−2秒以上、実測値+2秒+`SUSPEND_SPAN_SLACK`以下を検査する。7.6のT0-bでは同手順の約60秒の休止と復帰後60秒の待機を使う。
- A9はN=900秒で同一入力をT4bの切替前の版と切替後の版に与え、両版で同じ保存開始と完了の境界を使って所要時間を別々に採取する。速度の合格条件は切替後の時間が切替前の1.1倍以内。比較値が片方でも欠けたら速度は未実施とし、理由を記録する。MP4が900.0秒以下であることとcoverageの和は従来どおり保存物で検査する。
