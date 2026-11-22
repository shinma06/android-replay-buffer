package io.github.shinma06.arb.agent;

import java.io.EOFException;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;

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
     * Thread-safe atomic frame writer with per-stream monotonic sequence numbers.
     */
    public static final class FrameWriter {
        private final OutputStream out;
        private final long[] nextSeq = new long[8];
        private volatile boolean closed;

        public FrameWriter(OutputStream out) {
            this.out = out;
        }

        public synchronized long writeFrame(int streamId, int msgType, int flags, byte[] payload) throws IOException {
            if (closed) {
                throw new IOException("FrameWriter closed");
            }
            int idx = (streamId >= 0 && streamId < nextSeq.length) ? streamId : 0;
            long seq = (++nextSeq[idx]) & 0xFFFFFFFFL;
            if (seq == 0) {
                seq = 1;
                nextSeq[idx] = 1;
            }
            WireFrame frame = WireFrame.of(streamId, msgType, flags, seq, payload);
            out.write(frame.toBytes());
            out.flush();
            return seq;
        }

        public synchronized void closeQuietly() {
            closed = true;
            try {
                out.flush();
            } catch (IOException ignored) {
            }
            try {
                out.close();
            } catch (IOException ignored) {
            }
        }
    }
}
