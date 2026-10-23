package io.github.shinma06.replaybuffer.clock;

import android.os.SystemClock;
import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;

/**
 * Reads elapsed-before, monotonic, wall and elapsed-after under adb shell, all in nanoseconds.
 * Wall retains {@link System#currentTimeMillis()} resolution; the host caller owns deployment, deadlines and cleanup.
 */
public final class ClockProbe {
    public static void main(String[] args) throws Exception {
        BufferedReader input = new BufferedReader(new InputStreamReader(System.in, StandardCharsets.UTF_8));
        System.out.println("REPLAY_CLOCK_1");
        System.out.flush();
        String nonce;
        while ((nonce = readNonce(input)) != null) {
            if (!nonce.matches("[a-f0-9]{32}")) throw new IllegalArgumentException("nonce");
            long before = SystemClock.elapsedRealtimeNanos();
            long mono = System.nanoTime();
            long wall = System.currentTimeMillis() * 1000000L;
            long after = SystemClock.elapsedRealtimeNanos();
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
