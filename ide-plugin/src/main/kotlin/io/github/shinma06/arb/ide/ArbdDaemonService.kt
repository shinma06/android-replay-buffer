package io.github.shinma06.arb.ide

import com.intellij.ide.ActivityTracker
import com.intellij.ide.plugins.PluginManagerCore
import com.intellij.openapi.Disposable
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.components.Service
import com.intellij.openapi.diagnostic.Logger
import com.intellij.openapi.extensions.PluginId
import io.github.shinma06.arb.ipc.ArbPaths
import io.github.shinma06.arb.ipc.ArbdIoException
import io.github.shinma06.arb.ipc.ArbdProcessManager
import io.github.shinma06.arb.ipc.ArbdRpcClient
import io.github.shinma06.arb.model.ArbConfig
import io.github.shinma06.arb.model.SystemHelloResponse
import java.io.IOException
import java.nio.file.Path
import java.util.concurrent.CopyOnWriteArrayList

/**
 * Snapshot of live daemon + device state used by Running Devices & Tools Menu toolbar actions
 * so `update(AnActionEvent)` stays instant, non-blocking, and honest about enabled/tooltip state.
 */
data class ArbToolbarDaemonSnapshot(
    val probed: Boolean = false,
    val daemonConnected: Boolean = false,
    val captureActive: Boolean = false,
    val hasBufferedData: Boolean = false,
    val hasOnlineDevice: Boolean = false,
    val selectedDeviceSerial: String? = null,
    val disabledDeviceHint: String? = null,
)

/**
 * Listener notified immediately when user settings (`config.update`), custom `arbd` binary
 * settings, or saved/trimmed/trashed bundles change so Tool Windows and toolbar actions refresh
 * without waiting for polling or restart.
 */
interface ArbDaemonStateListener {
    fun onConfigUpdated(config: ArbConfig) {}
    fun onDaemonConnectionChanged() {}
    fun onCatalogChanged() {}
}

/**
 * Application-level service owning the thin IPC connection to the local `arbd` daemon.
 * Lazily spawns `arbd` (resolving bundled `<pluginPath>/native/<os>-<arch>/arbd` +
 * `<pluginPath>/device-agent` DEX JARs + `ARB_ADB_PATH`) and verifies `system.hello`.
 */
@Service(Service.Level.APP)
open class ArbdDaemonService internal constructor(
    private val processManager: ArbdProcessManager,
) : Disposable {
    constructor() : this(
        ArbdProcessManager(
            paths = ArbPaths.resolve(),
            pluginDir = resolveInstalledPluginPath(),
            customBinaryProvider = { ArbLocalSettingsState.getInstanceOrNull()?.customArbdBinaryPathOrNull() },
            ideAndroidSdkProvider = { resolveIdeAndroidSdkPath() },
        ),
    )

    private val logger = Logger.getInstance(ArbdDaemonService::class.java)
    private val lock = Any()
    private val listeners = CopyOnWriteArrayList<ArbDaemonStateListener>()

    @Volatile
    private var client: ArbdRpcClient? = null

    @Volatile
    var lastHello: SystemHelloResponse? = null
        private set

    @Volatile
    var cachedConfig: ArbConfig = ArbConfig()
        private set

    @Volatile
    var cachedCaptureActive: Boolean = false
        private set

    @Volatile
    var cachedToolbarSnapshot: ArbToolbarDaemonSnapshot = ArbToolbarDaemonSnapshot()
        private set

    fun addStateListener(listener: ArbDaemonStateListener) {
        listeners.addIfAbsent(listener)
    }

    fun removeStateListener(listener: ArbDaemonStateListener) {
        listeners.remove(listener)
    }

    fun updateCachedConfig(config: ArbConfig) {
        val changed = cachedConfig != config
        cachedConfig = config
        if (changed) {
            bumpActionToolbarRefresh()
        }
    }

    /**
     * Broadcasts an applied `config.update` immediately to Tool Windows and IntelliJ toolbar
     * actions (Running Devices / Logcat) without requiring an IDE restart or waiting for polling.
     */
    fun notifyConfigUpdated(config: ArbConfig) {
        cachedConfig = config
        bumpActionToolbarRefresh()
        for (listener in listeners) {
            runCatching { listener.onConfigUpdated(config) }
                .onFailure { logger.warn("ARB config listener failed", it) }
        }
    }

    /**
     * Drops the existing IPC connection after a custom `arbd` binary path change and notifies
     * listeners to reconnect and refresh UI immediately.
     */
    fun notifyBinaryPathChanged() {
        synchronized(lock) {
            closeClientLocked()
            lastHello = null
        }
        bumpActionToolbarRefresh()
        for (listener in listeners) {
            runCatching { listener.onDaemonConnectionChanged() }
                .onFailure { logger.warn("ARB connection listener failed", it) }
        }
    }

    /**
     * Gracefully stops any running (possibly stale/older) `arbd` instance, launches the currently
     * configured `arbd` binary, reconnects, and broadcasts refresh notifications to all IDE views.
     */
    @Throws(IOException::class)
    fun restartDaemon(): SystemHelloResponse {
        val hello = synchronized(lock) {
            closeClientLocked()
            lastHello = null
            val connected = processManager.restartDaemon()
            client = connected.client
            lastHello = connected.hello
            runCatching { connected.client.configGet().config }.getOrNull()?.let { cfg ->
                cachedConfig = cfg
            }
            connected.hello
        }
        bumpActionToolbarRefresh()
        for (listener in listeners) {
            runCatching { listener.onDaemonConnectionChanged() }
                .onFailure { logger.warn("ARB connection listener failed", it) }
        }
        return hello
    }

    fun updateCachedCaptureActive(active: Boolean) {
        val changed = cachedCaptureActive != active || cachedToolbarSnapshot.captureActive != active
        cachedCaptureActive = active
        cachedToolbarSnapshot = cachedToolbarSnapshot.copy(
            probed = true,
            daemonConnected = true,
            captureActive = active,
        )
        if (changed) {
            bumpActionToolbarRefresh()
        }
    }

    fun updateCachedToolbarSnapshot(snapshot: ArbToolbarDaemonSnapshot) {
        val changed = cachedToolbarSnapshot != snapshot || cachedCaptureActive != snapshot.captureActive
        cachedCaptureActive = snapshot.captureActive
        cachedToolbarSnapshot = snapshot
        if (changed) {
            bumpActionToolbarRefresh()
        }
    }

    /**
     * Broadcasts bundle creation/trim/trash/restore immediately so the ARB Tool Window's
     * library (`すべて` / `トリム済み` / `ゴミ箱`) refreshes without waiting for the 2s poll.
     */
    fun notifyCatalogChanged() {
        bumpActionToolbarRefresh()
        for (listener in listeners) {
            runCatching { listener.onCatalogChanged() }
                .onFailure { logger.warn("ARB catalog listener failed", it) }
        }
    }

    private fun bumpActionToolbarRefresh() {
        runCatching { ActivityTracker.getInstance()?.inc() }
    }

    /** Launches the bundled `arb-tray` menu-bar / system-tray app with zero terminal setup. */
    @Throws(IOException::class)
    open fun launchBundledTray(): Path = processManager.launchBundledTray()

    /** Resolves the bundled `arb-cli` binary path for Settings CLI/MCP copy actions. */
    open fun resolveBundledCliPath(): Path? =
        ArbCliMcpPathResolver.resolveCliPath(
            pluginDir = resolveInstalledPluginPath(),
            os = processManager.paths.os,
        )

    @Throws(IOException::class)
    open fun <T> withClient(block: (ArbdRpcClient) -> T): T = synchronized(lock) {
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

    fun isConnected(): Boolean = client != null

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
        const val PLUGIN_ID: String = "io.github.shinma06.android-replay-buffer"

        internal fun resolveInstalledPluginPath(): Path? = runCatching {
            PluginManagerCore.getPlugin(PluginId.getId(PLUGIN_ID))?.pluginPath
        }.getOrNull()

        /**
         * Best-effort lookup of Android Studio's configured Android SDK home via reflection so
         * `ide-plugin` compiles without hard-linking Android plugin internals while still finding
         * `platform-tools/adb` on macOS Dock / Windows Start Menu GUI launches.
         */
        internal fun resolveIdeAndroidSdkPath(): Path? = runCatching {
            val cls = Class.forName("com.android.tools.idea.sdk.IdeSdks")
            val inst = cls.getMethod("getInstance").invoke(null) ?: return@runCatching null
            val sdkDir = cls.getMethod("getAndroidSdkPath").invoke(inst) as? java.io.File
            sdkDir?.toPath()
        }.getOrNull()

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
                method == "config.get" ||
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
