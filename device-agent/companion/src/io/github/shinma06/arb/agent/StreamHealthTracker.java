package io.github.shinma06.arb.agent;

import java.util.ArrayList;
import java.util.List;

/**
 * Thread-safe per-stream health registry reported in every Stream 0 {@code Heartbeat}.
 */
public final class StreamHealthTracker {
    private static final int MAX_STREAMS = 5;

    private final int[] states = new int[MAX_STREAMS];
    private final int[] restarts = new int[MAX_STREAMS];
    private final long[] emittedSeq = new long[MAX_STREAMS];
    private final long[] lastPtsNs = new long[MAX_STREAMS];
    private final boolean[] active = new boolean[MAX_STREAMS];

    public StreamHealthTracker() {
        register(Protocol.STREAM_CONTROL, Protocol.STATE_STARTING);
        register(Protocol.STREAM_LOGCAT, Protocol.STATE_STARTING);
        register(Protocol.STREAM_INPUT, Protocol.STATE_STARTING);
    }

    public synchronized void register(int streamId, int initialState) {
        if (streamId < 0 || streamId >= MAX_STREAMS) {
            return;
        }
        active[streamId] = true;
        states[streamId] = initialState;
    }

    public synchronized int setState(int streamId, int state) {
        if (streamId < 0 || streamId >= MAX_STREAMS) {
            return 0;
        }
        active[streamId] = true;
        states[streamId] = state;
        return restarts[streamId];
    }

    public synchronized int incrementRestart(int streamId) {
        if (streamId < 0 || streamId >= MAX_STREAMS) {
            return 0;
        }
        active[streamId] = true;
        restarts[streamId] = (restarts[streamId] + 1) & 0xFFFF;
        return restarts[streamId];
    }

    public synchronized void recordEmit(int streamId, long seq, long ptsNs) {
        if (streamId < 0 || streamId >= MAX_STREAMS) {
            return;
        }
        active[streamId] = true;
        long masked = seq & 0xFFFFFFFFL;
        if (masked != 0L && masked > emittedSeq[streamId]) {
            emittedSeq[streamId] = masked;
        }
        if (ptsNs > 0 && ptsNs > lastPtsNs[streamId]) {
            lastPtsNs[streamId] = ptsNs;
        }
    }

    public synchronized long getEmittedSeq(int streamId) {
        if (streamId < 0 || streamId >= MAX_STREAMS) {
            return 0;
        }
        return emittedSeq[streamId];
    }

    public synchronized int getRestartCount(int streamId) {
        if (streamId < 0 || streamId >= MAX_STREAMS) {
            return 0;
        }
        return restarts[streamId];
    }

    public synchronized List<PayloadCodec.StreamHealthEntry> snapshot() {
        List<PayloadCodec.StreamHealthEntry> out = new ArrayList<>(MAX_STREAMS);
        for (int i = 0; i < MAX_STREAMS; i++) {
            if (active[i]) {
                out.add(new PayloadCodec.StreamHealthEntry(
                        i, states[i], restarts[i], emittedSeq[i], lastPtsNs[i]));
            }
        }
        return out;
    }
}
