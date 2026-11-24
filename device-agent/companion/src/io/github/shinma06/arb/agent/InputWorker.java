package io.github.shinma06.arb.agent;

import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.util.Set;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * Fault-isolated Stream 2 (Input / Taps) worker.
 *
 * <p>Discovers multi-touch devices via {@code getevent -lp}, streams {@code getevent -lt}
 * filtered to those touch devices, stamps every event with {@code device_read_boottime_ns},
 * and handles missing touch devices or subprocess errors cleanly without affecting other streams.
 */
public final class InputWorker {
    private static final long INITIAL_BACKOFF_MS = 500L;
    private static final long MAX_BACKOFF_MS = 5000L;
    /** {@code getevent -lp} lists every input device once; it never needs this long. */
    private static final long DISCOVERY_TIMEOUT_MS = 5000L;
    /** Discovery output is a few KiB; a runaway child must not grow it without bound. */
    private static final int MAX_DISCOVERY_LINES = 20_000;

    private static final LogcatWorker.ProcessStarter DEFAULT_STARTER =
            cmd -> new ProcessBuilder(cmd).redirectErrorStream(true).start();

    private final WireFrame.FrameWriter writer;
    private final ClockSource clock;
    private final StreamHealthTracker health;
    private final LogcatWorker.ProcessStarter starter;
    private final long discoveryTimeoutMs;
    private final AtomicBoolean running = new AtomicBoolean(true);

    /** The running {@code getevent} child (discovery or stream), destroyed by {@link #stop()}. */
    private volatile Process activeProcess;
    private Thread workerThread;

    public InputWorker(WireFrame.FrameWriter writer, ClockSource clock, StreamHealthTracker health) {
        this(writer, clock, health, DEFAULT_STARTER, DISCOVERY_TIMEOUT_MS);
    }

    InputWorker(
            WireFrame.FrameWriter writer,
            ClockSource clock,
            StreamHealthTracker health,
            LogcatWorker.ProcessStarter starter,
            long discoveryTimeoutMs) {
        this.writer = writer;
        this.clock = clock;
        this.health = health;
        this.starter = starter;
        this.discoveryTimeoutMs = Math.max(20L, discoveryTimeoutMs);
    }

    public void start() {
        workerThread = new Thread(this::runLoop, "arb-input");
        workerThread.setDaemon(true);
        workerThread.start();
    }

    public void stop() {
        running.set(false);
        Process p = activeProcess;
        if (p != null) {
            ChildProcesses.destroy(p);
        }
        if (workerThread != null) {
            workerThread.interrupt();
        }
    }

    private void runLoop() {
        long backoffMs = INITIAL_BACKOFF_MS;
        boolean recoveredNext = false;

        while (running.get()) {
            Process proc = null;
            try {
                emitState(Protocol.STATE_STARTING, recoveredNext ? "respawning" : "discovering");
                String lpOut = runGeteventLp();
                Set<String> touchDevices = GeteventParser.discoverTouchDevices(lpOut);
                if (touchDevices.isEmpty()) {
                    emitState(Protocol.STATE_STOPPED, "no_touch_device");
                    return;
                }

                // If exactly 1 touch device exists, target it directly; if multiple (e.g. emulator
                // virtio_input_multi_touch_1..11), stream all devices and filter to touchDevices.
                String singleDevice = touchDevices.size() == 1 ? touchDevices.iterator().next() : null;
                proc = (singleDevice != null)
                        ? starter.start("getevent", "-lt", singleDevice)
                        : starter.start("getevent", "-lt");
                activeProcess = proc;
                // stop() may have run between pb.start() and the assignment above, in which case
                // it saw activeProcess == null and could not destroy this child.
                if (!running.get()) {
                    emitState(Protocol.STATE_STOPPED, "shutdown");
                    return;
                }

                emitState(Protocol.STATE_RUNNING,
                        (recoveredNext ? "restarted:" : "started:") + "devices=" + touchDevices.size());
                backoffMs = INITIAL_BACKOFF_MS;

                try (BufferedReader br = new BufferedReader(
                        new InputStreamReader(proc.getInputStream(), StandardCharsets.UTF_8))) {
                    String line;
                    while (running.get() && (line = br.readLine()) != null) {
                        long readBoot = clock.boottimeNs();
                        PayloadCodec.TouchEvent ev = GeteventParser.parseEventLine(
                                line, singleDevice, touchDevices, readBoot);
                        if (ev == null) {
                            continue;
                        }
                        int flags = recoveredNext ? Protocol.FLAG_RECOVERED : Protocol.FLAG_NONE;
                        if (PayloadCodec.exceedsUtf8Bytes(line.trim(), GeteventParser.MAX_RAW_LINE_UTF8_BYTES)) {
                            flags |= Protocol.FLAG_TRUNCATED;
                        }
                        recoveredNext = false;
                        long seq = writer.writeFrame(
                                Protocol.STREAM_INPUT,
                                Protocol.MSG_TOUCH_EVENT,
                                flags,
                                PayloadCodec.encodeTouchEvent(ev));
                        health.recordEmit(Protocol.STREAM_INPUT, seq, readBoot);
                    }
                }

                if (!running.get()) {
                    emitState(Protocol.STATE_STOPPED, "shutdown");
                    return;
                }

                int exitCode = waitExitQuietly(proc);
                emitState(Protocol.STATE_ERROR, "process_exit:" + exitCode);
            } catch (IOException ioe) {
                if (!running.get()) {
                    emitState(Protocol.STATE_STOPPED, "shutdown");
                    return;
                }
                emitState(Protocol.STATE_ERROR, "io_error:" + safe(ioe.getMessage()));
            } catch (Throwable t) {
                if (!running.get()) {
                    return;
                }
                emitState(Protocol.STATE_ERROR, "error:" + t.getClass().getSimpleName() + ":" + safe(t.getMessage()));
            } finally {
                if (proc != null) {
                    ChildProcesses.destroyAndReap(proc);
                }
                activeProcess = null;
            }

            if (!running.get()) {
                return;
            }

            health.incrementRestart(Protocol.STREAM_INPUT);
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

    private void emitState(int state, String reason) {
        int restarts = health.setState(Protocol.STREAM_INPUT, state);
        long now = clock.boottimeNs();
        long emittedSeq = health.getEmittedSeq(Protocol.STREAM_INPUT);
        PayloadCodec.StreamState st = new PayloadCodec.StreamState(
                Protocol.STREAM_INPUT, state, restarts, now, emittedSeq, reason);
        try {
            writer.writeFrame(
                    Protocol.STREAM_INPUT,
                    Protocol.MSG_STREAM_STATE,
                    Protocol.FLAG_URGENT
                            | (PayloadCodec.wouldTruncateStr16(reason) ? Protocol.FLAG_TRUNCATED : Protocol.FLAG_NONE),
                    PayloadCodec.encodeStreamState(st));
        } catch (IOException ignored) {
        }
    }

    /**
     * Runs the {@code getevent -lp} discovery probe like the stream child: tracked as
     * {@link #activeProcess} so {@link #stop()} destroys it, bounded by a watchdog that
     * force-kills a hung probe after {@code discoveryTimeoutMs}, and always reaped. Returns
     * {@code ""} when stopped, timed out or failed.
     */
    String runGeteventLp() {
        if (!running.get()) {
            return "";
        }
        Process p = null;
        Thread watchdog = null;
        final CountDownLatch finished = new CountDownLatch(1);
        final AtomicBoolean timedOut = new AtomicBoolean(false);
        try {
            p = starter.start("getevent", "-lp");
            activeProcess = p;
            // stop() may have run before the assignment above and missed this child.
            if (!running.get()) {
                return "";
            }
            final Process probe = p;
            watchdog = new Thread(() -> {
                try {
                    if (!finished.await(discoveryTimeoutMs, TimeUnit.MILLISECONDS)) {
                        timedOut.set(true);
                        ChildProcesses.destroy(probe);
                    }
                } catch (InterruptedException ignored) {
                }
            }, "arb-input-discovery-watchdog");
            watchdog.setDaemon(true);
            watchdog.start();

            StringBuilder sb = new StringBuilder();
            try (BufferedReader br = new BufferedReader(
                    new InputStreamReader(p.getInputStream(), StandardCharsets.UTF_8))) {
                String line;
                int lines = 0;
                while (running.get() && lines++ < MAX_DISCOVERY_LINES && (line = br.readLine()) != null) {
                    sb.append(line).append('\n');
                }
            }
            return running.get() && !timedOut.get() ? sb.toString() : "";
        } catch (Exception ignored) {
            return "";
        } finally {
            finished.countDown();
            if (watchdog != null) {
                watchdog.interrupt();
            }
            if (p != null) {
                if (activeProcess == p) {
                    activeProcess = null;
                }
                ChildProcesses.destroyAndReap(p);
            }
        }
    }

    /** Joins the worker thread; host tests use it to prove {@link #stop()} unblocks discovery. */
    boolean awaitTerminated(long timeoutMs) throws InterruptedException {
        Thread t = workerThread;
        if (t == null) {
            return true;
        }
        t.join(Math.max(1L, timeoutMs));
        return !t.isAlive();
    }

    private static int waitExitQuietly(Process p) {
        try {
            return p.waitFor();
        } catch (InterruptedException ie) {
            return -1;
        }
    }

    private static String safe(String s) {
        return s != null ? s : "";
    }
}
