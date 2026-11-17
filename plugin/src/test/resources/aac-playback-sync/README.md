# 再生PCM形式の合成対照入力

`stereo-44100.m4a`は、44.1kHz・stereo・16bitの純合成PCMから作成した8秒のAAC-LCです。毎秒の先頭100msだけ、左440Hz／右880Hz・振幅12000のsin波が鳴ります。実機録音・UI動画・私有fixture・ログは含みません。

- 生成：同梱`generate.py`（Python 3、FFmpeg 4.4.2-0ubuntu0.22.04.1 / native AAC encoder、96kbit/s）。FFmpegは素材作成時だけ使い、製品・テストのruntime依存にはしません。
- SHA-256：`d9af70242f563342b6d61b13dc55e11becd196db6f1add204b949dd85b7e53ef`
- `ReplayPlaybackAudioTest`が既存の合成48kHz mono入力（`aac-lc-escape/beeps.m4a`）と組み合わせ、Java Sound出力がmono／48kHzへ固定されないことを検証します。
- テストは出力lineを置換してPCM・形式・frame数・beep周期を検証します。実時間の出力latency・音質・IDEでの音画同期・各OSのaudio device受入を代替しません。

`mono-no-pns.m4a`は、48kHz mono・振幅16000・1kHz・毎秒100msの別の合成対照です。同じgeneratorでPNSだけを無効化し、packet内seek／gapの厳密なbyte比較に使います。PNSありの別decoder同士では復号開始packetや乱数履歴により正常なnoise値が変わるためです。既存PNS入力は変更せず、priming／周期検査に引き続き使用し、PNSの有限性・エネルギーは`aac-pns`で別に検査します。

- SHA-256: `240e4bfafaf7eb840ecc9fa7dceff534a10c6642102ecf4ee69a430bc0b9fedd`
- 従来のseek／gapのbyte一致・位置・sample数・thresholdは変更しません。seek時点1.025秒はbeepの途中なので、冒頭に残り75msの音を含む7beepを要求します。従来の6beep期待値は、この冒頭音声のPNS欠損を見逃していました。
