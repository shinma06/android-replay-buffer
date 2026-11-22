package io.github.shinma06.arb.agent;

import java.io.EOFException;
import java.io.IOException;
import java.io.InputStream;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;

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
}
