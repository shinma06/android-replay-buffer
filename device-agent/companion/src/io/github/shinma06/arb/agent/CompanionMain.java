package io.github.shinma06.arb.agent;

import android.net.LocalServerSocket;
import android.net.LocalSocket;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * Entry point for the lightweight ARB Device Companion ({@code arb-companion.dex.jar}).
 *
 * <p>Launch via ADB:
 * <pre>
 * CLASSPATH=/data/local/tmp/arb-companion.dex.jar app_process / --nice-name=arb-companion \
 *   io.github.shinma06.arb.agent.CompanionMain [--socket &lt;abstract-name&gt; | --stdio] \
 *   [--heartbeat-ms 300] [--no-logcat] [--no-input]
 * </pre>
 */
public final class CompanionMain {
    private static final int DEFAULT_HEARTBEAT_MS = 300;
    private static final int MIN_HEARTBEAT_MS = 100;
    private static final int MAX_HEARTBEAT_MS = 5000;

    private CompanionMain() {}

    public static void main(String[] args) {
        String socketName = null;
        boolean useStdio = false;
        int heartbeatMs = DEFAULT_HEARTBEAT_MS;
        boolean enableLogcat = true;
        boolean enableInput = true;

        for (int i = 0; i < args.length; i++) {
            String arg = args[i];
            if ("--socket".equals(arg) && i + 1 < args.length) {
                socketName = args[++i];
            } else if ("--stdio".equals(arg)) {
                useStdio = true;
            } else if ("--heartbeat-ms".equals(arg) && i + 1 < args.length) {
                heartbeatMs = clampHeartbeat(Integer.parseInt(args[++i]));
            } else if ("--no-logcat".equals(arg)) {
                enableLogcat = false;
            } else if ("--no-input".equals(arg)) {
                enableInput = false;
            } else if ("--help".equals(arg) || "-h".equals(arg)) {
                printUsage();
                System.exit(0);
                return;
            } else {
                System.err.println("Unknown argument: " + arg);
                printUsage();
                System.exit(2);
                return;
            }
        }

        if ((socketName == null) == !useStdio) {
            System.err.println("Specify exactly one of --socket <abstract-name> or --stdio");
            printUsage();
            System.exit(2);
            return;
        }

        int exitCode = 0;
        LocalServerSocket serverSocket = null;
        LocalSocket clientSocket = null;
        try {
            InputStream in;
            OutputStream out;
            if (useStdio) {
                in = System.in;
                out = System.out;
            } else {
                serverSocket = new LocalServerSocket(socketName);
                // Signal readiness to host launcher before blocking in accept().
                System.out.println("ARB_COMPANION_READY socket=" + socketName);
                System.out.flush();
                clientSocket = serverSocket.accept();
                // Single-session server: close listening socket immediately after accept.
                try {
                    serverSocket.close();
                } catch (IOException ignored) {
                }
                serverSocket = null;
                in = clientSocket.getInputStream();
                out = clientSocket.getOutputStream();
            }

            runSession(in, out, heartbeatMs, enableLogcat, enableInput);
        } catch (Throwable t) {
            System.err.println("arb-companion fatal error: " + t);
            t.printStackTrace(System.err);
            exitCode = 1;
        } finally {
            closeQuietly(clientSocket);
            closeQuietly(serverSocket);
        }
        System.exit(exitCode);
    }

    private static void runSession(
            InputStream in,
            OutputStream out,
            int heartbeatMs,
            boolean enableLogcat,
            boolean enableInput) throws Exception {
        ClockSource clock = new AndroidClock();
        StreamHealthTracker health = new StreamHealthTracker();
        WireFrame.FrameWriter writer = new WireFrame.FrameWriter(out);
        PayloadCodec.Hello hello = DeviceInfo.collectHello(heartbeatMs);

        AtomicBoolean stopping = new AtomicBoolean(false);
        LogcatWorker logcatWorker = enableLogcat ? new LogcatWorker(writer, clock, health) : null;
        InputWorker inputWorker = enableInput ? new InputWorker(writer, clock, health) : null;

        if (!enableLogcat) {
            health.setState(Protocol.STREAM_LOGCAT, Protocol.STATE_STOPPED);
        }
        if (!enableInput) {
            health.setState(Protocol.STREAM_INPUT, Protocol.STATE_STOPPED);
        }

        Runnable shutdownAll = () -> {
            if (!stopping.compareAndSet(false, true)) {
                return;
            }
            if (logcatWorker != null) {
                logcatWorker.stop();
            }
            if (inputWorker != null) {
                inputWorker.stop();
            }
        };

        ControlWorker controlWorker = new ControlWorker(
                writer, in, clock, health, hello, heartbeatMs, shutdownAll);

        Runtime.getRuntime().addShutdownHook(new Thread(() -> {
            controlWorker.stop();
            shutdownAll.run();
        }, "arb-shutdown-hook"));

        // Stream 0 Hello & Heartbeat start first so host always receives handshake before data.
        controlWorker.sendHelloAndStart();

        // Start fault-isolated workers on independent threads.
        if (logcatWorker != null) {
            logcatWorker.start();
        }
        if (inputWorker != null) {
            inputWorker.start();
        }

        // Block until host disconnects or sends Shutdown.
        controlWorker.awaitCommandLoop();
        controlWorker.stop();
        shutdownAll.run();
        writer.closeQuietly();
    }

    private static int clampHeartbeat(int ms) {
        if (ms < MIN_HEARTBEAT_MS) {
            return MIN_HEARTBEAT_MS;
        }
        if (ms > MAX_HEARTBEAT_MS) {
            return MAX_HEARTBEAT_MS;
        }
        return ms;
    }

    private static void printUsage() {
        System.err.println("Usage: CompanionMain (--socket <abstract-name> | --stdio) "
                + "[--heartbeat-ms <100..5000>] [--no-logcat] [--no-input]");
    }

    private static void closeQuietly(LocalSocket s) {
        if (s != null) {
            try {
                s.close();
            } catch (IOException ignored) {
            }
        }
    }

    private static void closeQuietly(LocalServerSocket s) {
        if (s != null) {
            try {
                s.close();
            } catch (IOException ignored) {
            }
        }
    }
}
