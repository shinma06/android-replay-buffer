package io.github.shinma06.arb.agent;

/**
 * Abstraction for the Android 3-clock triple so core logic is testable on the host JVM.
 */
public interface ClockSource {
    long boottimeNs();
    long monotonicNs();
    long realtimeNs();
}
