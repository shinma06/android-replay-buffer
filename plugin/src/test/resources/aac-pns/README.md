# PNSの有限性・音声欠損を検査する純合成AAC

`generate.py`で作る48kHz mono・250msのAAC-LCです。sourceの40〜140msだけ振幅12000・1kHzのsin波が鳴り、実機録音・個人情報は含みません。FFmpeg 4.4.2-0ubuntu0.22.04.1のnative AAC encoder、96kbit/sを用い、PNS設定だけを変えています。FFmpegは生成時のみ必要で、製品・回帰試験のruntime依存ではありません。

| 入力 | PNS | SHA-256 |
|---|---|---|
| pns.m4a | 有効 | c083b00bfae62dcc3b236bb701ad36a2c31abbbd24378275fc6b84fe12fb5264 |
| no-pns.m4a | 無効対照 | 49ec61712a76bc2f2715c5539a49bfef1aaef54e525f68d0933493ca4e5f4e6a |

どちらも13packetで、edit適用前のraw復号は13,312samplesです。1024samplesのAAC primingにsourceの開始1920samplesと端の除外480samplesを足したraw sample3424から、中央80msを固定して検査します。shift=0・gain=1のまま各1ms窓のRMS>4000、全体RMS8000〜9000、1kHzの周波数・エネルギー、長さ、非clipを確認します。最適shift/gainやFFmpegとの乱数波形のbit一致は要求しません。

JCodec 0.2.5の元ICStream（IQTableだけを修復した版も同じ）は、PNS乱数の更新でmultiply-addでなく定数和を乗じます。stateが0に固定され、0 energyの正規化からNaN→PCMの0化が発生します。`pns.m4a`は中央80個の1ms窓の64個が無音となって失敗し、PNS無し対照は全窓成功します。別途、decoderを繰り返し作り全packetのfloat出力が有限なことも確認します。

PNS無しは診断対照であり製品の機能無効化案ではありません。このcodec回帰だけで実Java Sound出力や実時間の映像・音声同期を合格にはしません。
