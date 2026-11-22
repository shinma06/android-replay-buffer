package io.github.shinma06.arb.agent;

import java.io.IOException;
import java.io.InputStream;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * Stream 0 (Control / Beacon) worker.
 *
 * <p>Emits initial {@code Hello}, periodic {@code Heartbeat} frames with the 3-clock triple and
 * per-stream health snapshot, and responds immediately to host {@code Ping} and {@code Shutdown}
 * frames. Isolated from Streams 1 and 2 so child process failures never interrupt the beacon.
 */
public final class ControlWorker {
    private final WireFrame.FrameWriter writer;
    private final InputStream in;
    private final ClockSource clock;
    private final StreamHealthTracker health;
    private final PayloadCodec.Hello hello;
    private final int heartbeatIntervalMs;
    private final AtomicBoolean running = new AtomicBoolean(true);
    private final Runnable onShutdownRequested;

    private Thread heartbeatThread;
    private Thread commandThread;

    public ControlWorker(
            WireFrame.FrameWriter writer,
            InputStream in,
            ClockSource clock,
            StreamHealthTracker health,
            PayloadCodec.Hello hello,
            int heartbeatIntervalMs,
            Runnable onShutdownRequested) {
        this.writer = writer;
        this.in = in;
        this.clock = clock;
        this.health = health;
        this.hello = hello;
        this.heartbeatIntervalMs = heartbeatIntervalMs;
        this.onShutdownRequested = onShutdownRequested;
    }

    public void sendHelloAndStart() throws IOException {
        byte[] helloBytes = PayloadCodec.encodeHello(hello);
        long seq = writer.writeFrame(Protocol.STREAM_CONTROL, Protocol.MSG_HELLO, Protocol.FLAG_URGENT, helloBytes);
        long now = clock.boottimeNs();
        health.setState(Protocol.STREAM_CONTROL, Protocol.STATE_RUNNING);
        health.recordEmit(Protocol.STREAM_CONTROL, seq, now);

        heartbeatThread = new Thread(this::heartbeatLoop, "arb-beacon");
        heartbeatThread.setDaemon(true);
        heartbeatThread.start();

        commandThread = new Thread(this::commandLoop, "arb-control-rx");
        commandThread.setDaemon(false);
        commandThread.start();
    }

    public void awaitCommandLoop() throws InterruptedException {
        if (commandThread != null) {
            commandThread.join();
        }
    }

    public void stop() {
        running.set(false);
        if (heartbeatThread != null) {
            heartbeatThread.interrupt();
        }
    }

    private void heartbeatLoop() {
        // Emit first heartbeat immediately so host has a 3-clock triple right after Hello.
        while (running.get()) {
            try {
                // Read monotonic before boottime so boottime >= monotonic holds strictly even with zero suspend.
                long mono = clock.monotonicNs();
                long boot = clock.boottimeNs();
                long wall = clock.realtimeNs();
                PayloadCodec.Heartbeat hb = new PayloadCodec.Heartbeat(boot, mono, wall, health.snapshot());
                long seq = writer.writeFrame(
                        Protocol.STREAM_CONTROL,
                        Protocol.MSG_HEARTBEAT,
                        Protocol.FLAG_NONE,
                        PayloadCodec.encodeHeartbeat(hb));
                health.recordEmit(Protocol.STREAM_CONTROL, seq, boot);
            } catch (IOException e) {
                running.set(false);
                if (onShutdownRequested != null) {
                    onShutdownRequested.run();
                }
                return;
            } catch (Throwable t) {
                System.err.println("arb-beacon unexpected error: " + t);
            }

            try {
                Thread.sleep(heartbeatIntervalMs);
            } catch (InterruptedException ie) {
                if (!running.get()) {
                    return;
                }
            }
        }
    }

    private void commandLoop() {
        try {
            while (running.get()) {
                WireFrame frame = WireFrame.readFrom(in);
                if (frame == null) {
                    // Clean host disconnect
                    break;
                }
                if (frame.msgType == Protocol.MSG_PING) {
                    PayloadCodec.Ping ping = PayloadCodec.decodePing(frame.payload);
                    long before = clock.boottimeNs();
                    long mono = clock.monotonicNs();
                    long wall = clock.realtimeNs();
                    long after = clock.boottimeNs();
                    PayloadCodec.Pong pong = new PayloadCodec.Pong(ping.nonce, before, mono, wall, after);
                    long seq = writer.writeFrame(
                            Protocol.STREAM_CONTROL,
                            Protocol.MSG_PONG,
                            Protocol.FLAG_URGENT,
                            PayloadCodec.encodePong(pong));
                    health.recordEmit(Protocol.STREAM_CONTROL, seq, after);
                } else if (frame.msgType == Protocol.MSG_SHUTDOWN) {
                    PayloadCodec.Shutdown req = PayloadCodec.decodeShutdown(frame.payload);
                    running.set(false);
                    if (onShutdownRequested != null) {
                        onShutdownRequested.run();
                    }
                    PayloadCodec.Shutdown ack = new PayloadCodec.Shutdown(
                            clock.boottimeNs(), Protocol.SHUTDOWN_NORMAL, "ack:" + req.reason);
                    try {
                        writer.writeFrame(
                                Protocol.STREAM_CONTROL,
                                Protocol.MSG_SHUTDOWN,
                                Protocol.FLAG_URGENT,
                                PayloadCodec.encodeShutdown(ack));
                    } catch (IOException ignored) {
                    }
                    return;
                }
            }
        } catch (IOException ignored) {
            // Host closed socket or transport error
        } catch (Throwable t) {
            System.err.println("arb-control-rx unexpected error: " + t);
        } finally {
            running.set(false);
            if (onShutdownRequested != null) {
                onShutdownRequested.run();
            }
        }
    }
}
