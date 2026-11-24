package io.github.shinma06.arb.agent;

import java.util.HashMap;
import java.util.LinkedHashSet;
import java.util.Map;
import java.util.Set;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

/**
 * Parses {@code getevent -lp} (for multi-touch device discovery) and {@code getevent -lt}
 * event lines (converting common Linux evdev type/code symbols to numeric codes).
 */
public final class GeteventParser {
    /** PROTOCOL.md section 3.8: {@code raw_line} is capped at 256 UTF-8 bytes (never mid-codepoint). */
    public static final int MAX_RAW_LINE_UTF8_BYTES = 256;
    private static final Pattern ADD_DEVICE_PATTERN = Pattern.compile("^add device \\d+:\\s*(/dev/input/\\S+)");
    /** {@code getevent -p} event-type section header, e.g. {@code ABS (0003): <first entry>}. */
    private static final Pattern SECTION_HEADER_PATTERN =
            Pattern.compile("^([A-Z]+)\\s*\\(([0-9a-fA-F]{4})\\):\\s*(.*)$");
    /** One absolute-axis definition: {@code <axis label or hex code>  : value 0, min 0, max 1079, ...}. */
    private static final Pattern ABS_AXIS_PATTERN = Pattern.compile("^(\\S+)\\s*:\\s*value\\b.*$");
    /** Device metadata lines ({@code name:}, {@code events:}, {@code input props:}) end a section. */
    private static final Pattern METADATA_PATTERN = Pattern.compile("^[a-z][a-z ]*:.*$");
    // Matches both multi-device lines: "[  123.456789] /dev/input/event2: EV_ABS  ABS_MT_POSITION_X  00000123"
    // and single-device lines:         "[  123.456789] EV_ABS  ABS_MT_POSITION_X  00000123"
    private static final Pattern EVENT_LINE_PATTERN = Pattern.compile(
            "^\\[\\s*(\\d+)\\.(\\d{1,6})\\]\\s+(?:(/dev/input/\\S+):\\s+)?(\\S+)\\s+(\\S+)\\s+(\\S+).*$");

    private static final Map<String, Integer> EV_TYPES = new HashMap<>();
    private static final Map<String, Integer> EV_CODES = new HashMap<>();

    static {
        EV_TYPES.put("EV_SYN", 0x0000);
        EV_TYPES.put("EV_KEY", 0x0001);
        EV_TYPES.put("EV_REL", 0x0002);
        EV_TYPES.put("EV_ABS", 0x0003);
        EV_TYPES.put("EV_MSC", 0x0004);

        EV_CODES.put("SYN_REPORT", 0x0000);
        EV_CODES.put("SYN_CONFIG", 0x0001);
        EV_CODES.put("SYN_MT_REPORT", 0x0002);
        EV_CODES.put("SYN_DROPPED", 0x0003);
        EV_CODES.put("BTN_TOUCH", 0x014a);
        EV_CODES.put("BTN_TOOL_FINGER", 0x0145);
        EV_CODES.put("ABS_X", 0x0000);
        EV_CODES.put("ABS_Y", 0x0001);
        EV_CODES.put("ABS_PRESSURE", 0x0018);
        EV_CODES.put("ABS_MT_SLOT", 0x002f);
        EV_CODES.put("ABS_MT_TOUCH_MAJOR", 0x0030);
        EV_CODES.put("ABS_MT_TOUCH_MINOR", 0x0031);
        EV_CODES.put("ABS_MT_WIDTH_MAJOR", 0x0032);
        EV_CODES.put("ABS_MT_WIDTH_MINOR", 0x0033);
        EV_CODES.put("ABS_MT_ORIENTATION", 0x0034);
        EV_CODES.put("ABS_MT_POSITION_X", 0x0035);
        EV_CODES.put("ABS_MT_POSITION_Y", 0x0036);
        EV_CODES.put("ABS_MT_TOOL_TYPE", 0x0037);
        EV_CODES.put("ABS_MT_BLOB_ID", 0x0038);
        EV_CODES.put("ABS_MT_TRACKING_ID", 0x0039);
        EV_CODES.put("ABS_MT_PRESSURE", 0x003a);
        EV_CODES.put("ABS_MT_DISTANCE", 0x003b);
    }

    private GeteventParser() {}

    /**
     * Discovers all multi-touch input device paths reporting both {@code ABS_MT_POSITION_X}
     * and {@code ABS_MT_POSITION_Y} in {@code getevent -lp} output. Only axis definitions inside a
     * device's {@code ABS (0003)} section count, and only by exact token (the label, or the hex
     * code {@code 0035} / {@code 0036} when unlabelled): a key code, a vendor / product id or an
     * axis range that merely contains those digits is not a touch axis.
     */
    public static Set<String> discoverTouchDevices(String geteventLpOutput) {
        Set<String> devices = new LinkedHashSet<>();
        if (geteventLpOutput == null || geteventLpOutput.isEmpty()) {
            return devices;
        }
        String currentDevice = null;
        boolean hasMtX = false;
        boolean hasMtY = false;
        boolean inAbsSection = false;

        String[] lines = geteventLpOutput.split("\\r?\\n");
        for (String line : lines) {
            Matcher m = ADD_DEVICE_PATTERN.matcher(line.trim());
            if (m.find()) {
                if (currentDevice != null && hasMtX && hasMtY) {
                    devices.add(currentDevice);
                }
                currentDevice = m.group(1);
                hasMtX = false;
                hasMtY = false;
                inAbsSection = false;
                continue;
            }
            if (currentDevice == null) {
                continue;
            }
            String body = line.trim();
            Matcher header = SECTION_HEADER_PATTERN.matcher(body);
            if (header.matches()) {
                inAbsSection = "ABS".equals(header.group(1)) || "0003".equals(header.group(2));
                body = header.group(3);
            } else if (!ABS_AXIS_PATTERN.matcher(body).matches() && METADATA_PATTERN.matcher(body).matches()) {
                inAbsSection = false;
                continue;
            }
            if (!inAbsSection) {
                continue;
            }
            Matcher axis = ABS_AXIS_PATTERN.matcher(body);
            if (axis.matches()) {
                String token = axis.group(1);
                if ("ABS_MT_POSITION_X".equals(token) || "0035".equals(token)) {
                    hasMtX = true;
                } else if ("ABS_MT_POSITION_Y".equals(token) || "0036".equals(token)) {
                    hasMtY = true;
                }
            }
        }
        if (currentDevice != null && hasMtX && hasMtY) {
            devices.add(currentDevice);
        }
        return devices;
    }

    /**
     * Parses a single {@code getevent -lt} line. Returns {@code null} if the line is blank,
     * a device header, or not from {@code allowedDevices} (when non-empty).
     */
    public static PayloadCodec.TouchEvent parseEventLine(
            String rawLine,
            String defaultDevicePath,
            Set<String> allowedDevices,
            long readBoottimeNs) {
        if (rawLine == null) {
            return null;
        }
        String trimmed = rawLine.trim();
        if (trimmed.isEmpty() || !trimmed.startsWith("[")) {
            return null;
        }
        Matcher m = EVENT_LINE_PATTERN.matcher(trimmed);
        if (!m.matches()) {
            return null;
        }
        String devicePath = m.group(3) != null ? m.group(3) : defaultDevicePath;
        if (devicePath == null) {
            devicePath = "";
        }
        if (allowedDevices != null && !allowedDevices.isEmpty() && !allowedDevices.contains(devicePath)) {
            return null;
        }

        long sec = parseLongSafe(m.group(1), -1L);
        String usecStr = m.group(2);
        while (usecStr.length() < 6) {
            usecStr = usecStr + "0";
        }
        int usec = (int) parseLongSafe(usecStr, -1L);
        int evType = parseCode(m.group(4), EV_TYPES);
        int evCode = parseCode(m.group(5), EV_CODES);
        int evValue = parseEventValue(m.group(6));

        String cappedRaw = PayloadCodec.truncateUtf8(trimmed, MAX_RAW_LINE_UTF8_BYTES);
        return new PayloadCodec.TouchEvent(
                readBoottimeNs, sec, usec, evType, evCode, evValue, devicePath, cappedRaw);
    }

    private static int parseCode(String token, Map<String, Integer> table) {
        Integer known = table.get(token);
        if (known != null) {
            return known;
        }
        try {
            if (token.startsWith("0x") || token.startsWith("0X")) {
                return Integer.parseInt(token.substring(2), 16) & 0xFFFF;
            }
            return Integer.parseInt(token, 16) & 0xFFFF;
        } catch (NumberFormatException ignored) {
            return Protocol.UNKNOWN_U16;
        }
    }

    private static int parseEventValue(String token) {
        if ("DOWN".equals(token)) {
            return 1;
        }
        if ("UP".equals(token)) {
            return 0;
        }
        try {
            String hex = (token.startsWith("0x") || token.startsWith("0X")) ? token.substring(2) : token;
            return (int) Long.parseLong(hex, 16);
        } catch (NumberFormatException ignored) {
            try {
                return Integer.parseInt(token);
            } catch (NumberFormatException ignored2) {
                return 0;
            }
        }
    }

    private static long parseLongSafe(String s, long fallback) {
        try {
            return Long.parseLong(s);
        } catch (NumberFormatException ignored) {
            return fallback;
        }
    }
}
