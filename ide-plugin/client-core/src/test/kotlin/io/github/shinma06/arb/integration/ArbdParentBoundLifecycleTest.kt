package io.github.shinma06.arb.integration

import io.github.shinma06.arb.ipc.ArbPaths
import io.github.shinma06.arb.ipc.ArbdProcessManager
import io.github.shinma06.arb.ipc.ArbdRpcClient
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths
import java.time.Duration
import java.util.concurrent.TimeUnit
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * Parent-bound `arbd` lifecycle against the real `arbd` binary: this JVM plays Android Studio.
 *
 * - Closing only the held stdin pipe (no `system.shutdown`, no `destroy()` — exactly what the
 *   kernel does when the IDE is `kill -9`ed) stops `arbd` cleanly and frees the next start.
 * - An unbound `arbd` (older plugin, `arb-cli --auto-start`, started by hand) is replaced by a
 *   parent-bound one, which [ArbdProcessManager.stopManagedDaemon] then stops.
 * - A daemon already bound to a live parent is reused, never restarted or stopped by a
 *   non-owner.
 */
class ArbdParentBoundLifecycleTest {
    private companion object {
        /**
         * Upper bounds only: a healthy `arbd` exits within milliseconds, but on a loaded CI runner
         * (parallel Gradle workers, cold disk) a process can be starved for seconds. These waits
         * return as soon as the condition holds, so a generous margin costs nothing when healthy
         * and only turns a real hang into a failure. Behavioural assertions (exit code, who
         * survives) are unchanged.
         */
        const val EXIT_WAIT_SECONDS: Long = 30L
        val STARTUP_TIMEOUT: Duration = Duration.ofSeconds(30)
    }

    private fun repoRoot(): Path {
        System.getProperty("arb.repo.root")?.takeIf { it.isNotBlank() }?.let {
            return Paths.get(it).toAbsolutePath().normalize()
        }
        var cur: Path? = Paths.get("").toAbsolutePath().normalize()
        repeat(6) {
            val d = cur ?: error("Could not locate repository root")
            if (Files.isDirectory(d.resolve("core")) && Files.isDirectory(d.resolve("fixtures"))) return d
            cur = d.parent
        }
        error("Could not locate repository root containing core/ and fixtures/")
    }

    private fun hermeticPaths(tmp: Path) = ArbPaths(
        dataDir = tmp.resolve("data"),
        cacheDir = tmp.resolve("cache"),
        runtimeDir = tmp.resolve("run"),
    )

    private fun manager(paths: ArbPaths, parentBound: Boolean) = ArbdProcessManager(
        paths = paths,
        searchRoot = repoRoot(),
        parentBound = parentBound,
    )

    private fun endpointAnswers(paths: ArbPaths): Boolean =
        runCatching { ArbdRpcClient.connect(paths.ipcEndpoint(), paths.os).use { it.systemHello(false) } }.isSuccess

    private fun processOf(pid: Long): ProcessHandle? = ProcessHandle.of(pid).orElse(null)

    private fun awaitExit(handle: ProcessHandle, timeoutMs: Long): Boolean =
        runCatching { handle.onExit().get(timeoutMs, TimeUnit.MILLISECONDS) }.isSuccess || !handle.isAlive

    @Test
    fun startupFailureDoesNotDisableParentBindingOnTheNextLaunch(@TempDir tmp: Path) {
        val paths = hermeticPaths(tmp)
        val ide = manager(paths, parentBound = true)
        val invalidCatalog = Files.createDirectories(paths.dataDir.resolve("catalog.sqlite"))
        kotlin.test.assertFailsWith<java.io.IOException> { ide.connectOrSpawn() }
        Files.delete(invalidCatalog)
        val connected = ide.connectOrSpawn()
        try {
            assertTrue(connected.hello.parentBound)
        } finally {
            connected.client.close()
            ide.stopManagedDaemon()
        }
    }

    @Test
    fun siblingTrayDoesNotInheritTheDaemonWriter(@TempDir tmp: Path) {
        val paths = hermeticPaths(tmp)
        val ide = manager(paths, parentBound = true)
        val binary = assertNotNull(ide.resolveDaemonBinary())
        val tray = ProcessBuilder(
            assertNotNull(ide.resolveTrayBinary()).toString(), "--headless", "--exit-on-stdin-eof",
            "--runtime-dir", paths.runtimeDir.toString(), "--cache-dir", paths.cacheDir.toString(),
            "--data-dir", paths.dataDir.toString(),
        ).redirectError(ProcessBuilder.Redirect.DISCARD).redirectOutput(ProcessBuilder.Redirect.DISCARD)
        val daemon = ide.connectOrSpawn(STARTUP_TIMEOUT)
        val owner = assertNotNull(daemon.spawnedProcess)
        try {
            daemon.client.close()
            val sibling = tray.start()
            try {
                val contender = manager(paths, parentBound = true).spawnDaemon(binary)
                assertTrue(contender.waitFor(EXIT_WAIT_SECONDS, TimeUnit.SECONDS))
                assertNotEquals(0, contender.exitValue())
                owner.outputStream.close()
                assertTrue(owner.waitFor(1, TimeUnit.SECONDS), "owner must exit once its parent pipe closed")
                assertEquals(0, owner.exitValue())
                assertTrue(sibling.isAlive)
                sibling.outputStream.close()
                assertTrue(sibling.waitFor(EXIT_WAIT_SECONDS, TimeUnit.SECONDS), "sibling must exit once its own pipe closed")
                assertEquals(0, sibling.exitValue())
            } finally {
                sibling.destroyForcibly()
            }
        } finally {
            owner.destroyForcibly()
        }
    }

    @Test
    fun closingOnlyTheHeldStdinPipeStopsRealArbdAndFreesTheNextStart(@TempDir tmp: Path) {
        val paths = hermeticPaths(tmp)
        val ide = manager(paths, parentBound = true)
        assertNotNull(ide.resolveDaemonBinary(), "arbd binary not found; build core/target/{debug,release}/arbd first")

        val connected = ide.connectOrSpawn(STARTUP_TIMEOUT)
        val proc = assertNotNull(ide.managedDaemonProcess(), "the IDE must own the arbd it spawned")
        try {
            assertTrue(connected.hello.parentBound, "IDE-spawned arbd must be parent-bound")
            assertEquals(proc.pid(), connected.hello.pid, "the managed process is the daemon answering hello")
            assertTrue(proc.info().arguments().map { "--exit-on-stdin-eof" in it }.orElse(true))
            connected.client.systemStatus()
            Thread.sleep(300)
            assertTrue(proc.isAlive, "arbd must keep running while the parent holds the stdin pipe")

            // Parent death: only the pipe closes (plus the dead parent's IPC socket).
            connected.client.close()
            proc.outputStream.close()
            val started = System.nanoTime()
            assertTrue(proc.waitFor(EXIT_WAIT_SECONDS, TimeUnit.SECONDS), "arbd must exit after the parent pipe closed")
            val tookMs = (System.nanoTime() - started) / 1_000_000
            assertEquals(0, proc.exitValue(), "parent EOF must take the clean shutdown path (exit 0), took $tookMs ms")
            assertFalse(endpointAnswers(paths), "nothing may answer on the endpoint after exit")

            // Lock + endpoint released: the next parent-bound daemon starts immediately.
            val next = manager(paths, parentBound = true)
            val again = next.connectOrSpawn(STARTUP_TIMEOUT)
            again.client.close()
            assertNotEquals(connected.hello.pid, again.hello.pid, "a fresh daemon must have started")
            assertTrue(next.stopManagedDaemon(), "the new owner stops its daemon")
            assertNull(next.managedDaemonProcess())
            assertFalse(endpointAnswers(paths))
        } finally {
            if (proc.isAlive) proc.destroyForcibly()
        }
    }

    @Test
    fun unboundArbdIsReplacedByAParentBoundOneThatStopManagedDaemonStops(@TempDir tmp: Path) {
        val paths = hermeticPaths(tmp)
        // A standalone (unbound) arbd, as an older plugin or `arb-cli --auto-start` leaves behind.
        val standaloneManager = manager(paths, parentBound = false)
        val standalone = standaloneManager.connectOrSpawn(STARTUP_TIMEOUT)
        standalone.client.close()
        val standaloneProc = assertNotNull(standalone.spawnedProcess)
        try {
            assertFalse(standalone.hello.parentBound, "a standalone daemon reports parent_bound = false")
            assertNull(standaloneManager.managedDaemonProcess(), "an unbound launch is not managed")

            val ide = manager(paths, parentBound = true)
            val taken = ide.connectOrSpawn(STARTUP_TIMEOUT)
            taken.client.close()
            assertTrue(standaloneProc.waitFor(EXIT_WAIT_SECONDS, TimeUnit.SECONDS), "the unbound daemon must be stopped")
            assertTrue(taken.hello.parentBound, "the IDE must hand over to a parent-bound daemon")
            assertNotEquals(standalone.hello.pid, taken.hello.pid)
            val managed = assertNotNull(ide.managedDaemonProcess())
            assertEquals(managed.pid(), taken.hello.pid)

            // A healthy daemon the IDE already owns is never restarted.
            val reused = ide.connectOrSpawn(STARTUP_TIMEOUT)
            reused.client.close()
            assertEquals(taken.hello.pid, reused.hello.pid, "an owned parent-bound daemon must be reused as is")

            assertTrue(ide.stopManagedDaemon(), "stopManagedDaemon must report the managed daemon")
            assertFalse(managed.isAlive, "the managed daemon must be gone after stopManagedDaemon")
            assertEquals(0, managed.exitValue(), "stopManagedDaemon stops arbd cleanly (exit 0)")
            assertNull(ide.managedDaemonProcess())
            assertFalse(endpointAnswers(paths))
            assertFalse(ide.stopManagedDaemon(), "a second stop finds nothing to stop")
        } finally {
            if (standaloneProc.isAlive) standaloneProc.destroyForcibly()
        }
    }

    @Test
    fun daemonBoundToAnotherLiveParentIsReusedAndNeverStoppedByANonOwner(@TempDir tmp: Path) {
        val paths = hermeticPaths(tmp)
        val owner = manager(paths, parentBound = true)
        val first = owner.connectOrSpawn(STARTUP_TIMEOUT)
        first.client.close()
        val ownerProc = assertNotNull(owner.managedDaemonProcess())
        try {
            // Another IDE (its own manager) finds a daemon bound to a live parent: reuse it.
            val other = manager(paths, parentBound = true)
            val shared = other.connectOrSpawn(STARTUP_TIMEOUT)
            shared.client.close()
            assertEquals(first.hello.pid, shared.hello.pid, "a daemon bound to a live parent must not be replaced")
            assertNull(other.managedDaemonProcess(), "the non-owner does not manage it")
            assertFalse(other.stopManagedDaemon(), "a non-owner must not stop it")
            assertTrue(ownerProc.isAlive && endpointAnswers(paths), "the shared daemon keeps running")

            assertTrue(owner.stopManagedDaemon())
            val handle = assertNotNull(processOf(first.hello.pid) ?: ownerProc.toHandle())
            assertTrue(awaitExit(handle, EXIT_WAIT_SECONDS * 1_000), "the owner's stop ends the daemon")
        } finally {
            if (ownerProc.isAlive) ownerProc.destroyForcibly()
        }
    }
}
