package io.github.shinma06.arb.ide

import com.intellij.ide.ActivityTracker
import com.intellij.openapi.Disposable
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.application.PathManager
import com.intellij.openapi.components.Service
import com.intellij.openapi.diagnostic.Logger
import io.github.shinma06.arb.ipc.ArbPaths
import io.github.shinma06.arb.ipc.ArbdIoException
import io.github.shinma06.arb.ipc.ArbdProcessManager
import io.github.shinma06.arb.ipc.ArbdRpcClient
import io.github.shinma06.arb.model.ArbConfig
import io.github.shinma06.arb.model.SystemHelloResponse
import io.github.shinma06.arb.state.CaptureBarReducer
import io.github.shinma06.arb.state.CaptureBarState
import io.github.shinma06.arb.ui.CaptureToggleState
import io.github.shinma06.arb.ui.toToolbarSnapshot
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.TimeUnit
import java.io.IOException
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.Future
import java.util.concurrent.RejectedExecutionException
import java.util.concurrent.ScheduledExecutorService

/**
 * Snapshot of live daemon + device state used by Running Devices & Tools Menu toolbar actions
 * so `update(AnActionEvent)` stays instant, non-blocking, and honest about enabled/tooltip state.
 */
data class ArbToolbarDaemonSnapshot(
    val probed: Boolean = false,
    val daemonConnected: Boolean = false,
    val captureActive: Boolean = false,
    val captureEnabled: Boolean = false,
    val hasBufferedData: Boolean = false,
    val hasOnlineDevice: Boolean = false,
    val selectedDeviceSerial: String? = null,
    val disabledDeviceHint: String? = null,
    val captureConnected: Boolean = true,
) {
    /** OFF / ON・待機中 / ON・REC, by the same rule as the Tool Window ([CaptureToggleState.of]). */
    val captureToggleState: CaptureToggleState
        get() = CaptureToggleState.of(active = captureActive, armed = captureEnabled, connected = captureConnected)
}

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
 * Spawns `arbd` (resolving bundled `<pluginPath>/native/<os>-<arch>/arbd` +
 * `<pluginPath>/device-agent` DEX JARs + `ARB_ADB_PATH`) and verifies `system.hello`.
 *
 * Lifecycle (parent-bound to Android Studio — "Studio alive => running, Studio gone => not
 * running"):
 * - [ensureParentBoundLifecycleStarted] (called off-EDT by [ArbStartupActivity] when the first
 *   project opens, and by a light supervisor afterwards) starts `arbd --exit-on-stdin-eof` without
 *   waiting for the Tool Window, so `capture_enabled` recording resumes right after Studio starts.
 *   An unbound `arbd` left by an older plugin / `arb-cli` is replaced; one bound to a live parent
 *   is reused (see [ArbdProcessManager]).
 * - The opt-in `arb-tray` ([ArbLocalSettingsState.trayEnabled]) is launched as
 *   `arb-tray --exit-on-stdin-eof` with its stdin pipe held open; turning the setting off stops
 *   only `arb-tray`.
 * - Both children read EOF on stdin the moment this JVM dies (exit, crash, `kill -9`), so neither
 *   can outlive Studio. On a normal exit / plugin unload [dispose] stops `arb-tray`, then `arbd`
 *   (`system.shutdown` + stdin close, escalating to `destroy` / `destroyForcibly`), then drops the
 *   IPC connection. Being an application service, closing one of several projects never stops it.
 */
@Service(Service.Level.APP)
open class ArbdDaemonService internal constructor(
    private val processManager: ArbdProcessManager,
    private val trayBinaryResolver: () -> Path? = { processManager.resolveTrayBinary() },
    private val trayProcessStarter: (Path, Map<String, String>) -> Process = ::defaultStartTrayProcess,
    /** IDE-local "show tray" setting ([ArbLocalSettingsState.trayEnabled]) read by the supervisor. */
    private val trayEnabledSetting: () -> Boolean = { ArbLocalSettingsState.getInstanceOrNull()?.trayEnabled ?: false },
    /** Parent PID of a live process (the IDE that owns another `arb-tray`); injectable for tests. */
    private val parentPidOf: (Long) -> Long? = { pid ->
        ProcessHandle.of(pid).flatMap { it.parent() }.map { it.pid() }.orElse(null)
    },
    /** Opens the shared connection (adopting or spawning `arbd`); injectable for tests. */
    private val connector: () -> ArbdProcessManager.ConnectedDaemon = { processManager.connectOrSpawn() },
) : Disposable {
    constructor() : this(
        ArbdProcessManager(
            paths = ArbPaths.resolve(),
            pluginDir = resolveInstalledPluginPath(),
            customBinaryProvider = { ArbLocalSettingsState.getInstanceOrNull()?.customArbdBinaryPathOrNull() },
            ideAndroidSdkProvider = { resolveIdeAndroidSdkPath() },
            parentBound = true,
        ),
    )

    private val logger = Logger.getInstance(ArbdDaemonService::class.java)
    private val lock = Any()
    private val trayLock = Any()
    private val trayExecutor: ExecutorService = Executors.newSingleThreadExecutor { r ->
        Thread(r, "arb-tray-sync").apply { isDaemon = true }
    }
    private val listeners = CopyOnWriteArrayList<ArbDaemonStateListener>()

    @Volatile
    private var client: ArbdRpcClient? = null

    private var managedTrayProcess: Process? = null

    /** Strong reference to `arb-tray`'s stdin write end: closing it quits the parent-bound tray. */
    private var managedTrayStdin: java.io.OutputStream? = null
    private var managedTrayBinaryPath: Path? = null

    private val supervisorStarted = AtomicBoolean()
    private val supervisor: ScheduledExecutorService = Executors.newSingleThreadScheduledExecutor { r ->
        Thread(r, "arbd-parent-bound-supervisor").apply { isDaemon = true }
    }

    @Volatile
    var lastHello: SystemHelloResponse? = null
        private set

    @Volatile
    var cachedConfig: ArbConfig = ArbConfig()
        private set

    /**
     * Single immutable toolbar state: every reader sees one consistent snapshot, and
     * [cachedCaptureActive] / [cachedCaptureEnabled] are derived from it, never stored apart.
     */
    @Volatile
    var cachedToolbarSnapshot: ArbToolbarDaemonSnapshot = ArbToolbarDaemonSnapshot()
        private set
    private val toolbarSnapshotLock = Any()

    val cachedCaptureActive: Boolean
        get() = cachedToolbarSnapshot.captureActive

    val cachedCaptureEnabled: Boolean
        get() = cachedToolbarSnapshot.captureEnabled

    /**
     * PID and parent PID of the other `arb-tray` our own launch yielded to (it held the single-
     * instance lock), remembered while it lives so its exit can be told apart: owner IDE gone ->
     * take the tray over; owner IDE still alive -> the user quit that tray, do not resurrect it.
     */
    private var yieldedTrayHolder: Pair<Long, Long?>? = null

    private val toolbarProbeRunning = AtomicBoolean()
    @Volatile private var lastToolbarProbeNanos: Long? = null
    @Volatile private var disposed = false

    fun toolbarSnapshot(): ArbToolbarDaemonSnapshot {
        val now = System.nanoTime()
        val stale = lastToolbarProbeNanos?.let { now - it >= TimeUnit.SECONDS.toNanos(2) } ?: true
        if (!disposed && stale && toolbarProbeRunning.compareAndSet(false, true)) {
            ApplicationManager.getApplication().executeOnPooledThread {
                try {
                    if (!disposed) refreshToolbarSnapshot()
                } finally {
                    lastToolbarProbeNanos = System.nanoTime()
                    toolbarProbeRunning.set(false)
                }
            }
        }
        return cachedToolbarSnapshot
    }

    internal fun refreshToolbarSnapshot() {
        try {
            withClient { client ->
                val config = client.configGet().config
                val status = client.systemStatus()
                val devices = client.deviceList().devices
                updateCachedConfig(config)
                val armed = status.captureEnabled || config.captureEnabled
                val state = CaptureBarReducer.fromStatus(CaptureBarState(), status, devices, config = config)
                updateCachedToolbarSnapshot(state.toToolbarSnapshot(captureEnabled = armed))
            }
        } catch (_: Exception) {
            updateCachedToolbarSnapshot(ArbToolbarDaemonSnapshot(probed = true))
        }
    }

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
            // A new binary is a new upgrade target; re-arm the once-per-session identity upgrade
            // and move an idle managed daemon onto it now (a busy one switches on its next start).
            processManager.resetIdentityUpgrade()
            if (!disposed) {
                runCatching { processManager.restartManagedDaemonIfIdle() }
                    .onFailure { logger.warn("ARB arbd restart after binary change failed: ${it.message}", it) }
                    .getOrNull()
                    ?.let { connected ->
                        client = connected.client
                        lastHello = connected.hello
                    }
            }
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
            check(!disposed) { "ArbdDaemonService is already disposed" }
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
        updateToolbarSnapshot { it.copy(probed = true, daemonConnected = true, captureActive = active) }
    }

    fun updateCachedCaptureState(active: Boolean, enabled: Boolean) {
        updateToolbarSnapshot {
            it.copy(probed = true, daemonConnected = true, captureActive = active, captureEnabled = enabled)
        }
    }

    fun updateCachedToolbarSnapshot(snapshot: ArbToolbarDaemonSnapshot) {
        lastToolbarProbeNanos = System.nanoTime()
        updateToolbarSnapshot { snapshot }
    }

    /** Publishes one new immutable toolbar snapshot (read-modify-write is atomic). */
    private fun updateToolbarSnapshot(transform: (ArbToolbarDaemonSnapshot) -> ArbToolbarDaemonSnapshot) {
        val changed = synchronized(toolbarSnapshotLock) {
            val previous = cachedToolbarSnapshot
            cachedToolbarSnapshot = transform(previous)
            cachedToolbarSnapshot != previous
        }
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

    /**
     * Starts (or adopts) the parent-bound `arbd` now, without waiting for any UI, and keeps it
     * running while Studio lives: a supervisor re-checks every [SUPERVISOR_INTERVAL_SECONDS] s and
     * respawns `arbd` if it stopped (crash, tray "Stop Daemon && Quit", or the other IDE that owned
     * a shared daemon exited). Blocking; call off-EDT. Failures are logged, never thrown.
     */
    open fun ensureParentBoundLifecycleStarted() {
        if (disposed) return
        ensureDaemonRunning()
        if (supervisorStarted.compareAndSet(false, true)) {
            runCatching {
                supervisor.scheduleWithFixedDelay(
                    {
                        if (!disposed) ensureDaemonRunning()
                        if (!disposed) superviseTray()
                    },
                    SUPERVISOR_INTERVAL_SECONDS,
                    SUPERVISOR_INTERVAL_SECONDS,
                    TimeUnit.SECONDS,
                )
            }
        }
    }

    private fun ensureDaemonRunning() {
        try {
            // `system.hello` is idempotent: a dead connection reconnects through connectOrSpawn,
            // which spawns (or adopts) the parent-bound daemon.
            withClient { rpc ->
                val hello = rpc.systemHello(verifyProtocolVersion = false)
                if (processManager.parentBound && !hello.parentBound &&
                    processManager.managedDaemonProcess()?.pid() != hello.pid
                ) {
                    // Re-probe separately so a busy daemon retains its client and viewport leases.
                    val next = processManager.connectOrSpawn()
                    if (next.hello.pid == hello.pid) {
                        next.client.close()
                    } else {
                        closeClientLocked()
                        client = next.client
                        lastHello = next.hello
                    }
                }
            }
        } catch (e: Exception) {
            if (!disposed) logger.warn("ARB arbd parent-bound start failed: ${e.message}", e)
        }
    }

    /**
     * Supervisor step for the opt-in tray (P0-3): when [trayEnabledSetting] is on and the
     * `arb-tray` this IDE launched has exited, decide from `arb-tray.lock` (which holds the PID of
     * the last owner) whether to launch it again:
     * - our tray was the owner (or no owner is on record) -> it was quit from its own menu or
     *   failed: respect that until the setting is toggled or the IDE restarts;
     * - another live tray owns it -> we yielded (several IDE windows); remember it and its owner;
     * - that owner is gone and its parent (the IDE that launched it) is gone too -> the other IDE
     *   closed: take the tray over. If that parent still lives, the user quit it: do not resurrect.
     */
    internal fun superviseTray() {
        if (disposed || !runCatching(trayEnabledSetting).getOrDefault(false)) return
        synchronized(trayLock) {
            val own = managedTrayProcess ?: return
            if (own.isAlive || disposed) return
            val holder = readTrayLockPid()
            when {
                // No other owner on record (or it was ours): nothing was yielded, nothing to take over.
                holder == null || holder == own.pid() -> forgetManagedTrayLocked()
                isAlive(holder) -> {
                    if (yieldedTrayHolder?.first != holder) yieldedTrayHolder = holder to parentPidOf(holder)
                }
                else -> {
                    val ownerIde = yieldedTrayHolder?.takeIf { it.first == holder }?.second
                    if (ownerIde != null && isAlive(ownerIde)) {
                        forgetManagedTrayLocked()
                    } else {
                        yieldedTrayHolder = null
                        runCatching { launchBundledTray() }
                            .onFailure { logger.warn("ARB arb-tray takeover failed: ${it.message}", it) }
                    }
                }
            }
        }
    }

    private fun forgetManagedTrayLocked() {
        runCatching { managedTrayStdin?.close() }
        managedTrayProcess = null
        managedTrayStdin = null
        managedTrayBinaryPath = null
        yieldedTrayHolder = null
    }

    private fun readTrayLockPid(): Long? = runCatching {
        Files.readString(processManager.paths.trayLockFile()).trim().toLongOrNull()
    }.getOrNull()

    private fun isAlive(pid: Long): Boolean = ProcessHandle.of(pid).map { it.isAlive }.orElse(false)

    /**
     * Stops the parent-bound `arbd` this IDE launched (never one it merely connected to). Returns
     * `true` when a managed daemon was present.
     */
    open fun stopManagedDaemon(): Boolean = processManager.stopManagedDaemon()

    /**
     * Launches the bundled `arb-tray` as `arb-tray --exit-on-stdin-eof` under plugin-managed,
     * parent-bound lifecycle ownership (idempotent while already running; `arb-tray` also guards
     * its single-instance lock). Its stdin pipe stays open until [stopBundledTray] or JVM death.
     * No `--auto-start`: the IDE owns `arbd`, so the tray never spawns an unbound daemon.
     */
    @Throws(IOException::class)
    open fun launchBundledTray(): Path = synchronized(trayLock) {
        check(!disposed) { "ArbdDaemonService is already disposed" }
        val existing = managedTrayProcess
        if (existing != null && existing.isAlive) {
            return@synchronized managedTrayBinaryPath
                ?: trayBinaryResolver()
                ?: throw IOException("同梱の arb-tray バイナリが見つかりません。")
        }
        managedTrayProcess = null
        managedTrayBinaryPath = null
        yieldedTrayHolder = null
        val trayBin = trayBinaryResolver()
            ?: throw IOException("同梱の arb-tray バイナリが見つかりません。")
        val daemonBin = runCatching { processManager.resolveDaemonBinary() }.getOrNull()
        val env = processManager.buildDaemonEnvironment(daemonBin)
        val proc = trayProcessStarter(trayBin, env)
        managedTrayProcess = proc
        managedTrayStdin = proc.outputStream
        managedTrayBinaryPath = trayBin
        trayBin
    }

    /**
     * Cleanly terminates ONLY the plugin-managed `arb-tray` child process (never stopping `arbd`).
     * Returns `true` if a managed process was present and stopped.
     */
    open fun stopBundledTray(): Boolean = synchronized(trayLock) {
        // Held until the old process has exited: a launch queued right behind this stop must not
        // start while the old `arb-tray` still owns its single-instance lock (it would just exit).
        val proc = managedTrayProcess
        val stdin = managedTrayStdin
        managedTrayProcess = null
        managedTrayStdin = null
        managedTrayBinaryPath = null
        yieldedTrayHolder = null
        if (proc == null) {
            runCatching { stdin?.close() }
            return@synchronized false
        }
        // Parent-bound quit first: EOF on stdin makes `arb-tray` exit through its own quit path.
        runCatching { stdin?.close() }
        if (proc.isAlive && !waitForExit(proc, TRAY_STOP_TIMEOUT_MS)) {
            runCatching { proc.destroy() }
            if (!waitForExit(proc, TRAY_STOP_TIMEOUT_MS)) {
                runCatching { proc.destroyForcibly() }
                waitForExit(proc, TRAY_STOP_TIMEOUT_MS)
            }
        }
        true
    }

    private fun waitForExit(proc: Process, timeoutMs: Long): Boolean =
        runCatching { proc.waitFor(timeoutMs, TimeUnit.MILLISECONDS) }.getOrDefault(!proc.isAlive)

    /**
     * Synchronizes the plugin-managed `arb-tray` process with [enabled] (`true` -> launch
     * idempotently; `false` -> terminate ONLY `arb-tray`, leaving `arbd` untouched).
     */
    open fun syncTrayState(enabled: Boolean) {
        if (enabled) {
            if (disposed) return
            runCatching { launchBundledTray() }
                .onFailure { logger.warn("ARB arb-tray launch failed: ${it.message}", it) }
        } else {
            stopBundledTray()
        }
    }

    /**
     * Off-EDT helper invoked by Settings Apply. Requests run one at a time in submission order, so
     * a quick OFF -> ON ends with the tray running.
     */
    fun syncTrayStateAsync(enabled: Boolean): Future<*>? = submitTrayWork { syncTrayState(enabled) }

    private fun submitTrayWork(work: () -> Unit): Future<*>? = try {
        trayExecutor.submit(work)
    } catch (_: RejectedExecutionException) {
        null
    }

    internal fun restoreTrayAtStartup(enabled: () -> Boolean) = submitTrayWork {
        if (enabled()) syncTrayState(true)
    }

    /**
     * Startup sequence run by [ArbStartupActivity] (off-EDT): start the parent-bound `arbd` first
     * — regardless of [ArbLocalSettingsState.trayEnabled] — then restore the tray if it is enabled,
     * so the tray connects to the IDE-owned daemon instead of finding none.
     */
    internal fun startParentBoundLifecycle(trayEnabled: () -> Boolean): Future<*>? {
        ensureParentBoundLifecycleStarted()
        return restoreTrayAtStartup(trayEnabled)
    }

    internal fun isManagedTrayRunningForTest(): Boolean = synchronized(trayLock) {
        managedTrayProcess?.isAlive == true
    }

    /** Resolves the bundled `arb-cli` binary path for Settings CLI/MCP copy actions. */
    open fun resolveBundledCliPath(): Path? =
        ArbCliMcpPathResolver.resolveCliPath(
            pluginDir = resolveInstalledPluginPath(),
            os = processManager.paths.os,
        )

    /**
     * Runs [block] on the shared connection (connecting or spawning `arbd` first). After a
     * transport failure the whole block is replayed once on a fresh connection only when every
     * request it had already sent is an idempotent read ([isIdempotentRpc]): a mutating or
     * resource-acquiring call (`bundle.open` lease, `save.*`, `export.*`, ...) may have been
     * applied by the daemon even though its response was lost, and replaying it would apply it
     * twice (e.g. a second, orphaned lease). A request whose write failed never reached `arbd`.
     */
    @Throws(IOException::class)
    open fun <T> withClient(block: (ArbdRpcClient) -> T): T = synchronized(lock) {
        check(!disposed)
        val active = ensureConnectedLocked()
        val sent = ArrayList<String>()
        active.requestSentListener = { sent += it }
        try {
            block(active)
        } catch (ioe: ArbdIoException) {
            closeClientLocked()
            if (!sent.all(::isIdempotentRpc)) {
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
        } finally {
            active.requestSentListener = null
        }
    }

    fun isConnected(): Boolean = client != null

    fun disconnect() = synchronized(lock) {
        closeClientLocked()
    }

    private fun ensureConnectedLocked(): ArbdRpcClient {
        client?.let { return it }
        // Never (re)spawn arbd once disposed, e.g. from the idempotent-RPC retry in withClient.
        check(!disposed) { "ArbdDaemonService is already disposed" }
        val connected = connector()
        client = connected.client
        lastHello = connected.hello
        return connected.client
    }

    private fun closeClientLocked() {
        runCatching { client?.close() }
        client = null
    }

    override fun dispose() {
        disposed = true
        // Never interrupt (shutdownNow): a running check may be mid-RPC on the shared connection.
        supervisor.shutdown()
        trayExecutor.shutdown()
        // Stop the children outside the RPC [lock]: an in-flight call (a 120 s export) would
        // otherwise hold IDE exit. Stopping `arbd` makes that call fail fast with an IO error.
        runCatching { stopBundledTray() }.onFailure { logger.warn("ARB arb-tray stop failed", it) }
        runCatching { stopManagedDaemon() }.onFailure { logger.warn("ARB arbd stop failed", it) }
        // Not under [lock] either: closing the transport makes a still-blocked call return, and
        // no new connection can be made once [disposed] is set (see ensureConnectedLocked).
        client?.let { runCatching { it.close() } }
        client = null
    }

    companion object {
        const val PLUGIN_ID: String = "io.github.shinma06.android-replay-buffer"
        private const val TRAY_STOP_TIMEOUT_MS: Long = 1_500L
        internal const val SUPERVISOR_INTERVAL_SECONDS: Long = 10L

        /** `arb-tray` command line for the parent-bound, IDE-owned tray. */
        internal fun trayCommand(trayBin: Path): List<String> =
            listOf(trayBin.toAbsolutePath().toString(), ArbdProcessManager.FLAG_EXIT_ON_STDIN_EOF)

        /** Starts the tray with its stdin pipe left open (the caller keeps `outputStream`). */
        private fun defaultStartTrayProcess(trayBin: Path, env: Map<String, String>): Process {
            val pb = ProcessBuilder(trayCommand(trayBin))
                .redirectInput(ProcessBuilder.Redirect.PIPE)
                .redirectOutput(ProcessBuilder.Redirect.DISCARD)
                .redirectError(ProcessBuilder.Redirect.DISCARD)
            pb.environment().putAll(env)
            return pb.start()
        }

        /**
         * Installed plugin root (`<plugins>/arb-ide-plugin`). Derived from the jar that holds this
         * class (`<plugin>/lib/arb-ide-plugin-*.jar`) with the public [PathManager] API instead of the
         * `@ApiStatus.Internal` plugin-descriptor lookup that Plugin Verifier rejects.
         */
        internal fun resolveInstalledPluginPath(): Path? = runCatching {
            val jar = PathManager.getJarPathForClass(ArbdDaemonService::class.java)?.let { Path.of(it) }
            jar?.parent?.takeIf { it.fileName?.toString() == "lib" }?.parent
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
         * `*.list` / `save.list_jobs`, lease keep-alive/close, `timeline.*`, `media.*` and
         * `edit.get_session`. Mutating or resource-acquiring calls (`bundle.open` creates a
         * lease, `save.*`, `bundle.trash/restore/gc`, `capture.*`, `export.*`,
         * `edit.update_session`) are never retried.
         */
        internal fun isIdempotentRpc(method: String): Boolean {
            if (method == "system.shutdown" || method == "system.shutdown_for_handover") return false
            return method.startsWith("system.") ||
                method == "config.get" ||
                method.endsWith(".list") ||
                method.endsWith(".list_jobs") ||
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
