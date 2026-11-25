package io.github.shinma06.arb.ipc

import io.github.shinma06.arb.model.ArbdRpcException
import io.github.shinma06.arb.model.IPC_PROTOCOL_VERSION
import io.github.shinma06.arb.model.SystemHelloResponse
import java.io.File
import java.io.IOException
import java.io.OutputStream
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths
import java.nio.file.attribute.FileTime
import java.security.MessageDigest
import java.time.Duration
import java.time.Instant
import java.util.HexFormat
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Resolves the `arbd` (and sibling `arb-tray` / `device-agent` DEX JARs / `adb`) binary locations,
 * lazily spawns `arbd` when its local IPC endpoint is absent, transparently auto-upgrades stale
 * `arbd` instances lacking required RPCs or older than the bundled plugin binary, and verifies the
 * `system.hello` protocol handshake (`SPEC-01`, `SPEC-03`).
 *
 * Parent-bound lifecycle ([parentBound] `= true`, always used by the IDE plugin): `arbd` is
 * launched with `--exit-on-stdin-eof` and this JVM keeps the write end of its stdin pipe for as
 * long as it lives, so the kernel stops `arbd` whenever the IDE exits, crashes or is killed. A
 * running daemon that is not parent-bound (an older plugin's, `arb-cli --auto-start`'s, or one a
 * user started by hand) is replaced by a parent-bound one; a daemon already bound to a live parent
 * (this IDE, or another running IDE) is used as is. [stopManagedDaemon] stops the daemon this JVM
 * launched.
 */
class ArbdProcessManager(
    val paths: ArbPaths = ArbPaths.resolve(),
    private val pluginDir: Path? = null,
    private val searchRoot: Path? = null,
    private val os: HostOs = paths.os,
    private val arch: String = HostOs.detectArch(),
    private val env: (String) -> String? = System::getenv,
    private val customBinaryProvider: () -> Path? = { null },
    private val ideAndroidSdkProvider: () -> Path? = { null },
    /** Launch `arbd` with `--exit-on-stdin-eof` and hold its stdin pipe (see class KDoc). */
    val parentBound: Boolean = false,
) {
    private val managedLock = Any()
    private val handoverOwner = java.util.UUID.randomUUID().toString()
    private var lifecycleClosed = false

    /** The `arbd` this JVM launched parent-bound; its stdin pipe is [managedDaemonStdin]. */
    private var managedDaemonProcess: Process? = null

    /**
     * Strong reference to the write end of [managedDaemonProcess]'s stdin. Never closed while the
     * daemon is wanted: closing it (or this JVM dying) is what stops a parent-bound `arbd`.
     */
    private var managedDaemonStdin: OutputStream? = null

    /** The live parent-bound `arbd` this JVM launched, or `null`. */
    fun managedDaemonProcess(): Process? = synchronized(managedLock) {
        managedDaemonProcess?.takeIf { it.isAlive }
    }

    /**
     * Loop guard: identity-based (same-path mtime / `build_id`) auto-upgrade restarts `arbd` at most
     * once per IDE session. A failing replacement (e.g. an unkillable stale daemon or a clock skew
     * that keeps the on-disk mtime "newer") then degrades to using the running daemon instead of
     * restarting it on every reconnect. Protocol/RPC incompatibility upgrades are not limited.
     */
    private val identityUpgradeClaimed = AtomicBoolean(false)

    /** `true` once this session has already spent its single identity-based auto-upgrade. */
    val identityUpgradeAttempted: Boolean
        get() = identityUpgradeClaimed.get()

    /** Claims the single per-session identity-based auto-upgrade; `false` if already spent. */
    fun tryClaimIdentityUpgrade(): Boolean = identityUpgradeClaimed.compareAndSet(false, true)

    /** Re-arms the identity-based auto-upgrade, e.g. after the `arbd` binary setting changed. */
    fun resetIdentityUpgrade() = identityUpgradeClaimed.set(false)

    /**
     * Applies a changed `arbd` binary setting: when the managed daemon of this JVM runs another
     * executable than [resolveDaemonBinary] and is idle (no save job, no crash auto-save in
     * flight), it is stopped and the resolved binary started. A busy or foreign daemon is left
     * alone. Returns the connection after handover (or to the busy daemon when deferred),
     * or null if there is no managed daemon to move.
     */
    @Throws(IOException::class)
    fun restartManagedDaemonIfIdle(startupTimeout: Duration = DEFAULT_STARTUP_TIMEOUT): ConnectedDaemon? {
        val managed = managedDaemonProcess() ?: return null
        val target = resolveDaemonBinary() ?: return null
        val runningExe = managed.info().command().map { Paths.get(it) }.orElse(null)
        if (runningExe != null && runCatching { Files.isSameFile(runningExe, target) }.getOrDefault(false)) return null
        val endpoint = paths.ipcEndpoint()
        val connected = tryConnectAndHello(endpoint) ?: return null
        if (connected.hello.pid != managed.pid()) {
            connected.client.close()
            return null
        }
        return replaceConnectedDaemon(connected, target, startupTimeout, identityUpgrade = false)
    }

    /**
     * Identity-based auto-upgrade decision for a probed daemon: `true` (and the per-session
     * restart claimed) when [bundledBinary] differs from the running daemon per
     * [shouldAutoUpgradeBinary], at most once per session (loop guard).
     */
    fun shouldRestartForIdentity(
        runningExe: Path?,
        bundledBinary: Path?,
        hello: SystemHelloResponse,
    ): Boolean = shouldAutoUpgradeBinary(runningExe, bundledBinary, hello.binaryMtimeSecs, hello.buildId) &&
        tryClaimIdentityUpgrade()

    /**
     * Resolution order:
     * 1. `ARB_DAEMON_PATH` env var (and `ARB_DAEMON_BIN` fallback)
     * 2. User-configured custom `arbd` binary path override (IDE Settings)
     * 3. Bundled plugin binary `<pluginDir>/native/<os>-<arch>/arbd[.exe]` (with legacy `bin/` fallback)
     * 4. Sibling dev Cargo build (`core/target/debug/arbd` or `core/target/release/arbd`)
     * 5. Executable on `PATH`
     */
    fun resolveDaemonBinary(): Path? {
        val envOverride = env(ENV_ARB_DAEMON_PATH)?.takeIf { it.isNotBlank() }
            ?: env(ENV_ARB_DAEMON_BIN)?.takeIf { it.isNotBlank() }
        if (envOverride != null) {
            val candidate = Paths.get(envOverride)
            if (Files.isRegularFile(candidate)) {
                return ensureUnixExecutable(candidate)
            }
        }

        val customCandidate = runCatching { customBinaryProvider() }.getOrNull()
        if (customCandidate != null && Files.isRegularFile(customCandidate)) {
            return ensureUnixExecutable(customCandidate)
        }

        resolveBundledBinary(os.executableName("arbd"))?.let { return it }

        val devStart = searchRoot ?: Paths.get("").toAbsolutePath()
        findDevBinary(devStart, os.executableName("arbd"))?.let { return ensureUnixExecutable(it) }

        findOnPath(os.executableName("arbd"))?.let { return it }

        return null
    }

    /** Resolves bundled `<pluginDir>/native/<os>-<arch>/<exeName>` (or legacy `bin/<os>-<arch>/`). */
    fun resolveBundledBinary(exeName: String): Path? {
        val dir = pluginDir ?: return null
        val slug = HostOs.platformBinSlug(os, arch)
        // The Gradle bundler names Intel directories `<os>-x86_64`; accept it as the `x64` alias.
        val slugs = if (arch == "x64") listOf(slug, "${os.bundleSlug}-x86_64") else listOf(slug)
        val candidates = slugs.flatMap {
            listOf(
                dir.resolve("native").resolve(it).resolve(exeName),
                dir.resolve("bin").resolve(it).resolve(exeName),
            )
        }
        for (candidate in candidates) {
            if (Files.isRegularFile(candidate)) {
                return ensureUnixExecutable(candidate)
            }
        }
        return null
    }

    /** Resolves `arb-tray[.exe]` from bundled plugin dir, dev build, or `PATH`. */
    fun resolveTrayBinary(): Path? {
        val exeName = os.executableName("arb-tray")
        resolveBundledBinary(exeName)?.let { return it }
        val devStart = searchRoot ?: Paths.get("").toAbsolutePath()
        findDevBinary(devStart, exeName)?.let { return ensureUnixExecutable(it) }
        return findOnPath(exeName)
    }

    /** Resolves `arb-companion.dex.jar` from env, bundled `<pluginDir>/device-agent/`, or dev build. */
    fun resolveCompanionJar(): Path? = resolveDeviceAgentJar(ENV_ARB_COMPANION_JAR, "arb-companion.dex.jar")

    /** Resolves `replay-scrcpy-audio.dex.jar` from env, bundled `<pluginDir>/device-agent/`, or dev build. */
    fun resolveScrcpyJar(): Path? = resolveDeviceAgentJar(ENV_ARB_SCRCPY_JAR, "replay-scrcpy-audio.dex.jar")

    private fun resolveDeviceAgentJar(envKey: String, fileName: String): Path? {
        env(envKey)?.takeIf { it.isNotBlank() }?.let { raw ->
            val candidate = Paths.get(raw)
            if (Files.isRegularFile(candidate)) return candidate
        }
        pluginDir?.resolve("device-agent")?.resolve(fileName)?.takeIf { Files.isRegularFile(it) }?.let {
            return it
        }
        var current: Path? = (searchRoot ?: Paths.get("")).toAbsolutePath().normalize()
        repeat(6) {
            val dir = current ?: return null
            val inBuild = dir.resolve("device-agent").resolve("build").resolve(fileName)
            if (Files.isRegularFile(inBuild)) return inBuild
            val direct = dir.resolve("device-agent").resolve(fileName)
            if (Files.isRegularFile(direct)) return direct
            current = dir.parent
        }
        return null
    }

    /**
     * Resolves host `adb` binary even when Android Studio is launched from macOS Dock / GUI
     * with a minimal `PATH` (`/usr/bin:/bin`).
     */
    fun resolveAdbBinary(): Path? {
        val exeName = os.executableName("adb")
        env(ENV_ARB_ADB_PATH)?.takeIf { it.isNotBlank() }?.let { raw ->
            val p = Paths.get(raw)
            if (Files.isRegularFile(p)) return p
        }
        val sdkCandidates = buildList<Path> {
            runCatching { ideAndroidSdkProvider() }.getOrNull()?.let { add(it) }
            env("ANDROID_HOME")?.takeIf { it.isNotBlank() }?.let { add(Paths.get(it)) }
            env("ANDROID_SDK_ROOT")?.takeIf { it.isNotBlank() }?.let { add(Paths.get(it)) }
            env("LOCALAPPDATA")?.takeIf { it.isNotBlank() }?.let {
                add(Paths.get(it).resolve("Android").resolve("Sdk"))
            }
            val home = env("HOME")?.takeIf { it.isNotBlank() }
                ?: env("USERPROFILE")?.takeIf { it.isNotBlank() }
                ?: System.getProperty("user.home")?.takeIf { it.isNotBlank() }
            if (home != null) {
                val homePath = Paths.get(home)
                add(homePath.resolve("Library").resolve("Android").resolve("sdk"))
                add(homePath.resolve("Android").resolve("Sdk"))
                add(homePath.resolve("AppData").resolve("Local").resolve("Android").resolve("Sdk"))
            }
        }
        for (sdk in sdkCandidates) {
            val candidate = sdk.resolve("platform-tools").resolve(exeName)
            if (Files.isRegularFile(candidate)) return candidate
        }
        return findOnPath(exeName)
    }

    /** Builds environment overrides passed to spawned `arbd` / `arb-tray` processes. */
    fun buildDaemonEnvironment(daemonBinary: Path? = null): Map<String, String> = buildMap {
        if (daemonBinary != null && env(ENV_ARB_DAEMON_PATH).isNullOrBlank()) {
            put(ENV_ARB_DAEMON_PATH, daemonBinary.toAbsolutePath().normalize().toString())
        }
        if (env(ENV_ARB_COMPANION_JAR).isNullOrBlank()) {
            resolveCompanionJar()?.let { put(ENV_ARB_COMPANION_JAR, it.toAbsolutePath().normalize().toString()) }
        }
        if (env(ENV_ARB_SCRCPY_JAR).isNullOrBlank()) {
            resolveScrcpyJar()?.let { put(ENV_ARB_SCRCPY_JAR, it.toAbsolutePath().normalize().toString()) }
        }
        if (env(ENV_ARB_ADB_PATH).isNullOrBlank()) {
            resolveAdbBinary()?.let { put(ENV_ARB_ADB_PATH, it.toAbsolutePath().normalize().toString()) }
        }
    }

    /**
     * Connects to an already-running `arbd`, auto-upgrading transparently if the running daemon is
     * stale (missing required `config.get` RPC or strictly older than the bundled plugin binary),
     * or lazily spawns `arbd` and waits until `system.hello` succeeds.
     */
    @Throws(IOException::class)
    fun connectOrSpawn(startupTimeout: Duration = DEFAULT_STARTUP_TIMEOUT): ConnectedDaemon {
        val endpoint = paths.ipcEndpoint()
        val resolvedBinary = resolveDaemonBinary()
        val bundledBinary = resolveBundledBinary(os.executableName("arbd"))

        // Only auto-upgrade when the bundled binary is what a respawn would actually launch;
        // otherwise a custom/env binary older than the bundled one would loop restart forever.
        val upgradeTarget = bundledBinary?.takeIf { it == resolvedBinary }
        when (val probe = probeRunningDaemon(endpoint, upgradeTarget)) {
            is ProbeResult.Healthy -> {
                if (!parentBound || probe.connected.hello.parentBound || managedDaemonProcess()?.pid() == probe.connected.hello.pid ||
                    resolvedBinary == null
                ) {
                    return probe.connected
                }
                val connected = probe.connected
                return replaceConnectedDaemon(connected, resolvedBinary, startupTimeout, identityUpgrade = false)
            }
            is ProbeResult.Stale -> {
                val upgradeBin = resolvedBinary
                    ?: throw IOException(
                        "Running arbd is incompatible (${probe.reason}) and no newer arbd binary was found.",
                    )
                val next = replaceConnectedDaemon(probe.connected, upgradeBin, startupTimeout, probe.identityUpgrade)
                if (!probe.identityUpgrade && next === probe.connected) {
                    next.client.close()
                    throw IOException("Incompatible arbd cannot hand over safely: ${probe.reason}")
                }
                return next
            }
            ProbeResult.NotRunning -> {
                // A live managed daemon that does not answer yet is still starting (slow disk,
                // antivirus scan) or its pipe is momentarily busy: wait for it. Respawning would
                // close its stdin and stop a healthy `arbd` (an IOException retries next tick).
                managedDaemonProcess()?.let { return waitForHello(endpoint, startupTimeout, it) }
                val binary = resolvedBinary
                    ?: throw IOException(
                        "Could not locate arbd binary. Set $ENV_ARB_DAEMON_PATH or build core/target/debug/${os.executableName("arbd")}.",
                    )
                return spawnAndAwaitHello(binary, endpoint, startupTimeout)
            }
        }
    }

    private fun replaceConnectedDaemon(
        connected: ConnectedDaemon,
        binary: Path,
        timeout: Duration,
        identityUpgrade: Boolean,
    ): ConnectedDaemon {
        val rpc = connected.client
        val oldProcess = ProcessHandle.of(connected.hello.pid).orElse(null)
        try {
            val owner = handoverOwner.takeIf { managedDaemonProcess()?.pid() == connected.hello.pid }
            if (connected.hello.parentBound && owner == null) return connected
            if (!requestHandoverShutdown(rpc, owner)) return connected
        } catch (e: Exception) {
            rpc.close()
            throw e
        }
        rpc.close()
        if (identityUpgrade) tryClaimIdentityUpgrade()
        if (oldProcess != null && oldProcess.isAlive) {
            try {
                oldProcess.onExit().get(timeout.toMillis(), TimeUnit.MILLISECONDS)
            } catch (e: java.util.concurrent.TimeoutException) {
                throw IOException("Timed out waiting for arbd handover", e)
            }
        }
        val endpoint = paths.ipcEndpoint()
        return tryConnectAndHello(endpoint) ?: spawnAndAwaitHello(binary, endpoint, timeout)
    }

    /**
     * Asks the daemon on [rpc] to exit for a replacement without ever interrupting a save:
     * `system.shutdown_for_handover` lets `arbd` refuse atomically while a save or crash auto-save
     * is active. Older daemons cannot reserve admission after a status read, so defer.
     */
    internal fun requestHandoverShutdown(rpc: ArbdRpcClient, owner: String? = null): Boolean = try {
        rpc.shutdownForHandover(owner).ok
    } catch (e: ArbdRpcException) {
        if (!e.isMethodNotFound) throw e
        false
    }

    /**
     * Requests an idle handover and spawns the currently resolved `arbd` binary, waiting until `system.hello` succeeds.
     */
    @Throws(IOException::class)
    fun restartDaemon(startupTimeout: Duration = DEFAULT_STARTUP_TIMEOUT): ConnectedDaemon {
        val binary = resolveDaemonBinary()
            ?: throw IOException(
                "Could not locate arbd binary. Set $ENV_ARB_DAEMON_PATH or build core/target/debug/${os.executableName("arbd")}.",
            )
        val endpoint = paths.ipcEndpoint()
        val running = tryConnectAndHello(endpoint, verifyProtocolVersion = false)
        if (running == null) {
            if (managedDaemonProcess() != null) throw IOException("Managed arbd is not ready for restart")
            return spawnAndAwaitHello(binary, endpoint, startupTimeout)
        }
        val next = replaceConnectedDaemon(running, binary, startupTimeout, identityUpgrade = false)
        if (next === running) {
            running.client.close()
            throw IOException("arbd は保存中、動画を開いている Studio タブがある、別の IDE が所有中、または安全な再起動に未対応です。")
        }
        return next
    }

    private fun spawnAndAwaitHello(binary: Path, endpoint: String, startupTimeout: Duration): ConnectedDaemon {
        val process = spawnDaemon(binary)
        return try {
            waitForHello(endpoint, startupTimeout, process)
        } catch (e: IOException) {
            if (!parentBound || process.isAlive || process.exitValue() == 0 || !rejectsParentFlag(binary)) throw e
            waitForHello(endpoint, startupTimeout, spawnDaemon(binary, bound = false))
        }
    }

    // Probe the parser without starting a daemon. Lock contention and runtime failures must
    // never downgrade the lifecycle of this or a later launch.
    private fun rejectsParentFlag(binary: Path): Boolean {
        val probe = ProcessBuilder(binary.toString(), FLAG_EXIT_ON_STDIN_EOF, "--version")
            .redirectOutput(ProcessBuilder.Redirect.DISCARD)
            .start()
        return try {
            probe.waitFor(2, TimeUnit.SECONDS) && probe.exitValue() != 0 &&
                probe.errorStream.bufferedReader().use { it.readText().trim() } ==
                "arbd error: unknown argument `$FLAG_EXIT_ON_STDIN_EOF`"
        } finally {
            probe.destroyForcibly()
            probe.errorStream.close()
            probe.outputStream.close()
        }
    }

    @Throws(IOException::class)
    fun spawnDaemon(binary: Path): Process = spawnDaemon(binary, bound = parentBound)

    private fun spawnDaemon(binary: Path, bound: Boolean): Process {
        ensureUnixExecutable(binary)
        Files.createDirectories(paths.dataDir)
        Files.createDirectories(paths.cacheDir)
        Files.createDirectories(paths.runtimeDir)

        val cmd = listOf(
            binary.toAbsolutePath().toString(),
            "--data-dir",
            paths.dataDir.toAbsolutePath().toString(),
            "--cache-dir",
            paths.cacheDir.toAbsolutePath().toString(),
            "--runtime-dir",
            paths.runtimeDir.toAbsolutePath().toString(),
        )
        val pb = ProcessBuilder(if (bound) cmd + FLAG_EXIT_ON_STDIN_EOF else cmd)
            .redirectInput(ProcessBuilder.Redirect.PIPE)
            .redirectOutput(ProcessBuilder.Redirect.DISCARD)
            .redirectError(ProcessBuilder.Redirect.DISCARD)
        pb.environment().putAll(buildDaemonEnvironment(binary))
        if (bound) pb.environment()["ARB_HANDOVER_OWNER"] = handoverOwner
        if (!parentBound) {
            return pb.start().also { it.outputStream.close() }
        }
        return synchronized(managedLock) {
            check(!lifecycleClosed) { "IDE lifecycle already ended" }
            // Start first: a failed start must leave the previous managed daemon running.
            val process = pb.start()
            val replacedStdin = managedDaemonStdin
            managedDaemonProcess = process
            managedDaemonStdin = process.outputStream
            // The replaced managed daemon (stale / restart) is released only now.
            runCatching { replacedStdin?.close() }
            process
        }
    }

    /** Prevents a pending startup from publishing a new child after IDE disposal. */
    fun closeLifecycle() {
        synchronized(managedLock) { lifecycleClosed = true }
        stopManagedDaemon()
    }

    /**
     * Stops the parent-bound `arbd` this JVM launched: `system.shutdown` (bounded by
     * [shutdownRpcTimeoutMs], sent only if the endpoint still answers with that daemon's PID) plus
     * closing its stdin pipe — either one alone takes the daemon's clean shutdown path — then waits
     * up to [exitTimeoutMs] and escalates to `destroy()` / `destroyForcibly()`. Daemons this JVM
     * did not launch are never touched. Returns `true` when a managed daemon was present.
     */
    fun stopManagedDaemon(
        shutdownRpcTimeoutMs: Long = MANAGED_SHUTDOWN_RPC_TIMEOUT_MS,
        exitTimeoutMs: Long = MANAGED_EXIT_TIMEOUT_MS,
    ): Boolean {
        val (process, stdin) = synchronized(managedLock) {
            val taken = managedDaemonProcess to managedDaemonStdin
            managedDaemonProcess = null
            managedDaemonStdin = null
            taken
        }
        if (process == null) {
            runCatching { stdin?.close() }
            return false
        }
        if (process.isAlive) {
            requestShutdownOf(process.pid(), shutdownRpcTimeoutMs)
        }
        runCatching { stdin?.close() }
        if (!waitForExit(process, exitTimeoutMs)) {
            runCatching { process.destroy() }
            if (!waitForExit(process, MANAGED_DESTROY_TIMEOUT_MS)) {
                runCatching { process.destroyForcibly() }
                waitForExit(process, MANAGED_DESTROY_TIMEOUT_MS)
            }
        }
        return true
    }

    private fun requestShutdownOf(pid: Long, timeoutMs: Long) {
        runCatching {
            val transport = LocalIpcTransport.connect(paths.ipcEndpoint(), os)
            ArbdRpcClient(transport, readTimeoutMs = timeoutMs, longRunningReadTimeoutMs = timeoutMs).use { rpc ->
                if (rpc.systemHello(verifyProtocolVersion = false).pid == pid) {
                    rpc.systemShutdown()
                }
            }
        }
    }

    private fun waitForExit(process: Process, timeoutMs: Long): Boolean =
        runCatching { process.waitFor(timeoutMs, TimeUnit.MILLISECONDS) }.getOrDefault(!process.isAlive)

    @Throws(IOException::class)
    fun waitForHello(
        endpoint: String,
        timeout: Duration = DEFAULT_STARTUP_TIMEOUT,
        spawnedProcess: Process? = null,
    ): ConnectedDaemon {
        val deadline = Instant.now().plus(timeout)
        while (true) {
            tryConnectAndHello(endpoint)?.let { connected ->
                return connected.copy(spawnedProcess = spawnedProcess)
            }
            if (spawnedProcess != null && !spawnedProcess.isAlive) {
                throw IOException("arbd process exited prematurely with code ${spawnedProcess.exitValue()}")
            }
            if (Instant.now().isAfter(deadline)) {
                throw IOException("Timed out after ${timeout.toMillis()} ms waiting for arbd at $endpoint")
            }
            try {
                Thread.sleep(POLL_INTERVAL_MS)
            } catch (e: InterruptedException) {
                Thread.currentThread().interrupt()
                throw IOException("Interrupted while waiting for arbd at $endpoint", e)
            }
        }
    }

    private sealed interface ProbeResult {
        data class Healthy(val connected: ConnectedDaemon) : ProbeResult
        data class Stale(val reason: String, val connected: ConnectedDaemon, val identityUpgrade: Boolean = false) : ProbeResult
        data object NotRunning : ProbeResult
    }

    private fun probeRunningDaemon(endpoint: String, bundledBinary: Path?): ProbeResult {
        var client: ArbdRpcClient? = null
        return try {
            client = ArbdRpcClient.connect(endpoint, os)
            val hello = client.systemHello(verifyProtocolVersion = false)
            val connected = ConnectedDaemon(client, hello)
            if (hello.protocolVersion != IPC_PROTOCOL_VERSION) {
                return ProbeResult.Stale("protocol version ${hello.protocolVersion}", connected)
            }
            // Probe `config.get` to catch older arbd builds lacking T-1+ RPCs (-32601).
            try {
                client.configGet()
            } catch (rpc: ArbdRpcException) {
                if (rpc.isMethodNotFound) {
                    return ProbeResult.Stale("method not found: config.get", connected)
                }
            } catch (_: IOException) {
                // hello already succeeded: a busy daemon (read timeout) is not absent, but the
                // timed-out transport is closed, so hand back a fresh connection instead of it.
                runCatching { client.close() }
                val fresh = tryConnectAndHello(endpoint)
                return if (fresh != null) ProbeResult.Healthy(fresh) else ProbeResult.NotRunning
            }
            // If we have a bundled plugin binary that is strictly newer than the running daemon binary,
            // auto-upgrade transparently so installing a newer plugin ZIP takes effect immediately.
            val runningExe = ProcessHandle.of(hello.pid)
                .flatMap { it.info().command() }
                .map { Paths.get(it) }
                .orElse(null)
            if (hasPendingBundledUpgrade(runningExe, bundledBinary, hello)) {
                return ProbeResult.Stale("bundled arbd differs from running $runningExe (build ${hello.buildId})", connected, identityUpgrade = true)
            }
            ProbeResult.Healthy(connected)
        } catch (_: Exception) {
            runCatching { client?.close() }
            ProbeResult.NotRunning
        }
    }

    internal fun hasPendingBundledUpgrade(runningExe: Path?, bundledBinary: Path?, hello: SystemHelloResponse): Boolean {
        val foreignParent = hello.parentBound && managedDaemonProcess()?.pid() != hello.pid
        return !foreignParent && !identityUpgradeAttempted &&
            shouldAutoUpgradeBinary(runningExe, bundledBinary, hello.binaryMtimeSecs, hello.buildId)
    }

    private fun tryConnectAndHello(endpoint: String, verifyProtocolVersion: Boolean = true): ConnectedDaemon? {
        var client: ArbdRpcClient? = null
        return try {
            client = ArbdRpcClient.connect(endpoint, os)
            val hello = client.systemHello(verifyProtocolVersion)
            ConnectedDaemon(client = client, hello = hello, spawnedProcess = null)
        } catch (_: Exception) {
            runCatching { client?.close() }
            null
        }
    }

    private fun ensureUnixExecutable(path: Path): Path {
        // Only repair the bundled plugin files; never chmod user-supplied env/Settings paths.
        val nativeRoot = pluginDir?.toAbsolutePath()?.normalize()
        val isBundled = nativeRoot != null && path.toAbsolutePath().normalize().startsWith(nativeRoot)
        if (isBundled && os.usesExecuteBit && Files.isRegularFile(path) && !Files.isExecutable(path)) {
            runCatching { path.toFile().setExecutable(true, true) }
        }
        return path
    }

    private fun findDevBinary(start: Path, exeName: String): Path? {
        var current: Path? = start.toAbsolutePath().normalize()
        repeat(6) {
            val dir = current ?: return null
            val releaseCandidate = dir.resolve("core").resolve("target").resolve("release").resolve(exeName)
            if (Files.isRegularFile(releaseCandidate)) return releaseCandidate
            val debugCandidate = dir.resolve("core").resolve("target").resolve("debug").resolve(exeName)
            if (Files.isRegularFile(debugCandidate)) return debugCandidate
            current = dir.parent
        }
        return null
    }

    private fun findOnPath(exeName: String): Path? {
        val pathEnv = env("PATH").orEmpty()
        for (dir in pathEnv.split(File.pathSeparatorChar)) {
            if (dir.isBlank()) continue
            val candidate = Paths.get(dir).resolve(exeName)
            if (Files.isRegularFile(candidate) && Files.isExecutable(candidate)) {
                return candidate
            }
        }
        return null
    }

    data class ConnectedDaemon(
        val client: ArbdRpcClient,
        val hello: SystemHelloResponse,
        val spawnedProcess: Process? = null,
    )

    companion object {
        const val ENV_ARB_DAEMON_PATH: String = "ARB_DAEMON_PATH"
        const val ENV_ARB_DAEMON_BIN: String = "ARB_DAEMON_BIN"
        const val ENV_ARB_COMPANION_JAR: String = "ARB_COMPANION_JAR"
        const val ENV_ARB_SCRCPY_JAR: String = "ARB_SCRCPY_JAR"
        const val ENV_ARB_ADB_PATH: String = "ARB_ADB_PATH"
        val DEFAULT_STARTUP_TIMEOUT: Duration = Duration.ofSeconds(5)
        private const val POLL_INTERVAL_MS: Long = 50L

        /** `arbd` / `arb-tray` flag: exit when the parent closes the stdin pipe. */
        const val FLAG_EXIT_ON_STDIN_EOF: String = "--exit-on-stdin-eof"
        const val MANAGED_SHUTDOWN_RPC_TIMEOUT_MS: Long = 1_500L
        const val MANAGED_EXIT_TIMEOUT_MS: Long = 2_000L
        private const val MANAGED_DESTROY_TIMEOUT_MS: Long = 1_500L

        /**
         * Returns `true` when the bundled plugin `arbd` should replace the running daemon:
         * - when `system.hello` `build_id` carries a content digest (`...:sha256:<hex>`, the SHA-256
         *   of the running executable): restart iff the bundled binary's SHA-256 differs (same
         *   path), or — for a different path — never restart onto byte-identical content. This
         *   catches in-place plugin ZIP overwrites where only constants/strings changed and the
         *   reproducible ZIP produced the same size and mtime (M9);
         * - older daemons without a digest (`<version>-<size>-<mtime>`): same path restarts when the
         *   on-disk mtime is strictly newer than [runningBinaryMtimeSecs] or the size differs;
         * - different path (normal plugin upgrade): the bundled file is strictly newer on disk.
         *
         * Callers must pair this with [tryClaimIdentityUpgrade] so a restart happens at most once.
         */
        fun shouldAutoUpgradeBinary(
            runningExe: Path?,
            bundledBinary: Path?,
            runningBinaryMtimeSecs: Long? = null,
            runningBuildId: String? = null,
            digestOf: (Path) -> String? = ::sha256Hex,
        ): Boolean {
            if (runningExe == null || bundledBinary == null) return false
            if (!Files.isRegularFile(runningExe) || !Files.isRegularFile(bundledBinary)) return false
            val bundledMtime = runCatching { Files.getLastModifiedTime(bundledBinary) }.getOrNull() ?: return false
            val sameFile = runCatching { Files.isSameFile(runningExe, bundledBinary) }.getOrDefault(false)
            val identity = parseBuildIdentity(runningBuildId)
            val runningDigest = identity?.sha256Hex
            val bundledDigest = if (runningDigest != null) digestOf(bundledBinary) else null
            if (sameFile) {
                if (runningDigest != null && bundledDigest != null) {
                    return !bundledDigest.equals(runningDigest, ignoreCase = true)
                }
                if (runningBinaryMtimeSecs != null && bundledMtime.toInstant().epochSecond > runningBinaryMtimeSecs) {
                    return true
                }
                val runningSize = identity?.sizeBytes ?: return false
                val bundledSize = runCatching { Files.size(bundledBinary) }.getOrNull() ?: return false
                return runningSize != bundledSize
            }
            if (runningDigest != null && bundledDigest != null &&
                bundledDigest.equals(runningDigest, ignoreCase = true)
            ) {
                return false
            }
            val runningMtime = runCatching { Files.getLastModifiedTime(runningExe) }.getOrNull() ?: return false
            return bundledMtime > runningMtime
        }

        private const val SHA256_MARKER = ":sha256:"
        private val SHA256_HEX = Regex("^[0-9a-fA-F]{64}$")

        /**
         * Parses `arbd`'s `system.hello` `build_id`:
         * - `<CARGO_PKG_VERSION>:sha256:<hex>` (content digest of the running executable), optionally
         *   with a legacy `<version>-<size>-<mtime>` prefix before `:sha256:`;
         * - legacy `<CARGO_PKG_VERSION>-<size_bytes>-<mtime_secs>` (the version may itself contain
         *   `-`, so the two numeric fields are taken from the end).
         */
        fun parseBuildIdentity(buildId: String?): DaemonBuildIdentity? {
            val raw = buildId?.trim()?.takeIf { it.isNotEmpty() } ?: return null
            val markerAt = raw.lastIndexOf(SHA256_MARKER)
            if (markerAt >= 0) {
                val hex = raw.substring(markerAt + SHA256_MARKER.length)
                if (!SHA256_HEX.matches(hex)) return null
                val prefix = raw.substring(0, markerAt)
                val legacy = parseLegacyBuildIdentity(prefix)
                val version = legacy?.version ?: prefix.takeIf { it.isNotBlank() } ?: return null
                return DaemonBuildIdentity(
                    version = version,
                    sizeBytes = legacy?.sizeBytes,
                    mtimeSecs = legacy?.mtimeSecs,
                    sha256Hex = hex.lowercase(),
                )
            }
            return parseLegacyBuildIdentity(raw)
        }

        private fun parseLegacyBuildIdentity(raw: String): DaemonBuildIdentity? {
            val mtimeSep = raw.lastIndexOf('-')
            if (mtimeSep <= 0) return null
            val sizeSep = raw.lastIndexOf('-', mtimeSep - 1)
            if (sizeSep <= 0) return null
            val version = raw.substring(0, sizeSep)
            val size = raw.substring(sizeSep + 1, mtimeSep).toLongOrNull() ?: return null
            val mtime = raw.substring(mtimeSep + 1).toLongOrNull() ?: return null
            if (version.isBlank() || size < 0L || mtime < 0L) return null
            return DaemonBuildIdentity(version = version, sizeBytes = size, mtimeSecs = mtime)
        }

        private data class DigestStamp(val device: Long, val inode: Long, val changed: FileTime)
        private data class CachedDigest(val stamp: DigestStamp, val hex: String)
        private val digestCache = ConcurrentHashMap<Path, CachedDigest>()

        // An in-place reinstall may preserve inode, size and mtime. Cache only on
        // filesystems that also report a change time, and recheck it after hashing.
        private fun digestStamp(path: Path): DigestStamp? {
            if ("unix" !in path.fileSystem.supportedFileAttributeViews()) return null
            return runCatching {
                val attrs = Files.readAttributes(path, "unix:dev,ino,ctime")
                DigestStamp(attrs["dev"] as Long, attrs["ino"] as Long, attrs["ctime"] as FileTime)
            }.getOrNull()
        }

        fun sha256Hex(file: Path): String? = runCatching {
            val path = file.toAbsolutePath().normalize()
            val stamp = digestStamp(path) ?: return@runCatching computeSha256Hex(path)
            // A recently modified inode may still share the filesystem clock tick
            // with a subsequent overwrite, especially on coarse-grained filesystems.
            if (stamp.changed.toInstant().isAfter(Instant.now().minusSeconds(2))) {
                return@runCatching computeSha256Hex(path)
            }
            digestCache[path]?.takeIf { it.stamp == stamp }?.hex ?: computeSha256Hex(path).also {
                if (stamp == digestStamp(path)) digestCache[path] = CachedDigest(stamp, it)
            }
        }.getOrNull()

        private fun computeSha256Hex(file: Path): String {
            val md = MessageDigest.getInstance("SHA-256")
            Files.newInputStream(file).use { input ->
                val buf = ByteArray(1 shl 16)
                while (true) {
                    val n = input.read(buf)
                    if (n < 0) break
                    md.update(buf, 0, n)
                }
            }
            return HexFormat.of().formatHex(md.digest())
        }
    }
}

/**
 * Parsed `system.hello` `build_id` fingerprint of the running `arbd` executable: the content
 * digest ([sha256Hex]) when the daemon reports one, and/or the legacy size + mtime.
 */
data class DaemonBuildIdentity(
    val version: String,
    val sizeBytes: Long? = null,
    val mtimeSecs: Long? = null,
    val sha256Hex: String? = null,
)
