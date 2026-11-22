package io.github.shinma06.arb.agent;

import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.util.Set;
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

    private final WireFrame.FrameWriter writer;
    private final ClockSource clock;
    private final StreamHealthTracker health;
    private final AtomicBoolean running = new AtomicBoolean(true);

    private volatile Process activeProcess;
    private Thread workerThread;

    public InputWorker(WireFrame.FrameWriter writer, ClockSource clock, StreamHealthTracker health) {
        this.writer = writer;
        this.clock = clock;
        this.health = health;
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
            p.destroy();
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
                ProcessBuilder pb = (singleDevice != null)
                        ? new ProcessBuilder("getevent", "-lt", singleDevice)
                        : new ProcessBuilder("getevent", "-lt");
                pb.redirectErrorStream(true);
                proc = pb.start();
                activeProcess = proc;

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
                        if (line.trim().length() > 256) {
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
                    proc.destroy();
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
                    Protocol.FLAG_URGENT,
                    PayloadCodec.encodeStreamState(st));
        } catch (IOException ignored) {
        }
    }

    private static String runGeteventLp() {
        Process p = null;
        try {
            p = new ProcessBuilder("getevent", "-lp").redirectErrorStream(true).start();
            StringBuilder sb = new StringBuilder();
            try (BufferedReader br = new BufferedReader(
                    new InputStreamReader(p.getInputStream(), StandardCharsets.UTF_8))) {
                String line;
                while ((line = br.readLine()) != null) {
                    sb.append(line).append('\n');
                }
            }
            p.waitFor();
            return sb.toString();
        } catch (Throwable ignored) {
            return "";
        } finally {
            if (p != null) {
                p.destroy();
            }
        }
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
