package io.github.shinma06.arb.agent;

import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicInteger;

/**
 * Pure-Java host unit tests for Android 9 (API 28) compatibility hardening:
 * SELinux {@code EACCES} fallbacks in {@link BootIdResolver}, restricted
 * {@code logcat} buffer fallbacks / API 28 {@code logcat -S} stats parsing,
 * M10 24-byte legacy logger_entry unknown-UID invariant, and M11 stats probe
 * child-process ownership/cleanup on {@code stop()} and timeout.
 */
public final class Aos9CompatTest {
    private static int passed = 0;

    public static void main(String[] args) throws Exception {
        testBootIdResolverSelinuxEaccesFallbacks();
        testLogcatWorkerRestrictedBufferFallbackCommandAndAos9Stats();
        testM10Legacy24ByteHeadersStrictlyEmitUnknownUidNeverSystem1000();
        testM11StatsProbeChildDestroyedAndReapedOnStopMidLine();
        testM11StatsProbeTimeoutDestroysHungChildAndRecoversStatsLoop();
        System.out.println("Aos9CompatTest: ALL " + passed + " TESTS PASSED");
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
        assertEq("fallback:bc=12", fallbackBc.bootId, "EACCES deterministic fallback id");
        assertTrue(fallbackBc.bootIdDetail.contains("EACCES"), "detail preserves EACCES diagnostic");

        // 2b. /proc/stat btime shifts when the wall clock is stepped (NITZ/NTP, manual date change)
        //     within the same boot: the boot_count-based id must stay identical so a reconnect is
        //     never misclassified as DEVICE_REBOOT.
        BootIdResolver.ProcReader eaccesBootIdClockStepped = new BootIdResolver.ProcReader() {
            @Override
            public String readFirstLine(String path) throws IOException {
                throw new IOException("/proc/sys/kernel/random/boot_id: open failed: EACCES (Permission denied)");
            }

            @Override
            public long readProcStatBtime(String path) {
                return 1712345678L + 3600L;
            }
        };
        BootIdResolver.Result afterClockStep = BootIdResolver.resolve(
                eaccesBootIdClockStepped, BootIdResolver.DEFAULT_PROC_BOOT_ID, BootIdResolver.DEFAULT_PROC_STAT, 12, "reboot");
        assertEq(fallbackBc.bootId, afterClockStep.bootId, "fallback id is stable across wall-clock steps");
        assertEq(Protocol.BOOT_ID_FALLBACK_BOOT_COUNT_BTIME, afterClockStep.bootIdSource, "clock-step fallback source");

        // 2c. boot_count unreadable -> btime-only fallback (best effort, still deterministic).
        BootIdResolver.Result btimeOnly = BootIdResolver.resolve(
                eaccesBootId, BootIdResolver.DEFAULT_PROC_BOOT_ID, BootIdResolver.DEFAULT_PROC_STAT, -1, "reboot");
        assertEq(Protocol.BOOT_ID_FALLBACK_BOOT_COUNT_BTIME, btimeOnly.bootIdSource, "btime-only fallback source");
        assertEq("fallback:btime=1712345678", btimeOnly.bootId, "btime-only fallback id");

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

    /**
     * M10 regression: 24-byte (v2/v3) and 20-byte (v1) legacy {@code logger_entry} headers carry
     * no UID field and MUST strictly emit {@code UNKNOWN_U32} (never spoofing {@code 1000} /
     * {@code AID_SYSTEM} even immediately after a 28-byte {@code uid=1000} record on the same
     * parser instance or for {@code lid=2} events / {@code lid=3} system buffers).
     */
    private static void testM10Legacy24ByteHeadersStrictlyEmitUnknownUidNeverSystem1000() throws IOException {
        ByteArrayOutputStream stream = new ByteArrayOutputStream();
        byte[] binaryAmCrashLike = new byte[] {0x07, 0x75, 0x00, 0x00, 0x03, 0x00};
        byte[] systemText = "\u0006ActivityManager\u0000Crash\u0000".getBytes(StandardCharsets.UTF_8);

        // Record 1: 28-byte v4 header with lid=2 (events) and confirmed uid=1000 (system_server).
        // Deliberately populates extraBuf[4..7] with 1000 first to verify no stale-buffer bleed!
        ByteBuffer v4System = ByteBuffer.allocate(28 + binaryAmCrashLike.length).order(ByteOrder.LITTLE_ENDIAN);
        v4System.putShort((short) binaryAmCrashLike.length)
                .putShort((short) 28)
                .putInt(500).putInt(501)
                .putInt(1700000001).putInt(10)
                .putInt(2)    // LOG_ID_EVENTS
                .putInt(1000) // AID_SYSTEM
                .put(binaryAmCrashLike);
        stream.write(v4System.array());

        // Record 2: 24-byte legacy v3 header with lid=2 (events) — NO uid field!
        ByteBuffer v3Events = ByteBuffer.allocate(24 + binaryAmCrashLike.length).order(ByteOrder.LITTLE_ENDIAN);
        v3Events.putShort((short) binaryAmCrashLike.length)
                .putShort((short) 24)
                .putInt(600).putInt(601)
                .putInt(1700000002).putInt(20)
                .putInt(2) // LOG_ID_EVENTS
                .put(binaryAmCrashLike);
        stream.write(v3Events.array());

        // Record 3: 24-byte legacy v3 header with lid=3 (system) — NO uid field!
        ByteBuffer v3System = ByteBuffer.allocate(24 + systemText.length).order(ByteOrder.LITTLE_ENDIAN);
        v3System.putShort((short) systemText.length)
                .putShort((short) 24)
                .putInt(700).putInt(701)
                .putInt(1700000003).putInt(30)
                .putInt(3) // LOG_ID_SYSTEM
                .put(systemText);
        stream.write(v3System.array());

        // Record 4: 20-byte legacy v1 header (hdr_size=0) — NO lid and NO uid!
        ByteBuffer v1Base = ByteBuffer.allocate(20 + systemText.length).order(ByteOrder.LITTLE_ENDIAN);
        v1Base.putShort((short) systemText.length)
                .putShort((short) 0)
                .putInt(800).putInt(801)
                .putInt(1700000004).putInt(40)
                .put(systemText);
        stream.write(v1Base.array());

        ClockSource clock = fixedClock();
        LoggerEntryParser parser = new LoggerEntryParser();
        ByteArrayInputStream in = new ByteArrayInputStream(stream.toByteArray());
        long expectedUnknown = Protocol.UNKNOWN_U32 & 0xFFFFFFFFL;

        PayloadCodec.LogRecord r1 = parser.readNext(in, clock);
        assertEq(28, r1.hdrSize, "v4 hdrSize");
        assertEq(1000L, r1.uid, "v4 preserves confirmed uid=1000");

        PayloadCodec.LogRecord r2 = parser.readNext(in, clock);
        assertEq(24, r2.hdrSize, "v3 events hdrSize");
        assertEq(2L, r2.lid, "v3 events lid");
        assertEq(expectedUnknown, r2.uid, "24-byte events record strictly emits UNKNOWN_U32");
        assertTrue(r2.uid != 1000L, "24-byte events record must NEVER spoof uid=1000");
        assertEq(Protocol.LOG_RECORD_FLAG_BINARY_EVENT, r2.recordFlags, "24-byte events keeps binary flag");
        PayloadCodec.LogRecord r2Decoded = PayloadCodec.decodeLogRecord(PayloadCodec.encodeLogRecord(r2));
        assertEq(expectedUnknown, r2Decoded.uid, "UNKNOWN_U32 round-trips intact on wire");

        PayloadCodec.LogRecord r3 = parser.readNext(in, clock);
        assertEq(24, r3.hdrSize, "v3 system hdrSize");
        assertEq(3L, r3.lid, "v3 system lid");
        assertEq(expectedUnknown, r3.uid, "24-byte system record strictly emits UNKNOWN_U32");
        assertTrue(r3.uid != 1000L, "24-byte system record must NEVER spoof uid=1000");

        PayloadCodec.LogRecord r4 = parser.readNext(in, clock);
        assertEq(20, r4.hdrSize, "v1 hdrSize");
        assertEq(expectedUnknown, r4.lid, "v1 emits UNKNOWN_U32 lid");
        assertEq(expectedUnknown, r4.uid, "v1 strictly emits UNKNOWN_U32 uid");
        assertTrue(r4.uid != 1000L, "v1 record must NEVER spoof uid=1000");
        passed++;
    }

    /**
     * M11 regression 1: A stats-probe child blocking mid-line on stdout must be forcibly
     * destroyed, have its streams closed and exit reaped, and let the stats thread terminate
     * cleanly when {@link LogcatWorker#stop()} is called.
     */
    private static void testM11StatsProbeChildDestroyedAndReapedOnStopMidLine() throws Exception {
        BlockingMidLineProcess fakeLogcat = new BlockingMidLineProcess(new byte[0], true);
        BlockingMidLineProcess fakeStats = new BlockingMidLineProcess(
                "size/num main\npartial_without_newline".getBytes(StandardCharsets.UTF_8), true);

        ByteArrayOutputStream wireOut = new ByteArrayOutputStream();
        WireFrame.FrameWriter writer = new WireFrame.FrameWriter(wireOut);
        StreamHealthTracker health = new StreamHealthTracker();
        LogcatWorker worker = new LogcatWorker(
                writer,
                fixedClock(),
                health,
                cmd -> fakeLogcat,
                cmd -> fakeStats,
                /* statsIntervalMs = */ 5_000L,
                /* statsProbeTimeoutMs = */ 30_000L);

        worker.start();
        try {
            assertTrue(fakeStats.blockedMidRead.await(5, TimeUnit.SECONDS), "stats probe blocked mid-line");
            worker.stop();
            assertTrue(worker.awaitThreadsTerminated(5_000L), "reader and stats threads terminated after stop()");
            assertTrue(fakeStats.destroyedForcibly.get(), "stats probe child was destroyForcibly()'d");
            assertTrue(fakeStats.stdoutClosed.get(), "stats probe stdout closed");
            assertTrue(fakeStats.reaped.get(), "stats probe child reaped via waitFor");
            assertTrue(fakeLogcat.destroyedForcibly.get(), "main logcat child destroyed on stop()");
        } finally {
            worker.stop();
            writer.closeQuietly();
        }
        passed++;
    }

    /**
     * M11 regression 2: A hung stats-probe child must be forcibly destroyed and reaped upon
     * probe timeout so the stats thread never stalls and continues collecting subsequent snapshots.
     */
    private static void testM11StatsProbeTimeoutDestroysHungChildAndRecoversStatsLoop() throws Exception {
        BlockingMidLineProcess fakeLogcat = new BlockingMidLineProcess(new byte[0], true);
        BlockingMidLineProcess hungProbe = new BlockingMidLineProcess(
                "size/num main\nhung_mid_line".getBytes(StandardCharsets.UTF_8), true);
        String validStatsOutput = "size/num main               system             Total\n"
                + "Total    12000/120          2000/20            14000/140\n"
                + "Now      12000/120          2000/20            14000/140\n";
        AtomicInteger statsInvocations = new AtomicInteger(0);
        CountDownLatch recoveredStatsRead = new CountDownLatch(1);

        ByteArrayOutputStream wireOut = new ByteArrayOutputStream();
        WireFrame.FrameWriter writer = new WireFrame.FrameWriter(wireOut);
        StreamHealthTracker health = new StreamHealthTracker();
        LogcatWorker worker = new LogcatWorker(
                writer,
                fixedClock(),
                health,
                cmd -> fakeLogcat,
                cmd -> {
                    int n = statsInvocations.incrementAndGet();
                    if (n == 1) {
                        return hungProbe;
                    }
                    recoveredStatsRead.countDown();
                    return new BlockingMidLineProcess(validStatsOutput.getBytes(StandardCharsets.UTF_8), false);
                },
                /* statsIntervalMs = */ 30L,
                /* statsProbeTimeoutMs = */ 120L);

        worker.start();
        try {
            assertTrue(hungProbe.blockedMidRead.await(5, TimeUnit.SECONDS), "first probe entered hung mid-line read");
            assertTrue(recoveredStatsRead.await(5, TimeUnit.SECONDS),
                    "stats loop timed out hung probe and recovered to run next probe");
            assertTrue(hungProbe.destroyedForcibly.get(), "hung probe was destroyForcibly()'d on timeout");
            assertTrue(hungProbe.stdoutClosed.get(), "hung probe stdout was closed on timeout");
            assertTrue(hungProbe.reaped.get(), "hung probe was reaped on timeout");
            worker.stop();
            assertTrue(worker.awaitThreadsTerminated(5_000L), "worker threads stopped cleanly after recovery");
        } finally {
            worker.stop();
            writer.closeQuietly();
        }
        passed++;
    }

    private static ClockSource fixedClock() {
        return new ClockSource() {
            private long t = 1_000_000L;
            @Override public long boottimeNs() { return t += 1_000L; }
            @Override public long monotonicNs() { return t; }
            @Override public long realtimeNs() { return t; }
        };
    }

    /**
     * Deterministic fake {@link Process} that emits {@code initialBytes} and optionally blocks
     * mid-line until {@link #destroyForcibly()} / {@link #destroy()} or {@code InputStream.close()}
     * is invoked, modeling a stuck {@code logcat -S} / {@code logcat -g} pipe.
     */
    private static final class BlockingMidLineProcess extends Process {
        private final byte[] initialBytes;
        private final boolean blockAfterInitial;
        private final Object monitor = new Object();
        final CountDownLatch blockedMidRead = new CountDownLatch(1);
        final AtomicBoolean destroyedForcibly = new AtomicBoolean(false);
        final AtomicBoolean stdoutClosed = new AtomicBoolean(false);
        final AtomicBoolean reaped = new AtomicBoolean(false);
        private volatile boolean alive = true;

        private final InputStream stdout = new InputStream() {
            private int offset = 0;

            @Override
            public int read() throws IOException {
                byte[] one = new byte[1];
                int n = read(one, 0, 1);
                return n < 0 ? -1 : (one[0] & 0xFF);
            }

            @Override
            public int read(byte[] b, int off, int len) throws IOException {
                if (offset < initialBytes.length) {
                    int count = Math.min(len, initialBytes.length - offset);
                    System.arraycopy(initialBytes, offset, b, off, count);
                    offset += count;
                    return count;
                }
                if (!blockAfterInitial) {
                    alive = false;
                    return -1;
                }
                blockedMidRead.countDown();
                synchronized (monitor) {
                    while (alive && !stdoutClosed.get()) {
                        try {
                            // Deliberately ignore Thread.interrupt() while still alive/open,
                            // matching native pipe readLine() which is not guaranteed to unblock
                            // on Java thread interrupt until the child is destroyed or closed.
                            monitor.wait(100L);
                        } catch (InterruptedException ignored) {
                            // Keep waiting until process is destroyed or stream closed!
                        }
                    }
                }
                if (stdoutClosed.get()) {
                    throw new IOException("Stream closed");
                }
                return -1;
            }

            @Override
            public void close() {
                stdoutClosed.set(true);
                synchronized (monitor) {
                    monitor.notifyAll();
                }
            }
        };

        private final InputStream stderr = new ByteArrayInputStream(new byte[0]);
        private final OutputStream stdin = new ByteArrayOutputStream();

        BlockingMidLineProcess(byte[] initialBytes, boolean blockAfterInitial) {
            this.initialBytes = initialBytes;
            this.blockAfterInitial = blockAfterInitial;
            if (!blockAfterInitial) {
                this.alive = false;
            }
        }

        @Override public OutputStream getOutputStream() { return stdin; }
        @Override public InputStream getInputStream() { return stdout; }
        @Override public InputStream getErrorStream() { return stderr; }

        @Override
        public int waitFor() {
            reaped.set(true);
            return alive ? 137 : 0;
        }

        @Override
        public boolean waitFor(long timeout, TimeUnit unit) {
            reaped.set(true);
            return !alive;
        }

        @Override
        public int exitValue() {
            if (alive) {
                throw new IllegalThreadStateException("still running");
            }
            return destroyedForcibly.get() ? 137 : 0;
        }

        @Override
        public void destroy() {
            alive = false;
            synchronized (monitor) {
                monitor.notifyAll();
            }
        }

        @Override
        public Process destroyForcibly() {
            destroyedForcibly.set(true);
            destroy();
            return this;
        }

        @Override
        public boolean isAlive() {
            return alive;
        }
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

    private static void assertEq(String expected, String actual, String msg) {
        if (!expected.equals(actual)) {
            throw new AssertionError("Assertion failed (" + msg + "): expected='" + expected + "' actual='" + actual + "'");
        }
    }
}
