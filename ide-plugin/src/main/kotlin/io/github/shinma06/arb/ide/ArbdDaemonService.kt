package io.github.shinma06.arb.ide

import com.intellij.openapi.Disposable
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.components.Service
import com.intellij.openapi.diagnostic.Logger
import io.github.shinma06.arb.ipc.ArbPaths
import io.github.shinma06.arb.ipc.ArbdIoException
import io.github.shinma06.arb.ipc.ArbdProcessManager
import io.github.shinma06.arb.ipc.ArbdRpcClient
import io.github.shinma06.arb.model.SystemHelloResponse
import java.io.IOException

/**
 * Application-level service owning the thin IPC connection to the local `arbd` daemon.
 * Lazily spawns `arbd` when needed and verifies `system.hello`.
 */
@Service(Service.Level.APP)
class ArbdDaemonService : Disposable {
    private val logger = Logger.getInstance(ArbdDaemonService::class.java)
    private val lock = Any()
    private val processManager = ArbdProcessManager(paths = ArbPaths.resolve())

    @Volatile
    private var client: ArbdRpcClient? = null

    @Volatile
    var lastHello: SystemHelloResponse? = null
        private set

    @Throws(IOException::class)
    fun <T> withClient(block: (ArbdRpcClient) -> T): T = synchronized(lock) {
        val active = ensureConnectedLocked()
        try {
            block(active)
        } catch (ioe: ArbdIoException) {
            // R4 S4: a mutating call may already have committed on the daemon — replaying
            // it could double-apply (trash+restore, duplicate save job, ...). Retry only
            // idempotent read RPCs after a transport reset.
            closeClientLocked()
            if (!isIdempotentRpc(ioe.rpcMethod)) {
                throw ioe
            }
            logger.info("ARB IPC connection reset after IO error: ${ioe.message}")
            val reconnected = ensureConnectedLocked()
            block(reconnected)
        } catch (ioe: IOException) {
            // Protocol-level failure (id mismatch, malformed frame): never replay — the
            // request may already have committed on the daemon.
            closeClientLocked()
            throw ioe
        }
    }

    fun isConnected(): Boolean = synchronized(lock) { client != null }

    fun disconnect() = synchronized(lock) {
        closeClientLocked()
    }

    private fun ensureConnectedLocked(): ArbdRpcClient {
        client?.let { return it }
        val connected = processManager.connectOrSpawn()
        client = connected.client
        lastHello = connected.hello
        return connected.client
    }

    private fun closeClientLocked() {
        runCatching { client?.close() }
        client = null
    }

    override fun dispose() {
        disconnect()
    }

    companion object {
        /**
         * Whether a JSON-RPC `method` is safe to replay after a transport reset (R4 S4):
         * idempotent reads only — `system.*` (except the mutating `system.shutdown`),
         * `*.list` / `save.list_jobs`, `bundle.open`, lease keep-alive/close,
         * `timeline.*`, `media.*` and `edit.get_session`. Mutating calls (`save.*`,
         * `bundle.trash/restore/gc`, `capture.*`, `export.*`, `edit.update_session`) are
         * never retried.
         */
        internal fun isIdempotentRpc(method: String): Boolean {
            if (method == "system.shutdown") return false
            return method.startsWith("system.") ||
                method.endsWith(".list") ||
                method.endsWith(".list_jobs") ||
                method == "bundle.open" ||
                method == "bundle.lease_renew" ||
                method == "bundle.close" ||
                method.startsWith("timeline.") ||
                method.startsWith("media.") ||
                method == "edit.get_session"
        }

        fun getInstance(): ArbdDaemonService =
            ApplicationManager.getApplication().getService(ArbdDaemonService::class.java)
    }
}
