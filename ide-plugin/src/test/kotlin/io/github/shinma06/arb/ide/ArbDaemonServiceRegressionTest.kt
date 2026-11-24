package io.github.shinma06.arb.ide

import com.google.gson.JsonObject
import com.google.gson.JsonParser
import io.github.shinma06.arb.ipc.ArbPaths
import io.github.shinma06.arb.ipc.ArbdIoException
import io.github.shinma06.arb.ipc.ArbdProcessManager
import io.github.shinma06.arb.ipc.ArbdRpcClient
import io.github.shinma06.arb.ipc.HostOs
import io.github.shinma06.arb.ipc.LocalIpcTransport
import io.github.shinma06.arb.model.ArbConfig
import io.github.shinma06.arb.model.SystemHelloResponse
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.io.IOException
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths
import java.nio.file.StandardCopyOption
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNotEquals
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

/**
 * Regression tests for the IDE lifecycle defects of Trail P-2 (B-06 / B-10 / B-11): retry policy,
 * IDE exit while an RPC runs, multi-window tray takeover and the `arbd` binary setting.
 */
class ArbDaemonServiceRegressionTest {
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

    // ---------------------------------------------------------------- retry policy (P1-01)

    private sealed interface Reply {
        data class Ok(val json: String = "{}") : Reply
        /** The request reached `arbd`, then the connection broke before its response. */
        data object LostResponse : Reply
        /** The connection was already dead: the request was never written. */
        data object WriteFails : Reply
    }

    /** One fake daemon reached through successive connections; records what it received. */
    private class FakeDaemon(private val script: (connection: Int, method: String) -> Reply) {
        val received = CopyOnWriteArrayList<String>()
        var connections = 0
            private set

        fun connect(): ArbdProcessManager.ConnectedDaemon {
            val index = connections++
            val responses = LinkedBlockingQueue<String>()
            val transport = object : LocalIpcTransport {
                override val endpoint = "fake://arbd"
                override fun writeLine(jsonLine: String) {
                    val req = JsonParser.parseString(jsonLine).asJsonObject
                    val method = req.get("method").asString
                    when (val reply = script(index, method)) {
                        Reply.WriteFails -> {
                            received += method
                            throw IOException("flush failed after delivery")
                        }
                        Reply.LostResponse -> {
                            received += method
                            responses += LOST
                        }
                        is Reply.Ok -> {
                            received += method
                            responses += """{"jsonrpc":"2.0","id":${req.get("id")},"result":${reply.json}}"""
                        }
                    }
                }

                override fun readLine(): String? {
                    val line = responses.take()
                    if (line == LOST) throw IOException("connection reset")
                    return line
                }

                override fun close() = Unit
            }
            val hello = SystemHelloResponse(1, 2, 1, "", "", "", "test")
            return ArbdProcessManager.ConnectedDaemon(ArbdRpcClient(transport), hello)
        }

        private companion object {
            const val LOST = "\u0000lost"
        }
    }

    private fun serviceFor(daemon: FakeDaemon, tmp: Path) = ArbdDaemonService(
        processManager = ArbdProcessManager(paths = ArbPaths(tmp.resolve("d"), tmp.resolve("c"), tmp.resolve("r"))),
        connector = daemon::connect,
    )

    @Test
    fun bundleOpenIsNeverReplayedAfterItsResponseWasLost(@TempDir tmp: Path) {
        val daemon = FakeDaemon { connection, method ->
            if (connection == 0 && method == "bundle.open") Reply.LostResponse else Reply.Ok()
        }
        val service = serviceFor(daemon, tmp)
        try {
            assertFailsWith<ArbdIoException> { service.withClient { it.callRaw("bundle.open", JsonObject()) } }
            assertEquals(listOf("bundle.open"), daemon.received, "a replay would leave an orphaned second lease")
        } finally {
            service.dispose()
        }
    }

    @Test
    fun lostViewportReplyDoesNotOpenAnotherViewport(@TempDir tmp: Path) {
        val daemon = FakeDaemon { connection, _ -> if (connection == 0) Reply.LostResponse else Reply.Ok() }
        val service = serviceFor(daemon, tmp)
        try {
            assertFailsWith<ArbdIoException> { service.withClient { it.callRaw("media.open_viewport") } }
            assertEquals(listOf("media.open_viewport"), daemon.received)
        } finally {
            service.dispose()
        }
    }

    @Test
    fun aBlockThatAlreadyOpenedALeaseIsNotReplayedWhenALaterReadFails(@TempDir tmp: Path) {
        val daemon = FakeDaemon { connection, method ->
            if (connection == 0 && method == "timeline.query_taps") Reply.LostResponse else Reply.Ok()
        }
        val service = serviceFor(daemon, tmp)
        try {
            assertFailsWith<ArbdIoException> {
                service.withClient {
                    it.callRaw("bundle.open", JsonObject())
                    it.callRaw("timeline.query_taps", JsonObject())
                }
            }
            assertEquals(listOf("bundle.open", "timeline.query_taps"), daemon.received)
        } finally {
            service.dispose()
        }
    }

    @Test
    fun readsRetryButAnAmbiguousSaveWriteNeverReplays(@TempDir tmp: Path) {
        val daemon = FakeDaemon { connection, method ->
            when {
                connection == 0 && method == "timeline.summary" -> Reply.LostResponse
                connection == 1 && method == "save.trigger" -> Reply.WriteFails
                else -> Reply.Ok()
            }
        }
        val service = serviceFor(daemon, tmp)
        try {
            service.withClient { it.callRaw("timeline.summary", JsonObject()) }
            assertEquals(listOf("timeline.summary", "timeline.summary"), daemon.received)
            // A write error cannot tell whether the server received the frame.
            assertFailsWith<ArbdIoException> { service.withClient { it.callRaw("save.trigger", JsonObject()) } }
            assertEquals(listOf("timeline.summary", "timeline.summary", "save.trigger"), daemon.received)
            assertEquals(2, daemon.connections)
            assertFalse(ArbdDaemonService.isIdempotentRpc("bundle.open"))
            assertTrue(ArbdDaemonService.isIdempotentRpc("bundle.close"))
        } finally {
            service.dispose()
        }
    }

    @Test
    fun disposalDuringConnectRejectsLateConnection(@TempDir tmp: Path) {
        val entered = java.util.concurrent.CountDownLatch(1)
        val resume = java.util.concurrent.CountDownLatch(1)
        val service = ArbdDaemonService(
            processManager = ArbdProcessManager(paths = hermeticPaths(tmp)),
            connector = {
                entered.countDown()
                check(resume.await(10, TimeUnit.SECONDS))
                FakeDaemon { _, _ -> Reply.Ok() }.connect()
            },
        )
        val result = java.util.concurrent.CompletableFuture.supplyAsync {
            runCatching { service.withClient { true } }
        }
        try {
            assertTrue(entered.await(10, TimeUnit.SECONDS))
            service.dispose()
        } finally {
            resume.countDown()
        }
        assertTrue(result.get(10, TimeUnit.SECONDS).isFailure)
        assertFalse(service.isConnected())
    }

    // ---------------------------------------------------------------- dispose (C7 / P1-6)

    private fun hermeticPaths(tmp: Path) = ArbPaths(tmp.resolve("data"), tmp.resolve("cache"), tmp.resolve("run"))

    @Test
    fun disposeStopsTheChildrenWithoutWaitingForAnInFlightRpc(@TempDir tmp: Path) {
        val processManager = ArbdProcessManager(paths = hermeticPaths(tmp), searchRoot = repoRoot(), parentBound = true)
        val service = ArbdDaemonService(processManager = processManager)
        service.ensureParentBoundLifecycleStarted()
        val daemon = assertNotNull(processManager.managedDaemonProcess(), "startup spawns the managed arbd")

        val inFlight = CountDownLatch(1)
        val release = CountDownLatch(1)
        val longRpc = Thread {
            runCatching { service.withClient { inFlight.countDown(); release.await(60, TimeUnit.SECONDS) } }
        }.apply { isDaemon = true; start() }
        try {
            assertTrue(inFlight.await(10, TimeUnit.SECONDS))
            val disposer = Thread { service.dispose() }.apply { isDaemon = true; start() }
            disposer.join(10_000)
            assertFalse(disposer.isAlive, "IDE exit must not wait for an in-flight RPC (up to 120 s exports)")
            assertTrue(daemon.waitFor(5, TimeUnit.SECONDS), "dispose() stops the managed arbd")
        } finally {
            release.countDown()
            longRpc.join(5_000)
            service.dispose()
            if (daemon.isAlive) daemon.destroyForcibly()
        }
    }

    // ---------------------------------------------------------------- tray takeover (P0-3 / P1-02)

    private class TrayIde(paths: ArbPaths, root: Path, ownerOfOtherTray: () -> Long?) {
        val processManager = ArbdProcessManager(paths = paths, searchRoot = root, parentBound = true)
        val trayBin: Path = assertNotNull(processManager.resolveTrayBinary(), "build core/target/{debug,release}/arb-tray first")
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
            trayEnabledSetting = { true },
            parentPidOf = { ownerOfOtherTray() },
        )
    }

    private fun trayLockHolder(paths: ArbPaths): Long? =
        runCatching { Files.readString(paths.trayLockFile()).trim().toLongOrNull() }.getOrNull()

    private fun awaitTrue(timeoutMs: Long = 5_000, condition: () -> Boolean): Boolean {
        val deadline = System.nanoTime() + TimeUnit.MILLISECONDS.toNanos(timeoutMs)
        while (System.nanoTime() < deadline) {
            if (condition()) return true
            Thread.sleep(50)
        }
        return condition()
    }

    /** Two IDE windows with the tray ON: B yields to A's tray, then takes over once A closes. */
    @Test
    fun secondIdeTakesTheTrayOverWhenTheOwningIdeCloses(@TempDir tmp: Path) {
        val paths = hermeticPaths(tmp)
        val ownerOfA = AtomicReference<Long?>(null)
        val a = TrayIde(paths, repoRoot()) { null }
        val b = TrayIde(paths, repoRoot()) { ownerOfA.get() }
        // The "IDE A" process that launched tray A: modelled by a process that has already exited
        // by the time A closes (the real parent JVM is shared by both services in this test).
        val ideA = ProcessBuilder(a.trayBin.toString(), "--version").redirectOutput(ProcessBuilder.Redirect.DISCARD).start()
        try {
            a.service.startParentBoundLifecycle { true }?.get(15, TimeUnit.SECONDS)
            val trayA = a.trays.single()
            assertTrue(awaitTrue { trayLockHolder(paths) == trayA.pid() }, "tray A owns the tray lock")

            b.service.syncTrayState(true)
            val yielded = b.trays.single()
            assertTrue(yielded.waitFor(5, TimeUnit.SECONDS), "tray B yields to tray A (single instance)")
            assertEquals(0, yielded.exitValue())
            ownerOfA.set(ideA.pid())
            b.service.superviseTray()
            assertEquals(1, b.trays.size, "no relaunch while tray A lives")

            assertTrue(ideA.waitFor(5, TimeUnit.SECONDS))
            a.service.dispose()
            assertTrue(trayA.waitFor(5, TimeUnit.SECONDS), "closing IDE A stops tray A")

            b.service.superviseTray()
            assertEquals(2, b.trays.size, "IDE B takes the tray over")
            val takenOver = b.trays.last()
            assertTrue(awaitTrue { trayLockHolder(paths) == takenOver.pid() } && takenOver.isAlive)

            // Quitting B's own tray from its menu (modelled by ending it) is respected.
            takenOver.destroy()
            assertTrue(takenOver.waitFor(5, TimeUnit.SECONDS))
            b.service.superviseTray()
            b.service.superviseTray()
            assertEquals(2, b.trays.size, "an explicitly quit tray is not resurrected")
        } finally {
            a.service.dispose()
            b.service.dispose()
            ideA.destroyForcibly()
        }
    }

    /** The user quits the other window's tray while that IDE keeps running: no resurrection. */
    @Test
    fun trayQuitInAnotherLiveIdeIsNotResurrected(@TempDir tmp: Path) {
        val paths = hermeticPaths(tmp)
        val a = TrayIde(paths, repoRoot()) { null }
        val b = TrayIde(paths, repoRoot()) { ProcessHandle.current().pid() }
        try {
            a.service.startParentBoundLifecycle { true }?.get(15, TimeUnit.SECONDS)
            val trayA = a.trays.single()
            assertTrue(awaitTrue { trayLockHolder(paths) == trayA.pid() })

            b.service.syncTrayState(true)
            assertTrue(b.trays.single().waitFor(5, TimeUnit.SECONDS))

            a.service.syncTrayState(false) // tray A is quit; IDE A (this live JVM) keeps running
            assertTrue(trayA.waitFor(5, TimeUnit.SECONDS))
            b.service.superviseTray()
            b.service.superviseTray()
            assertEquals(1, b.trays.size, "a tray the user quit must not come back from another window")
        } finally {
            a.service.dispose()
            b.service.dispose()
        }
    }

    // ---------------------------------------------------------------- binary setting (P1-5)

    @Test
    fun changingTheArbdBinarySettingReArmsTheUpgradeAndMovesAnIdleDaemonOntoIt(@TempDir tmp: Path) {
        val built = assertNotNull(
            ArbdProcessManager(paths = hermeticPaths(tmp.resolve("probe")), searchRoot = repoRoot()).resolveDaemonBinary(),
            "arbd binary not found",
        )
        val custom = tmp.resolve("custom").resolve(HostOs.detect().executableName("arbd"))
        Files.createDirectories(custom.parent)
        Files.copy(built, custom, StandardCopyOption.REPLACE_EXISTING)
        custom.toFile().setExecutable(true)

        val customSetting = AtomicReference<Path?>(null)
        val processManager = ArbdProcessManager(
            paths = hermeticPaths(tmp),
            searchRoot = repoRoot(),
            // CI exports ARB_DAEMON_BIN, which (correctly) outranks the Settings path.
            env = { key -> if (key.startsWith("ARB_DAEMON_")) null else System.getenv(key) },
            customBinaryProvider = { customSetting.get() },
            parentBound = true,
        )
        val service = ArbdDaemonService(processManager = processManager)
        try {
            service.ensureParentBoundLifecycleStarted()
            val before = assertNotNull(processManager.managedDaemonProcess())
            assertTrue(processManager.tryClaimIdentityUpgrade(), "spend this session's identity upgrade")

            customSetting.set(custom)
            service.notifyBinaryPathChanged()

            assertFalse(processManager.identityUpgradeAttempted, "a new binary re-arms the identity upgrade")
            assertTrue(before.waitFor(5, TimeUnit.SECONDS), "the idle managed daemon was replaced")
            val after = assertNotNull(processManager.managedDaemonProcess())
            assertNotEquals(before.pid(), after.pid())
            assertEquals(after.pid(), service.withClient { it.systemHello(false) }.pid)
            val runningExe = Paths.get(after.info().command().orElseThrow())
            assertTrue(Files.isSameFile(custom, runningExe), "the daemon now runs the configured binary")
        } finally {
            service.dispose()
        }
    }

    @Test
    fun restartWithAnEmptiedCustomPathPersistsTheBundledDefault() {
        var stored = "/opt/custom/arbd"
        var restarts = 0
        val configurable = ArbSettingsConfigurable(
            project = null,
            daemonConfigReader = { ArbConfig() },
            daemonConfigWriter = { _, _ -> ArbConfig() },
            localBinaryReader = { stored },
            localBinaryWriter = { stored = it },
            daemonRestarter = { restarts++ },
        )
        val form = configurable.createFormModel()
        assertEquals("/opt/custom/arbd", form.currentDraft().customArbdBinaryPath)
        form.loadFromDraft(form.currentDraft().copy(customArbdBinaryPath = ""))
        configurable.restartDaemonAndReload()
        assertEquals("", stored, "clearing the path must switch back to the bundled arbd before restarting")
        assertEquals(1, restarts)
        assertFalse(configurable.isModified())
        configurable.disposeUIResources()
    }

    @Test
    fun screenshotDestinationIsResolvedOffTheEdtAndAppliedAfterwards() {
        val pending = ArrayList<() -> Unit>()
        var resolved = 0
        var cliLookups = 0
        val configurable = ArbSettingsConfigurable(
            project = null,
            daemonConfigReader = { ArbConfig() },
            daemonConfigWriter = { _, _ -> ArbConfig() },
            localBinaryReader = { "" },
            localBinaryWriter = {},
            cliPathProvider = { cliLookups++; "/tools/arb-cli" },
            screenshotStatusResolver = { _, _ ->
                resolved++
                "Logcat 継承先: /shots"
            },
            backgroundUi = object : BackgroundUiRunner {
                override fun <T> run(work: () -> T, onEdt: (T) -> Unit) {
                    pending += { onEdt(work()) }
                }
            },
        )
        val form = configurable.createFormModel()
        assertEquals(0, cliLookups, "CLI discovery must run in background")
        assertEquals(0, resolved, "reset() must not read Studio's screenshot settings on the calling (EDT) thread")
        assertEquals("", form.screenshotStatusText)
        pending.toList().forEach { it() }
        assertEquals(1, resolved)
        assertEquals("Logcat 継承先: /shots", form.screenshotStatusText)
        configurable.disposeUIResources()
    }

    // ---------------------------------------------------------------- toolbar snapshot (P2-02)

    @Test
    fun toolbarFlagsAreDerivedFromOneImmutableSnapshot(@TempDir tmp: Path) {
        val service = serviceFor(FakeDaemon { _, _ -> Reply.Ok() }, tmp)
        try {
            service.updateCachedCaptureState(active = true, enabled = true)
            assertTrue(service.cachedCaptureActive && service.cachedCaptureEnabled)
            assertEquals(service.cachedToolbarSnapshot.captureActive, service.cachedCaptureActive)
            service.updateCachedToolbarSnapshot(ArbToolbarDaemonSnapshot(probed = true))
            assertFalse(service.cachedCaptureActive || service.cachedCaptureEnabled, "a reset snapshot resets the flags too")
            service.updateCachedCaptureActive(true)
            assertEquals(
                ArbToolbarDaemonSnapshot(probed = true, daemonConnected = true, captureActive = true),
                service.cachedToolbarSnapshot,
            )
        } finally {
            service.dispose()
        }
    }
}
