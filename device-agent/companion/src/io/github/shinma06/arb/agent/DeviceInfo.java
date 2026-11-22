package io.github.shinma06.arb.agent;

import android.os.Build;
import java.io.BufferedReader;
import java.io.File;
import java.io.FileInputStream;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.util.Locale;
import java.util.UUID;

/**
 * Collects device identity metadata and {@code boot_id} with graceful SELinux fallback.
 */
public final class DeviceInfo {
    private static final String PROC_BOOT_ID = "/proc/sys/kernel/random/boot_id";
    private static final String PROC_STAT = "/proc/stat";

    private DeviceInfo() {}

    public static PayloadCodec.Hello collectHello(int heartbeatIntervalMs) {
        int apiLevel = Build.VERSION.SDK_INT;
        int bootCount = readBootCount();
        String model = safe(Build.MODEL);
        String manufacturer = safe(Build.MANUFACTURER);
        boolean isEmulator = detectEmulator();

        String bootId = null;
        int bootIdSource = Protocol.BOOT_ID_KERNEL_PROC;
        String bootIdDetail = "";

        try {
            String raw = readFirstLine(PROC_BOOT_ID);
            if (raw != null && !raw.trim().isEmpty()) {
                bootId = raw.trim();
                bootIdSource = Protocol.BOOT_ID_KERNEL_PROC;
            } else {
                bootIdDetail = "empty_proc_boot_id";
            }
        } catch (Throwable t) {
            bootIdDetail = "proc_boot_id_error:" + t.getClass().getSimpleName() + ":" + safe(t.getMessage());
        }

        if (bootId == null) {
            long btime = readProcStatBtime();
            if (bootCount >= 0 || btime > 0) {
                bootId = "fallback:bc=" + bootCount + ":btime=" + btime;
                bootIdSource = Protocol.BOOT_ID_FALLBACK_BOOT_COUNT_BTIME;
            } else {
                bootId = "fallback:random:" + UUID.randomUUID();
                bootIdSource = Protocol.BOOT_ID_FALLBACK_RANDOM_UUID;
            }
        }

        return new PayloadCodec.Hello(
                Protocol.VERSION,
                apiLevel,
                bootCount,
                bootIdSource,
                isEmulator,
                heartbeatIntervalMs,
                bootId,
                model,
                manufacturer,
                Protocol.AGENT_VERSION,
                bootIdDetail);
    }

    private static int readBootCount() {
        String out = runShortCommand("settings", "get", "global", "boot_count");
        if (out == null) {
            return -1;
        }
        try {
            return Integer.parseInt(out.trim());
        } catch (NumberFormatException ignored) {
            return -1;
        }
    }

    private static long readProcStatBtime() {
        File f = new File(PROC_STAT);
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
        } catch (Throwable ignored) {
        }
        return -1L;
    }

    private static boolean detectEmulator() {
        String fp = safe(Build.FINGERPRINT).toLowerCase(Locale.ROOT);
        String model = safe(Build.MODEL).toLowerCase(Locale.ROOT);
        String hw = safe(Build.HARDWARE).toLowerCase(Locale.ROOT);
        String product = safe(Build.PRODUCT).toLowerCase(Locale.ROOT);
        if (fp.contains("generic")
                || fp.contains("emulator")
                || model.contains("emulator")
                || model.contains("sdk_gphone")
                || hw.contains("ranchu")
                || hw.contains("goldfish")
                || product.contains("sdk_gphone")
                || product.contains("emulator")) {
            return true;
        }
        String qemu = runShortCommand("getprop", "ro.kernel.qemu");
        if ("1".equals(safe(qemu).trim())) {
            return true;
        }
        String bootQemu = runShortCommand("getprop", "ro.boot.qemu");
        return "1".equals(safe(bootQemu).trim());
    }

    private static String readFirstLine(String path) throws Exception {
        try (BufferedReader br = new BufferedReader(
                new InputStreamReader(new FileInputStream(path), StandardCharsets.UTF_8))) {
            return br.readLine();
        }
    }

    private static String runShortCommand(String... cmd) {
        Process p = null;
        try {
            p = new ProcessBuilder(cmd).redirectErrorStream(true).start();
            StringBuilder sb = new StringBuilder();
            try (BufferedReader br = new BufferedReader(
                    new InputStreamReader(p.getInputStream(), StandardCharsets.UTF_8))) {
                String line;
                while ((line = br.readLine()) != null) {
                    if (sb.length() > 0) {
                        sb.append('\n');
                    }
                    sb.append(line);
                }
            }
            p.waitFor();
            return sb.toString();
        } catch (Throwable ignored) {
            return null;
        } finally {
            if (p != null) {
                p.destroy();
            }
        }
    }

    private static String safe(String s) {
        return s != null ? s : "";
    }
}
