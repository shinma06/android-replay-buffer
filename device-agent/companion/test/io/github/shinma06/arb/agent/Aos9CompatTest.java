package io.github.shinma06.arb.agent;

import java.io.IOException;
import java.util.Arrays;

/**
 * Pure-Java host unit tests for Android 9 (API 28) compatibility hardening:
 * SELinux {@code EACCES} fallbacks in {@link BootIdResolver} and restricted
 * {@code logcat} buffer fallbacks / API 28 {@code logcat -S} stats parsing.
 */
public final class Aos9CompatTest {
    private static int passed = 0;

    public static void main(String[] args) {
        testBootIdResolverSelinuxEaccesFallbacks();
        testLogcatWorkerRestrictedBufferFallbackCommandAndAos9Stats();
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

    private static void assertEq(String expected, String actual, String msg) {
        if (!expected.equals(actual)) {
            throw new AssertionError("Assertion failed (" + msg + "): expected='" + expected + "' actual='" + actual + "'");
        }
    }
}
