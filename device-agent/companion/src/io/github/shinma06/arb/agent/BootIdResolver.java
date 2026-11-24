package io.github.shinma06.arb.agent;

import java.io.BufferedReader;
import java.io.File;
import java.io.FileInputStream;
import java.io.IOException;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.util.UUID;

/**
 * Pure-Java {@code boot_id} resolver with graceful SELinux {@code EACCES} fallback for
 * Android 9 (API 28) and hardened work devices where {@code /proc/sys/kernel/random/boot_id}
 * (and sometimes {@code /proc/stat}) is unreadable by {@code shell} (UID 2000).
 */
public final class BootIdResolver {
    public static final String DEFAULT_PROC_BOOT_ID = "/proc/sys/kernel/random/boot_id";
    public static final String DEFAULT_PROC_STAT = "/proc/stat";

    private BootIdResolver() {}

    /** Abstraction over procfs reads so SELinux {@code EACCES} fallbacks are unit-testable. */
    public interface ProcReader {
        String readFirstLine(String path) throws Exception;

        long readProcStatBtime(String path) throws Exception;
    }

    public static final class Result {
        public final String bootId;
        public final int bootIdSource;
        public final String bootIdDetail;

        public Result(String bootId, int bootIdSource, String bootIdDetail) {
            this.bootId = bootId;
            this.bootIdSource = bootIdSource;
            this.bootIdDetail = bootIdDetail != null ? bootIdDetail : "";
        }
    }

    public static final ProcReader SYSTEM_PROC_READER = new ProcReader() {
        @Override
        public String readFirstLine(String path) throws IOException {
            try (BufferedReader br = new BufferedReader(
                    new InputStreamReader(new FileInputStream(path), StandardCharsets.UTF_8))) {
                return br.readLine();
            }
        }

        @Override
        public long readProcStatBtime(String path) throws IOException {
            File f = new File(path);
            if (!f.canRead()) {
                return -1L;
            }
            try (BufferedReader br = new BufferedReader(
                    new InputStreamReader(new FileInputStream(f), StandardCharsets.UTF_8))) {
                String line;
                while ((line = br.readLine()) != null) {
                    if (line.startsWith("btime ")) {
                        String[] parts = line.trim().split("\\s+");
                        if (parts.length >= 2) {
                            return Long.parseLong(parts[1]);
                        }
                    }
                }
            }
            return -1L;
        }
    };

    /**
     * Resolves {@code boot_id} with deterministic fallback order:
     * <ol>
     *   <li>Kernel UUID from {@code /proc/sys/kernel/random/boot_id}</li>
     *   <li>Deterministic {@code fallback:bc=<boot_count>} when {@code boot_count >= 0} (clock-independent),
     *       else {@code fallback:btime=<sec>} when {@code /proc/stat btime > 0}</li>
     *   <li>Last-resort {@code fallback:random:<uuid>} with honest {@code BOOT_ID_FALLBACK_RANDOM_UUID}
     *       source so the host treats reconnect continuity as unverified instead of failing startup.</li>
     * </ol>
     */
    public static Result resolve(
            ProcReader procReader,
            String procBootIdPath,
            String procStatPath,
            int bootCount,
            String bootReason) {
        String bootIdDetail = "";
        try {
            String raw = procReader.readFirstLine(procBootIdPath);
            if (raw != null && !raw.trim().isEmpty()) {
                return new Result(raw.trim(), Protocol.BOOT_ID_KERNEL_PROC, "");
            }
            bootIdDetail = "empty_proc_boot_id";
        } catch (Throwable t) {
            bootIdDetail = "proc_boot_id_error:" + t.getClass().getSimpleName() + ":" + safe(t.getMessage());
        }

        long btime = -1L;
        try {
            btime = procReader.readProcStatBtime(procStatPath);
        } catch (Throwable t) {
            bootIdDetail = appendDetail(
                    bootIdDetail,
                    "proc_stat_error:" + t.getClass().getSimpleName() + ":" + safe(t.getMessage()));
        }

        // /proc/stat btime is derived from the wall clock and shifts whenever it is stepped
        // (NITZ/NTP sync, manual date change) within one boot, so it is only used when
        // boot_count (boot-scoped, clock-independent) is unavailable.
        if (bootCount >= 0) {
            return new Result("fallback:bc=" + bootCount, Protocol.BOOT_ID_FALLBACK_BOOT_COUNT_BTIME, bootIdDetail);
        }
        if (btime > 0) {
            return new Result("fallback:btime=" + btime, Protocol.BOOT_ID_FALLBACK_BOOT_COUNT_BTIME, bootIdDetail);
        }

        String trimmedReason = safe(bootReason).trim();
        if (!trimmedReason.isEmpty()) {
            bootIdDetail = appendDetail(bootIdDetail, "bootreason=" + trimmedReason);
        }
        String randomId = "fallback:random:" + UUID.randomUUID();
        return new Result(randomId, Protocol.BOOT_ID_FALLBACK_RANDOM_UUID, bootIdDetail);
    }

    private static String appendDetail(String base, String extra) {
        if (base == null || base.isEmpty()) {
            return extra;
        }
        return base + ";" + extra;
    }

    private static String safe(String s) {
        return s != null ? s : "";
    }
}
