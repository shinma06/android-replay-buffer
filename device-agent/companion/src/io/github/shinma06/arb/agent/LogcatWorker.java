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
 * <p>Spawns {@code logcat -b all -B -T 1} (respawns resume with {@code -T <sec>.<nsec>} of the
 * last emitted entry, de-duplicating the boundary), stamps every binary {@code logger_entry} with
 * {@code device_read_boottime_ns}, periodically probes {@code logcat -b all -S} (every 5s),
 * and restarts cleanly with bounded backoff on any subprocess crash/kill without affecting
 * Stream 0 (Beacon) or Stream 2 (Input).
 */
public final class LogcatWorker {
    private static final long STATS_INTERVAL_MS = 5000L;
    private static final long INITIAL_BACKOFF_MS = 500L;
    private static final long MAX_BACKOFF_MS = 5000L;
    private static final String SAFE_FALLBACK_BUFFERS = "main,system,crash,events";

    /**
     * Builds the {@code logcat -B} invocation. Uses {@code -b all} by default and falls back to
     * {@code -b main,system,crash,events} if a restricted Android 9 ROM rejects {@code -b all}.
     */
    static String[] buildLogcatCommand(boolean restrictedBuffers, String tArg) {
        String buffers = restrictedBuffers ? SAFE_FALLBACK_BUFFERS : "all";
        return new String[] {"logcat", "-b", buffers, "-B", "-T", tArg};
    }

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
        LoggerEntryParser.ResumeTracker resume = new LoggerEntryParser.ResumeTracker();
        long backoffMs = INITIAL_BACKOFF_MS;
        boolean recoveredNext = false;
        boolean useRestrictedBuffers = false;

        while (running.get()) {
            Process proc = null;
            long emittedInAttempt = 0L;
            boolean immediateFallback = false;
            try {
                emitState(Protocol.STATE_STARTING, recoveredNext ? "respawning" : "starting");
                // Resume from the last emitted (sec, nsec) so a respawn neither replays the
                // previous last line nor silently skips lines written while logcat was down.
                // `-T 1` is used only when nothing has been emitted yet.
                resume.beginResume();
                String tArg = resume.nextTArgument();
                proc = new ProcessBuilder(buildLogcatCommand(useRestrictedBuffers, tArg)).start();
                activeProcess = proc;
                drainStderrQuietly(proc);
                // stop() may have run between start() and the assignment above, in which case
                // it saw activeProcess == null and could not destroy this child.
                if (!running.get()) {
                    emitState(Protocol.STATE_STOPPED, "shutdown");
                    return;
                }

                emitState(Protocol.STATE_RUNNING, recoveredNext ? "restarted" : "started");
                backoffMs = INITIAL_BACKOFF_MS;

                BufferedInputStream bin = new BufferedInputStream(proc.getInputStream(), 16384);
                PayloadCodec.LogRecord rec;
                while (running.get() && (rec = parser.readNext(bin, clock)) != null) {
                    LoggerEntryParser.ResumeTracker.Decision decision = resume.evaluate(rec);
                    if (decision == LoggerEntryParser.ResumeTracker.Decision.SKIP_DUPLICATE) {
                        continue;
                    }
                    if (decision == LoggerEntryParser.ResumeTracker.Decision.EMIT_WITH_GAP) {
                        // The boundary entry was evicted from the ring before respawn: real loss.
                        emitOverflowGap("resume_boundary_evicted:T=" + tArg);
                    }
                    int flags = recoveredNext ? Protocol.FLAG_RECOVERED : Protocol.FLAG_NONE;
                    recoveredNext = false;
                    long seq = writer.writeFrame(
                            Protocol.STREAM_LOGCAT,
                            Protocol.MSG_LOG_RECORD,
                            flags,
                            PayloadCodec.encodeLogRecord(rec));
                    if (seq != 0L) {
                        emittedInAttempt++;
                        totalRecordsEmitted.incrementAndGet();
                        health.recordEmit(Protocol.STREAM_LOGCAT, seq, rec.deviceReadBoottimeNs);
                    }
                }

                if (!running.get()) {
                    emitState(Protocol.STATE_STOPPED, "shutdown");
                    return;
                }

                int exitCode = waitExitQuietly(proc);
                if (!useRestrictedBuffers && emittedInAttempt == 0L && exitCode != 0) {
                    // Some hardened Android 9 user builds reject `-b all` (e.g. restricted security/kernel
                    // buffer). Retry immediately with explicit main,system,crash,events buffers.
                    useRestrictedBuffers = true;
                    immediateFallback = true;
                    emitState(Protocol.STATE_RESTARTING, "fallback_buffers:exit=" + exitCode);
                } else {
                    String reason = "process_exit:" + exitCode;
                    emitState(Protocol.STATE_ERROR, reason);
                    emitGapIfUnresumable(resume, reason);
                }
            } catch (IOException ioe) {
                if (!running.get()) {
                    emitState(Protocol.STATE_STOPPED, "shutdown");
                    return;
                }
                String reason = "io_error:" + safe(ioe.getMessage());
                emitState(Protocol.STATE_ERROR, reason);
                emitGapIfUnresumable(resume, reason);
            } catch (Throwable t) {
                if (!running.get()) {
                    return;
                }
                String reason = "error:" + t.getClass().getSimpleName() + ":" + safe(t.getMessage());
                emitState(Protocol.STATE_ERROR, reason);
                emitGapIfUnresumable(resume, reason);
            } finally {
                if (proc != null) {
                    proc.destroy();
                }
                activeProcess = null;
            }

            if (!running.get()) {
                return;
            }
            if (immediateFallback) {
                continue;
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
        // Fallback 1: explicit main,system,crash,events buffers if `-b all` is restricted.
        String restrictedOutput = runCommand("logcat", "-b", SAFE_FALLBACK_BUFFERS, "-S");
        LogcatStatsParser.Snapshot restrictedSnap = LogcatStatsParser.parseLogcatS(restrictedOutput);
        if (restrictedSnap.valid) {
            return restrictedSnap;
        }
        // Fallback 2: default `logcat -S`.
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

    private static void drainStderrQuietly(Process proc) {
        Thread drainer = new Thread(() -> {
            byte[] buf = new byte[1024];
            try {
                java.io.InputStream err = proc.getErrorStream();
                while (err.read(buf) >= 0) {
                    // Drain so stderr pipe never backs up and blocks binary `-B` stdout.
                }
            } catch (IOException ignored) {
            }
        }, "arb-logcat-stderr");
        drainer.setDaemon(true);
        drainer.start();
    }

    /**
     * Without a resume timestamp the respawn falls back to {@code -T 1}, so lines written while
     * logcat was down are lost: report the gap. With a timestamp the respawn resumes exactly and
     * a gap is only reported if the boundary entry turns out to be evicted.
     */
    private void emitGapIfUnresumable(LoggerEntryParser.ResumeTracker resume, String reason) {
        if (!resume.hasTimestamp()) {
            emitOverflowGap(reason);
        }
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
                    Protocol.FLAG_URGENT | truncatedFlag(gap.detail),
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
                    Protocol.FLAG_URGENT | truncatedFlag(reason),
                    PayloadCodec.encodeStreamState(st));
        } catch (IOException ignored) {
        }
    }

    private static int truncatedFlag(String str16Value) {
        return PayloadCodec.wouldTruncateStr16(str16Value) ? Protocol.FLAG_TRUNCATED : Protocol.FLAG_NONE;
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
