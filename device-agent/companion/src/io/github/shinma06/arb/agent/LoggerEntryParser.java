package io.github.shinma06.arb.agent;

import java.io.IOException;
import java.io.InputStream;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.Locale;

/**
 * Streaming parser for Android {@code logcat -B} binary {@code logger_entry} records.
 *
 * <p>Supports v1 (20-byte header, {@code hdr_size == 0}), v2/v3 (24-byte header with {@code lid}),
 * v4 (28-byte header with {@code lid} and {@code uid}), and forward-compatible larger headers.
 */
public final class LoggerEntryParser {
    private static final int BASE_HEADER_SIZE = 20;
    private static final int MAX_RECORD_PAYLOAD = 65536;
    private static final int MAX_HEADER_SIZE = 256;

    private final byte[] baseBuf = new byte[BASE_HEADER_SIZE];
    private final byte[] extraBuf = new byte[MAX_HEADER_SIZE];

    /**
     * Reads the next binary {@code logger_entry}, stamping {@code device_read_boottime_ns} immediately
     * upon reading the record. Returns {@code null} on clean EOF before the next header.
     */
    public PayloadCodec.LogRecord readNext(InputStream in, ClockSource clock) throws IOException {
        int first = in.read();
        if (first < 0) {
            return null;
        }
        baseBuf[0] = (byte) first;
        WireFrame.readFully(in, baseBuf, 1, BASE_HEADER_SIZE - 1);

        ByteBuffer bb = ByteBuffer.wrap(baseBuf).order(ByteOrder.LITTLE_ENDIAN);
        int payloadLen = bb.getShort() & 0xFFFF;
        int rawHdrSize = bb.getShort() & 0xFFFF;
        int pid = bb.getInt();
        int tid = bb.getInt();
        long sec = bb.getInt() & 0xFFFFFFFFL;
        long nsec = bb.getInt() & 0xFFFFFFFFL;

        int hdrSize = (rawHdrSize == 0) ? BASE_HEADER_SIZE : rawHdrSize;
        if (hdrSize < BASE_HEADER_SIZE || hdrSize > MAX_HEADER_SIZE) {
            throw new IOException("Invalid logger_entry hdr_size: " + hdrSize);
        }
        if (payloadLen > MAX_RECORD_PAYLOAD) {
            throw new IOException("Invalid logger_entry payload_len: " + payloadLen);
        }

        long lid = Protocol.UNKNOWN_U32 & 0xFFFFFFFFL;
        long uid = Protocol.UNKNOWN_U32 & 0xFFFFFFFFL;
        int extra = hdrSize - BASE_HEADER_SIZE;
        if (extra > 0) {
            WireFrame.readFully(in, extraBuf, 0, extra);
            ByteBuffer eb = ByteBuffer.wrap(extraBuf, 0, extra).order(ByteOrder.LITTLE_ENDIAN);
            if (extra >= 4) {
                lid = eb.getInt() & 0xFFFFFFFFL;
            }
            if (extra >= 8) {
                uid = eb.getInt() & 0xFFFFFFFFL;
            }
        }

        byte[] payload = new byte[payloadLen];
        if (payloadLen > 0) {
            WireFrame.readFully(in, payload, 0, payloadLen);
        }

        long readBoottimeNs = clock.boottimeNs();

        boolean binaryEvent = isBinaryEventBuffer(lid);
        int priority = 0;
        int recordFlags = 0;
        if (binaryEvent) {
            recordFlags |= Protocol.LOG_RECORD_FLAG_BINARY_EVENT;
        } else if (payloadLen > 0) {
            priority = payload[0] & 0xFF;
        }

        return new PayloadCodec.LogRecord(
                readBoottimeNs,
                sec,
                nsec,
                pid,
                tid,
                lid,
                uid,
                hdrSize,
                priority,
                recordFlags,
                payload);
    }

    public static boolean isBinaryEventBuffer(long lid) {
        // 2 = LOG_ID_EVENTS, 5 = LOG_ID_STATS, 6 = LOG_ID_SECURITY
        return lid == 2L || lid == 5L || lid == 6L;
    }

    /**
     * Tracks the last emitted logcat entry timestamp {@code (sec, nsec)} and
     * boundary entry fingerprints so a respawned {@code logcat -T '<sec>.<nsec>'}
     * resumes seamlessly without duplicating the boundary line or silently
     * hiding a ring-buffer eviction gap.
     */
    public static final class ResumeTracker {
        public enum Decision {
            /** Emit record normally. */
            EMIT,
            /** Skip record because it predates or duplicates the saved resume boundary. */
            SKIP_DUPLICATE,
            /** Emit record, and signal an overflow gap because the boundary entry was evicted. */
            EMIT_WITH_GAP
        }

        private static final class EntryFingerprint {
            final int pid;
            final int tid;
            final long lid;
            final int priority;
            final byte[] payload;

            EntryFingerprint(PayloadCodec.LogRecord rec) {
                this.pid = rec.pid;
                this.tid = rec.tid;
                this.lid = rec.lid;
                this.priority = rec.priority;
                this.payload = rec.payload;
            }

            boolean matches(PayloadCodec.LogRecord rec) {
                return this.pid == rec.pid
                        && this.tid == rec.tid
                        && this.lid == rec.lid
                        && this.priority == rec.priority
                        && Arrays.equals(this.payload, rec.payload);
            }
        }

        private boolean hasTimestamp;
        private long lastSec;
        private long lastNsec;
        private final List<EntryFingerprint> lastFingerprints = new ArrayList<>();

        private boolean resuming;
        private long resumeSec;
        private long resumeNsec;
        private boolean boundaryObserved;
        private final List<EntryFingerprint> resumeFingerprints = new ArrayList<>();

        public boolean hasTimestamp() {
            return hasTimestamp;
        }

        /**
         * Returns the {@code -T} argument for launching {@code logcat}:
         * {@code "<sec>.<9-digit-nsec>"} when a previous timestamp exists, or
         * {@code "1"} on initial startup.
         */
        public String nextTArgument() {
            if (!hasTimestamp) {
                return "1";
            }
            return String.format(Locale.US, "%d.%09d", lastSec, lastNsec);
        }

        /**
         * Arms boundary deduplication before starting a respawned {@code logcat}
         * process with {@link #nextTArgument()}.
         */
        public void beginResume() {
            if (!hasTimestamp) {
                resuming = false;
                return;
            }
            resuming = true;
            resumeSec = lastSec;
            resumeNsec = lastNsec;
            boundaryObserved = false;
            resumeFingerprints.clear();
            resumeFingerprints.addAll(lastFingerprints);
        }

        /**
         * Evaluates an incoming {@link PayloadCodec.LogRecord}, updating the last
         * emitted watermark when the record should be emitted.
         */
        public Decision evaluate(PayloadCodec.LogRecord rec) {
            if (resuming) {
                int cmp = compareTs(rec.logSec, rec.logNsec, resumeSec, resumeNsec);
                if (cmp < 0) {
                    return Decision.SKIP_DUPLICATE;
                }
                if (cmp == 0) {
                    for (EntryFingerprint fp : resumeFingerprints) {
                        if (fp.matches(rec)) {
                            boundaryObserved = true;
                            return Decision.SKIP_DUPLICATE;
                        }
                    }
                    // Distinct entry sharing the exact boundary nanosecond.
                    boundaryObserved = true;
                    recordWatermark(rec);
                    return Decision.EMIT;
                }
                // Strictly past the resume timestamp.
                resuming = false;
                boolean gapDetected = !boundaryObserved;
                resumeFingerprints.clear();
                recordWatermark(rec);
                return gapDetected ? Decision.EMIT_WITH_GAP : Decision.EMIT;
            }

            recordWatermark(rec);
            return Decision.EMIT;
        }

        private void recordWatermark(PayloadCodec.LogRecord rec) {
            if (!hasTimestamp) {
                hasTimestamp = true;
                lastSec = rec.logSec;
                lastNsec = rec.logNsec;
                lastFingerprints.clear();
                lastFingerprints.add(new EntryFingerprint(rec));
                return;
            }
            int cmp = compareTs(rec.logSec, rec.logNsec, lastSec, lastNsec);
            if (cmp > 0) {
                lastSec = rec.logSec;
                lastNsec = rec.logNsec;
                lastFingerprints.clear();
                lastFingerprints.add(new EntryFingerprint(rec));
            } else if (cmp == 0 && lastFingerprints.size() < 64) {
                lastFingerprints.add(new EntryFingerprint(rec));
            }
        }

        private static int compareTs(long secA, long nsecA, long secB, long nsecB) {
            int c = Long.compare(secA, secB);
            return (c != 0) ? c : Long.compare(nsecA, nsecB);
        }
    }
}
