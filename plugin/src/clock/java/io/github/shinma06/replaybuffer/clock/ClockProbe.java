package io.github.shinma06.replaybuffer.clock;

import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.lang.reflect.Method;

/** Runs as the adb shell user, never installs an APK or changes device state. */
public final class ClockProbe {
    public static void main(String[] args) throws Exception {
        Method elapsed = Class.forName("android.os.SystemClock").getMethod("elapsedRealtimeNanos");
        BufferedReader input = new BufferedReader(new InputStreamReader(System.in, "UTF-8"));
        System.out.println("REPLAY_CLOCK_1");
        System.out.flush();
        String nonce;
        while ((nonce = readNonce(input)) != null) {
            if (!nonce.matches("[a-f0-9]{32}")) throw new IllegalArgumentException("nonce");
            long before = (Long) elapsed.invoke(null);
            long mono = System.nanoTime();
            long wall = System.currentTimeMillis() * 1000000L;
            long after = (Long) elapsed.invoke(null);
            System.out.println(nonce + "\t" + before + "\t" + mono + "\t" + wall + "\t" + after);
            System.out.flush();
        }
    }
    private static String readNonce(BufferedReader input) throws Exception {
        StringBuilder line = new StringBuilder(32);
        int value;
        while ((value = input.read()) >= 0) {
            if (value == '\n') return line.toString();
            if (line.length() >= 32) throw new IllegalArgumentException("nonce length");
            line.append((char) value);
        }
        if (line.length() != 0) throw new IllegalArgumentException("partial nonce");
        return null;
    }
}
