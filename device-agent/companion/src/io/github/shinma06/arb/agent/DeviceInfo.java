package io.github.shinma06.arb.agent;

import android.os.Build;
import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.util.Locale;

/**
 * Collects device identity metadata and {@code boot_id} with graceful SELinux fallback.
 */
public final class DeviceInfo {
    private DeviceInfo() {}

    public static PayloadCodec.Hello collectHello(int heartbeatIntervalMs) {
        int apiLevel = Build.VERSION.SDK_INT;
        int bootCount = readBootCount();
        String model = safe(Build.MODEL);
        String manufacturer = safe(Build.MANUFACTURER);
        boolean isEmulator = detectEmulator();
        String bootReason = readBootReason();

        BootIdResolver.Result resolved = BootIdResolver.resolve(
                BootIdResolver.SYSTEM_PROC_READER,
                BootIdResolver.DEFAULT_PROC_BOOT_ID,
                BootIdResolver.DEFAULT_PROC_STAT,
                bootCount,
                bootReason);

        return new PayloadCodec.Hello(
                Protocol.VERSION,
                apiLevel,
                bootCount,
                resolved.bootIdSource,
                isEmulator,
                heartbeatIntervalMs,
                resolved.bootId,
                model,
                manufacturer,
                Protocol.AGENT_VERSION,
                resolved.bootIdDetail);
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

    private static String readBootReason() {
        String reason = safe(runShortCommand("getprop", "ro.boot.bootreason")).trim();
        if (!reason.isEmpty()) {
            return reason;
        }
        return safe(runShortCommand("getprop", "sys.boot.reason")).trim();
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
