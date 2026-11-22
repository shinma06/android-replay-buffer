package io.github.shinma06.arb.agent;

import java.io.BufferedInputStream;
import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicLong;

/**
 * Fault-isolated Stream 1 (Logcat) worker.
 *
 * <p>Spawns {@code logcat -b all -B -T 1}, stamps every binary {@code logger_entry} with
 * {@code device_read_boottime_ns}, periodically probes {@code logcat -b all -S} (every 5s),
 * and restarts cleanly with bounded backoff on any subprocess crash/kill without affecting
 * Stream 0 (Beacon) or Stream 2 (Input).
 */
public final class LogcatWorker {
    private static final long STATS_INTERVAL_MS = 5000L;
    private static final long INITIAL_BACKOFF_MS = 500L;
    private static final long MAX_BACKOFF_MS = 5000L;

    private final WireFrame.FrameWriter writer;
    private final ClockSource clock;
    private final StreamHealthTracker health;
    private final AtomicBoolean running = new AtomicBoolean(true);
    private final AtomicLong totalRecordsEmitted = new AtomicLong(0);

    private volatile Process activeProcess;
    private Thread readerThread;
    private Thread statsThread;

    public LogcatWorker(WireFrame.FrameWriter writer, ClockSource clock, StreamHealthTracker health) {
        this.writer = writer;
        this.clock = clock;
        this.health = health;
    }

    public void start() {
        readerThread = new Thread(this::runReaderLoop, "arb-logcat");
        readerThread.setDaemon(true);
        readerThread.start();

        statsThread = new Thread(this::runStatsLoop, "arb-logcat-stats");
        statsThread.setDaemon(true);
        statsThread.start();
    }

    public void stop() {
        running.set(false);
        Process p = activeProcess;
        if (p != null) {
            p.destroy();
        }
        if (readerThread != null) {
            readerThread.interrupt();
        }
        if (statsThread != null) {
            statsThread.interrupt();
        }
    }

    private void runReaderLoop() {
        LoggerEntryParser parser = new LoggerEntryParser();
        long backoffMs = INITIAL_BACKOFF_MS;
        boolean recoveredNext = false;

        while (running.get()) {
            Process proc = null;
            try {
                emitState(Protocol.STATE_STARTING, recoveredNext ? "respawning" : "starting");
                proc = new ProcessBuilder("logcat", "-b", "all", "-B", "-T", "1").start();
                activeProcess = proc;

                emitState(Protocol.STATE_RUNNING, recoveredNext ? "restarted" : "started");
                backoffMs = INITIAL_BACKOFF_MS;

                BufferedInputStream bin = new BufferedInputStream(proc.getInputStream(), 16384);
                PayloadCodec.LogRecord rec;
                while (running.get() && (rec = parser.readNext(bin, clock)) != null) {
                    int flags = recoveredNext ? Protocol.FLAG_RECOVERED : Protocol.FLAG_NONE;
                    recoveredNext = false;
                    long seq = writer.writeFrame(
                            Protocol.STREAM_LOGCAT,
                            Protocol.MSG_LOG_RECORD,
                            flags,
                            PayloadCodec.encodeLogRecord(rec));
                    totalRecordsEmitted.incrementAndGet();
                    health.recordEmit(Protocol.STREAM_LOGCAT, seq, rec.deviceReadBoottimeNs);
                }

                if (!running.get()) {
                    emitState(Protocol.STATE_STOPPED, "shutdown");
                    return;
                }

                int exitCode = waitExitQuietly(proc);
                String reason = "process_exit:" + exitCode;
                emitState(Protocol.STATE_ERROR, reason);
                emitOverflowGap(reason);
            } catch (IOException ioe) {
                if (!running.get()) {
                    emitState(Protocol.STATE_STOPPED, "shutdown");
                    return;
                }
                String reason = "io_error:" + safe(ioe.getMessage());
                emitState(Protocol.STATE_ERROR, reason);
                emitOverflowGap(reason);
            } catch (Throwable t) {
                if (!running.get()) {
                    return;
                }
                String reason = "error:" + t.getClass().getSimpleName() + ":" + safe(t.getMessage());
                emitState(Protocol.STATE_ERROR, reason);
                emitOverflowGap(reason);
            } finally {
                if (proc != null) {
                    proc.destroy();
                }
                activeProcess = null;
            }

            if (!running.get()) {
                return;
            }

            health.incrementRestart(Protocol.STREAM_LOGCAT);
            emitState(Protocol.STATE_RESTARTING, "backoff_ms:" + backoffMs);
            recoveredNext = true;
            try {
                Thread.sleep(backoffMs);
            } catch (InterruptedException ie) {
                if (!running.get()) {
                    return;
                }
            }
            backoffMs = Math.min(backoffMs * 2L, MAX_BACKOFF_MS);
        }
    }

    private void runStatsLoop() {
        LogcatStatsParser.Snapshot prev = null;
        long prevEmitted = 0L;

        while (running.get()) {
            try {
                long nowEmitted = totalRecordsEmitted.get();
                long emittedDelta = Math.max(0L, nowEmitted - prevEmitted);
                LogcatStatsParser.Snapshot curr = probeLogcatStats();
                PayloadCodec.LogOverflowStats stats = LogcatStatsParser.evaluate(
                        clock.boottimeNs(), prev, curr, emittedDelta, false);
                long seq = writer.writeFrame(
                        Protocol.STREAM_LOGCAT,
                        Protocol.MSG_LOG_OVERFLOW_STATS,
                        Protocol.FLAG_NONE,
                        PayloadCodec.encodeLogOverflowStats(stats));
                health.recordEmit(Protocol.STREAM_LOGCAT, seq, stats.boottimeNs);
                if (curr.valid) {
                    prev = curr;
                    prevEmitted = nowEmitted;
                }
            } catch (IOException ioe) {
                // Socket closed — stop cleanly without crashing other threads.
                return;
            } catch (Throwable t) {
                System.err.println("arb-logcat-stats error: " + t);
            }

            try {
                Thread.sleep(STATS_INTERVAL_MS);
            } catch (InterruptedException ie) {
                if (!running.get()) {
                    return;
                }
            }
        }
    }

    private LogcatStatsParser.Snapshot probeLogcatStats() {
        String output = runCommand("logcat", "-b", "all", "-S");
        LogcatStatsParser.Snapshot snap = LogcatStatsParser.parseLogcatS(output);
        if (snap.valid) {
            return snap;
        }
        // Fallback: try default `logcat -S` in case `-b all` is restricted on a device.
        String fallbackOutput = runCommand("logcat", "-S");
        LogcatStatsParser.Snapshot fallbackSnap = LogcatStatsParser.parseLogcatS(fallbackOutput);
        if (fallbackSnap.valid) {
            return fallbackSnap;
        }
        // Also probe `logcat -g` purely for diagnostic detail when reporting Unverified.
        String gOut = runCommand("logcat", "-g");
        if (gOut != null && !gOut.trim().isEmpty()) {
            return LogcatStatsParser.Snapshot.invalid("logcat_S_unavailable_g_present");
        }
        return snap;
    }

    private void emitOverflowGap(String reason) {
        try {
            PayloadCodec.LogOverflowStats gap = new PayloadCodec.LogOverflowStats(
                    clock.boottimeNs(),
                    Protocol.COMPLETENESS_OVERFLOW_DETECTED,
                    0,
                    -1L,
                    -1L,
                    "logcat_gap:" + reason);
            long seq = writer.writeFrame(
                    Protocol.STREAM_LOGCAT,
                    Protocol.MSG_LOG_OVERFLOW_STATS,
                    Protocol.FLAG_URGENT,
                    PayloadCodec.encodeLogOverflowStats(gap));
            health.recordEmit(Protocol.STREAM_LOGCAT, seq, gap.boottimeNs);
        } catch (IOException ignored) {
        }
    }

    private void emitState(int state, String reason) {
        int restarts = health.setState(Protocol.STREAM_LOGCAT, state);
        long now = clock.boottimeNs();
        long emittedSeq = health.getEmittedSeq(Protocol.STREAM_LOGCAT);
        PayloadCodec.StreamState st = new PayloadCodec.StreamState(
                Protocol.STREAM_LOGCAT, state, restarts, now, emittedSeq, reason);
        try {
            writer.writeFrame(
                    Protocol.STREAM_LOGCAT,
                    Protocol.MSG_STREAM_STATE,
                    Protocol.FLAG_URGENT,
                    PayloadCodec.encodeStreamState(st));
        } catch (IOException ignored) {
        }
    }

    private static int waitExitQuietly(Process p) {
        try {
            return p.waitFor();
        } catch (InterruptedException ie) {
            return -1;
        }
    }

    private static String runCommand(String... cmd) {
        Process p = null;
        try {
            p = new ProcessBuilder(cmd).redirectErrorStream(true).start();
            StringBuilder sb = new StringBuilder();
            try (BufferedReader br = new BufferedReader(
                    new InputStreamReader(p.getInputStream(), StandardCharsets.UTF_8))) {
                String line;
                int lines = 0;
                // Only the header table + initial summary are needed; cap read to stay lightweight.
                while ((line = br.readLine()) != null && lines < 120) {
                    sb.append(line).append('\n');
                    lines++;
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
