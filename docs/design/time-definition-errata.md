# 時間の定義・正誤表（T1）

<a id="scope"></a>
## 適用範囲と優先順位

本書は、承認済み原本を保持して、次の3点だけを訂正する規範文書である。後続の子セッションは、原本に定めた着手前の文書に加えて本書も読む。該当する式・型・試験・保存例については、**本書を原本とtimelineの転載より優先する**。ほかの事項、別途提示されたオーナー判断、各Trailの順序と関門は変更しない。

| 固定原本 | SHA-256 |
| --- | --- |
| [方針書・改訂4](time-definition-policy.md) | `3e29f94cad5b55beec7adceafbfe3450266aa8312da5aaaa12c0c0bcb7268e20` |
| [実装仕様書・第2版](time-definition-impl-spec.md) | `b70707e8fcb8161f035a8ecd340552d36aeae62b624def77a2911c267ab9d7e7` |

以下の行番号はこのSHAの原本に対するもの。原本のバイトは変更しない。本T1で追加するのは本書と参照リンクだけで、本番コード、既存・新規ベースライン、schema 4の動作・出力・数え方、自動保存の受付は変更しない。以下の型・保存形・試験は後続Trailの実装契約であり、本T1の実装済み機能を意味しない。

時刻・区間長はすべて整数ns。例の「秒」は説明用の換算であり、JSONのnsは十進文字列とする。位置の誤差範囲は閉区間、coverageの長さを数える区間は半開区間。`null`の下限は負の無限大、`null`の上限は正の無限大を意味し、未知を0に置換しない。`0 ≤ ρ < 1`を前提に、ρによる誤差の拡大は外向きに切り上げる。

<a id="e1-candidates"></a>
## E1 推定ログの時計区間の候補をすべて含める

対象: finding `01M7MA4YJ3PBCA9XNDPMBRCRRB`。実装仕様書4.4の317行の「同じ時計区間で…最も近い有効なサンプル…」という単一offset・単一範囲の規則を、以下で置き換える。関連箇所は同4.2（260〜263行）、4.3（267〜290行）、4.4（314〜324行）、方針書4.3の推定等級（205行）。

### 反例

epochログの元時刻がwall=90秒、測定済みの段差の前後で`D − wall`が10秒と20秒なら、Dの候補は100秒と110秒になる。段差をD=105秒で既に観測し、最新の有効な測定がH=108秒・D=108秒・wall=88秒、受信H=114秒なら、現行の5秒規則では不明となり、将来の等級は推定になる。受信区間のoffsetだけを選ぶと、読取誤差1msと500ppm×6秒で110秒±4msとなり、100秒を除いてしまう。両候補は受信より前なので、因果関係の上限だけでは一方に決められない。これは324行の「まだ測定されていない段差」の制限とは別である。

### 候補と原点

保存要求Sで凍結した測定・錨を用い、**記録自身の等級がestimatedの場合だけ**次の候補列挙を行う。現行の`sequence`が非nullの記録の換算、等級、値、誤差をこの列挙で置き換えない。追加候補を理由にmeasuredへ昇格させたり、現行のmeasuredを降格させたりしない。終点の等級と最終行等級の悪い方を取る規則も変更しない（I6）。

候補は少なくとも次をひとまとまりで持つ。単位や原点の識別子は数値と分離しない。

```text
Candidate = (sourceDomain, bootSegment, AnchorId, segmentOffset,
             sourceSample, D_lo, D_hi)
AnchorId = (region, suspendIndex)                 // 原本4.3
sourceDomain = epoch または monotonic             // 系の生時刻を混ぜない
D = bootSegment固有のBOOTTIME                    // seqではない
seq = D + その候補のsegmentOffset
```

1. epoch時刻`v`について、同じ取得接続であること等からbootを特定できるならそのbootだけ、特定できないなら否定できないbootごとに列挙する。そのbootの**既知の全時計区間**の有効なサンプルを候補元とする。MONOTONIC系にも同じ原点管理を使う。受信時のregionと異なること、古いregionであること、現行の換算がstale・境界・曖昧さによりnullであることは、候補を捨てる理由にしない。
2. サンプル`j`ごとに`offset_j = elapsed_j − source_j`、`q_j = v + offset_j`、`e_j = readError_j + ceil(ρ × |受信H − received_j|)`、`D範囲_j = [q_j − e_j, q_j + e_j] ∩ [0, +∞)`を作る。原本の「最も近い1件」に絞らず、同じregion内も含めて全候補を残す保守的な規則とする。対応するregionの各既知AnchorIdを候補の錨として列挙する（PC休止はregionを変えずAnchorIdを変える）。行に関する既知候補の根拠となる測定・錨は、保持中の行とともに保持するか、同じ候補範囲を復元できる要約を残す。
3. **受信Hをイベント発生Hと同一視しない。** 区間Aの錨が発生H=`x`について返すelapsed系の範囲を`[L_A(x), U_A(x)] = [lo'_A(x), hi'_A(x)]`とする。4.3の最小形との合成・矛盾時の退避をそのまま用いる。候補の可能な組は次の集合で定める。

   ```text
   F_A = { (x, D) | x ≤ 受信H,
                     D_lo ≤ D ≤ D_hi,
                     L_A(x) ≤ D − x ≤ U_A(x) }
   ```

   `U_A(x)`が無い場合は右側の制約を置かず、elapsed系の`L_A(x)`も持たなければ左側の制約も置かない。生epoch系のfloorをelapsed系のθに代用しない。この最小形では、段差の発生Hや錨の有効期間を推測した追加の時間制約を置かない（段差の最初の検出Hは発生Hそのものではない）。`F_A`が空の候補だけを除く。この集合のD方向・H方向への射影の下限・上限を、以降の範囲として使う。絶対値とρの式は区分的な一次不等式なので、境界を外向きに丸めて解く。独立なサンプルの錨範囲を交差して狭める処理には変更しない。
4. 原本317行の因果上限`受信H + hi'(受信H)`を使う場合も**候補自身のelapsed系の錨**を使う。そこへ別候補のsegmentOffsetを混ぜない。最終的には3の`x ≤ 受信H`を満たす組を含む範囲を残す。補助候補の代表値を「既知のelapsed」とみなして最小形へ戻してはならない（自分で選んだoffsetを根拠に他の候補を消してしまうため）。

この列挙は、既知の対応から否定できない候補をすべて包むためのもの。真の候補以外も残り得る。受信区間の1候補に狭めた確度を表示してはならない。候補集合が空になった場合は点や原点を捏造せず、原本0.6に従って停止・報告する。

### 各候補をtへ変換してから包絡する

先に異なるseqやθの数値をmin/maxしてはならない。候補ごとに原本4.2の同じ錨／別の錨の分岐を守る。

- **保存終点と同じAnchorId**: `F_A`のD射影にそのbootのsegmentOffsetを加え、`[d_lo, d_hi]`を作る。`T_S`も同じseq原点であることを確認して、`t_lo = W − T_S_hi + d_lo`、`t_hi = W − T_S_lo + d_hi`とする。`T_S_hi`が無ければ`t_lo = null`。同一の組から得たDとHの相関は、この包絡で失ってよい（範囲が広がるだけである）。
- **異なるAnchorId**（同bootの段差・休止を含む）: `F_A`のH射影`[h_lo, h_hi]`を用いて、`[t_lo, t_hi] = [W − S + h_lo, W − S + h_hi]`。ここには終点の別錨のθやsegmentOffsetを足さない。旧bootのseqを新bootの`T_S`から直接引かない。
- 最終的な位置範囲は、全候補のt範囲の**包絡**`[min(t_lo), max(t_hi)]`とする。どれかの下限が無ければ下限はnull。候補間の交差は取らない。このestimated記録の点`t_ns`は、有限な包絡なら`lo + floor((hi − lo)/2)`、下限なしなら有限な`hi`とする。これは候補を一意に識別したという意味ではない。点は必ず宣言した範囲内とする。measured記録の点・誤差は原本どおりであり、この代表点の選び直しの対象外である。

反例でDとHの中心の差を0、S=114秒、W=30秒とすれば、候補の中心位置は16秒と26秒であり、最終範囲は両方を含む。異なるbootのsegmentOffsetが数千秒違っても、各候補を正しい錨でHへ戻してからtに置くので、その差を遅延や誤差幅として足さない。

**変更しない限界**: サンプルでまだ観測していない端末時計の段差に対して、真値包含を保証するものではない（原本324行）。ρの仮定と測定済み対応に基づく範囲である。凍結後の新測定で、候補・位置・等級を変更しない。

### 後続Trailと試験

- T2: 影の候補列挙・原点を持つ範囲・tでの包絡を実装する。`estimatedBackwardWallStepKeepsBothCandidates`（上の100/110秒と4msの反例）、`candidateCausalityUsesItsOwnAnchor`、`differentAnchorsConvertThroughHostBeforeHull`（異なるsegmentOffsetも使用）、`estimatedPointIsInsideHull`、`knownCandidatesDoNotChangeMeasuredRows`を加える。5秒ちょうど／+1ns、20msちょうど／+1nsとI6の既存比較は保持する。
- T4a: T2と同じ計算規則に従う候補の広いt範囲を持つfixtureを読み、点と範囲を変更せず保持する。読み手がoffsetを選び直さない。
- T4b: T2の凍結した点・範囲・等級を保存する。推定位置を再計算して狭めず、trimでは各位置・有限な範囲の両端を同じ量だけ移す。

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

この保存規約はE1のestimated記録が終点との組合せで最終arrivalになる場合にも適用する。候補列挙・内部範囲を変えず、下限の出力だけをnullにする。所属判定には内部区間に原本4.2・6.6の規則を適用し、保存されたnullから「内部にも有限下限が無かった」と逆推定しない。読み手が保存物からその内部下限を推測復元することもない。

全等級について、**有限の点が内部範囲および宣言した出力範囲の中にあること**を検査する。最終arrivalは下限を常にnullとし、点・上限は内部の値を保持する。ほかの等級は内部の上下限をそのまま出す。measuredの点・誤差とI6、MP4の表示範囲を`[0,W]`に切る規則、coverage区間を`[0,W)`に切る規則は変更しない。内部の所属判定、下限を主張しない保存規約、表示・長さの切り取りは別の操作である。

### 上限を証明できる前提

321行の`t = t_latest ≤ W`は、次の条件を**すべて**満たす生時刻系のarrivalの構成について検査する。

1. 記録と終点が同じAnchorId、同じboot、同じ生時刻系に属し、両方ともその系の到着基準の構成を使う。別錨・別系・別bootの数値を引かない。まだ測定されていない段差等で同じ系の連続性の仮定が破れていない。
2. 記録`i`の生時刻`v_i`の換算幅は0で、独立に拡大したmeasured／estimatedの`d_hi`を流用しない。記録の受信Hを`h_i`とし、凍結した対象の全行で`h_i ≤ S`。
3. 同じ系の最小形`f(S)`が、その記録による下限を保っていること、すなわち`f(S) ≥ v_i − h_i − ceil(ρ × (S − h_i))`を満たす。終点の点と下限にはともに`S + f(S)`を用い、上限は無い。別の錨やサンプルのoffsetをこの式へ足さない。

このとき、整数nsで`(S + f(S)) − v_i ≥ (S − h_i) − ceil(ρ × (S − h_i)) ≥ 0`なので、`t_i = t_latest_i = W − ((S + f(S)) − v_i) ≤ W`、`t_earliest_i = null`となる。受信がSちょうどなら上限の等号も許す。この証明の条件を持たない行に、ラベルarrivalだけを根拠にこの上限を適用しない。

### 後続Trailと試験

- T2: 670行(e)を「上の前提を持つarrival構成で`t == t_latest ≤ W`。一般の行では点が内部・出力範囲内」に置換する。`measuredAtEndKeepsLatestBeyondWindow`（±1msずつ、W+2ms）、`mixedArrivalGradeDoesNotImplyLatestWithinWindow`（記録measured/estimatedの両方）、`otherAnchorUsesHostBounds`、`rawArrivalBoundRequiresSameSourceAndFloor`を追加する。`arrivalHasNullEarliest`は維持し、`everyFinalArrivalSerializesNullEarliest`で記録／終点の等級全9組、同じ錨／別錨、内部下限が有限／不明の場合を検査する。最終arrivalなら例外なく出力下限はnullであり、内部の有限下限・点・上限は変わらない。
- 境界は整数nsで検査する。生時刻arrivalは`h_i = S`／`S−1ns`、ρ=0／500ppm、同じ系のfloorへの自己の寄与を保持した場合の等号を含める。一般範囲は`t_latest = W`／`W+1ns`／`W+2ms`、内部下限0／−1ns、点と両端の等号、片側nullを含める。原本の内部区間による所属判定（上限0は窓内、−1nsは窓外、下限Wは窓内、W+1nsは窓外）も保持する。内部下限がW+1nsの最終arrival行を窓外と判定しても、出力下限を数値へ変更しない。
- T4a: 窓外にはみ出す誤差と混合arrivalのfixtureを読む。measuredのW+2ms、最終arrivalのW+1msの上限を保持し、arrivalのfixtureは内部で有限下限を知り得るケースも含めて必ず`t_earliest_ns=null`とする。読み手はそのnullを保持し、0や推測値を入れない。
- T4b: JSONLは上の保存規約に従い、最終arrivalの全行で`t_earliest_ns=null`を検査する。trimでは点と有限な上下限からaを引き、arrivalのnull下限はそのまま保持する。窓外への上限のはみ出しや、ほかの等級の有限下限は新しい窓でも切り詰めない。内部所属判定と保存の区別、直接保存・読み取り・繰り返しtrimでのarrival下限nullの維持を試験する。MP4・coverageの窓内切り取りとは分ける。
