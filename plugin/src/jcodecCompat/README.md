# JCodec 0.2.5・JAAD互換補修

buildで検証する元JCodec 0.2.5のpinとSHA-256は変更しません。archive内のIQTableとICStreamだけを同名の一classずつに置き換え、他のclassは元JARのまま保持します。新しいruntime依存はありません。

- `IQTable.java`は既存0..8190のfloat bitsを変えず、AAC escape上限8191の欠けた要素だけを補います。
- `ICStream.java`は同pinのsource JARから取得した原文です。変更はPNS乱数更新の一行だけで、`randomState *= 1664525+1013904223`を`randomState = randomState*1664525+1013904223`へ修正します。32bit intのwrapを保ち、PCMのgain/offset補正・NaNの無音化・packet破棄はしません。

ICStreamの取得元:

- https://repo.maven.apache.org/maven2/org/jcodec/jcodec/0.2.5/jcodec-0.2.5-sources.jar
- source JAR SHA-256: `9945efa92e16b3fbe8eb8bf37f00be53fa0adb52d09ac978a0744ca2ef3697db`
- entry: `net/sourceforge/jaad/aac/syntax/ICStream.java`

原sourceのauthor・Public Domain／JCodec変更分FreeBSD licenseのheaderを保持しています。bytecodeの既存IQTable参照は同梱の修復tableへ解決されます。小さい純合成のPNS有効／無効対照はtest resourcesの`aac-pns`にあり、外部FFmpegは生成時だけ使用します。

JCodecのcopyright・再配布条件・免責事項は原文の`LICENSE-JCodec`を保持し、修復JARの`META-INF/LICENSE-JCodec`にも同梱します。取得元: https://github.com/jcodec/jcodec/blob/567c13f163a4c1ec82146bac384178c1c6fd2624/LICENSE
