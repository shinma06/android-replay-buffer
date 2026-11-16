# H.264 range・行列の既知色 fixture

実録画・画面・ログを含まない純合成です。256×32、10fps、12frameで、各32pixel幅の
黒・白・灰64・灰128・灰192・赤・緑・青を並べています。入力はRGB24の
`000000 ffffff 404040 808080 c0c0c0 ff0000 00ff00 0000ff` です。

`generate.py` はRGB入力からFFmpeg 4.4.2 / libx264 core 163 r3060でオフライン生成しました。
BT.601で4:2:0化し、Constrained Baseline / QP1 / GOP4 / B-frameなしで符号化しています。
AUDでpacket境界を識別し、12frame中のIDRは0・4・8です。

| 入力 | video_signal_type_present_flag | video_full_range_flag | 値域 |
|---|---:|---:|---|
| limited.h264 | 1 | 0 | limited |
| full.h264 | 1 | 1 | full |
| default.h264 | 0 | 未記載 | H.264既定のlimited |

明示rangeの2本はcolour primaries / transfer / matrixもSMPTE 170M (6)を指定しています。
defaultには色指定がありません。H.264のVUI semantics（E.2.1）ではrange未記載はlimitedで、
JCodecの`YUV420J`というencoder入力名からfullと推定してはいけません。
JCodec 0.2.5のencoderはこの名前しか受け付けませんが、YUVのsample値域を変換せず、
既定のSPSにもfull rangeを記載しません。一方、decoderはfull指定でも`YUV420`と返します。
テストでは、これらのライブラリ内ラベルではなくSPSとsample値の組合せを確認します。

FFmpegによる先頭frameの独立参照復号（各色中央、RGB）は次の通りです。

| range | 黒 | 白 | 灰64 | 灰128 | 灰192 | 赤 | 緑 | 青 |
|---|---|---|---|---|---|---|---|---|
| limited / default | 0,0,0 | 255,255,255 | 64,64,64 | 128,128,128 | 192,192,192 | 253,0,0 | 0,255,0 | 0,0,254 |
| full | 0,0,0 | 255,255,255 | 64,64,64 | 128,128,128 | 192,192,192 | 254,0,0 | 0,254,1 | 0,0,254 |

`python3 generate.py` で再生成と参照値の確認ができます。FFmpegは生成・参照専用で、
製品・通常テスト・CIのruntime依存ではありません。encoder versionが異なるとbyte列は変わり得ます。
通常テストはチェックイン済みの小さな3本をJCodecで復号し、入力の既知RGBと各channel±3以内で比較します
（4:2:0と不可逆符号化、整数丸めを含むため）。PNGとpreviewの全pixel一致、先頭GOP再encodeと
後続コピー、再trim後のRGBおよび全sample entryのrangeも検証します。

既存trim-parity fixtureのSPSは元から明示full rangeです。映像・AAC・既存goldenは変更せず、
そのrangeも専用テストで固定しています。これはrangeとBT.601変換の回帰であり、HDR・他のmatrixや
色域・transferのカラーマネジメント、IDE描画、実機の受入、3OS byte一致の代替ではありません。

## 明示BT.709の追加回帰

`python3 generate.py --bt709-only` は既存3本を書き換えず、同じRGB入力から
`bt709-limited.h264` / `bt709-full.h264` だけを生成します。4:2:0化の行列とSPSの
primaries / transfer / matrixはBT.709 (1)です。FFmpeg独立参照の各色中央は次の通りです。

| range | 黒 | 白 | 灰64 | 灰128 | 灰192 | 赤 | 緑 | 青 |
|---|---|---|---|---|---|---|---|---|
| bt709-limited | 0,0,0 | 255,255,255 | 64,64,64 | 128,128,128 | 192,192,192 | 254,0,0 | 0,254,0 | 0,0,254 |
| bt709-full | 0,0,0 | 255,255,255 | 64,64,64 | 128,128,128 | 192,192,192 | 254,0,0 | 0,254,0 | 0,0,253 |

追加テストは、PNG・preview・再生cursorの既知RGB（各channel±3）、両range、
別playerとseekの交互実行、原本hash不変を検査します。直接YUVの回帰は浮動小数の独立式
（Kr=0.2126、Kb=0.0722）で入力を作り、整数変換のclippingも照合します。
BT.601・未指定・その他matrixの従来経路は既存変換とのbyte一致を検査し、
その他matrixの色が正しいという保証へは読み替えません。
これは表示用RGBの行列回帰であり、YUV再encode・packet・SPSや既存goldenは変更しません。
HDR・色域・transfer・包括色管理・IDE表示・実機の色精度は引き続き保証外です。
