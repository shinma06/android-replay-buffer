package io.github.shinma06.arb.ipc

import io.github.shinma06.arb.model.ArbdRpcException
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
     * alone (the setting applies on the next start). Returns the new connection, or `null` when
     * nothing was restarted.
     */
    @Throws(IOException::class)
    fun restartManagedDaemonIfIdle(startupTimeout: Duration = DEFAULT_STARTUP_TIMEOUT): ConnectedDaemon? {
        val managed = managedDaemonProcess() ?: return null
        val target = resolveDaemonBinary() ?: return null
        val runningExe = managed.info().command().map { Paths.get(it) }.orElse(null)
        if (runningExe != null && runCatching { Files.isSameFile(runningExe, target) }.getOrDefault(false)) return null
        val endpoint = paths.ipcEndpoint()
        val idle = runCatching {
            ArbdRpcClient.connect(endpoint, os).use { rpc ->
                val status = rpc.takeIf { it.systemHello(verifyProtocolVersion = false).pid == managed.pid() }?.systemStatus()
                status != null && status.activeSaveJobs == 0L && status.crashAutosave?.inFlight != true
            }
        }.getOrDefault(false)
        if (!idle) return null
        stopManagedDaemon()
        return spawnAndAwaitHello(target, endpoint, startupTimeout)
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
                probe.connected?.let {
                    return replaceConnectedDaemon(it, upgradeBin, startupTimeout, identityUpgrade = true)
                }
                stopRunningDaemonBestEffort(endpoint)
                return spawnAndAwaitHello(upgradeBin, endpoint, startupTimeout)
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
            if (!requestHandoverShutdown(rpc, identityUpgrade)) return connected
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
     * `system.shutdown_for_handover` lets `arbd` refuse atomically while a save, crash auto-save
     * or parent-bound owner is active (`ok = false`, the replacement is deferred). Only a daemon
     * predating that RPC (`MethodNotFound`) falls back, for an [identityUpgrade], to the legacy
     * status check followed by `system.shutdown`. Returns `true` once the daemon accepted.
     */
    internal fun requestHandoverShutdown(rpc: ArbdRpcClient, identityUpgrade: Boolean): Boolean = try {
        rpc.shutdownForHandover().ok
    } catch (e: ArbdRpcException) {
        if (!e.isMethodNotFound) throw e
        if (!identityUpgrade) {
            false
        } else {
            // Pre-M2 daemon: preserve G-M9 updates, but defer known active saves.
            val status = rpc.systemStatus()
            status.activeSaveJobs == 0L && status.crashAutosave?.inFlight != true && rpc.systemShutdown().ok
        }
    }

    /**
     * Gracefully shuts down any currently running `arbd` (even an older build lacking newer RPCs)
     * and spawns the currently resolved `arbd` binary, waiting until `system.hello` succeeds.
     */
    @Throws(IOException::class)
    fun restartDaemon(startupTimeout: Duration = DEFAULT_STARTUP_TIMEOUT): ConnectedDaemon {
        val binary = resolveDaemonBinary()
            ?: throw IOException(
                "Could not locate arbd binary. Set $ENV_ARB_DAEMON_PATH or build core/target/debug/${os.executableName("arbd")}.",
            )
        val endpoint = paths.ipcEndpoint()
        // An explicit restart is the one place that stops this JVM's managed daemon on purpose.
        stopManagedDaemon()
        stopRunningDaemonBestEffort(endpoint)
        return spawnAndAwaitHello(binary, endpoint, startupTimeout)
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

    private fun stopRunningDaemonBestEffort(endpoint: String) {
        var existingPid: Long? = null
        // 1. Ask the running daemon to exit cleanly via JSON-RPC `system.shutdown`.
        try {
            ArbdRpcClient.connect(endpoint, os).use { rpc ->
                existingPid = runCatching { rpc.systemHello(verifyProtocolVersion = false).pid }.getOrNull()
                runCatching { rpc.systemShutdown() }
            }
        } catch (_: Exception) {
            // Already stopped or non-responsive; fall through to lockfile check.
        }

        if (existingPid == null && isEndpointReachable(endpoint)) {
            existingPid = readLockfilePid()
        }

        // 2. Wait briefly for the old daemon to release its IPC endpoint / exit.
        val gracefulDeadline = Instant.now().plus(Duration.ofMillis(2_000))
        while (Instant.now().isBefore(gracefulDeadline)) {
            val pidStillAlive = existingPid?.let { pid ->
                ProcessHandle.of(pid).map { it.isAlive }.orElse(false)
            } ?: false
            if (!pidStillAlive && !isEndpointReachable(endpoint)) {
                return
            }
            runCatching { Thread.sleep(POLL_INTERVAL_MS) }
        }

        // 3. Fallback: terminate the stale daemon PID politely (SIGTERM on Unix / destroy on Windows).
        existingPid?.let { pid ->
            if (pid != ProcessHandle.current().pid()) {
                ProcessHandle.of(pid).ifPresent { handle ->
                    // Never signal a PID that is not arbd (stale lockfile PID reuse).
                    if (isArbdProcess(handle)) {
                        runCatching { handle.destroy() }
                    }
                }
            }
        }
        val terminateDeadline = Instant.now().plus(Duration.ofMillis(1_500))
        while (Instant.now().isBefore(terminateDeadline)) {
            if (!isEndpointReachable(endpoint)) {
                return
            }
            runCatching { Thread.sleep(POLL_INTERVAL_MS) }
        }
    }

    private fun isArbdProcess(handle: ProcessHandle): Boolean {
        val command = handle.info().command().orElse(null) ?: return false
        val name = runCatching { Paths.get(command).fileName.toString() }.getOrNull() ?: return false
        return name.removeSuffix(".exe").equals("arbd", ignoreCase = true)
    }

    private fun isEndpointReachable(endpoint: String): Boolean = try {
        ArbdRpcClient.connect(endpoint, os).use { true }
    } catch (_: Exception) {
        false
    }

    private fun readLockfilePid(): Long? = runCatching {
        val lockFile = paths.runtimeDir.resolve("arbd.lock")
        if (!Files.isRegularFile(lockFile)) return@runCatching null
        val digits = Files.readString(lockFile).filter { it.isDigit() }
        digits.toLongOrNull()
    }.getOrNull()

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
        if (!parentBound) {
            return pb.start().also { it.outputStream.close() }
        }
        return synchronized(managedLock) {
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
        data class Stale(val reason: String, val connected: ConnectedDaemon? = null) : ProbeResult
        data object NotRunning : ProbeResult
    }

    private fun probeRunningDaemon(endpoint: String, bundledBinary: Path?): ProbeResult {
        var client: ArbdRpcClient? = null
        return try {
            client = ArbdRpcClient.connect(endpoint, os)
            val hello = try {
                client.systemHello(verifyProtocolVersion = true)
            } catch (e: IOException) {
                runCatching { client.close() }
                if (e.message?.contains("protocol version", ignoreCase = true) == true) {
                    return ProbeResult.Stale(e.message ?: "protocol mismatch")
                }
                return ProbeResult.NotRunning
            }
            // Probe `config.get` to catch older arbd builds lacking T-1+ RPCs (-32601).
            try {
                client.configGet()
            } catch (rpc: ArbdRpcException) {
                if (rpc.isMethodNotFound) {
                    runCatching { client.close() }
                    return ProbeResult.Stale("method not found: config.get")
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
            val connected = ConnectedDaemon(client, hello)
            if (hasPendingBundledUpgrade(runningExe, bundledBinary, hello)) {
                return ProbeResult.Stale("bundled arbd differs from running $runningExe (build ${hello.buildId})", connected)
            }
            ProbeResult.Healthy(connected)
        } catch (_: Exception) {
            runCatching { client?.close() }
            ProbeResult.NotRunning
        }
    }

    internal fun hasPendingBundledUpgrade(runningExe: Path?, bundledBinary: Path?, hello: SystemHelloResponse): Boolean {
        val foreignParent = parentBound && hello.parentBound && managedDaemonProcess()?.pid() != hello.pid
        return !foreignParent && !identityUpgradeAttempted &&
            shouldAutoUpgradeBinary(runningExe, bundledBinary, hello.binaryMtimeSecs, hello.buildId)
    }

    private fun tryConnectAndHello(endpoint: String): ConnectedDaemon? {
        var client: ArbdRpcClient? = null
        return try {
            client = ArbdRpcClient.connect(endpoint, os)
            val hello = client.systemHello(verifyProtocolVersion = true)
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
