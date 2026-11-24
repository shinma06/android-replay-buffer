package io.github.shinma06.arb.ipc

import org.junit.jupiter.api.Test
import java.io.IOException
import java.nio.file.Files
import java.nio.file.Paths
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNotEquals
import kotlin.test.assertTrue

class ArbPathsTest {
    @Test
    fun envOverridesTakePrecedenceAcrossAllOperatingSystems() {
        val env = mapOf(
            ArbPaths.ENV_ARB_DATA_DIR to "/tmp/arb-test/data",
            ArbPaths.ENV_ARB_CACHE_DIR to "/tmp/arb-test/cache",
            ArbPaths.ENV_ARB_RUNTIME_DIR to "/tmp/arb-test/run",
        )
        for (os in HostOs.entries) {
            val resolved = ArbPaths.resolve(os = os, env = env::get)
            assertEquals(Paths.get("/tmp/arb-test/data"), resolved.dataDir)
            assertEquals(Paths.get("/tmp/arb-test/cache"), resolved.cacheDir)
            assertEquals(Paths.get("/tmp/arb-test/run"), resolved.runtimeDir)
        }
    }

    @Test
    fun macOsDefaultsMatchSpec03() {
        val env = mapOf("HOME" to "/Users/tester", ArbPaths.ENV_ARB_DATA_DIR to "")
        val resolved = ArbPaths.resolve(os = HostOs.MACOS, env = env::get)
        assertEquals(Paths.get("/Users/tester/Library/Application Support/arb"), resolved.dataDir)
        assertEquals(Paths.get("/Users/tester/Library/Caches/arb"), resolved.cacheDir)
        assertEquals(Paths.get("/Users/tester/Library/Caches/arb"), resolved.runtimeDir)
        // Host-normalize the expectation: a Windows JVM resolves simulated Unix paths with
        // backslash separators even when `os = MACOS`, which is fine — production endpoints
        // are only ever computed for the real host OS.
        assertEquals(
            Paths.get("/Users/tester/Library/Caches/arb/arbd.sock").toString(),
            resolved.ipcEndpoint(),
        )
    }

    @Test
    fun linuxXdgDefaultsMatchSpec03() {
        val env = mapOf(
            "HOME" to "/home/tester",
            "XDG_DATA_HOME" to "/xdg/data",
            "XDG_CACHE_HOME" to "/xdg/cache",
            "XDG_RUNTIME_DIR" to "/run/user/1000",
        )
        val resolved = ArbPaths.resolve(os = HostOs.LINUX, env = env::get)
        assertEquals(Paths.get("/xdg/data/arb"), resolved.dataDir)
        assertEquals(Paths.get("/xdg/cache/arb"), resolved.cacheDir)
        assertEquals(Paths.get("/run/user/1000/arb"), resolved.runtimeDir)
        assertEquals(
            Paths.get("/run/user/1000/arb/arbd.sock").toString(),
            resolved.ipcEndpoint(),
        )
    }

    @Test
    fun windowsNamedPipeIsDeterministicAndKeyedByTheRuntimeDir() {
        val pipeA = ArbPaths.computeWindowsPipeEndpoint("""C:\Users\alice\AppData\Local\arb\runtime""", GOLDEN_SID)
        val pipeB = ArbPaths.computeWindowsPipeEndpoint("""C:\Users\bob\AppData\Local\arb\runtime""", GOLDEN_SID)
        assertTrue(pipeA.startsWith("""\\.\pipe\arb-daemon-"""))
        assertEquals("""\\.\pipe\arb-daemon-""".length + 16, pipeA.length)
        assertNotEquals(pipeA, pipeB)
    }

    @Test
    fun windowsPipeHashMatchesRustGoldenVectors() {
        // Golden vectors shared bit-for-bit with `arb-os/src/paths.rs`
        // (`windows_pipe_endpoint_matches_kotlin_golden_vectors`).
        val runtime = """C:\Users\alice\AppData\Local\arb\runtime"""
        assertEquals(
            """\\.\pipe\arb-daemon-7fb9c25d31c01ee3""",
            ArbPaths.computeWindowsPipeEndpoint(runtime, null),
            "no-SID FNV-1a pipe name must match the Rust golden vector",
        )
        assertEquals(
            """\\.\pipe\arb-daemon-5e550661a2714c87""",
            ArbPaths.computeWindowsPipeEndpoint(runtime, GOLDEN_SID),
            "SID-mixed FNV-1a pipe name must match the Rust golden vector",
        )
        assertEquals(
            """\\.\pipe\arb-daemon-5e550661a2714c87""",
            ArbPaths(Paths.get("d"), Paths.get("c"), Paths.get(runtime), os = HostOs.WINDOWS).ipcEndpoint(GOLDEN_SID),
        )
    }

    /**
     * P0-6: the pipe name needs the token user's SID. An unavailable SID must fail closed instead
     * of silently hashing without it (a name `arbd` never listens on), and the SID must come from
     * the in-process Win32 lookup, never from spawning `whoami`.
     */
    @Test
    fun windowsPipeNameFailsClosedWithoutTheUserSid() {
        val runtime = """C:\Users\alice\AppData\Local\arb\runtime"""
        val noSidName = ArbPaths.computeWindowsPipeEndpoint(runtime, null)
        assertFailsWith<IOException> { ArbPaths.windowsPipeEndpoint(runtime) { throw IOException("token query failed") } }
        assertFailsWith<IOException> { ArbPaths.windowsPipeEndpoint(runtime) { ByteArray(0) } }

        // Real lookup on this host: the Win32 token SID on Windows, an explicit failure elsewhere.
        val viaHost = runCatching {
            ArbPaths(Paths.get("d"), Paths.get("c"), Paths.get(runtime), os = HostOs.WINDOWS).ipcEndpoint()
        }
        viaHost.exceptionOrNull()?.let { assertIs<IOException>(it) }
        assertNotEquals(noSidName, viaHost.getOrNull(), "never a silent SID-less pipe name")

        val source = Files.readString(
            Paths.get(System.getProperty("arb.repo.root") ?: "..")
                .resolve("ide-plugin/client-core/src/main/kotlin/io/github/shinma06/arb/ipc/ArbPaths.kt"),
        )
        assertFalse("ProcessBuilder" in source, "ArbPaths must not spawn an external process (whoami)")
    }

    private companion object {
        /** Binary Win32 SID of `S-1-5-21-3623811015-3361044348-30300820-1013`. */
        val GOLDEN_SID = byteArrayOf(
            1, 5, 0, 0, 0, 0, 0, 5, 21, 0, 0, 0, -57, -9, -2, -41,
            124, 119, 85, -56, -108, 90, -50, 1, -11, 3, 0, 0,
        )
    }
}
