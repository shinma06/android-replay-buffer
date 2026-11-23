package io.github.shinma06.arb.ipc

import java.io.Closeable
import java.time.Duration
import java.util.concurrent.Executors
import java.util.concurrent.ScheduledExecutorService
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger

/**
 * Manages a viewer tab's active Viewer Lease lifecycle:
 * - Periodically renews the lease via `bundle.lease_renew` while the viewer tab remains open.
 * - On `close()` (tab close / dispose), cancels the heartbeat, optionally closes the SHM viewport,
 *   and releases the lease via `bundle.close` exactly once.
 */
class BundleLeaseController(
    val leaseId: String,
    private val renewAction: (String) -> Unit,
    private val closeLeaseAction: (String) -> Unit,
    private val closeViewportAction: (() -> Unit)? = null,
    heartbeatInterval: Duration = DEFAULT_HEARTBEAT_INTERVAL,
    private val onHeartbeatError: (Throwable) -> Unit = {},
) : Closeable {
    private val closed = AtomicBoolean(false)
    private val renewCount = AtomicInteger(0)
    private val lifecycleLock = Any()
    @Volatile
    private var heartbeatThread: Thread? = null
    private val scheduler: ScheduledExecutorService = Executors.newSingleThreadScheduledExecutor { runnable ->
        Thread(runnable, "arb-lease-heartbeat-$leaseId").apply {
            isDaemon = true
            heartbeatThread = this
        }
    }
    private val heartbeatTask: ScheduledFuture<*>

    init {
        val periodMs = heartbeatInterval.toMillis().coerceAtLeast(10L)
        heartbeatTask = scheduler.scheduleAtFixedRate(
            {
                synchronized(lifecycleLock) {
                    if (closed.get()) {
                        return@scheduleAtFixedRate
                    }
                    try {
                        renewAction(leaseId)
                        renewCount.incrementAndGet()
                    } catch (t: Throwable) {
                        onHeartbeatError(t)
                    }
                }
            },
            periodMs,
            periodMs,
            TimeUnit.MILLISECONDS,
        )
    }

    val isClosed: Boolean
        get() = closed.get()

    val successfulRenewals: Int
        get() = renewCount.get()

    override fun close() {
        if (!closed.compareAndSet(false, true)) {
            return
        }
        heartbeatTask.cancel(false)
        scheduler.shutdown()
        // Serialize teardown against any heartbeat already inside renewAction so
        // bundle.lease_renew never runs concurrently with or after bundle.close.
        synchronized(lifecycleLock) {
            try {
                closeViewportAction?.invoke()
            } catch (_: Throwable) {
                // Best effort: always proceed to release the bundle lease even if viewport close fails.
            }
            closeLeaseAction(leaseId)
        }
        scheduler.shutdownNow()
        if (Thread.currentThread() !== heartbeatThread) {
            try {
                scheduler.awaitTermination(1, TimeUnit.SECONDS)
            } catch (_: InterruptedException) {
                Thread.currentThread().interrupt()
            }
        }
    }

    companion object {
        val DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration.ofSeconds(15)

        fun forClient(
            client: ArbdRpcClient,
            leaseId: String,
            viewportId: String? = null,
            heartbeatInterval: Duration = DEFAULT_HEARTBEAT_INTERVAL,
            onHeartbeatError: (Throwable) -> Unit = {},
        ): BundleLeaseController = BundleLeaseController(
            leaseId = leaseId,
            renewAction = { id -> client.bundleLeaseRenew(id) },
            closeLeaseAction = { id -> client.bundleClose(id) },
            closeViewportAction = viewportId?.let { vpId -> { client.mediaCloseViewport(vpId) } },
            heartbeatInterval = heartbeatInterval,
            onHeartbeatError = onHeartbeatError,
        )
    }
}
