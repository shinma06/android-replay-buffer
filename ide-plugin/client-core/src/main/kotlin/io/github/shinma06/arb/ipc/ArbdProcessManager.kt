package io.github.shinma06.arb.ipc

import io.github.shinma06.arb.model.ArbdRpcException
import io.github.shinma06.arb.model.SystemHelloResponse
import java.io.File
import java.io.IOException
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths
import java.time.Duration
import java.time.Instant

/**
 * Resolves the `arbd` (and sibling `arb-tray` / `device-agent` DEX JARs / `adb`) binary locations,
 * lazily spawns `arbd` when its local IPC endpoint is absent, transparently auto-upgrades stale
 * `arbd` instances lacking required RPCs or older than the bundled plugin binary, and verifies the
 * `system.hello` protocol handshake (`SPEC-01`, `SPEC-03`).
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
) {
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

        resolveBundledBinary(os.daemonExecutableName())?.let { return it }

        val devStart = searchRoot ?: Paths.get("").toAbsolutePath()
        findDevBinary(devStart, os.daemonExecutableName())?.let { return ensureUnixExecutable(it) }

        findOnPath(os.daemonExecutableName())?.let { return it }

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
        val exeName = if (os == HostOs.WINDOWS) "arb-tray.exe" else "arb-tray"
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
        val exeName = if (os == HostOs.WINDOWS) "adb.exe" else "adb"
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
        val bundledBinary = resolveBundledBinary(os.daemonExecutableName())

        // Only auto-upgrade when the bundled binary is what a respawn would actually launch;
        // otherwise a custom/env binary older than the bundled one would loop restart forever.
        val upgradeTarget = bundledBinary?.takeIf { it == resolvedBinary }
        when (val probe = probeRunningDaemon(endpoint, upgradeTarget)) {
            is ProbeResult.Healthy -> return probe.connected
            is ProbeResult.Stale -> {
                val upgradeBin = resolvedBinary
                    ?: throw IOException(
                        "Running arbd is incompatible (${probe.reason}) and no newer arbd binary was found.",
                    )
                stopRunningDaemonBestEffort(endpoint)
                val process = spawnDaemon(upgradeBin)
                return waitForHello(endpoint, startupTimeout, process)
            }
            ProbeResult.NotRunning -> {
                val binary = resolvedBinary
                    ?: throw IOException(
                        "Could not locate arbd binary. Set $ENV_ARB_DAEMON_PATH or build core/target/debug/${os.daemonExecutableName()}.",
                    )
                val process = spawnDaemon(binary)
                return waitForHello(endpoint, startupTimeout, process)
            }
        }
    }

    /** Launches the bundled `arb-tray` menu-bar / system-tray app with zero terminal setup. */
    @Throws(IOException::class)
    fun launchBundledTray(): Path {
        val trayBin = resolveTrayBinary()
            ?: throw IOException("同梱の arb-tray バイナリが見つかりません。")
        ensureUnixExecutable(trayBin)
        val daemonBin = resolveDaemonBinary()
        val pb = ProcessBuilder(listOf(trayBin.toAbsolutePath().toString(), "--auto-start"))
            .redirectInput(ProcessBuilder.Redirect.PIPE)
            .redirectOutput(ProcessBuilder.Redirect.DISCARD)
            .redirectError(ProcessBuilder.Redirect.DISCARD)
        pb.environment().putAll(buildDaemonEnvironment(daemonBin))
        pb.start().also { it.outputStream.close() }
        return trayBin
    }

    /**
     * Gracefully shuts down any currently running `arbd` (even an older build lacking newer RPCs)
     * and spawns the currently resolved `arbd` binary, waiting until `system.hello` succeeds.
     */
    @Throws(IOException::class)
    fun restartDaemon(startupTimeout: Duration = DEFAULT_STARTUP_TIMEOUT): ConnectedDaemon {
        val binary = resolveDaemonBinary()
            ?: throw IOException(
                "Could not locate arbd binary. Set $ENV_ARB_DAEMON_PATH or build core/target/debug/${os.daemonExecutableName()}.",
            )
        val endpoint = paths.ipcEndpoint()
        stopRunningDaemonBestEffort(endpoint)
        val process = spawnDaemon(binary)
        return waitForHello(endpoint, startupTimeout, process)
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
    fun spawnDaemon(binary: Path): Process {
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
        val pb = ProcessBuilder(cmd)
            .redirectInput(ProcessBuilder.Redirect.PIPE)
            .redirectOutput(ProcessBuilder.Redirect.DISCARD)
            .redirectError(ProcessBuilder.Redirect.DISCARD)
        pb.environment().putAll(buildDaemonEnvironment(binary))
        return pb.start().also { it.outputStream.close() }
    }

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
        data class Stale(val reason: String) : ProbeResult
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
            if (shouldAutoUpgradeBinary(runningExe, bundledBinary, hello.binaryMtimeSecs)) {
                runCatching { client.close() }
                return ProbeResult.Stale("bundled arbd is newer than running $runningExe")
            }
            ProbeResult.Healthy(ConnectedDaemon(client = client, hello = hello, spawnedProcess = null))
        } catch (_: Exception) {
            runCatching { client?.close() }
            ProbeResult.NotRunning
        }
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
        if (isBundled && os != HostOs.WINDOWS && Files.isRegularFile(path) && !Files.isExecutable(path)) {
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

        /**
         * Returns `true` when a bundled plugin `arbd` is strictly newer on disk than the running
         * daemon binary — either at a different path (normal plugin upgrade) OR at the same path
         * when [runningBinaryMtimeSecs] from `system.hello` is older than the current on-disk file
         * mtime (in-place plugin upgrade).
         */
        fun shouldAutoUpgradeBinary(
            runningExe: Path?,
            bundledBinary: Path?,
            runningBinaryMtimeSecs: Long? = null,
        ): Boolean {
            if (runningExe == null || bundledBinary == null) return false
            if (!Files.isRegularFile(runningExe) || !Files.isRegularFile(bundledBinary)) return false
            val bundledMtime = runCatching { Files.getLastModifiedTime(bundledBinary) }.getOrNull() ?: return false
            val sameFile = runCatching { Files.isSameFile(runningExe, bundledBinary) }.getOrDefault(false)
            if (sameFile) {
                return runningBinaryMtimeSecs != null && bundledMtime.toInstant().epochSecond > runningBinaryMtimeSecs
            }
            val runningMtime = runCatching { Files.getLastModifiedTime(runningExe) }.getOrNull() ?: return false
            return bundledMtime > runningMtime
        }
    }
}
