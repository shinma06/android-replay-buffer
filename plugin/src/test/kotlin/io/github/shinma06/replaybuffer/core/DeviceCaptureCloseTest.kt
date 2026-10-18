package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonParser
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlin.concurrent.thread
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertFailsWith
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertSame
import kotlin.test.assertTrue

class DeviceCaptureCloseTest {
    @Test
    fun allReaderRecoveryInvalidatesPendingEvidenceAndDrainedPairsButKeepsAnAdmittedPin() {
        for (stage in listOf("java", "paired", "admitted", "main-only", "stale-owner")) {
            Fixture().use { fixture ->
                val store = field(fixture.capture, "store") as CaptureStore
                CrashFixture(fixture.root, CaptureMode.VIDEO_AND_LOGS, suppliedStore = store).use { f ->
                    f.log(javaCrash(), 10_005_000_000)
                    if (stage != "java") f.log(platformCrash(), 10_006_000_000)
                    val pair = if (stage in listOf("paired", "admitted")) f.pair() else null
                    val pin = if (stage == "admitted") assertNotNull(store.capture(f.settings, pair, f.at(10_020_000_000))) else null
                    if (pin != null) store.autosave.admitted(pair!!, pin.id, f.at(10_020_000_000))
                    val before = store.autosave.snapshot()
                    if (stage == "stale-owner") store.logReaderBoundary(0) else {
                        val kind = if (stage == "main-only") "device_log-main" else "device_log-all"
                        DeviceCapture::class.java.getDeclaredMethod("status", String::class.java, StreamState::class.java,
                            String::class.java).apply { isAccessible = true }
                            .invoke(fixture.capture, kind, StreamState.RECOVERING, "synthetic reader restart")
                    }
                    store.configureAuto(f.settings, 1, "fixture", true, f.at(10_030_000_000))
                    when (stage) {
                        "java" -> {
                            f.log(platformCrash(), 10_040_000_000)
                            assertTrue(store.autosave.drain(f.at(10_050_000_000)).isEmpty())
                            assertEquals(AutoSaveReason.NOT_SAVED_CLOCK_UNKNOWN, store.autosave.snapshot().lastOutcome?.reason)
                        }
                        "paired" -> {
                            assertEquals(AutoSaveReason.NOT_SAVED_CLOCK_UNKNOWN,
                                assertFailsWith<AutoSaveRejected> { store.capture(f.settings, pair, f.at(10_050_000_000)) }.reason)
                            assertNotNull(store.capture(f.settings, host = f.at(10_050_000_000)))
                        }
                        "admitted" -> {
                            assertSame(pin, field(store, "pinned"))
                            assertEquals(before, store.autosave.snapshot())
                            store.release(pin!!.id)
                            assertNotNull(store.capture(f.settings, host = f.at(10_050_000_000)))
                        }
                        else -> {
                            val preserved = store.autosave.drain(f.at(10_050_000_000)).single()
                            assertNull(store.autosave.admissionProblem(preserved, f.at(10_050_000_000)))
                        }
                    }
                    assertEquals(if (stage == "admitted") 7 else 8, store.autosave.snapshot().admissionsRemaining)
                }
            }
        }
    }

    @Test
    fun failedOwnedClientAndReaderTerminationPreservesFirstFailureAndPendingIdentity() {
        Fixture().use { fixture ->
            fixture.blockAdbDrain()
            fixture.blockCaptureReader()
            val record = fixture.capture.cleanupRecord()
            val failure = assertFailsWith<IllegalStateException> { fixture.capture.close() }
            assertEquals("所有adb stderr readerの終了を確認できません", failure.message)
            assertEquals("取得readerの終了を確認できません", failure.suppressed.single().message)
            assertTrue(fixture.capture.cleanupPending)
            assertEquals(record, fixture.capture.cleanupRecord())
        }
    }

    @Test
    fun coreShutdownFailurePersistsExactRemoteOwnershipForNextJournalOwner() {
        Fixture().use { fixture ->
            fixture.blockCaptureReader()
            val record = fixture.capture.cleanupRecord()
            val core = fixture.core(fixture.root.resolve("journal"))
            val failure = assertFailsWith<java.util.concurrent.ExecutionException> {
                core.closeAsync().get(10, TimeUnit.SECONDS)
            }.cause!!
            assertEquals("取得readerの終了を確認できません", failure.message)
            assertSame(fixture.capture, field(core, "backend"))
            val path = fixture.root.resolve("journal/remote-cleanup-${record.token}.json")
            val json = JsonParser.parseString(Files.readString(path)).asJsonObject
            assertEquals(record.serial, json["serial"].asString)
            assertEquals(record.token, json["token"].asString)
            assertEquals(record.forwards, json["forwards"].asJsonObject.entrySet().associate { it.key to it.value.asString })
            RemoteCleanupJournal(path.parent).use { next ->
                next.recover()
                assertEquals(1, next.pendingCount)
                assertFalse(next.hasRecoverable) // Unconfirmed originating lifetime is quarantined after disposal.
            }
        }
    }

    @Test
    fun journalFailureIsSuppressedWithoutLosingBackendOrOriginalShutdownFailure() {
        Fixture().use { fixture ->
            fixture.blockCaptureReader()
            val stable = fixture.root.resolve("blocked-journal")
            val core = fixture.core(stable)
            Files.writeString(stable, "unrelated")
            val failure = assertFailsWith<java.util.concurrent.ExecutionException> {
                core.closeAsync().get(10, TimeUnit.SECONDS)
            }.cause!!
            assertEquals("取得readerの終了を確認できません", failure.message)
            assertTrue(failure.suppressed.single() is java.nio.file.FileAlreadyExistsException)
            assertSame(fixture.capture, field(core, "backend"))
            assertEquals("unrelated", Files.readString(stable))
            assertTrue(fixture.capture.cleanupPending)
        }
    }

    @Test
    fun journalReleaseFailureCannotReplaceOriginalShutdownFailure() {
        Fixture().use { fixture ->
            fixture.blockCaptureReader()
            val core = fixture.core(fixture.root.resolve("journal"))
            val journal = field(core, "cleanup") as RemoteCleanupJournal
            journal.retain(fixture.capture.cleanupRecord(), hostPending = true)
            val entry = (field(journal, "owned") as Map<*, *>).values.single()!!
            (field(entry, "channel") as java.nio.channels.FileChannel).close()
            val failure = assertFailsWith<java.util.concurrent.ExecutionException> {
                core.closeAsync().get(10, TimeUnit.SECONDS)
            }.cause!!
            assertEquals("取得readerの終了を確認できません", failure.message)
            assertTrue(failure.suppressed.any { it is java.nio.channels.ClosedChannelException })
            assertSame(fixture.capture, field(core, "backend"))
        }
    }

    @Test
    fun successfulStopWithFailedJournalWriteKeepsBackendAndCanRetryDisable() {
        Fixture().use { fixture ->
            val stable = fixture.root.resolve("journal")
            val core = fixture.core(stable, enabled = true)
            val record = fixture.capture.cleanupRecord()
            Files.writeString(stable, "unrelated")
            try {
                val result = core.setEnabled(false).get(10, TimeUnit.SECONDS)
                assertFalse(result.accepted)
                assertSame(fixture.capture, field(core, "backend"))
                assertTrue(core.snapshot().enabled)
                assertEquals("unrelated", Files.readString(stable))
                Files.delete(stable)
                assertTrue(core.setEnabled(false).get(10, TimeUnit.SECONDS).accepted)
                assertEquals(null, field(core, "backend"))
                assertFalse(core.snapshot().enabled)
                assertEquals(1, core.snapshot().cleanupPendingCount)
                assertTrue(Files.exists(stable.resolve("remote-cleanup-${record.token}.json")))
            } finally { core.closeAsync().get(10, TimeUnit.SECONDS) }
        }
    }

    @Test
    fun cleanupSnapshotWaitsForActualInFlightForwardAllocation() {
        Fixture(forwardFixture = true).use { fixture ->
            val executor = java.util.concurrent.Executors.newFixedThreadPool(2)
            val video = DeviceCapture::class.java.getDeclaredMethod("video").apply { isAccessible = true }
            val acquisition = executor.submit { runCatching { video.invoke(fixture.capture) } }
            try {
                val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
                while (!Files.exists(fixture.root.resolve("allocating")) && System.nanoTime() < deadline) Thread.sleep(10)
                assertTrue(Files.exists(fixture.root.resolve("allocating")))
                val snapshotStarted = CountDownLatch(1)
                val snapshot = executor.submit<RemoteCleanup> {
                    snapshotStarted.countDown()
                    fixture.capture.cleanupRecord()
                }
                assertTrue(snapshotStarted.await(5, TimeUnit.SECONDS))
                assertFailsWith<java.util.concurrent.TimeoutException> { snapshot.get(100, TimeUnit.MILLISECONDS) }
                Files.writeString(fixture.root.resolve("allocated"), "release")
                assertTrue(Regex("localabstract:scrcpy_[0-9a-f]{8}").matches(snapshot.get(5, TimeUnit.SECONDS).forwards.getValue("23456")))
                acquisition.get(5, TimeUnit.SECONDS)
            } finally {
                Files.writeString(fixture.root.resolve("allocated"), "release")
                executor.shutdown()
                assertTrue(executor.awaitTermination(10, TimeUnit.SECONDS))
            }
        }
    }

    @Test
    fun interruptedReaderJoinRetainsIdentityAndRepeatedCloseCanFinish() {
        Fixture().use { fixture ->
            fixture.blockCaptureReader()
            val record = fixture.capture.cleanupRecord()
            val stopped = java.util.concurrent.CompletableFuture<Throwable>()
            val closer = thread(name = "replay-close-interruption-fixture", isDaemon = true) {
                try {
                    fixture.capture.close()
                    stopped.complete(AssertionError("The blocked reader must not report a successful close"))
                } catch (failure: Throwable) { stopped.complete(failure) }
            }
            try {
                val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
                while (closer.state != Thread.State.TIMED_WAITING && System.nanoTime() < deadline) Thread.sleep(10)
                assertEquals(Thread.State.TIMED_WAITING, closer.state)
                closer.interrupt()
                assertTrue(stopped.get(5, TimeUnit.SECONDS) is InterruptedException)
                assertTrue(fixture.capture.cleanupPending)
                assertEquals(record, fixture.capture.cleanupRecord())
                fixture.releaseReaders()
                fixture.capture.close()
                assertTrue(fixture.capture.cleanupPending) // Offline cleanup stays recoverable after a successful host stop.
                assertEquals(record, fixture.capture.cleanupRecord())
            } finally { closer.interrupt(); closer.join(5000); check(!closer.isAlive) }
        }
    }

    private class Fixture(forwardFixture: Boolean = false) : AutoCloseable {
        val root = Files.createTempDirectory("replay-close-failure-").toRealPath()
        private val resources = CaptureResources(Files.createDirectory(root.resolve("resources")))
        private val store = CaptureStore(root.resolve("ring"))
        private val adbPath = if (forwardFixture) {
            Files.writeString(root.resolve("adb"), """#!/bin/sh
if [ "${'$'}1" = forward ] && [ "${'$'}2" = --list ]; then exit 0; fi
if [ "${'$'}3" = push ]; then exit 0; fi
if [ "${'$'}3" = forward ]; then
    touch "${root.resolve("allocating")}"
    while [ ! -e "${root.resolve("allocated")}" ]; do sleep 0.01; done
    printf '23456\n'
    exit 0
fi
exit 1
""").also { assertTrue(it.toFile().setExecutable(true)) }
        } else Path.of("/bin/sh")
        val capture = DeviceCapture(adbPath, "fixture-1", resources, store, 1, ApplicationTarget())
        private val release = CountDownLatch(1)
        private val blocked = mutableListOf<Thread>()

        init {
            @Suppress("UNCHECKED_CAST")
            val ports = field(capture, "ports") as MutableMap<String, String>
            ports["12345"] = "localabstract:scrcpy_00000001"
        }

        private fun blockedReader(): Thread {
            val entered = CountDownLatch(1)
            return thread(name = "replay-owned-failure-fixture", isDaemon = true) {
                entered.countDown()
                while (release.count > 0) {
                    try { release.await() } catch (_: InterruptedException) { /* Simulate a reader that cannot yet exit. */ }
                }
            }.also { blocked += it; assertTrue(entered.await(5, TimeUnit.SECONDS)) }
        }

        fun blockCaptureReader() {
            @Suppress("UNCHECKED_CAST")
            val workers = field(capture, "workers") as MutableList<Thread>
            workers += blockedReader()
        }

        fun blockAdbDrain() {
            val adb = field(capture, "adb") as OwnedAdb
            val process = adb.start("-c", "exit 0")
            assertTrue(process.waitFor(5, TimeUnit.SECONDS))
            @Suppress("UNCHECKED_CAST")
            val drains = field(adb, "drains") as MutableMap<Process, Thread>
            drains.getValue(process).join(5000)
            drains[process] = blockedReader()
        }

        fun core(journal: Path, enabled: Boolean = false): ReplayCore = ReplayCore(ReplaySettings(), root.resolve("workspace"), journal).also {
            // Finish initialization on the actual control executor before installing the owned failing backend.
            assertTrue(it.applySettings(ReplaySettings()).get(5, TimeUnit.SECONDS).accepted)
            if (enabled) assertTrue(it.setEnabled(true).get(5, TimeUnit.SECONDS).accepted)
            ReplayCore::class.java.getDeclaredField("backend").apply { isAccessible = true }.set(it, capture)
        }

        fun releaseReaders() {
            release.countDown()
            blocked.forEach { it.join(5000); check(!it.isAlive) }
        }

        override fun close() {
            releaseReaders()
            capture.close()
            store.close()
            resources.close()
            Files.walk(root).use { it.sorted(java.util.Comparator.reverseOrder()).forEach(Files::deleteIfExists) }
        }
    }

    companion object {
        private fun field(owner: Any, name: String): Any? = owner.javaClass.getDeclaredField(name)
            .apply { isAccessible = true }.get(owner)
    }
}
