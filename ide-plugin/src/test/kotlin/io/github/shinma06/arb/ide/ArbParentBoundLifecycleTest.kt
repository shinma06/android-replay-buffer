package io.github.shinma06.arb.ide

import io.github.shinma06.arb.ipc.ArbPaths
import io.github.shinma06.arb.ipc.ArbdProcessManager
import io.github.shinma06.arb.ipc.ArbdRpcClient
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths
import java.time.Duration
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.TimeUnit
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotEquals
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

/**
 * Android Studio-parented lifecycle of [ArbdDaemonService] against the real `arbd` and `arb-tray`
 * binaries ("Studio alive => running, Studio gone => not running").
 *
 * `arb-tray` runs with `--headless` here so the real process lifecycle (lock, stdin watch, quit
 * path) is exercised on CI runners that have no tray host; the IDE launches it without that flag.
 */
class ArbParentBoundLifecycleTest {
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

    private fun endpointAnswers(paths: ArbPaths): Boolean =
        runCatching { ArbdRpcClient.connect(paths.ipcEndpoint(), paths.os).use { it.systemHello(false) } }.isSuccess

    private class Harness(val paths: ArbPaths, root: Path) {
        val processManager = ArbdProcessManager(paths = paths, searchRoot = root, parentBound = true)
        val trayBin: Path = assertNotNull(
            processManager.resolveTrayBinary(),
            "arb-tray binary not found; build core/target/{debug,release}/arb-tray first",
        )
        val trays = CopyOnWriteArrayList<Process>()
        val service = ArbdDaemonService(
            processManager = processManager,
            trayBinaryResolver = { trayBin },
            trayProcessStarter = { bin, env ->
                val cmd = ArbdDaemonService.trayCommand(bin) + listOf(
                    "--headless",
                    "--data-dir", paths.dataDir.toString(),
                    "--cache-dir", paths.cacheDir.toString(),
                    "--runtime-dir", paths.runtimeDir.toString(),
                )
                val pb = ProcessBuilder(cmd)
                    .redirectInput(ProcessBuilder.Redirect.PIPE)
                    .redirectOutput(ProcessBuilder.Redirect.DISCARD)
                    .redirectError(ProcessBuilder.Redirect.DISCARD)
                pb.environment().putAll(env)
                pb.start().also { trays += it }
            },
        )
    }

    @Test
    fun startupWithTrayOffStartsParentBoundArbdAndTrayToggleOnlyTouchesTheTrayAndDisposeStopsBoth(
        @TempDir tmp: Path,
    ) {
        val h = Harness(hermeticPaths(tmp), repoRoot())
        assertNotNull(h.processManager.resolveDaemonBinary(), "arbd binary not found")
        assertEquals(
            listOf(h.trayBin.toAbsolutePath().toString(), "--exit-on-stdin-eof"),
            ArbdDaemonService.trayCommand(h.trayBin),
            "the IDE launches a parent-bound tray and leaves arbd to the IDE (no --auto-start)",
        )
        var daemon: Process? = null
        try {
            // ArbStartupActivity with the default trayEnabled = false: arbd starts, no tray.
            h.service.startParentBoundLifecycle { false }?.get(10, TimeUnit.SECONDS)
            daemon = assertNotNull(h.processManager.managedDaemonProcess(), "startup must spawn the managed arbd")
            assertTrue(h.service.withClient { it.systemHello(false) }.parentBound, "startup arbd is parent-bound")
            assertTrue(h.trays.isEmpty() && !h.service.isManagedTrayRunningForTest(), "tray stays off by default")

            // Settings: tray ON -> a parent-bound arb-tray runs.
            h.service.syncTrayState(true)
            assertEquals(1, h.trays.size)
            Thread.sleep(300)
            assertTrue(h.trays[0].isAlive && h.service.isManagedTrayRunningForTest(), "tray must be running")

            // Settings: tray OFF -> only arb-tray stops (through its stdin quit path, exit 0); arbd keeps running.
            h.service.syncTrayState(false)
            assertFalse(h.trays[0].isAlive, "tray OFF must stop arb-tray")
            assertEquals(0, h.trays[0].exitValue(), "closing the tray's stdin quits it cleanly (no kill needed)")
            assertTrue(daemon.isAlive, "tray OFF must never stop arbd while Studio lives")
            h.service.withClient { it.systemStatus() }

            h.service.syncTrayState(true)
            assertEquals(2, h.trays.size)
            Thread.sleep(300)
            assertTrue(h.trays[1].isAlive)
        } finally {
            h.service.dispose()
        }
        // Studio exit: both children gone, cleanly.
        kotlin.test.assertFailsWith<IllegalStateException> { h.service.restartDaemon() }
        val d = assertNotNull(daemon)
        assertTrue(d.waitFor(3, TimeUnit.SECONDS), "dispose() must stop the managed arbd")
        assertEquals(0, d.exitValue(), "arbd stops through its clean shutdown path")
        val tray = h.trays.last()
        assertTrue(tray.waitFor(3, TimeUnit.SECONDS), "dispose() must stop the managed arb-tray")
        assertEquals(0, tray.exitValue(), "arb-tray quits cleanly on dispose")
        assertFalse(endpointAnswers(h.paths), "no arbd may answer after dispose()")
    }

    @Test
    fun unmanagedRunningArbdIsTakenOverAtStartupAndStoppedOnDispose(@TempDir tmp: Path) {
        val paths = hermeticPaths(tmp)
        val leftover = ArbdProcessManager(paths = paths, searchRoot = repoRoot())
            .connectOrSpawn(Duration.ofSeconds(10))
        leftover.client.close()
        val leftoverProc = assertNotNull(leftover.spawnedProcess)
        assertFalse(leftover.hello.parentBound, "precondition: an unbound daemon is running")

        val h = Harness(paths, repoRoot())
        var daemon: Process? = null
        try {
            h.service.ensureParentBoundLifecycleStarted()
            assertTrue(leftoverProc.waitFor(5, TimeUnit.SECONDS), "the unbound daemon must be stopped")
            daemon = assertNotNull(h.processManager.managedDaemonProcess(), "the IDE must own the replacement")
            val hello = h.service.withClient { it.systemHello(false) }
            assertTrue(hello.parentBound)
            assertNotEquals(leftover.hello.pid, hello.pid)
        } finally {
            h.service.dispose()
            if (leftoverProc.isAlive) leftoverProc.destroyForcibly()
        }
        val d = assertNotNull(daemon)
        assertTrue(d.waitFor(3, TimeUnit.SECONDS), "dispose() must stop the adopted-by-replacement arbd")
        assertEquals(0, d.exitValue())
        assertFalse(endpointAnswers(paths))
    }

    @Test
    fun startupActivityStartsArbdBeforeAndIndependentlyOfTheTraySetting() {
        val src = Files.readString(
            repoRoot().resolve("ide-plugin/src/main/kotlin/io/github/shinma06/arb/ide/ArbStartupActivity.kt"),
        )
        val call = Regex("""startParentBoundLifecycle\s*\{\s*ArbLocalSettingsState""")
        assertTrue(call.containsMatchIn(src), "startup must run the parent-bound lifecycle (arbd first, then the tray)")
        assertFalse("restoreTrayAtStartup" in src, "startup must not restore only the tray")
    }
}
