package io.github.shinma06.arb.agent;

import java.io.Closeable;
import java.util.concurrent.TimeUnit;

/** Shared teardown for the worker subprocesses ({@code logcat}, {@code getevent}). */
final class ChildProcesses {
    static final long REAP_TIMEOUT_MS = 500L;

    private ChildProcesses() {}

    /**
     * Force-kills {@code p} and closes its pipes, which also unblocks a reader stuck mid-line
     * ({@code readLine()} is not interruptible on a native pipe).
     */
    static void destroy(Process p) {
        try {
            p.destroyForcibly();
        } catch (NoSuchMethodError e) {
            p.destroy();
        } catch (Throwable ignored) {
        }
        closeQuietly(p.getInputStream());
        closeQuietly(p.getErrorStream());
        closeQuietly(p.getOutputStream());
    }

    /** {@link #destroy} and then reap the child, preserving the caller's interrupt status. */
    static void destroyAndReap(Process p) {
        destroy(p);
        boolean wasInterrupted = Thread.interrupted();
        try {
            p.waitFor(REAP_TIMEOUT_MS, TimeUnit.MILLISECONDS);
        } catch (InterruptedException ignored) {
            wasInterrupted = true;
        } catch (Throwable ignored) {
        } finally {
            if (wasInterrupted) {
                Thread.currentThread().interrupt();
            }
        }
    }

    private static void closeQuietly(Closeable c) {
        if (c == null) {
            return;
        }
        try {
            c.close();
        } catch (Throwable ignored) {
        }
    }
}
