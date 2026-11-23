package io.github.shinma06.arb.ipc

import org.junit.jupiter.api.Test
import java.nio.file.Paths
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertNotEquals
import kotlin.test.assertNotNull
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
        assertEquals("/Users/tester/Library/Caches/arb/arbd.sock", resolved.ipcEndpoint())
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
        assertEquals("/run/user/1000/arb/arbd.sock", resolved.ipcEndpoint())
    }

    @Test
    fun windowsNamedPipeAndSidBinaryEncodingAreDeterministic() {
        val sidBytes = ArbPaths.encodeWindowsSidStringToBytes("S-1-5-21-3623811015-3361044348-30300820-1013")
        assertNotNull(sidBytes)
        // Revision=1, SubAuthorityCount=5, Authority=5 (big-endian 6 bytes)
        assertEquals(8 + 5 * 4, sidBytes.size)
        assertContentEquals(byteArrayOf(1, 5, 0, 0, 0, 0, 0, 5), sidBytes.copyOfRange(0, 8))

        val pipeA = ArbPaths.computeWindowsPipeEndpoint("""C:\Users\alice\AppData\Local\arb\runtime""", sidBytes)
        val pipeB = ArbPaths.computeWindowsPipeEndpoint("""C:\Users\bob\AppData\Local\arb\runtime""", sidBytes)
        assertTrue(pipeA.startsWith("""\\.\pipe\arb-daemon-"""))
        assertEquals("""\\.\pipe\arb-daemon-""".length + 16, pipeA.length)
        assertNotEquals(pipeA, pipeB)
    }
}
