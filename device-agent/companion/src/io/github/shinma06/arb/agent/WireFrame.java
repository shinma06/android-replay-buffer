package io.github.shinma06.arb.agent;

import java.io.EOFException;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.locks.Condition;
import java.util.concurrent.locks.ReentrantLock;

/**
 * 20-byte Little-Endian frame header and atomic frame reader/writer.
 *
 * <pre>
 * Offset 0..2  : magic ("AR" = 0x41, 0x52)
 * Offset 2..3  : version (u8 = 1)
 * Offset 3..4  : stream_id (u8)
 * Offset 4..5  : msg_type (u8)
 * Offset 5..6  : flags (u8)
 * Offset 6..8  : reserved (u16 LE = 0)
 * Offset 8..12 : seq (u32 LE)
 * Offset 12..20: payload_len (u64 LE)
 * </pre>
 */
public final class WireFrame {
    public final int version;
    public final int streamId;
    public final int msgType;
    public final int flags;
    public final int reserved;
    public final long seq; // unsigned 32-bit held in low bits
    public final byte[] payload;

    public WireFrame(int version, int streamId, int msgType, int flags, int reserved, long seq, byte[] payload) {
        this.version = version & 0xFF;
        this.streamId = streamId & 0xFF;
        this.msgType = msgType & 0xFF;
        this.flags = flags & 0xFF;
        this.reserved = reserved & 0xFFFF;
        this.seq = seq & 0xFFFFFFFFL;
        this.payload = payload != null ? payload : new byte[0];
    }

    public static WireFrame of(int streamId, int msgType, int flags, long seq, byte[] payload) {
        return new WireFrame(Protocol.VERSION, streamId, msgType, flags, 0, seq, payload);
    }

    public byte[] toBytes() {
        byte[] out = new byte[Protocol.HEADER_SIZE + payload.length];
        ByteBuffer buf = ByteBuffer.wrap(out).order(ByteOrder.LITTLE_ENDIAN);
        buf.put(Protocol.MAGIC_0);
        buf.put(Protocol.MAGIC_1);
        buf.put((byte) version);
        buf.put((byte) streamId);
        buf.put((byte) msgType);
        buf.put((byte) flags);
        buf.putShort((short) reserved);
        buf.putInt((int) (seq & 0xFFFFFFFFL));
        buf.putLong(payload.length);
        if (payload.length > 0) {
            buf.put(payload);
        }
        return out;
    }

    /**
     * Reads one complete frame from {@code in}, returning {@code null} on clean EOF before the first byte.
     */
    public static WireFrame readFrom(InputStream in) throws IOException {
        byte[] hdr = new byte[Protocol.HEADER_SIZE];
        int first = in.read();
        if (first < 0) {
            return null;
        }
        hdr[0] = (byte) first;
        readFully(in, hdr, 1, Protocol.HEADER_SIZE - 1);

        if (hdr[0] != Protocol.MAGIC_0 || hdr[1] != Protocol.MAGIC_1) {
            throw new IOException(String.format(
                    "Invalid frame magic: 0x%02x 0x%02x", hdr[0] & 0xFF, hdr[1] & 0xFF));
        }

        ByteBuffer buf = ByteBuffer.wrap(hdr).order(ByteOrder.LITTLE_ENDIAN);
        buf.position(2);
        int version = buf.get() & 0xFF;
        if (version != Protocol.VERSION) {
            throw new IOException("Unsupported protocol version: " + version);
        }
        int streamId = buf.get() & 0xFF;
        int msgType = buf.get() & 0xFF;
        int flags = buf.get() & 0xFF;
        int reserved = buf.getShort() & 0xFFFF;
        long seq = buf.getInt() & 0xFFFFFFFFL;
        long payloadLen = buf.getLong();
        if (payloadLen < 0 || payloadLen > Protocol.MAX_PAYLOAD_LEN) {
            throw new IOException("Invalid payload length: " + payloadLen);
        }

        byte[] payload = new byte[(int) payloadLen];
        if (payloadLen > 0) {
            readFully(in, payload, 0, (int) payloadLen);
        }
        return new WireFrame(version, streamId, msgType, flags, reserved, seq, payload);
    }

    public static void readFully(InputStream in, byte[] dst, int off, int len) throws IOException {
        int pos = 0;
        while (pos < len) {
            int n = in.read(dst, off + pos, len - pos);
            if (n < 0) {
                throw new EOFException("Unexpected EOF after " + pos + " of " + len + " bytes");
            }
            pos += n;
        }
    }

    /**
     * Non-blocking prioritized frame writer with per-stream monotonic sequence numbers.
     *
     * <p>Producers enqueue frames into per-stream bounded queues; a single dedicated
     * {@code arb-writer} thread drains them, always preferring Stream 0 (Control/Beacon:
     * {@code Hello}, {@code Heartbeat}, {@code Pong}, {@code Shutdown}) over Stream 1/2 so the
     * beacon never waits behind log bursts or host back-pressure (PROTOCOL.md section 4).
     *
     * <p>When the Stream 1 (Logcat) queue is full, non-urgent {@code LogRecord} frames are
     * dropped instead of blocking, {@link #writeFrame} returns {@code 0} (never a valid seq),
     * and a {@code LogOverflowStats(OverflowDetected, "writer_queue_overflow:dropped=N")} frame
     * is queued on Stream 1 as soon as there is room. Urgent frames ({@code FLAG_URGENT}) on
     * Stream 1/2 and all Stream 2 frames apply back-pressure only to their own producer thread.
     *
     * <p>Transport failures are recorded by the writer thread and re-thrown as
     * {@link IOException} from every subsequent {@link #writeFrame} call, so the heartbeat loop
     * notices a dead host immediately.
     */
    public static final class FrameWriter {
        public static final int DEFAULT_CONTROL_CAPACITY = 256;
        public static final int DEFAULT_DATA_CAPACITY = 4096;
        private static final int STREAM_SLOTS = 8;

        private final OutputStream out;
        private final ClockSource clock;
        private final long[] nextSeq = new long[STREAM_SLOTS];
        private final int[] capacity = new int[STREAM_SLOTS];
        private final List<ArrayDeque<byte[]>> queues = new ArrayList<>(STREAM_SLOTS);
        private final ReentrantLock lock = new ReentrantLock();
        private final Condition notEmpty = lock.newCondition();
        private final Condition changed = lock.newCondition();
        private final Thread writerThread;

        private long droppedLogFramesPending;
        private long droppedLogFramesTotal;
        private int inFlight;
        private boolean closed;
        private IOException failure;
        private int roundRobin;

        public FrameWriter(OutputStream out) {
            this(out, null, DEFAULT_CONTROL_CAPACITY, DEFAULT_DATA_CAPACITY);
        }

        public FrameWriter(OutputStream out, ClockSource clock) {
            this(out, clock, DEFAULT_CONTROL_CAPACITY, DEFAULT_DATA_CAPACITY);
        }

        public FrameWriter(OutputStream out, ClockSource clock, int controlCapacity, int dataCapacity) {
            this.out = out;
            this.clock = clock;
            for (int i = 0; i < STREAM_SLOTS; i++) {
                queues.add(new ArrayDeque<>());
                capacity[i] = (i == Protocol.STREAM_CONTROL)
                        ? Math.max(1, controlCapacity)
                        : Math.max(2, dataCapacity);
            }
            writerThread = new Thread(this::drainLoop, "arb-writer");
            writerThread.setDaemon(true);
            writerThread.start();
        }

        /**
         * Enqueues one frame and returns its assigned per-stream seq, or {@code 0} when a
         * non-urgent Stream 1 frame was dropped because the Stream 1 queue is full.
         */
        public long writeFrame(int streamId, int msgType, int flags, byte[] payload) throws IOException {
            int idx = (streamId >= 0 && streamId < STREAM_SLOTS) ? streamId : 0;
            boolean droppable = idx == Protocol.STREAM_LOGCAT && (flags & Protocol.FLAG_URGENT) == 0;
            lock.lock();
            try {
                while (true) {
                    throwIfUnusable();
                    ArrayDeque<byte[]> q = queues.get(idx);
                    if (idx == Protocol.STREAM_LOGCAT && droppedLogFramesPending > 0) {
                        if (q.size() + 2 <= capacity[idx]) {
                            enqueueOverflowNoticeLocked();
                        } else if (droppable) {
                            return dropLocked();
                        }
                    }
                    if (q.size() < capacity[idx]) {
                        long seq = assignSeqLocked(idx);
                        q.addLast(WireFrame.of(streamId, msgType, flags, seq, payload).toBytes());
                        notEmpty.signal();
                        return seq;
                    }
                    if (droppable) {
                        return dropLocked();
                    }
                    // Back-pressure only on this producer's own stream queue.
                    changed.awaitUninterruptibly();
                }
            } finally {
                lock.unlock();
            }
        }

        /** Total Stream 1 frames dropped because the Stream 1 queue was full. */
        public long droppedLogFrames() {
            lock.lock();
            try {
                return droppedLogFramesTotal;
            } finally {
                lock.unlock();
            }
        }

        /**
         * Waits until every queued frame has been written (or the transport failed).
         *
         * @return {@code true} if all queues drained within {@code timeoutMs}
         */
        public boolean awaitDrained(long timeoutMs) throws InterruptedException {
            long deadline = System.nanoTime() + TimeUnit.MILLISECONDS.toNanos(Math.max(0L, timeoutMs));
            lock.lock();
            try {
                while (failure == null && (inFlight > 0 || !allQueuesEmptyLocked())) {
                    long remaining = deadline - System.nanoTime();
                    if (remaining <= 0L) {
                        return false;
                    }
                    changed.awaitNanos(remaining);
                }
                return failure == null;
            } finally {
                lock.unlock();
            }
        }

        /** Drains queued frames (bounded wait), stops the writer thread and closes the stream. */
        public void closeQuietly() {
            try {
                awaitDrained(2000L);
            } catch (InterruptedException ie) {
                Thread.currentThread().interrupt();
            }
            lock.lock();
            try {
                closed = true;
                notEmpty.signalAll();
                changed.signalAll();
            } finally {
                lock.unlock();
            }
            try {
                out.flush();
            } catch (IOException ignored) {
            }
            try {
                out.close();
            } catch (IOException ignored) {
            }
        }

        private void throwIfUnusable() throws IOException {
            if (failure != null) {
                throw new IOException("FrameWriter transport failed: " + failure.getMessage(), failure);
            }
            if (closed) {
                throw new IOException("FrameWriter closed");
            }
        }

        private long assignSeqLocked(int idx) {
            long seq = (++nextSeq[idx]) & 0xFFFFFFFFL;
            if (seq == 0) {
                seq = 1;
                nextSeq[idx] = 1;
            }
            return seq;
        }

        private long dropLocked() {
            droppedLogFramesPending++;
            droppedLogFramesTotal++;
            return 0L;
        }

        private void enqueueOverflowNoticeLocked() {
            long dropped = droppedLogFramesPending;
            droppedLogFramesPending = 0;
            long now = clock != null ? clock.boottimeNs() : 0L;
            PayloadCodec.LogOverflowStats gap = new PayloadCodec.LogOverflowStats(
                    now,
                    Protocol.COMPLETENESS_OVERFLOW_DETECTED,
                    0,
                    -1L,
                    -1L,
                    "writer_queue_overflow:dropped=" + dropped);
            long seq = assignSeqLocked(Protocol.STREAM_LOGCAT);
            queues.get(Protocol.STREAM_LOGCAT).addLast(WireFrame.of(
                    Protocol.STREAM_LOGCAT,
                    Protocol.MSG_LOG_OVERFLOW_STATS,
                    Protocol.FLAG_URGENT,
                    seq,
                    PayloadCodec.encodeLogOverflowStats(gap)).toBytes());
            notEmpty.signal();
        }

        private boolean allQueuesEmptyLocked() {
            for (ArrayDeque<byte[]> q : queues) {
                if (!q.isEmpty()) {
                    return false;
                }
            }
            return true;
        }

        /** Stream 0 strictly first, then round-robin across the remaining streams. */
        private byte[] pollNextLocked() {
            byte[] control = queues.get(Protocol.STREAM_CONTROL).pollFirst();
            if (control != null) {
                return control;
            }
            for (int i = 1; i < STREAM_SLOTS; i++) {
                int idx = 1 + ((roundRobin + i - 1) % (STREAM_SLOTS - 1));
                byte[] frame = queues.get(idx).pollFirst();
                if (frame != null) {
                    roundRobin = idx % (STREAM_SLOTS - 1);
                    if (idx == Protocol.STREAM_LOGCAT
                            && droppedLogFramesPending > 0
                            && queues.get(idx).size() + 1 <= capacity[idx]) {
                        enqueueOverflowNoticeLocked();
                    }
                    return frame;
                }
            }
            return null;
        }

        private void drainLoop() {
            while (true) {
                byte[] frame = null;
                lock.lock();
                try {
                    while (!closed && failure == null) {
                        frame = pollNextLocked();
                        if (frame != null) {
                            break;
                        }
                        notEmpty.awaitUninterruptibly();
                    }
                    if (frame == null) {
                        changed.signalAll();
                        return;
                    }
                    inFlight++;
                    changed.signalAll();
                } finally {
                    lock.unlock();
                }
                IOException err = null;
                try {
                    out.write(frame);
                    out.flush();
                } catch (IOException ioe) {
                    err = ioe;
                }
                lock.lock();
                try {
                    inFlight--;
                    if (err != null) {
                        failure = err;
                        for (ArrayDeque<byte[]> q : queues) {
                            q.clear();
                        }
                    }
                    changed.signalAll();
                } finally {
                    lock.unlock();
                }
            }
        }
    }
}
