package io.github.shinma06.arb.agent;

import android.os.SystemClock;

/**
 * Android 3-clock triple implementation using {@code SystemClock.elapsedRealtimeNanos()}
 * ({@code CLOCK_BOOTTIME}), {@code System.nanoTime()} ({@code CLOCK_MONOTONIC}), and
 * {@code System.currentTimeMillis() * 1_000_000L} ({@code CLOCK_REALTIME}).
 */
public final class AndroidClock implements ClockSource {
    @Override
    public long boottimeNs() {
        return SystemClock.elapsedRealtimeNanos();
    }

    @Override
    public long monotonicNs() {
        return System.nanoTime();
    }

    @Override
    public long realtimeNs() {
        return System.currentTimeMillis() * 1_000_000L;
    }
}
