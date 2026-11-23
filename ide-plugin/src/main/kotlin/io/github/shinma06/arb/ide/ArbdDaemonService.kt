package io.github.shinma06.arb.ide

import com.intellij.openapi.Disposable
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.components.Service
import com.intellij.openapi.diagnostic.Logger
import io.github.shinma06.arb.ipc.ArbPaths
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
        } catch (ioe: IOException) {
            // Reset broken transport and retry once on fresh connection/spawn.
            closeClientLocked()
            logger.info("ARB IPC connection reset after IO error: ${ioe.message}")
            val reconnected = ensureConnectedLocked()
            block(reconnected)
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
        fun getInstance(): ArbdDaemonService =
            ApplicationManager.getApplication().getService(ArbdDaemonService::class.java)
    }
}
