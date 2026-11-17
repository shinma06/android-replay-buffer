# AAC-LC escape上限の合成回帰入力

`beeps.m4a`は、48kHz・mono・16bitの純合成PCMから作成した8秒のAAC-LCです。毎秒の先頭100msだけ、振幅16000・1kHzのsin波が鳴ります。実機の録音、UI動画、ログ、個人情報は含みません。

- 生成：同梱`generate.py`（Python 3、FFmpeg 4.4.2-0ubuntu0.22.04.1 / native AAC encoder、96kbit/s）。FFmpegは生成時だけ使用し、製品・回帰試験のruntime依存ではありません。
- SHA-256：`4d92185c244f223893b203a70559e0de4e8517463475600d850abae55ec19b30`
- AudioSpecificConfig：`118856e500`、AAC-LC・48000Hz・mono。
- MP4のaccess unitは376個（encoder delayを含む）。packet index 50は213bytesで、escape符号の逆量子化値8191を含みます。
- 未修正JCodec 0.2.5ではpacket index 50で`Index 8191 out of bounds for length 8191`。修正後は全376個を順番に復号し、各1024sampleのPCMと非無音を確かめます。packetの破棄、無音差し替え、時刻短縮で回避しません。

この入力はcodec/MP4 packetの回帰用で、Android実機のAAC収録や実時間の映像・音声同期の受入を代替しません。
