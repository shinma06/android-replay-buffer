package io.github.shinma06.arb.ipc

import java.io.IOException
import java.nio.charset.StandardCharsets
import java.nio.file.Path
import java.nio.file.Paths

/**
 * Resolves ARB data, cache, runtime, and local IPC endpoint paths identically to
 * `core/crates/arb-os/src/paths.rs`.
 */
data class ArbPaths(
    val dataDir: Path,
    val cacheDir: Path,
    val runtimeDir: Path,
    val os: HostOs = HostOs.detect(),
    val explicitEndpoint: String? = null,
) {
    fun lockFile(): Path = runtimeDir.resolve("arbd.lock")

    /** `arb-tray`'s single-instance lock; holds the PID of its last owner (`arb-tray/instance.rs`). */
    fun trayLockFile(): Path = runtimeDir.resolve("arb-tray.lock")

    fun shmDir(): Path = runtimeDir.resolve("shm")

    /**
     * Platform-appropriate local IPC endpoint:
     * - macOS / Linux: `<runtimeDir>/arbd.sock`
     * - Windows: `\\.\pipe\arb-daemon-<fnv1a64>` (matching `arb-os/src/paths.rs`), keyed by the
     *   process token user's SID. The SID comes from [Win32NamedPipes.currentUserSid] (no external
     *   `whoami` process); when it is unavailable this throws instead of silently naming a pipe
     *   without the SID that `arbd` would never listen on (fail closed).
     */
    @Throws(IOException::class)
    fun ipcEndpoint(windowsSidBytes: ByteArray? = null): String {
        if (!explicitEndpoint.isNullOrBlank()) {
            return explicitEndpoint
        }
        return when (os) {
            HostOs.MACOS, HostOs.LINUX -> runtimeDir.resolve("arbd.sock").toString()
            HostOs.WINDOWS -> windowsPipeEndpoint(runtimeDir.toString()) {
                windowsSidBytes ?: Win32NamedPipes.currentUserSid()
            }
        }
    }

    companion object {
        const val ENV_ARB_DATA_DIR: String = "ARB_DATA_DIR"
        const val ENV_ARB_CACHE_DIR: String = "ARB_CACHE_DIR"
        const val ENV_ARB_RUNTIME_DIR: String = "ARB_RUNTIME_DIR"
        const val ENV_ARB_IPC_ENDPOINT: String = "ARB_IPC_ENDPOINT"

        fun resolve(
            os: HostOs = HostOs.detect(),
            env: (String) -> String? = System::getenv,
        ): ArbPaths {
            val dataOverride = nonEmpty(env(ENV_ARB_DATA_DIR))
            val cacheOverride = nonEmpty(env(ENV_ARB_CACHE_DIR))
            val runtimeOverride = nonEmpty(env(ENV_ARB_RUNTIME_DIR))
            val explicitEndpoint = env(ENV_ARB_IPC_ENDPOINT)?.takeIf { it.isNotEmpty() }

            return when (os) {
                HostOs.MACOS -> {
                    val home = if (dataOverride == null || cacheOverride == null || runtimeOverride == null) {
                        requireEnv(env, "HOME")
                    } else {
                        null
                    }
                    val dataDir = dataOverride ?: home!!.resolve("Library").resolve("Application Support").resolve("arb")
                    val cacheDir = cacheOverride ?: home!!.resolve("Library").resolve("Caches").resolve("arb")
                    val runtimeDir = runtimeOverride ?: home!!.resolve("Library").resolve("Caches").resolve("arb")
                    ArbPaths(dataDir, cacheDir, runtimeDir, os, explicitEndpoint)
                }

                HostOs.LINUX -> {
                    val dataDir = dataOverride
                        ?: nonEmpty(env("XDG_DATA_HOME"))?.resolve("arb")
                        ?: requireEnv(env, "HOME").resolve(".local").resolve("share").resolve("arb")
                    val cacheDir = cacheOverride
                        ?: nonEmpty(env("XDG_CACHE_HOME"))?.resolve("arb")
                        ?: requireEnv(env, "HOME").resolve(".cache").resolve("arb")
                    val runtimeDir = runtimeOverride
                        ?: nonEmpty(env("XDG_RUNTIME_DIR"))?.resolve("arb")
                        ?: cacheDir.resolve("run")
                    ArbPaths(dataDir, cacheDir, runtimeDir, os, explicitEndpoint)
                }

                HostOs.WINDOWS -> {
                    val base = if (dataOverride == null || cacheOverride == null || runtimeOverride == null) {
                        val localAppData = nonEmpty(env("LOCALAPPDATA"))
                            ?: nonEmpty(env("USERPROFILE"))?.resolve("AppData")?.resolve("Local")
                            ?: throw IllegalStateException("Neither LOCALAPPDATA nor USERPROFILE is set")
                        localAppData.resolve("arb")
                    } else {
                        null
                    }
                    val dataDir = dataOverride ?: base!!
                    val cacheDir = cacheOverride ?: base!!.resolve("cache")
                    val runtimeDir = runtimeOverride ?: base!!.resolve("runtime")
                    ArbPaths(dataDir, cacheDir, runtimeDir, os, explicitEndpoint)
                }
            }
        }

        /**
         * Computes `\\.\pipe\arb-daemon-<16-hex-fnv1a>` matching `arb-os/src/paths.rs`.
         */
        fun computeWindowsPipeEndpoint(runtimeDir: String, sidBytes: ByteArray?): String {
            var hash = -0x340d631b7bdddcdbL // 0xcbf29ce484222325u64
            val prime = 0x0100000001b3L
            if (sidBytes != null && sidBytes.isNotEmpty()) {
                for (b in sidBytes) {
                    hash = (hash xor (b.toLong() and 0xFFL)) * prime
                }
                hash = (hash xor '|'.code.toLong()) * prime
            }
            for (b in runtimeDir.toByteArray(StandardCharsets.UTF_8)) {
                hash = (hash xor (b.toLong() and 0xFFL)) * prime
            }
            val hex = java.lang.Long.toUnsignedString(hash, 16).padStart(16, '0')
            return """\\.\pipe\arb-daemon-$hex"""
        }

        /** [computeWindowsPipeEndpoint] keyed by a required SID from [sid] (fail closed). */
        @Throws(IOException::class)
        internal fun windowsPipeEndpoint(runtimeDir: String, sid: () -> ByteArray): String {
            val sidBytes = try {
                sid()
            } catch (e: Exception) {
                throw IOException("Windows user SID unavailable; cannot derive the arbd pipe name", e)
            } catch (e: LinkageError) {
                throw IOException("Windows user SID unavailable; cannot derive the arbd pipe name", e)
            }
            if (sidBytes.isEmpty()) throw IOException("Windows user SID is empty; cannot derive the arbd pipe name")
            return computeWindowsPipeEndpoint(runtimeDir, sidBytes)
        }

        private fun nonEmpty(raw: String?): Path? =
            raw?.takeIf { it.isNotEmpty() }?.let { Paths.get(it) }

        private fun requireEnv(env: (String) -> String?, key: String): Path =
            nonEmpty(env(key)) ?: throw IllegalStateException("$key environment variable is unset or empty")
    }
}
