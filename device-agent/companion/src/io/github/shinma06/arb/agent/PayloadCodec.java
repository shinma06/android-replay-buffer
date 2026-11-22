package io.github.shinma06.arb.agent;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;

/**
 * Exact Little-Endian payload serializers and deserializers for all v1 wire messages.
 */
public final class PayloadCodec {
    private PayloadCodec() {}

    // ---------------- Data Records ----------------

    public static final class Hello {
        public final int protocolVersion;
        public final int apiLevel;
        public final int bootCount;
        public final int bootIdSource;
        public final boolean isEmulator;
        public final int heartbeatIntervalMs;
        public final String bootId;
        public final String model;
        public final String manufacturer;
        public final String agentVersion;
        public final String bootIdDetail;

        public Hello(
                int protocolVersion,
                int apiLevel,
                int bootCount,
                int bootIdSource,
                boolean isEmulator,
                int heartbeatIntervalMs,
                String bootId,
                String model,
                String manufacturer,
                String agentVersion,
                String bootIdDetail) {
            this.protocolVersion = protocolVersion;
            this.apiLevel = apiLevel;
            this.bootCount = bootCount;
            this.bootIdSource = bootIdSource;
            this.isEmulator = isEmulator;
            this.heartbeatIntervalMs = heartbeatIntervalMs;
            this.bootId = nonNull(bootId);
            this.model = nonNull(model);
            this.manufacturer = nonNull(manufacturer);
            this.agentVersion = nonNull(agentVersion);
            this.bootIdDetail = nonNull(bootIdDetail);
        }
    }

    public static final class StreamHealthEntry {
        public final int streamId;
        public final int state;
        public final int restartCount;
        public final long emittedSeq;
        public final long lastPtsNs;

        public StreamHealthEntry(int streamId, int state, int restartCount, long emittedSeq, long lastPtsNs) {
            this.streamId = streamId & 0xFF;
            this.state = state & 0xFF;
            this.restartCount = restartCount & 0xFFFF;
            this.emittedSeq = emittedSeq & 0xFFFFFFFFL;
            this.lastPtsNs = lastPtsNs;
        }
    }

    public static final class Heartbeat {
        public final long boottimeNs;
        public final long monotonicNs;
        public final long realtimeNs;
        public final List<StreamHealthEntry> streams;

        public Heartbeat(long boottimeNs, long monotonicNs, long realtimeNs, List<StreamHealthEntry> streams) {
            this.boottimeNs = boottimeNs;
            this.monotonicNs = monotonicNs;
            this.realtimeNs = realtimeNs;
            this.streams = streams;
        }
    }

    public static final class Ping {
        public final long nonce;

        public Ping(long nonce) {
            this.nonce = nonce;
        }
    }

    public static final class Pong {
        public final long nonce;
        public final long boottimeBeforeNs;
        public final long monotonicNs;
        public final long realtimeNs;
        public final long boottimeAfterNs;

        public Pong(long nonce, long boottimeBeforeNs, long monotonicNs, long realtimeNs, long boottimeAfterNs) {
            this.nonce = nonce;
            this.boottimeBeforeNs = boottimeBeforeNs;
            this.monotonicNs = monotonicNs;
            this.realtimeNs = realtimeNs;
            this.boottimeAfterNs = boottimeAfterNs;
        }
    }

    public static final class StreamState {
        public final int targetStreamId;
        public final int state;
        public final int restartCount;
        public final long boottimeNs;
        public final long emittedSeq;
        public final String reason;

        public StreamState(int targetStreamId, int state, int restartCount, long boottimeNs, long emittedSeq, String reason) {
            this.targetStreamId = targetStreamId & 0xFF;
            this.state = state & 0xFF;
            this.restartCount = restartCount & 0xFFFF;
            this.boottimeNs = boottimeNs;
            this.emittedSeq = emittedSeq & 0xFFFFFFFFL;
            this.reason = nonNull(reason);
        }
    }

    public static final class LogRecord {
        public final long deviceReadBoottimeNs;
        public final long logSec;
        public final long logNsec;
        public final int pid;
        public final int tid;
        public final long lid;
        public final long uid;
        public final int hdrSize;
        public final int priority;
        public final int recordFlags;
        public final byte[] payload;

        public LogRecord(
                long deviceReadBoottimeNs,
                long logSec,
                long logNsec,
                int pid,
                int tid,
                long lid,
                long uid,
                int hdrSize,
                int priority,
                int recordFlags,
                byte[] payload) {
            this.deviceReadBoottimeNs = deviceReadBoottimeNs;
            this.logSec = logSec & 0xFFFFFFFFL;
            this.logNsec = logNsec & 0xFFFFFFFFL;
            this.pid = pid;
            this.tid = tid;
            this.lid = lid & 0xFFFFFFFFL;
            this.uid = uid & 0xFFFFFFFFL;
            this.hdrSize = hdrSize & 0xFFFF;
            this.priority = priority & 0xFF;
            this.recordFlags = recordFlags & 0xFF;
            this.payload = payload != null ? payload : new byte[0];
        }
    }

    public static final class LogOverflowStats {
        public final long boottimeNs;
        public final int completeness;
        public final int buffersCount;
        public final long prunedDelta;
        public final long prunedCumulative;
        public final String detail;

        public LogOverflowStats(
                long boottimeNs,
                int completeness,
                int buffersCount,
                long prunedDelta,
                long prunedCumulative,
                String detail) {
            this.boottimeNs = boottimeNs;
            this.completeness = completeness & 0xFF;
            this.buffersCount = buffersCount & 0xFFFF;
            this.prunedDelta = prunedDelta;
            this.prunedCumulative = prunedCumulative;
            this.detail = nonNull(detail);
        }
    }

    public static final class TouchEvent {
        public final long deviceReadBoottimeNs;
        public final long eventSec;
        public final int eventUsec;
        public final int evType;
        public final int evCode;
        public final int evValue;
        public final String devicePath;
        public final String rawLine;

        public TouchEvent(
                long deviceReadBoottimeNs,
                long eventSec,
                int eventUsec,
                int evType,
                int evCode,
                int evValue,
                String devicePath,
                String rawLine) {
            this.deviceReadBoottimeNs = deviceReadBoottimeNs;
            this.eventSec = eventSec;
            this.eventUsec = eventUsec;
            this.evType = evType & 0xFFFF;
            this.evCode = evCode & 0xFFFF;
            this.evValue = evValue;
            this.devicePath = nonNull(devicePath);
            this.rawLine = nonNull(rawLine);
        }
    }

    public static final class Shutdown {
        public final long boottimeNs;
        public final int code;
        public final String reason;

        public Shutdown(long boottimeNs, int code, String reason) {
            this.boottimeNs = boottimeNs;
            this.code = code & 0xFFFF;
            this.reason = nonNull(reason);
        }
    }

    // ---------------- Encoders ----------------

    public static byte[] encodeHello(Hello h) {
        Writer w = new Writer(128);
        w.putU16(h.protocolVersion);
        w.putU32(h.apiLevel);
        w.putI32(h.bootCount);
        w.putU8(h.bootIdSource);
        w.putU8(h.isEmulator ? 1 : 0);
        w.putU32(h.heartbeatIntervalMs);
        w.putStr16(h.bootId);
        w.putStr16(h.model);
        w.putStr16(h.manufacturer);
        w.putStr16(h.agentVersion);
        w.putStr16(h.bootIdDetail);
        return w.toByteArray();
    }

    public static byte[] encodeHeartbeat(Heartbeat hb) {
        int count = Math.min(hb.streams.size(), 255);
        ByteBuffer buf = ByteBuffer.allocate(25 + count * 20).order(ByteOrder.LITTLE_ENDIAN);
        buf.putLong(hb.boottimeNs);
        buf.putLong(hb.monotonicNs);
        buf.putLong(hb.realtimeNs);
        buf.put((byte) count);
        for (int i = 0; i < count; i++) {
            StreamHealthEntry e = hb.streams.get(i);
            buf.put((byte) e.streamId);
            buf.put((byte) e.state);
            buf.putShort((short) e.restartCount);
            buf.putInt((int) (e.emittedSeq & 0xFFFFFFFFL));
            buf.putLong(e.lastPtsNs);
            buf.putInt(0); // reserved
        }
        return buf.array();
    }

    public static byte[] encodePing(Ping p) {
        ByteBuffer buf = ByteBuffer.allocate(8).order(ByteOrder.LITTLE_ENDIAN);
        buf.putLong(p.nonce);
        return buf.array();
    }

    public static byte[] encodePong(Pong p) {
        ByteBuffer buf = ByteBuffer.allocate(40).order(ByteOrder.LITTLE_ENDIAN);
        buf.putLong(p.nonce);
        buf.putLong(p.boottimeBeforeNs);
        buf.putLong(p.monotonicNs);
        buf.putLong(p.realtimeNs);
        buf.putLong(p.boottimeAfterNs);
        return buf.array();
    }

    public static byte[] encodeStreamState(StreamState s) {
        Writer w = new Writer(64);
        w.putU8(s.targetStreamId);
        w.putU8(s.state);
        w.putU16(s.restartCount);
        w.putI64(s.boottimeNs);
        w.putU32(s.emittedSeq);
        w.putStr16(s.reason);
        return w.toByteArray();
    }

    public static byte[] encodeLogRecord(LogRecord r) {
        int payloadLen = r.payload.length;
        ByteBuffer buf = ByteBuffer.allocate(40 + payloadLen).order(ByteOrder.LITTLE_ENDIAN);
        buf.putLong(r.deviceReadBoottimeNs);
        buf.putInt((int) (r.logSec & 0xFFFFFFFFL));
        buf.putInt((int) (r.logNsec & 0xFFFFFFFFL));
        buf.putInt(r.pid);
        buf.putInt(r.tid);
        buf.putInt((int) (r.lid & 0xFFFFFFFFL));
        buf.putInt((int) (r.uid & 0xFFFFFFFFL));
        buf.putShort((short) r.hdrSize);
        buf.put((byte) r.priority);
        buf.put((byte) r.recordFlags);
        buf.putInt(payloadLen);
        buf.put(r.payload);
        return buf.array();
    }

    public static byte[] encodeLogOverflowStats(LogOverflowStats s) {
        Writer w = new Writer(64);
        w.putI64(s.boottimeNs);
        w.putU8(s.completeness);
        w.putU8(0); // reserved
        w.putU16(s.buffersCount);
        w.putI64(s.prunedDelta);
        w.putI64(s.prunedCumulative);
        w.putStr16(s.detail);
        return w.toByteArray();
    }

    public static byte[] encodeTouchEvent(TouchEvent e) {
        Writer w = new Writer(96);
        w.putI64(e.deviceReadBoottimeNs);
        w.putI64(e.eventSec);
        w.putI32(e.eventUsec);
        w.putU16(e.evType);
        w.putU16(e.evCode);
        w.putI32(e.evValue);
        w.putStr16(e.devicePath);
        w.putStr16(e.rawLine);
        return w.toByteArray();
    }

    public static byte[] encodeShutdown(Shutdown s) {
        Writer w = new Writer(32);
        w.putI64(s.boottimeNs);
        w.putU16(s.code);
        w.putStr16(s.reason);
        return w.toByteArray();
    }

    // ---------------- Decoders ----------------

    public static Hello decodeHello(byte[] data) throws IOException {
        Reader r = new Reader(data);
        int protocolVersion = r.getU16();
        int apiLevel = (int) r.getU32();
        int bootCount = r.getI32();
        int bootIdSource = r.getU8();
        boolean isEmulator = r.getU8() != 0;
        int heartbeatIntervalMs = (int) r.getU32();
        String bootId = r.getStr16();
        String model = r.getStr16();
        String manufacturer = r.getStr16();
        String agentVersion = r.getStr16();
        String bootIdDetail = r.getStr16();
        return new Hello(protocolVersion, apiLevel, bootCount, bootIdSource, isEmulator,
                heartbeatIntervalMs, bootId, model, manufacturer, agentVersion, bootIdDetail);
    }

    public static Heartbeat decodeHeartbeat(byte[] data) throws IOException {
        Reader r = new Reader(data);
        long boottimeNs = r.getI64();
        long monotonicNs = r.getI64();
        long realtimeNs = r.getI64();
        int count = r.getU8();
        List<StreamHealthEntry> streams = new ArrayList<>(count);
        for (int i = 0; i < count; i++) {
            int streamId = r.getU8();
            int state = r.getU8();
            int restartCount = r.getU16();
            long emittedSeq = r.getU32();
            long lastPtsNs = r.getI64();
            r.getU32(); // reserved
            streams.add(new StreamHealthEntry(streamId, state, restartCount, emittedSeq, lastPtsNs));
        }
        return new Heartbeat(boottimeNs, monotonicNs, realtimeNs, streams);
    }

    public static Ping decodePing(byte[] data) throws IOException {
        Reader r = new Reader(data);
        return new Ping(r.getI64());
    }

    public static Pong decodePong(byte[] data) throws IOException {
        Reader r = new Reader(data);
        return new Pong(r.getI64(), r.getI64(), r.getI64(), r.getI64(), r.getI64());
    }

    public static StreamState decodeStreamState(byte[] data) throws IOException {
        Reader r = new Reader(data);
        int targetStreamId = r.getU8();
        int state = r.getU8();
        int restartCount = r.getU16();
        long boottimeNs = r.getI64();
        long emittedSeq = r.getU32();
        String reason = r.getStr16();
        return new StreamState(targetStreamId, state, restartCount, boottimeNs, emittedSeq, reason);
    }

    public static LogRecord decodeLogRecord(byte[] data) throws IOException {
        Reader r = new Reader(data);
        long readBoot = r.getI64();
        long sec = r.getU32();
        long nsec = r.getU32();
        int pid = r.getI32();
        int tid = r.getI32();
        long lid = r.getU32();
        long uid = r.getU32();
        int hdrSize = r.getU16();
        int priority = r.getU8();
        int recordFlags = r.getU8();
        byte[] payload = r.getBytes32();
        return new LogRecord(readBoot, sec, nsec, pid, tid, lid, uid, hdrSize, priority, recordFlags, payload);
    }

    public static LogOverflowStats decodeLogOverflowStats(byte[] data) throws IOException {
        Reader r = new Reader(data);
        long boottimeNs = r.getI64();
        int completeness = r.getU8();
        r.getU8(); // reserved
        int buffersCount = r.getU16();
        long prunedDelta = r.getI64();
        long prunedCumulative = r.getI64();
        String detail = r.getStr16();
        return new LogOverflowStats(boottimeNs, completeness, buffersCount, prunedDelta, prunedCumulative, detail);
    }

    public static TouchEvent decodeTouchEvent(byte[] data) throws IOException {
        Reader r = new Reader(data);
        long readBoot = r.getI64();
        long sec = r.getI64();
        int usec = r.getI32();
        int evType = r.getU16();
        int evCode = r.getU16();
        int evValue = r.getI32();
        String devicePath = r.getStr16();
        String rawLine = r.getStr16();
        return new TouchEvent(readBoot, sec, usec, evType, evCode, evValue, devicePath, rawLine);
    }

    public static Shutdown decodeShutdown(byte[] data) throws IOException {
        Reader r = new Reader(data);
        long boottimeNs = r.getI64();
        int code = r.getU16();
        String reason = r.getStr16();
        return new Shutdown(boottimeNs, code, reason);
    }

    // ---------------- Helpers ----------------

    private static String nonNull(String s) {
        return s != null ? s : "";
    }

    private static final class Writer {
        private final ByteArrayOutputStream out;
        private final byte[] tmp = new byte[8];
        private final ByteBuffer bb = ByteBuffer.wrap(tmp).order(ByteOrder.LITTLE_ENDIAN);

        Writer(int initialCapacity) {
            this.out = new ByteArrayOutputStream(initialCapacity);
        }

        void putU8(int v) {
            out.write(v & 0xFF);
        }

        void putU16(int v) {
            bb.clear();
            bb.putShort((short) (v & 0xFFFF));
            out.write(tmp, 0, 2);
        }

        void putI32(int v) {
            bb.clear();
            bb.putInt(v);
            out.write(tmp, 0, 4);
        }

        void putU32(long v) {
            putI32((int) (v & 0xFFFFFFFFL));
        }

        void putI64(long v) {
            bb.clear();
            bb.putLong(v);
            out.write(tmp, 0, 8);
        }

        void putStr16(String s) {
            byte[] bytes = nonNull(s).getBytes(StandardCharsets.UTF_8);
            int len = Math.min(bytes.length, 65535);
            putU16(len);
            out.write(bytes, 0, len);
        }

        byte[] toByteArray() {
            return out.toByteArray();
        }
    }

    private static final class Reader {
        private final ByteBuffer buf;

        Reader(byte[] data) {
            this.buf = ByteBuffer.wrap(data).order(ByteOrder.LITTLE_ENDIAN);
        }

        private void require(int n) throws IOException {
            if (buf.remaining() < n) {
                throw new IOException("Truncated payload: need " + n + " bytes, have " + buf.remaining());
            }
        }

        int getU8() throws IOException {
            require(1);
            return buf.get() & 0xFF;
        }

        int getU16() throws IOException {
            require(2);
            return buf.getShort() & 0xFFFF;
        }

        int getI32() throws IOException {
            require(4);
            return buf.getInt();
        }

        long getU32() throws IOException {
            return getI32() & 0xFFFFFFFFL;
        }

        long getI64() throws IOException {
            require(8);
            return buf.getLong();
        }

        String getStr16() throws IOException {
            int len = getU16();
            require(len);
            byte[] bytes = new byte[len];
            buf.get(bytes);
            return new String(bytes, StandardCharsets.UTF_8);
        }

        byte[] getBytes32() throws IOException {
            long len = getU32();
            if (len > Protocol.MAX_PAYLOAD_LEN || len > buf.remaining()) {
                throw new IOException("Invalid bytes32 length: " + len);
            }
            byte[] bytes = new byte[(int) len];
            buf.get(bytes);
            return bytes;
        }
    }
}
