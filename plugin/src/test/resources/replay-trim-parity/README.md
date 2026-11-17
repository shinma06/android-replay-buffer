# トリム・PNGの3OS出力一致 fixture

実端末の録画・ログを含まない合成入力です。`ReplayTrimTest` の
`rawSidecarIsRegeneratedForKeptSamples` と同じ H.264 生成方法を使い、
64×32 / 32×64 の2区間を64×64に正規化した24コマと元H.264を保存しています。
ログとタップには範囲内・範囲外・時刻不明の行を含めています。

## AACの来歴

音声は `ReplayPlaybackTest.bundledJaadDecodesAacLcToPcm` にある合成440Hz正弦波の
6個のraw access unitを順番に繰り返したものです。実録音は含みません。
AudioSpecificConfigは `1188`（AAC-LC / 48000Hz / mono）、1packetは1024sampleです。
`1fbaf70` の同名fixtureの映像sample・sample entry・edit listをJCodec 0.2.5で
そのままremuxし、`SaveWriter` と同じ `MP4MuxerTrack` / `AudioSampleEntry` /
`EsdsBox` 経路で80個の音声packetを追加しました。映像・H.264・JSONLは変更していません。
音声のmovie timescaleは1000000、media timescaleは48000です。

| segment | movie開始µs | durationµs | media開始sample | packet数 |
|---|---:|---:|---:|---:|
| 1 | 0 | 640000 | 0 | 30 |
| gap（empty edit） | 640000 | 460001 | -1 | 0 |
| 2 | 1100001 | 1066666 | 30720 | 50 |

音声trackのdurationは2166667µs、movie全体は2200002µsです。
`replay.json` の音声設定・coverage・segmentsをこの表に合わせ、READMEとMP4の
`files_sha256` を再計算しています。生成にもテストにも外部ffmpegは不要です。

`ReplayTrimParityTest` はこの同一入力を200001〜1900001µsで切り取り、
先頭GOP再エンコード・後続コピー・AAC・side data・先頭コマPNGの出力SHA-256を
全OS共通の `output.sha256` と照合します。通常の `check` から呼ばれるため、
Linux `test` とmacOS / Windows `os-parity` で同じ比較が行われます。

開始200000µsはAAC packetの内部（384sample目）です。音声gapを残し、
その出力をさらに100001〜1500001µsで切り取ると開始は64sample目になります。
再トリムしたMP4全体（`retrim-video.mp4`）と `audio_track` 全体のJSON
（`retrim-audio.json`）も同じgoldenに照合します。両段階でpacketの採否、
MP4のedit listとmanifestの一致、音声coverage、同梱JAADでの復号を確認します。
再トリムの音声以外のside dataはgolden比較の追加対象ではありません。

AAC overlap維持のため、trimは提示segmentの途中で切るときに直前1packetを
コピーし、editと`media_start_samples`で隠します。既に隠れprefixを持つ出力の再trimでは
そのpacketを再利用します。`samples`は提示に必要なpacket数の
ままで、隠れprefixは含みません。この入力のtrimは元packet 8〜29と30〜67、再trimは
13〜29と30〜58を格納します。最初の提示開始はそれぞれ1408 / 1088sample、後半segmentの
開始は22528 / 17408sampleです。後半segment先頭には前segment由来のprerollを足しません。
元packetのbyte比較とedit/manifest検査を維持し、この構造差から派生するMP4/JSONの
4個のgolden hashだけを更新しています。映像、PNG、ログ、タップ、入力fixtureは不変です。

`ReplayTrimAudioPrerollTest`は既存`aac-playback-sync/mono-no-pns.m4a`の先頭30 / 50packetを
独立segmentとしてremuxし、同じ合成IDRを切り取り境界に配置します。preflightの実際の
frame境界が要求範囲と一致することを検査してから、segment先頭からの連続PCMをoracleに
packet内部・packet境界・seek・segment先頭・隠れprefixの再trimをbyte比較します。
保存packetの直接復号＋edit適用と、fake Java Sound出力による実player経路の両方を検査します。
PNSの乱数履歴はこの厳密比較に混ぜず、既存のPNS/escape復号テストはそのまま残します。

既知の制約: schema 4の提示segmentはAAC sessionの識別を保存しません。現playerは
別segmentの直前packetもwarm-upするため、独立encoderのsegment先頭はcold decodeと
一致しないことがあります。今回はplayer・その境界動作を変更せず、この差を非skipの
characterization testで第1packetに局在する既知制約として記録します。gapをまたぐtrim／
再trimでは元playerのPCMを維持することを比較し、独立segmentのresetを直したとは扱いません。
同じAAC streamを2msの映像holeで分けた対照は連続PCM・元player・trim・再trimが一致する
ことを確認します。提示segmentをdecoder resetと見なす案はこの対照を壊すため不採用です。
AAC session／decode-run情報の後方互換設計は別タスクであり、この変更へ混ぜません。

生成IDとMP4の作成・更新日時（およびその日時由来のMP4ハッシュ）以外は除外しません。
PNGの`arb:source_save_id`は直のmanifest IDとの一致とchunk CRCを検証してから、
manifestと同じ`generated-id`へ正規化しCRCだけ再計算します。新IDのchunkだけを除いた
PNGが従来のhashと一致することも確認し、画像や既存metadataは正規化しません。
改行、JSONの項目順、PNGの圧縮結果も比較対象です。入力のハッシュも検証し、
切り取り後に元ファイルが変わっていないことを確認します。
このfixtureは単一音声形式・合成波形に限り、実端末音声や実時間の音ずれの受入ではありません。
goldenをLinuxで更新しただけでは3OS一致とはせず、CIの3OS完走で判定します。
