/*
 * Copyright (C) 2018 Genymobile
 * Copyright (C) 2018-2026 Romain Vimont
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 *
 * Modified by Android Replay Buffer: replaces scrcpy 5.0.1 (a60891ae) AudioRecordReader at class-load time.
 * The PTS of each read buffer is the time of its first frame, derived from AudioRecord.getTimestamp()'s
 * (framePosition, nanoTime) pair, instead of the nanoTime of the latest captured frame.
 */
package com.genymobile.scrcpy.audio;

import android.media.AudioRecord;
import android.media.AudioTimestamp;
import android.media.MediaCodec;

import java.nio.ByteBuffer;

public class AudioRecordReader {
    // Same values as scrcpy 5.0.1 AudioConfig (48 kHz, stereo, 16-bit, 1024-frame reads).
    private static final int SAMPLE_RATE = 48000;
    private static final int FRAME_BYTES = 2 * 2;
    private static final int MAX_READ_SIZE = 1024 * FRAME_BYTES;
    private static final long ONE_SAMPLE_US = (1000000 + SAMPLE_RATE - 1) / SAMPLE_RATE;

    private final AudioRecord recorder;
    private final AudioTimestamp timestamp = new AudioTimestamp();
    private long framesRead;
    private long previousPts;
    private long nextPts;

    public AudioRecordReader(AudioRecord recorder) {
        this.recorder = recorder;
    }

    public int read(ByteBuffer outDirectBuffer, MediaCodec.BufferInfo outBufferInfo) {
        int r = recorder.read(outDirectBuffer, MAX_READ_SIZE);
        if (r <= 0) {
            return r;
        }
        long first = framesRead;
        framesRead += r / FRAME_BYTES;
        long pts;
        if (recorder.getTimestamp(timestamp, AudioTimestamp.TIMEBASE_MONOTONIC) == AudioRecord.SUCCESS) {
            // nanoTime is when frame framePosition was captured; this buffer starts at frame `first`.
            pts = (timestamp.nanoTime - (timestamp.framePosition - first) * 1000000000L / SAMPLE_RATE) / 1000;
        } else {
            if (nextPts == 0) {
                nextPts = System.nanoTime() / 1000;
            }
            pts = nextPts;
        }
        nextPts = pts + (r / FRAME_BYTES) * 1000000L / SAMPLE_RATE;
        if (previousPts != 0 && pts < previousPts + ONE_SAMPLE_US) {
            pts = previousPts + ONE_SAMPLE_US;
        }
        previousPts = pts;
        outBufferInfo.set(0, r, pts, 0);
        return r;
    }
}
