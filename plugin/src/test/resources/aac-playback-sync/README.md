# 再生PCM形式の合成対照入力

`stereo-44100.m4a`は、44.1kHz・stereo・16bitの純合成PCMから作成した8秒のAAC-LCです。毎秒の先頭100msだけ、左440Hz／右880Hz・振幅12000のsin波が鳴ります。実機録音・UI動画・私有fixture・ログは含みません。

- 生成：同梱`generate.py`（Python 3、FFmpeg 4.4.2-0ubuntu0.22.04.1 / native AAC encoder、96kbit/s）。FFmpegは素材作成時だけ使い、製品・テストのruntime依存にはしません。
- SHA-256：`d9af70242f563342b6d61b13dc55e11becd196db6f1add204b949dd85b7e53ef`
- `ReplayPlaybackAudioTest`が既存の合成48kHz mono入力（`aac-lc-escape/beeps.m4a`）と組み合わせ、Java Sound出力がmono／48kHzへ固定されないことを検証します。
- テストは出力lineを置換してPCM・形式・frame数・beep周期を検証します。実時間の出力latency・音質・IDEでの音画同期・各OSのaudio device受入を代替しません。
