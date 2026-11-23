package io.github.shinma06.arb.ipc

import io.github.shinma06.arb.model.SystemHelloResponse
import java.io.File
import java.io.IOException
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths
import java.time.Duration
import java.time.Instant

/**
 * Resolves the `arbd` binary location, lazily spawns `arbd` when its local IPC endpoint is absent,
 * and verifies the `system.hello` protocol handshake (`SPEC-01`, `SPEC-03`).
 */
class ArbdProcessManager(
    val paths: ArbPaths = ArbPaths.resolve(),
    private val pluginDir: Path? = null,
    private val searchRoot: Path? = null,
    private val os: HostOs = paths.os,
    private val arch: String = HostOs.detectArch(),
    private val env: (String) -> String? = System::getenv,
    private val customBinaryProvider: () -> Path? = { null },
) {
    /**
     * Resolution order:
     * 1. `ARB_DAEMON_PATH` env var (and `ARB_DAEMON_BIN` fallback)
     * 2. User-configured custom `arbd` binary path override (IDE Settings)
     * 3. Bundled plugin binary `<pluginDir>/bin/<os>-<arch>/arbd[.exe]`
     * 4. Sibling dev Cargo build (`core/target/debug/arbd` or `core/target/release/arbd`)
     * 5. Executable on `PATH`
     */
    fun resolveDaemonBinary(): Path? {
        val envOverride = env(ENV_ARB_DAEMON_PATH)?.takeIf { it.isNotBlank() }
            ?: env(ENV_ARB_DAEMON_BIN)?.takeIf { it.isNotBlank() }
        if (envOverride != null) {
            val candidate = Paths.get(envOverride)
            if (Files.isRegularFile(candidate)) {
                return candidate
            }
        }

        val customCandidate = runCatching { customBinaryProvider() }.getOrNull()
        if (customCandidate != null && Files.isRegularFile(customCandidate)) {
            return customCandidate
        }

        val exeName = os.daemonExecutableName()
        if (pluginDir != null) {
            val bundled = pluginDir
                .resolve("bin")
                .resolve(HostOs.platformBinSlug(os, arch))
                .resolve(exeName)
            if (Files.isRegularFile(bundled)) {
                return bundled
            }
        }

        val devStart = searchRoot ?: Paths.get("").toAbsolutePath()
        findDevDaemonBinary(devStart, exeName)?.let { return it }

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

    /**
     * Connects to an already-running `arbd`, or lazily spawns it and waits until `system.hello` succeeds.
     */
    @Throws(IOException::class)
    fun connectOrSpawn(startupTimeout: Duration = DEFAULT_STARTUP_TIMEOUT): ConnectedDaemon {
        val endpoint = paths.ipcEndpoint()
        tryConnectAndHello(endpoint)?.let { return it }

        val binary = resolveDaemonBinary()
            ?: throw IOException(
                "Could not locate arbd binary. Set $ENV_ARB_DAEMON_PATH or build core/target/debug/${os.daemonExecutableName()}.",
            )

        val process = spawnDaemon(binary)
        return waitForHello(endpoint, startupTimeout, process)
    }

    @Throws(IOException::class)
    fun spawnDaemon(binary: Path): Process {
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
        return ProcessBuilder(cmd)
            .redirectInput(ProcessBuilder.Redirect.PIPE)
            .redirectOutput(ProcessBuilder.Redirect.DISCARD)
            .redirectError(ProcessBuilder.Redirect.DISCARD)
            .start()
            .also { it.outputStream.close() }
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

    private fun findDevDaemonBinary(start: Path, exeName: String): Path? {
        var current: Path? = start.toAbsolutePath().normalize()
        repeat(6) {
            val dir = current ?: return null
            val debugCandidate = dir.resolve("core").resolve("target").resolve("debug").resolve(exeName)
            if (Files.isRegularFile(debugCandidate)) return debugCandidate
            val releaseCandidate = dir.resolve("core").resolve("target").resolve("release").resolve(exeName)
            if (Files.isRegularFile(releaseCandidate)) return releaseCandidate
            current = dir.parent
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
        val DEFAULT_STARTUP_TIMEOUT: Duration = Duration.ofSeconds(5)
        private const val POLL_INTERVAL_MS: Long = 50L
    }
}
