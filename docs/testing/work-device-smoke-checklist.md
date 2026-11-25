# 職場実機スモークチェック（約 2 分、都合の良いときに 1 回）

日常の作業は止めない。結果は「見たまま」をメモするだけでよい（失敗しても開発はブロックされない）。実機は USB 接続、`arbd` と IDE（または `arb-cli`）が使える PC で行う。

1. **USB 接続** — 実機を USB 接続して `arb-cli devices`。端末が `device` で出る。
2. **`boot_id` の由来** — `arb-cli --json status` で `boot_id` の取得元（source）を確認。メモ: ______
3. **静止画面 vs 切断** — 画面を静止させて 10 秒録画し、その後 USB を抜く。静止は「欠損」ではなく継続として扱われ、切断だけが途切れ区間になること。
4. **音声再生キャプチャ** — 音の出る動画を再生して 10 秒録画 → 保存。再生して映像と音のズレが目標 0.1 秒以内に感じるか（主観で OK / 気になる）。
5. **GOP 途中トリム** — 保存したバンドルで途中のフレームから `arb-cli trim <bundle> --in-frame <n> --out-frame <m>`。結果が再生でき、開始前の映像が混ざらない。
6. **ノート PC の蓋閉じスリープ** — 録画中に蓋を閉じて約 30 秒待ち開く。スリープ分の時間差が不連続区間として記録され、時刻がずれていない。
7. **Running Devices ミラーリング＋ARB 同時録画（約 30 秒）** — Android Studio の Running Devices で実機をミラーリングした状態のまま ARB で録画を開始・10 秒後に保存する。端末の H.264 HW エンコーダ競合が起きた場合は、ARB が `MediaCodec` 競合ヒント（`video_max_size=1024` または `logs_only` モードへの切替案内）を明示すること、かつ `arb-cli capture-start --mode logs_only` でゼロ動画負荷のログ常時録画＋保存（`app-logs/<package>.jsonl` 自動抽出含む）が正常動作することを確認。メモ: 同時録画 OK / 1024px で OK / `logs_only` のみ OK: ______
8. **Wi-Fi ADB 接続＋複数端末の自動切替禁止（約 30 秒）** — ワイヤレスデバッグ（`192.168.x.x:5555` または `adb-..._adb-tls-connect._tcp`）で `arb-cli devices` に出ることを確認し、対象シリアル指定で `arb-cli capture-start --serial <WIFI_SERIAL>` → 保存できること。また USB＋Wi-Fi 等で 2 台オンラインのとき未指定では自動開始されず選択待ちになり、選択済み端末を切断しても別端末へ勝手に切り替わらないことを確認。
9. **既定方式での Android 13（API 33）以上の音声（約 1 分）** — `adb shell getprop ro.build.version.sdk` が 33 以上の端末で、音の出る動画を再生しながら録画を開始する。`arb-cli --json status` の `capture.audio_state` が `available`、`capture.audio_cause` が `none` になり、保存したバンドルに音声が入ること。`unavailable` になった場合は「音声キャプチャ非対応」と表示されないこと（この表示は API 32 以下専用）を確かめ、`audio_cause`（`device_capture_failed` / `encoder_fatal` など）と `audio_reason` の `scrcpy stderr:` 以降をメモする。Running Devices のミラーリングを止めて再度試し、結果が変わるかも書く。`encoder_fatal` の後は再接続後に映像とログの取得が再開し、`audio_cause` が `runtime_sticky_disabled` のまま続くこと。メモ: 端末名 / API / `audio_cause` / ミラーリング停止で改善したか: ______

10. **出力キャプチャ互換（Android 11 / API 30 以上）** — 音を再生し、既定方式でのスピーカー出力を確認する。Settings で消音の説明を読んで「出力キャプチャ互換」を選び、再接続後の `capture.audio_capture_mode = output_compat`、録音中の端末消音、保存した動画の音声を個別に確認する。録画を停止して端末の音が戻ること、再び `playback_dup` に戻せることを確認する。Posiflex BT-2000 での成否は実測して記入する。結果: ______

実機の録画・logcat は公開しないこと。
