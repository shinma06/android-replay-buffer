package io.github.shinma06.arb.agent;

import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.CharacterCodingException;
import java.nio.charset.CharsetDecoder;
import java.nio.charset.CodingErrorAction;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;
import java.util.Collections;
import java.util.List;
import java.util.Set;

/**
 * Zero-dependency pure-Java unit test suite for ARB device-agent binary wire framing,
 * payload codecs, binary {@code logger_entry} parsing (20/24/28-byte headers),
 * {@code logcat -S} stats parsing, and {@code getevent} parsing.
 */
public final class ProtocolTest {
    private static int passed = 0;

    public static void main(String[] args) throws Exception {
        testWireFrameHeaderLayoutAndRoundTrip();
        testFrameWriterMonotonicPerStreamSeq();
        testHelloRoundTrip();
        testHeartbeatRoundTrip();
        testPingPongRoundTrip();
        testStreamStateRoundTrip();
        testLogRecordRoundTrip();
        testLogOverflowStatsRoundTrip();
        testTouchEventRoundTrip();
        testShutdownRoundTrip();
        testLoggerEntry20_24_28ByteHeaders();
        testLogcatStatsParserApi37Format();
        testGeteventParserDiscoveryAndLines();
        testStr16TruncatesOnUtf8CodepointBoundary();
        testGeteventRawLineCappedAt256Utf8Bytes();
        testFrameWriterStream0PriorityAndStream1OverflowNeverBlocks();
        testFrameWriterSurfacesTransportFailure();
        testLogcatResumeTrackerDedupsBoundaryAndDetectsEviction();
        testStreamHealthEmittedSeqNeverMovesBackwards();
        testBootIdResolverSelinuxEaccesFallbacks();
        testLogcatWorkerRestrictedBufferFallbackCommandAndAos9Stats();
        System.out.println("ProtocolTest: ALL " + passed + " TESTS PASSED");
    }

    private static void testWireFrameHeaderLayoutAndRoundTrip() throws IOException {
        byte[] payload = new byte[] {0x11, 0x22, 0x33, 0x44, 0x55};
        WireFrame f = WireFrame.of(Protocol.STREAM_LOGCAT, Protocol.MSG_LOG_RECORD, Protocol.FLAG_RECOVERED, 42L, payload);
        byte[] wire = f.toBytes();
        assertEq(25, wire.length, "20-byte header + 5-byte payload");
        assertEq(0x41, wire[0] & 0xFF, "magic[0] = 'A'");
        assertEq(0x52, wire[1] & 0xFF, "magic[1] = 'R'");
        assertEq(1, wire[2] & 0xFF, "version = 1");
        assertEq(1, wire[3] & 0xFF, "stream_id = 1");
        assertEq(0x10, wire[4] & 0xFF, "msg_type = LogRecord");
        assertEq(0x04, wire[5] & 0xFF, "flags = FLAG_RECOVERED");
        assertEq(0, wire[6] & 0xFF, "reserved[0]");
        assertEq(0, wire[7] & 0xFF, "reserved[1]");
        // seq = 42 LE
        assertEq(42, wire[8] & 0xFF, "seq LE byte 0");
        assertEq(0, wire[9] & 0xFF, "seq LE byte 1");
        // payload_len = 5 LE
        assertEq(5, wire[12] & 0xFF, "payload_len LE byte 0");
        assertEq(0, wire[19] & 0xFF, "payload_len LE byte 7");

        WireFrame decoded = WireFrame.readFrom(new ByteArrayInputStream(wire));
        assertTrue(decoded != null, "decoded frame non-null");
        assertEq(Protocol.VERSION, decoded.version, "decoded version");
        assertEq(Protocol.STREAM_LOGCAT, decoded.streamId, "decoded streamId");
        assertEq(Protocol.MSG_LOG_RECORD, decoded.msgType, "decoded msgType");
        assertEq(Protocol.FLAG_RECOVERED, decoded.flags, "decoded flags");
        assertEq(42L, decoded.seq, "decoded seq");
        assertTrue(Arrays.equals(payload, decoded.payload), "decoded payload bytes");

        // Reject corrupt magic
        byte[] badMagic = Arrays.copyOf(wire, wire.length);
        badMagic[0] = 0x00;
        boolean threw = false;
        try {
            WireFrame.readFrom(new ByteArrayInputStream(badMagic));
        } catch (IOException expected) {
            threw = true;
        }
        assertTrue(threw, "bad magic must throw IOException");
        passed++;
    }

    private static void testFrameWriterMonotonicPerStreamSeq() throws IOException {
        ByteArrayOutputStream baos = new ByteArrayOutputStream();
        WireFrame.FrameWriter w = new WireFrame.FrameWriter(baos);
        assertEq(1L, w.writeFrame(Protocol.STREAM_CONTROL, Protocol.MSG_HELLO, 0, new byte[0]), "stream 0 seq 1");
        assertEq(1L, w.writeFrame(Protocol.STREAM_LOGCAT, Protocol.MSG_LOG_RECORD, 0, new byte[1]), "stream 1 seq 1");
        assertEq(2L, w.writeFrame(Protocol.STREAM_CONTROL, Protocol.MSG_HEARTBEAT, 0, new byte[2]), "stream 0 seq 2");
        assertEq(2L, w.writeFrame(Protocol.STREAM_LOGCAT, Protocol.MSG_LOG_RECORD, 0, new byte[3]), "stream 1 seq 2");
        try {
            assertTrue(w.awaitDrained(5000L), "writer drains all queued frames");
        } catch (InterruptedException ie) {
            throw new IOException(ie);
        }

        ByteArrayInputStream bais = new ByteArrayInputStream(baos.toByteArray());
        WireFrame f1 = WireFrame.readFrom(bais);
        WireFrame f2 = WireFrame.readFrom(bais);
        WireFrame f3 = WireFrame.readFrom(bais);
        WireFrame f4 = WireFrame.readFrom(bais);
        // The writer may reorder across streams (Stream 0 has priority) but never within a stream.
        long[] lastSeq = new long[8];
        for (WireFrame f : new WireFrame[] {f1, f2, f3, f4}) {
            assertEq(lastSeq[f.streamId] + 1, f.seq, "per-stream seq in order for stream " + f.streamId);
            lastSeq[f.streamId] = f.seq;
        }
        assertEq(2L, lastSeq[Protocol.STREAM_CONTROL], "stream 0 frames written");
        assertEq(2L, lastSeq[Protocol.STREAM_LOGCAT], "stream 1 frames written");
        assertTrue(WireFrame.readFrom(bais) == null, "clean EOF returns null");
        passed++;
    }

    private static void testHelloRoundTrip() throws IOException {
        PayloadCodec.Hello in = new PayloadCodec.Hello(
                1, 37, 4, Protocol.BOOT_ID_KERNEL_PROC, true, 300,
                "c729eaec-5428-4a38-b957-441109da33f7", "sdk_gphone64_arm64", "Google", "0.1.0", "");
        PayloadCodec.Hello out = PayloadCodec.decodeHello(PayloadCodec.encodeHello(in));
        assertEq(in.protocolVersion, out.protocolVersion, "hello protocolVersion");
        assertEq(in.apiLevel, out.apiLevel, "hello apiLevel");
        assertEq(in.bootCount, out.bootCount, "hello bootCount");
        assertEq(in.bootIdSource, out.bootIdSource, "hello bootIdSource");
        assertEq(in.isEmulator, out.isEmulator, "hello isEmulator");
        assertEq(in.heartbeatIntervalMs, out.heartbeatIntervalMs, "hello heartbeatIntervalMs");
        assertEq(in.bootId, out.bootId, "hello bootId");
        assertEq(in.model, out.model, "hello model");
        assertEq(in.manufacturer, out.manufacturer, "hello manufacturer");
        assertEq(in.agentVersion, out.agentVersion, "hello agentVersion");
        assertEq(in.bootIdDetail, out.bootIdDetail, "hello bootIdDetail");
        passed++;
    }

    private static void testHeartbeatRoundTrip() throws IOException {
        List<PayloadCodec.StreamHealthEntry> entries = Arrays.asList(
                new PayloadCodec.StreamHealthEntry(Protocol.STREAM_CONTROL, Protocol.STATE_RUNNING, 0, 10, 1000L),
                new PayloadCodec.StreamHealthEntry(Protocol.STREAM_LOGCAT, Protocol.STATE_RUNNING, 1, 999, 2000L),
                new PayloadCodec.StreamHealthEntry(Protocol.STREAM_INPUT, Protocol.STATE_STOPPED, 0, 0, 0L));
        PayloadCodec.Heartbeat in = new PayloadCodec.Heartbeat(500_000_000L, 400_000_000L, 1_700_000_000_000_000_000L, entries);
        byte[] encoded = PayloadCodec.encodeHeartbeat(in);
        assertEq(25 + 3 * 20, encoded.length, "heartbeat exact byte size");
        PayloadCodec.Heartbeat out = PayloadCodec.decodeHeartbeat(encoded);
        assertEq(in.boottimeNs, out.boottimeNs, "hb boottime");
        assertEq(in.monotonicNs, out.monotonicNs, "hb monotonic");
        assertEq(in.realtimeNs, out.realtimeNs, "hb realtime");
        assertEq(3, out.streams.size(), "hb streams count");
        assertEq(1, out.streams.get(1).restartCount, "hb logcat restartCount");
        assertEq(999L, out.streams.get(1).emittedSeq, "hb logcat seq");
        assertEq(2000L, out.streams.get(1).lastPtsNs, "hb logcat pts");
        passed++;
    }

    private static void testPingPongRoundTrip() throws IOException {
        long nonce = 0xDEADBEEFCAFEBABEL;
        byte[] pingBytes = PayloadCodec.encodePing(new PayloadCodec.Ping(nonce));
        assertEq(8, pingBytes.length, "ping size = 8");
        assertEq(nonce, PayloadCodec.decodePing(pingBytes).nonce, "ping nonce");

        PayloadCodec.Pong in = new PayloadCodec.Pong(nonce, 1000L, 900L, 1_700_000_000_000_000_000L, 1050L);
        byte[] pongBytes = PayloadCodec.encodePong(in);
        assertEq(40, pongBytes.length, "pong size = 40");
        PayloadCodec.Pong out = PayloadCodec.decodePong(pongBytes);
        assertEq(nonce, out.nonce, "pong nonce");
        assertEq(1000L, out.boottimeBeforeNs, "pong before");
        assertEq(900L, out.monotonicNs, "pong mono");
        assertEq(1050L, out.boottimeAfterNs, "pong after");
        passed++;
    }

    private static void testStreamStateRoundTrip() throws IOException {
        PayloadCodec.StreamState in = new PayloadCodec.StreamState(
                Protocol.STREAM_LOGCAT, Protocol.STATE_ERROR, 2, 123456789L, 55L, "process_exit:137");
        PayloadCodec.StreamState out = PayloadCodec.decodeStreamState(PayloadCodec.encodeStreamState(in));
        assertEq(in.targetStreamId, out.targetStreamId, "state stream");
        assertEq(in.state, out.state, "state enum");
        assertEq(in.restartCount, out.restartCount, "state restarts");
        assertEq(in.boottimeNs, out.boottimeNs, "state boottime");
        assertEq(in.emittedSeq, out.emittedSeq, "state seq");
        assertEq(in.reason, out.reason, "state reason");
        passed++;
    }

    private static void testLogRecordRoundTrip() throws IOException {
        byte[] body = "\u0004MyTag\u0000Hello world\u0000".getBytes(StandardCharsets.UTF_8);
        PayloadCodec.LogRecord in = new PayloadCodec.LogRecord(
                987654321L, 1700000000L, 123456789L, 421, 422, 0L, 1000L, 28, 4, 0, body);
        PayloadCodec.LogRecord out = PayloadCodec.decodeLogRecord(PayloadCodec.encodeLogRecord(in));
        assertEq(in.deviceReadBoottimeNs, out.deviceReadBoottimeNs, "log readBoottimeNs");
        assertEq(in.logSec, out.logSec, "log sec");
        assertEq(in.logNsec, out.logNsec, "log nsec");
        assertEq(in.pid, out.pid, "log pid");
        assertEq(in.tid, out.tid, "log tid");
        assertEq(in.lid, out.lid, "log lid");
        assertEq(in.uid, out.uid, "log uid");
        assertEq(in.hdrSize, out.hdrSize, "log hdrSize");
        assertEq(in.priority, out.priority, "log priority");
        assertTrue(Arrays.equals(body, out.payload), "log payload");
        passed++;
    }

    private static void testLogOverflowStatsRoundTrip() throws IOException {
        PayloadCodec.LogOverflowStats in = new PayloadCodec.LogOverflowStats(
                777777L, Protocol.COMPLETENESS_VERIFIED, 4, 0L, 12L, "main:12 system:0");
        PayloadCodec.LogOverflowStats out = PayloadCodec.decodeLogOverflowStats(PayloadCodec.encodeLogOverflowStats(in));
        assertEq(in.boottimeNs, out.boottimeNs, "stats boottime");
        assertEq(in.completeness, out.completeness, "stats completeness");
        assertEq(in.buffersCount, out.buffersCount, "stats buffersCount");
        assertEq(in.prunedDelta, out.prunedDelta, "stats prunedDelta");
        assertEq(in.prunedCumulative, out.prunedCumulative, "stats prunedCumulative");
        assertEq(in.detail, out.detail, "stats detail");
        passed++;
    }

    private static void testTouchEventRoundTrip() throws IOException {
        PayloadCodec.TouchEvent in = new PayloadCodec.TouchEvent(
                888888L, 123L, 456789, 0x0003, 0x0035, 16384, "/dev/input/event1", "[ 123.456789] EV_ABS ABS_MT_POSITION_X 00004000");
        PayloadCodec.TouchEvent out = PayloadCodec.decodeTouchEvent(PayloadCodec.encodeTouchEvent(in));
        assertEq(in.deviceReadBoottimeNs, out.deviceReadBoottimeNs, "touch boot");
        assertEq(in.eventSec, out.eventSec, "touch sec");
        assertEq(in.eventUsec, out.eventUsec, "touch usec");
        assertEq(in.evType, out.evType, "touch evType");
        assertEq(in.evCode, out.evCode, "touch evCode");
        assertEq(in.evValue, out.evValue, "touch evValue");
        assertEq(in.devicePath, out.devicePath, "touch devicePath");
        assertEq(in.rawLine, out.rawLine, "touch rawLine");
        passed++;
    }

    private static void testShutdownRoundTrip() throws IOException {
        PayloadCodec.Shutdown in = new PayloadCodec.Shutdown(999L, Protocol.SHUTDOWN_NORMAL, "host_requested");
        PayloadCodec.Shutdown out = PayloadCodec.decodeShutdown(PayloadCodec.encodeShutdown(in));
        assertEq(in.boottimeNs, out.boottimeNs, "shutdown boot");
        assertEq(in.code, out.code, "shutdown code");
        assertEq(in.reason, out.reason, "shutdown reason");
        passed++;
    }

    private static void testLoggerEntry20_24_28ByteHeaders() throws IOException {
        ByteArrayOutputStream stream = new ByteArrayOutputStream();
        byte[] p1 = "\u0004Tag1\u0000Msg1\u0000".getBytes(StandardCharsets.UTF_8);
        byte[] p2 = "\u0005Tag2\u0000Msg2\u0000".getBytes(StandardCharsets.UTF_8);
        byte[] p3 = new byte[] {0x01, 0x02, 0x03, 0x04}; // binary events buffer (lid=2)

        // v1: 20-byte header (hdr_size = 0)
        ByteBuffer b1 = ByteBuffer.allocate(20 + p1.length).order(ByteOrder.LITTLE_ENDIAN);
        b1.putShort((short) p1.length).putShort((short) 0).putInt(10).putInt(11).putInt(1000).putInt(100).put(p1);
        stream.write(b1.array());

        // v3: 24-byte header (hdr_size = 24, lid = 3 system)
        ByteBuffer b2 = ByteBuffer.allocate(24 + p2.length).order(ByteOrder.LITTLE_ENDIAN);
        b2.putShort((short) p2.length).putShort((short) 24).putInt(20).putInt(21).putInt(2000).putInt(200).putInt(3).put(p2);
        stream.write(b2.array());

        // v4: 28-byte header (hdr_size = 28, lid = 2 events, uid = 10123)
        ByteBuffer b3 = ByteBuffer.allocate(28 + p3.length).order(ByteOrder.LITTLE_ENDIAN);
        b3.putShort((short) p3.length).putShort((short) 28).putInt(30).putInt(31).putInt(3000).putInt(300).putInt(2).putInt(10123).put(p3);
        stream.write(b3.array());

        ClockSource fixedClock = new ClockSource() {
            private long t = 10_000L;
            @Override public long boottimeNs() { return t += 1000L; }
            @Override public long monotonicNs() { return t; }
            @Override public long realtimeNs() { return t; }
        };

        LoggerEntryParser parser = new LoggerEntryParser();
        ByteArrayInputStream in = new ByteArrayInputStream(stream.toByteArray());

        PayloadCodec.LogRecord r1 = parser.readNext(in, fixedClock);
        assertEq(20, r1.hdrSize, "v1 hdrSize");
        assertEq(10, r1.pid, "v1 pid");
        assertEq(Protocol.UNKNOWN_U32 & 0xFFFFFFFFL, r1.lid, "v1 unknown lid");
        assertEq(4, r1.priority, "v1 priority");
        assertEq(0, r1.recordFlags, "v1 text flags");

        PayloadCodec.LogRecord r2 = parser.readNext(in, fixedClock);
        assertEq(24, r2.hdrSize, "v3 hdrSize");
        assertEq(3L, r2.lid, "v3 lid=system");
        assertEq(Protocol.UNKNOWN_U32 & 0xFFFFFFFFL, r2.uid, "v3 unknown uid");
        assertEq(5, r2.priority, "v3 priority");

        PayloadCodec.LogRecord r3 = parser.readNext(in, fixedClock);
        assertEq(28, r3.hdrSize, "v4 hdrSize");
        assertEq(2L, r3.lid, "v4 lid=events");
        assertEq(10123L, r3.uid, "v4 uid");
        assertEq(0, r3.priority, "v4 binary event priority=0");
        assertEq(Protocol.LOG_RECORD_FLAG_BINARY_EVENT, r3.recordFlags, "v4 binary flag");

        assertTrue(parser.readNext(in, fixedClock) == null, "EOF after 3 records");
        passed++;
    }

    private static void testLogcatStatsParserApi37Format() {
        String s1 = "size/num main               system             crash              kernel             Total\n"
                + "Total    5589250/35817      2309367/13067      0/0                321952/2774        8220569/51658\n"
                + "Now      5327600/33794      2309367/13067      0/0                321952/2774        7958919/49635\n";
        String s2 = "size/num main               system             crash              kernel             Total\n"
                + "Total    5590000/35827      2309367/13067      0/0                321952/2774        8221319/51668\n"
                + "Now      5328350/33804      2309367/13067      0/0                321952/2774        7959669/49645\n";
        String s3 = "size/num main               system             crash              kernel             Total\n"
                + "Total    5600000/35927      2309367/13067      0/0                321952/2774        8231319/51768\n"
                + "Now      5328350/33804      2309367/13067      0/0                321952/2774        7959669/49645\n";

        LogcatStatsParser.Snapshot snap1 = LogcatStatsParser.parseLogcatS(s1);
        assertTrue(snap1.valid, "snap1 valid");
        assertEq(4, snap1.bufferCount, "4 buffers (excluding Total col)");
        assertEq(2023L, snap1.prunedCumulative, "snap1 prunedCumulative = 35817 - 33794");

        // Zero pruned delta -> Verified
        LogcatStatsParser.Snapshot snap2 = LogcatStatsParser.parseLogcatS(s2);
        PayloadCodec.LogOverflowStats evalOk = LogcatStatsParser.evaluate(100L, snap1, snap2, 10L, false);
        assertEq(Protocol.COMPLETENESS_VERIFIED, evalOk.completeness, "zero pruned delta => Verified");
        assertEq(0L, evalOk.prunedDelta, "prunedDelta = 0");

        // Non-zero ring pruning without per-client drop counter -> Unverified (honest)
        LogcatStatsParser.Snapshot snap3 = LogcatStatsParser.parseLogcatS(s3);
        PayloadCodec.LogOverflowStats evalPruned = LogcatStatsParser.evaluate(200L, snap2, snap3, 95L, false);
        assertEq(Protocol.COMPLETENESS_UNVERIFIED, evalPruned.completeness, "ring wrap without per-client counter => Unverified");
        assertEq(100L, evalPruned.prunedDelta, "prunedDelta = 100");

        // Reader gap -> OverflowDetected
        PayloadCodec.LogOverflowStats evalGap = LogcatStatsParser.evaluate(300L, snap2, snap3, 0L, true);
        assertEq(Protocol.COMPLETENESS_OVERFLOW_DETECTED, evalGap.completeness, "reader gap => OverflowDetected");

        // Invalid output -> Unverified
        LogcatStatsParser.Snapshot bad = LogcatStatsParser.parseLogcatS("permission denied");
        assertTrue(!bad.valid, "invalid output");
        PayloadCodec.LogOverflowStats evalBad = LogcatStatsParser.evaluate(400L, null, bad, 0L, false);
        assertEq(Protocol.COMPLETENESS_UNVERIFIED, evalBad.completeness, "invalid => Unverified");
        passed++;
    }

    private static void testGeteventParserDiscoveryAndLines() {
        String lp = "add device 1: /dev/input/event1\n"
                + "  name:     \"virtio_input_multi_touch_1\"\n"
                + "    ABS_MT_POSITION_X     : value 0, min 0, max 32767\n"
                + "    ABS_MT_POSITION_Y     : value 0, min 0, max 32767\n"
                + "add device 2: /dev/input/event2\n"
                + "  name:     \"qwerty2\"\n"
                + "    KEY_POWER\n";
        Set<String> devices = GeteventParser.discoverTouchDevices(lp);
        assertEq(1, devices.size(), "only 1 touch device");
        assertTrue(devices.contains("/dev/input/event1"), "contains event1");

        String line = "[    45.678901] /dev/input/event1: EV_ABS       ABS_MT_POSITION_X    00004000";
        PayloadCodec.TouchEvent ev = GeteventParser.parseEventLine(line, null, devices, 999_000L);
        assertTrue(ev != null, "parsed touch event");
        assertEq(45L, ev.eventSec, "eventSec");
        assertEq(678901, ev.eventUsec, "eventUsec");
        assertEq(0x0003, ev.evType, "EV_ABS");
        assertEq(0x0035, ev.evCode, "ABS_MT_POSITION_X");
        assertEq(0x4000, ev.evValue, "evValue");
        assertEq("/dev/input/event1", ev.devicePath, "devicePath");

        // Non-touch device line filtered out
        String keyLine = "[    45.678902] /dev/input/event2: EV_KEY       KEY_POWER            DOWN";
        assertTrue(GeteventParser.parseEventLine(keyLine, null, devices, 999_001L) == null, "filtered non-touch device");

        // Single-device mode omits device prefix
        String singleLine = "[    45.678903] EV_SYN       SYN_REPORT           00000000";
        PayloadCodec.TouchEvent syn = GeteventParser.parseEventLine(singleLine, "/dev/input/event1", Collections.singleton("/dev/input/event1"), 999_002L);
        assertTrue(syn != null, "single-device SYN_REPORT");
        assertEq(0x0000, syn.evType, "EV_SYN");
        assertEq(0x0000, syn.evCode, "SYN_REPORT");
        passed++;
    }

    private static void assertStrictUtf8(byte[] bytes, String msg) {
        CharsetDecoder dec = StandardCharsets.UTF_8.newDecoder()
                .onMalformedInput(CodingErrorAction.REPORT)
                .onUnmappableCharacter(CodingErrorAction.REPORT);
        try {
            dec.decode(ByteBuffer.wrap(bytes));
        } catch (CharacterCodingException e) {
            throw new AssertionError("Assertion failed: " + msg + " (" + e + ")");
        }
    }

    private static String repeat(String unit, int count) {
        StringBuilder sb = new StringBuilder(unit.length() * count);
        for (int i = 0; i < count; i++) {
            sb.append(unit);
        }
        return sb.toString();
    }

    private static void testStr16TruncatesOnUtf8CodepointBoundary() throws IOException {
        // "あ" = 3 UTF-8 bytes, "😀" = 4 UTF-8 bytes (surrogate pair in UTF-16).
        byte[] cut = PayloadCodec.truncateUtf8Bytes("aあ😀", 6);
        assertEq(4, cut.length, "6-byte cap must back off to the end of 'aあ' (4 bytes), never split 😀");
        assertEq("aあ", new String(cut, StandardCharsets.UTF_8), "truncated text");
        assertEq("ab", PayloadCodec.truncateUtf8("ab", 10), "short string untouched");

        // 1 ASCII byte + 3-byte codepoints: a naive cut at 65,535 bytes would split a codepoint.
        String huge = "a" + repeat("あ", 30000); // 90,001 bytes
        assertTrue(PayloadCodec.wouldTruncateStr16(huge), "huge reason would be truncated");
        assertTrue(!PayloadCodec.wouldTruncateStr16("ok", null), "short values are not truncated");
        PayloadCodec.StreamState st = new PayloadCodec.StreamState(
                Protocol.STREAM_LOGCAT, Protocol.STATE_ERROR, 1, 5L, 7L, huge);
        byte[] enc = PayloadCodec.encodeStreamState(st);
        ByteBuffer b = ByteBuffer.wrap(enc).order(ByteOrder.LITTLE_ENDIAN);
        b.position(1 + 1 + 2 + 8 + 4);
        int len = b.getShort() & 0xFFFF;
        assertEq(65_533, len, "str16 length backs off to the last whole 3-byte codepoint");
        byte[] str = new byte[len];
        b.get(str);
        assertStrictUtf8(str, "truncated str16 must stay valid UTF-8 for strict host decoders");
        PayloadCodec.StreamState out = PayloadCodec.decodeStreamState(enc);
        assertEq(21_845, out.reason.length(), "decoded truncated reason codepoints");
        passed++;
    }

    private static void testGeteventRawLineCappedAt256Utf8Bytes() {
        Set<String> devices = Collections.singleton("/dev/input/event1");
        String line = "[    45.678901] /dev/input/event1: EV_ABS       ABS_MT_POSITION_X    00000123 "
                + repeat("é", 200); // 2-byte codepoints push the line past 256 UTF-8 bytes
        assertTrue(PayloadCodec.exceedsUtf8Bytes(line.trim(), GeteventParser.MAX_RAW_LINE_UTF8_BYTES),
                "test line exceeds 256 UTF-8 bytes");
        PayloadCodec.TouchEvent ev = GeteventParser.parseEventLine(line, null, devices, 1L);
        assertTrue(ev != null, "line parses");
        byte[] raw = ev.rawLine.getBytes(StandardCharsets.UTF_8);
        assertTrue(raw.length <= 256, "raw_line capped at 256 UTF-8 bytes (was " + raw.length + ")");
        assertTrue(raw.length >= 255, "raw_line keeps as much as fits");
        assertStrictUtf8(raw, "raw_line never splits a codepoint");
        passed++;
    }

    /** OutputStream that blocks every write until {@link #open()} is called. */
    private static final class GatedOutputStream extends java.io.OutputStream {
        private final ByteArrayOutputStream sink = new ByteArrayOutputStream();
        private final java.util.concurrent.CountDownLatch gate = new java.util.concurrent.CountDownLatch(1);
        private final java.util.concurrent.CountDownLatch firstWriteEntered = new java.util.concurrent.CountDownLatch(1);

        @Override
        public void write(int b) throws IOException {
            write(new byte[] {(byte) b}, 0, 1);
        }

        @Override
        public void write(byte[] b, int off, int len) throws IOException {
            firstWriteEntered.countDown();
            try {
                gate.await();
            } catch (InterruptedException ie) {
                throw new IOException(ie);
            }
            synchronized (sink) {
                sink.write(b, off, len);
            }
        }

        void open() {
            gate.countDown();
        }

        byte[] bytes() {
            synchronized (sink) {
                return sink.toByteArray();
            }
        }
    }

    private static final class FixedClock implements ClockSource {
        @Override
        public long boottimeNs() {
            return 4242L;
        }

        @Override
        public long monotonicNs() {
            return 4000L;
        }

        @Override
        public long realtimeNs() {
            return 1_700_000_000_000_000_000L;
        }
    }

    private static void testFrameWriterStream0PriorityAndStream1OverflowNeverBlocks() throws Exception {
        GatedOutputStream gated = new GatedOutputStream();
        WireFrame.FrameWriter w = new WireFrame.FrameWriter(gated, new FixedClock(), 8, 4);

        // Host back-pressure: the writer thread is stuck inside the first (Stream 1) write.
        assertEq(1L, w.writeFrame(Protocol.STREAM_LOGCAT, Protocol.MSG_LOG_RECORD, 0, new byte[] {1}), "log 1 seq");
        assertTrue(gated.firstWriteEntered.await(5, java.util.concurrent.TimeUnit.SECONDS), "writer thread entered write");

        // Fill the Stream 1 queue (capacity 4), then burst: excess frames are dropped without blocking.
        long start = System.nanoTime();
        int accepted = 0;
        int dropped = 0;
        for (int i = 0; i < 50; i++) {
            long seq = w.writeFrame(Protocol.STREAM_LOGCAT, Protocol.MSG_LOG_RECORD, 0, new byte[] {2});
            if (seq == 0L) {
                dropped++;
            } else {
                accepted++;
            }
        }
        assertEq(4, accepted, "Stream 1 queue accepts exactly its capacity");
        assertEq(46, dropped, "Stream 1 overflow drops (seq 0) instead of blocking");
        assertEq(46L, w.droppedLogFrames(), "dropped counter");

        // Stream 0 Heartbeat/Pong never block behind the full Stream 1 queue.
        long hbSeq = w.writeFrame(Protocol.STREAM_CONTROL, Protocol.MSG_HEARTBEAT, 0, new byte[] {9});
        long pongSeq = w.writeFrame(Protocol.STREAM_CONTROL, Protocol.MSG_PONG, Protocol.FLAG_URGENT, new byte[] {8});
        long elapsedMs = (System.nanoTime() - start) / 1_000_000L;
        assertEq(1L, hbSeq, "heartbeat seq");
        assertEq(2L, pongSeq, "pong seq");
        assertTrue(elapsedMs < 2000L, "producers never blocked on a stuck transport (" + elapsedMs + " ms)");

        gated.open();
        assertTrue(w.awaitDrained(5000L), "writer drains after back-pressure clears");

        ByteArrayInputStream in = new ByteArrayInputStream(gated.bytes());
        java.util.List<WireFrame> frames = new java.util.ArrayList<>();
        WireFrame f;
        while ((f = WireFrame.readFrom(in)) != null) {
            frames.add(f);
        }
        // 1 in-flight log + 2 control + 4 queued logs + 1 overflow notice.
        assertEq(8, frames.size(), "total frames written");
        assertEq(Protocol.STREAM_LOGCAT, frames.get(0).streamId, "in-flight log frame first");
        assertEq(Protocol.MSG_HEARTBEAT, frames.get(1).msgType, "heartbeat jumps ahead of queued logs");
        assertEq(Protocol.MSG_PONG, frames.get(2).msgType, "pong jumps ahead of queued logs");
        WireFrame notice = null;
        long prevSeq = 1L;
        for (int i = 3; i < frames.size(); i++) {
            WireFrame lf = frames.get(i);
            assertEq(Protocol.STREAM_LOGCAT, lf.streamId, "remaining frames are Stream 1");
            assertEq(prevSeq + 1, lf.seq, "Stream 1 seq stays contiguous (dropped frames consume no seq)");
            prevSeq = lf.seq;
            if (lf.msgType == Protocol.MSG_LOG_OVERFLOW_STATS) {
                notice = lf;
            }
        }
        assertTrue(notice != null, "overflow notice emitted on Stream 1");
        PayloadCodec.LogOverflowStats st = PayloadCodec.decodeLogOverflowStats(notice.payload);
        assertEq(Protocol.COMPLETENESS_OVERFLOW_DETECTED, st.completeness, "notice completeness");
        assertEq("writer_queue_overflow:dropped=46", st.detail, "notice detail");
        assertEq(4242L, st.boottimeNs, "notice stamped with device boottime");
        w.closeQuietly();
        passed++;
    }

    private static void testFrameWriterSurfacesTransportFailure() throws Exception {
        java.io.OutputStream broken = new java.io.OutputStream() {
            @Override
            public void write(int b) throws IOException {
                throw new IOException("Broken pipe");
            }

            @Override
            public void write(byte[] b, int off, int len) throws IOException {
                throw new IOException("Broken pipe");
            }
        };
        WireFrame.FrameWriter w = new WireFrame.FrameWriter(broken);
        w.writeFrame(Protocol.STREAM_CONTROL, Protocol.MSG_HEARTBEAT, 0, new byte[1]);
        assertTrue(!w.awaitDrained(5000L), "awaitDrained reports failure");
        boolean threw = false;
        try {
            w.writeFrame(Protocol.STREAM_CONTROL, Protocol.MSG_HEARTBEAT, 0, new byte[1]);
        } catch (IOException expected) {
            threw = true;
        }
        assertTrue(threw, "writes after a broken pipe throw IOException so heartbeat stops the session");
        passed++;
    }

    private static PayloadCodec.LogRecord logAt(long sec, long nsec, int pid, String msg) {
        return new PayloadCodec.LogRecord(
                0L, sec, nsec, pid, pid, 0L, 1000L, 28, 4, 0, msg.getBytes(StandardCharsets.UTF_8));
    }

    private static void testLogcatResumeTrackerDedupsBoundaryAndDetectsEviction() {
        LoggerEntryParser.ResumeTracker t = new LoggerEntryParser.ResumeTracker();
        assertEq("1", t.nextTArgument(), "initial spawn uses -T 1");
        t.beginResume();
        LoggerEntryParser.ResumeTracker.Decision emit = LoggerEntryParser.ResumeTracker.Decision.EMIT;
        LoggerEntryParser.ResumeTracker.Decision skip = LoggerEntryParser.ResumeTracker.Decision.SKIP_DUPLICATE;
        LoggerEntryParser.ResumeTracker.Decision gap = LoggerEntryParser.ResumeTracker.Decision.EMIT_WITH_GAP;
        assertTrue(t.evaluate(logAt(100, 5, 1, "a")) == emit, "first line emitted");
        assertTrue(t.evaluate(logAt(100, 7, 1, "b")) == emit, "second line emitted");
        assertTrue(t.evaluate(logAt(100, 7, 2, "c")) == emit, "same-ns distinct line emitted");
        assertEq("100.000000007", t.nextTArgument(), "respawn uses -T <sec>.<9-digit nsec>");

        // Respawn: logcat -T replays entries at the boundary timestamp; already-sent ones are skipped.
        t.beginResume();
        assertTrue(t.evaluate(logAt(100, 5, 1, "a")) == skip, "older replayed line skipped");
        assertTrue(t.evaluate(logAt(100, 7, 1, "b")) == skip, "boundary duplicate skipped");
        assertTrue(t.evaluate(logAt(100, 7, 2, "c")) == skip, "second boundary duplicate skipped");
        assertTrue(t.evaluate(logAt(100, 7, 3, "d")) == emit, "unsent line at boundary ns emitted");
        assertTrue(t.evaluate(logAt(101, 0, 1, "e")) == emit, "lines after boundary emitted without gap");

        // Respawn where the boundary entry was already evicted from the ring buffer: real loss.
        t.beginResume();
        assertTrue(t.evaluate(logAt(150, 0, 1, "f")) == gap, "evicted boundary reports gap");
        assertTrue(t.evaluate(logAt(150, 1, 1, "g")) == emit, "subsequent lines emitted normally");
        passed++;
    }

    private static void testStreamHealthEmittedSeqNeverMovesBackwards() {
        StreamHealthTracker h = new StreamHealthTracker();
        h.recordEmit(Protocol.STREAM_LOGCAT, 5L, 100L);
        h.recordEmit(Protocol.STREAM_LOGCAT, 3L, 50L); // out-of-order thread
        h.recordEmit(Protocol.STREAM_LOGCAT, 0L, 200L); // dropped frame (seq 0)
        assertEq(5L, h.getEmittedSeq(Protocol.STREAM_LOGCAT), "emitted_seq monotonic and ignores dropped frames");
        passed++;
    }

    private static void testBootIdResolverSelinuxEaccesFallbacks() {
        // 1. Readable /proc/sys/kernel/random/boot_id -> BOOT_ID_KERNEL_PROC
        BootIdResolver.ProcReader readable = new BootIdResolver.ProcReader() {
            @Override
            public String readFirstLine(String path) {
                return "c729eaec-5428-4a38-b957-441109da33f7\n";
            }

            @Override
            public long readProcStatBtime(String path) {
                return 1700000000L;
            }
        };
        BootIdResolver.Result ok = BootIdResolver.resolve(
                readable, BootIdResolver.DEFAULT_PROC_BOOT_ID, BootIdResolver.DEFAULT_PROC_STAT, 7, "reboot");
        assertEq(Protocol.BOOT_ID_KERNEL_PROC, ok.bootIdSource, "readable boot_id source");
        assertEq("c729eaec-5428-4a38-b957-441109da33f7", ok.bootId, "readable boot_id");
        assertEq("", ok.bootIdDetail, "readable boot_id has empty detail");

        // 2. Android 9 SELinux EACCES on /proc/sys/kernel/random/boot_id -> deterministic boot_count + btime fallback
        BootIdResolver.ProcReader eaccesBootId = new BootIdResolver.ProcReader() {
            @Override
            public String readFirstLine(String path) throws IOException {
                throw new IOException("/proc/sys/kernel/random/boot_id: open failed: EACCES (Permission denied)");
            }

            @Override
            public long readProcStatBtime(String path) {
                return 1712345678L;
            }
        };
        BootIdResolver.Result fallbackBc = BootIdResolver.resolve(
                eaccesBootId, BootIdResolver.DEFAULT_PROC_BOOT_ID, BootIdResolver.DEFAULT_PROC_STAT, 12, "reboot");
        assertEq(Protocol.BOOT_ID_FALLBACK_BOOT_COUNT_BTIME, fallbackBc.bootIdSource, "EACCES fallback source");
        assertEq("fallback:bc=12:btime=1712345678", fallbackBc.bootId, "EACCES deterministic fallback id");
        assertTrue(fallbackBc.bootIdDetail.contains("EACCES"), "detail preserves EACCES diagnostic");

        // 3. Double SELinux EACCES (/proc/sys/kernel/random/boot_id AND /proc/stat) + unreadable boot_count
        //    -> honest random UUID fallback with bootreason diagnostic (never crashes startup!)
        BootIdResolver.ProcReader doubleEacces = new BootIdResolver.ProcReader() {
            @Override
            public String readFirstLine(String path) throws IOException {
                throw new IOException("EACCES");
            }

            @Override
            public long readProcStatBtime(String path) throws IOException {
                throw new IOException("EACCES");
            }
        };
        BootIdResolver.Result fallbackRand = BootIdResolver.resolve(
                doubleEacces, BootIdResolver.DEFAULT_PROC_BOOT_ID, BootIdResolver.DEFAULT_PROC_STAT, -1, "cold_boot");
        assertEq(Protocol.BOOT_ID_FALLBACK_RANDOM_UUID, fallbackRand.bootIdSource, "double EACCES source");
        assertTrue(fallbackRand.bootId.startsWith("fallback:random:"), "random fallback prefix");
        assertTrue(fallbackRand.bootIdDetail.contains("proc_boot_id_error"), "detail includes boot_id error");
        assertTrue(fallbackRand.bootIdDetail.contains("proc_stat_error"), "detail includes proc_stat error");
        assertTrue(fallbackRand.bootIdDetail.contains("bootreason=cold_boot"), "detail includes bootreason");
        passed++;
    }

    private static void testLogcatWorkerRestrictedBufferFallbackCommandAndAos9Stats() {
        String[] defaultCmd = LogcatWorker.buildLogcatCommand(false, "1");
        assertTrue(Arrays.equals(new String[] {"logcat", "-b", "all", "-B", "-T", "1"}, defaultCmd),
                "default logcat command uses -b all");
        String[] fallbackCmd = LogcatWorker.buildLogcatCommand(true, "1700000000.000000042");
        assertTrue(Arrays.equals(
                new String[] {"logcat", "-b", "main,system,crash,events", "-B", "-T", "1700000000.000000042"},
                fallbackCmd),
                "restricted AOS 9 fallback command uses main,system,crash,events");

        // Android 9 (API 28) `logcat -S` format includes `Loggable` row alongside `Total` and `Now`.
        String aos9Stats = "size/num main               system             crash              events             Total\n"
                + "Total    1200000/12000      200000/2000        0/0                300000/3000        1700000/17000\n"
                + "Now      1200000/12000      200000/2000        0/0                300000/3000        1700000/17000\n"
                + "Loggable 1200000/12000      200000/2000        0/0                300000/3000        1700000/17000\n";
        LogcatStatsParser.Snapshot snap = LogcatStatsParser.parseLogcatS(aos9Stats);
        assertTrue(snap.valid, "AOS 9 logcat -S parses cleanly");
        assertEq(4, snap.bufferCount, "4 buffers excluding Total");
        assertEq(0L, snap.prunedCumulative, "0 pruned");
        passed++;
    }

    private static void assertTrue(boolean cond, String msg) {
        if (!cond) {
            throw new AssertionError("Assertion failed: " + msg);
        }
    }

    private static void assertEq(long expected, long actual, String msg) {
        if (expected != actual) {
            throw new AssertionError("Assertion failed (" + msg + "): expected=" + expected + " actual=" + actual);
        }
    }

    private static void assertEq(boolean expected, boolean actual, String msg) {
        if (expected != actual) {
            throw new AssertionError("Assertion failed (" + msg + "): expected=" + expected + " actual=" + actual);
        }
    }

    private static void assertEq(String expected, String actual, String msg) {
        if (!expected.equals(actual)) {
            throw new AssertionError("Assertion failed (" + msg + "): expected='" + expected + "' actual='" + actual + "'");
        }
    }
}
