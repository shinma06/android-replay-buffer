package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonParser
import io.github.shinma06.replaybuffer.ui.autoSaveDescription
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.condition.EnabledOnOs
import org.junit.jupiter.api.condition.OS
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Future
import java.util.concurrent.ScheduledExecutorService
import java.util.concurrent.TimeUnit
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertSame
import kotlin.test.assertTrue

@EnabledOnOs(OS.MAC) // Exercises the production descriptor-relative macOS publication, not IDE/device acceptance.
class CrashSaveLifecycleTest {
    @TempDir lateinit var root: Path

    @Test
    fun coreApplyRejectsAnUndrainedPairAndWritesOnlyFreshReportsWithTheAppliedRevision() {
        val captures = mutableListOf<FrozenCapture>()
        CoreCrashFixture(root) { capture, directory, cancelled, publish ->
            captures += capture
            SaveWriter().write(capture, directory, cancelled, publish)
        }.use { f ->
            val destination = Files.createDirectory(root.resolve("applied"))
            val applied = f.core.snapshot().settings.copy(saveDirectory = destination)
            val applying = f.onControl { f.data.standard(); f.core.applySettings(applied) }
            assertTrue(applying.get(5, TimeUnit.SECONDS).accepted)
            assertTrue(captures.isEmpty())
            assertNull(f.field<FrozenCapture?>("pending"))
            assertEquals(2, f.core.snapshot().autoSave.notSavedCount)
            assertEquals(AutoSaveReason.NOT_SAVED_PRE_ARM_EVENT, f.core.snapshot().autoSave.lastOutcome?.reason)
            val revision = f.core.snapshot().settingsRevision
            f.onControl {
                val time = System.nanoTime() - f.data.host + 20_000_000
                f.data.anchor(time)
                f.data.reporter(time + 1_000_000, time + 2_000_000)
                f.data.log(javaCrash(wall = CRASH_WALL + time + 10_000_000), time + 12_000_000)
                f.data.log(platformCrash(wall = CRASH_WALL + time + 11_000_000), time + 13_000_000)
                f.tick(time + 30_000_000)
            }
            f.finishWriter()
            val capture = captures.single()
            assertEquals(revision, capture.autoTrigger?.facts?.get("option_revision"))
            assertEquals(destination, capture.settings.saveDirectory)
            assertEquals(SavePhase.COMPLETED, f.core.snapshot().lastSave?.phase)
            assertEquals(7, f.core.snapshot().autoSave.admissionsRemaining)
        }
    }

    @Test
    fun failedHistoryDoesNotAdvertiseRetryAfterDiscardOffOrSuccessfulRetry() {
        for (transition in listOf("discard", "off", "retry")) {
            var calls = 0
            CoreCrashFixture(Files.createDirectory(root.resolve(transition))) { capture, directory, cancelled, publish ->
                calls++
                SaveWriter().write(capture, directory, cancelled) { publication ->
                    check(calls != 1) { "Synthetic prepublication failure" }
                    publish(publication)
                }
            }.use { f ->
                f.trigger()
                f.finishWriter()
                val failed = assertNotNull(f.core.snapshot().lastSave)
                val retained = assertNotNull(failed.retainedPath)
                val requestId = assertNotNull(failed.requestId)
                val warning = f.core.snapshot().autoSave.warning
                assertEquals(SavePhase.FAILED, f.core.snapshot().save.phase)
                assertEquals(requestId, f.field<FrozenCapture>("pending").id)
                assertEquals(AutoSaveReason.SAVE_FAILED.description, warning)
                assertTrue(Files.isDirectory(retained))
                when (transition) {
                    "discard" -> assertTrue(f.core.discard(requestId).get(5, TimeUnit.SECONDS).accepted)
                    "off" -> assertTrue(f.core.setEnabled(false).get(5, TimeUnit.SECONDS).accepted)
                    else -> {
                        assertTrue(f.core.retry(requestId).get(5, TimeUnit.SECONDS).accepted)
                        f.finishWriter()
                    }
                }
                val current = f.core.snapshot()
                val text = autoSaveDescription(current)
                assertFalse(text.contains("再試行"), transition)
                assertEquals(warning, current.autoSave.warning)
                assertTrue(text.contains("要確認: $warning"), transition)
                assertTrue(text.contains(requestId), transition)
                assertNull(f.field<FrozenCapture?>("pending"))
                assertFalse(f.core.retry(requestId).get(5, TimeUnit.SECONDS).accepted)
                assertTrue(Files.isDirectory(retained))
                if (transition == "retry") {
                    assertEquals(2, calls)
                    assertEquals(SavePhase.COMPLETED, current.save.phase)
                    assertEquals(requestId, current.lastSave?.requestId)
                    assertEquals(failed.attempt + 1, current.lastSave?.attempt)
                    assertTrue(text.contains("保存: 完了"))
                } else {
                    assertEquals(1, calls)
                    assertEquals(SavePhase.IDLE, current.save.phase)
                    assertEquals(failed, current.lastSave)
                    for (value in listOf("保存失敗", retained.toString(), failed.directory.toString(), "試行${failed.attempt}"))
                        assertTrue(text.contains(value), "$transition: $value")
                }
            }
        }
    }

    @Test
    fun failedAutoSaveKeepsOneCaptureAndRetryUsesTheSameTriggerWithANewAttemptAndDestination() {
        val captures = mutableListOf<FrozenCapture>()
        CoreCrashFixture(root) { capture, directory, cancelled, publish ->
            captures += capture
            (if (captures.size == 1) SaveWriter { 0 } else SaveWriter()).write(capture, directory, cancelled, publish)
        }.use { f ->
            f.trigger()
            f.finishWriter()
            val failed = assertNotNull(f.core.snapshot().lastSave)
            val oldAttempt = f.field<SaveAttempt>("saveAttempt")
            assertEquals(SavePhase.FAILED, failed.phase)
            assertEquals(AutoSaveReason.SAVE_FAILED, f.core.snapshot().autoSave.lastOutcome?.reason)
            assertFalse(f.core.save().get(5, TimeUnit.SECONDS).accepted)
            f.onControl {
                f.data.log(javaCrash(wall = CRASH_WALL + 10_007_000_000), 10_009_000_000)
                f.data.log(platformCrash(wall = CRASH_WALL + 10_008_000_000), 10_010_000_000)
                f.tick(10_020_000_000)
            }
            assertEquals(AutoSaveReason.NOT_SAVED_FAILED_PIN, f.core.snapshot().autoSave.lastOutcome?.reason)
            assertEquals(1, captures.size)
            val destination = Files.createDirectory(root.resolve("retry"))
            assertTrue(f.core.retryAtDirectory(failed.requestId!!, destination).get(5, TimeUnit.SECONDS).accepted)
            f.finishWriter()
            val completed = assertNotNull(f.core.snapshot().lastSave)
            assertEquals(SavePhase.COMPLETED, completed.phase)
            assertEquals(failed.requestId, completed.requestId)
            assertEquals(failed.attempt + 1, completed.attempt)
            assertEquals(failed.autoEventId, completed.autoEventId)
            assertSame(captures[0], captures[1])
            assertEquals(destination.toRealPath(), completed.directory?.parent)
            val manifest = JsonParser.parseString(Files.readString(completed.directory!!.resolve("session.json"))).asJsonObject
            assertEquals(failed.autoEventId, manifest["auto_trigger"].asJsonObject["event_id"].asString)
            assertFalse(manifest["auto_trigger"].asJsonObject.has("terminal_outcome"))
            assertNull(f.field<FrozenCapture?>("pending"))
            f.onControl { f.settle(oldAttempt) }
            assertEquals(completed, f.field<SaveSnapshot>("saveState"))
        }
    }

    @Test
    fun manualSaveBlocksAutoWithoutQueueingOrChangingItsPinnedWindow() {
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        var calls = 0
        CoreCrashFixture(root) { capture, directory, cancelled, publish ->
            calls++
            entered.countDown()
            check(release.await(10, TimeUnit.SECONDS))
            SaveWriter().write(capture, directory, cancelled, publish)
        }.use { f ->
            try {
                assertTrue(f.core.save().get(5, TimeUnit.SECONDS).accepted)
                assertTrue(entered.await(5, TimeUnit.SECONDS))
                val manual = f.field<FrozenCapture>("pending")
                f.trigger()
                assertEquals(AutoSaveReason.NOT_SAVED_BUSY, f.core.snapshot().autoSave.lastOutcome?.reason)
                assertSame(manual, f.field("pending"))
                assertNull(manual.autoTrigger)
                release.countDown()
                f.finishWriter()
                f.onControl { f.tick(10_030_000_000) }
                assertEquals(1, calls)
                assertEquals(8, f.core.snapshot().autoSave.admissionsRemaining)
            } finally { release.countDown() }
        }
    }

    @Test
    fun unassessedPairCanBeAdmittedOnceAfterManualResultIsSettled() {
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val captures = mutableListOf<FrozenCapture>()
        CoreCrashFixture(root) { capture, directory, cancelled, publish ->
            captures += capture
            if (capture.autoTrigger == null) {
                entered.countDown()
                check(release.await(10, TimeUnit.SECONDS))
            }
            SaveWriter().write(capture, directory, cancelled, publish)
        }.use { f ->
            try {
                assertTrue(f.core.save().get(5, TimeUnit.SECONDS).accepted)
                assertTrue(entered.await(5, TimeUnit.SECONDS))
                val manual = f.field<FrozenCapture>("pending")
                val attempt = f.field<SaveAttempt>("saveAttempt")
                val (manualResult, manualBytes) = f.onControl {
                    f.data.standard()
                    release.countDown()
                    f.field<Future<*>>("saveTask").get(5, TimeUnit.SECONDS)
                    val result = assertNotNull(attempt.result)
                    assertEquals(SavePhase.COMPLETED, result.phase)
                    val bytes = Files.readAllBytes(result.directory!!.resolve("session.json"))
                    f.settle(attempt)
                    assertNull(f.field<FrozenCapture?>("pending"))
                    assertEquals(8, f.core.snapshot().autoSave.admissionsRemaining)
                    f.tick(10_030_000_000)
                    result to bytes
                }
                f.finishWriter()
                assertEquals(2, captures.size)
                assertSame(manual, captures[0])
                assertNull(manual.autoTrigger)
                assertEquals(manualResult, attempt.result)
                assertContentEquals(manualBytes, Files.readAllBytes(manualResult.directory!!.resolve("session.json")))
                val automatic = captures[1]
                assertTrue(automatic.id != manual.id)
                val facts = assertNotNull(automatic.autoTrigger).facts
                for ((key, receipt) in listOf("java_evidence" to 10_005_000_000L, "platform_evidence" to 10_006_000_000L)) {
                    val evidence = facts[key] as Map<*, *>
                    assertEquals(f.data.at(receipt).toString(), evidence["receipt_host_ns"])
                    assertEquals(f.data.at(receipt), automatic.logs.single { it.id == evidence["record_id"] }.host)
                    assertTrue(manual.logs.none { it.id == evidence["record_id"] })
                }
                assertEquals(7, f.core.snapshot().autoSave.admissionsRemaining)
                assertEquals(0, f.core.snapshot().autoSave.notSavedCount)
                assertEquals(SavePhase.COMPLETED, f.core.snapshot().lastSave?.phase)
            } finally { release.countDown() }
        }
    }

    @Test
    fun verifiedWriterResultSurvivesOptionOffCaptureOffAndDisposeBeforeControlNotification() {
        for (transition in listOf("option", "off", "dispose")) {
            val resultReady = CountDownLatch(1)
            val returnResult = CountDownLatch(1)
            CoreCrashFixture(Files.createDirectory(root.resolve(transition))) { capture, directory, cancelled, publish ->
                SaveWriter().write(capture, directory, cancelled, publish).also {
                    resultReady.countDown()
                    check(returnResult.await(15, TimeUnit.SECONDS))
                }
            }.use { f ->
                try {
                    f.trigger()
                    assertTrue(resultReady.await(5, TimeUnit.SECONDS))
                    // Hold the callback lane: completion must already be visible from the writer.
                    val controlEntered = CountDownLatch(1)
                    val controlRelease = CountDownLatch(1)
                    f.control.execute { controlEntered.countDown(); check(controlRelease.await(15, TimeUnit.SECONDS)) }
                    assertTrue(controlEntered.await(5, TimeUnit.SECONDS))
                    val change = when (transition) {
                        "option" -> f.core.applySettings(f.data.settings.copy(autoSaveOnCrash = false))
                        "off" -> f.core.setEnabled(false)
                        else -> f.core.closeAsync()
                    }
                    returnResult.countDown()
                    f.field<Future<*>>("saveTask").get(5, TimeUnit.SECONDS)
                    val completed = assertNotNull(f.core.snapshot().lastSave)
                    assertEquals(SavePhase.COMPLETED, completed.phase, transition)
                    assertEquals(AutoSaveReason.SAVED, f.core.snapshot().autoSave.lastOutcome?.reason, transition)
                    assertTrue(Files.isRegularFile(completed.directory!!.resolve("session.json")))
                    controlRelease.countDown()
                    change.get(5, TimeUnit.SECONDS)
                    assertEquals(completed, f.core.snapshot().lastSave, transition)
                    assertEquals(AutoSaveReason.SAVED, f.core.snapshot().autoSave.lastOutcome?.reason, transition)
                } finally { returnResult.countDown() }
            }
        }
    }

    @Test
    fun prePublicationCancellationIsDistinctFromPostRenameVerificationFailure() {
        for (cancel in listOf(true, false)) {
            val atPublication = CountDownLatch(1)
            val release = CountDownLatch(1)
            var published: Path? = null
            CoreCrashFixture(Files.createDirectory(root.resolve("cancel-$cancel"))) { capture, directory, cancelled, publish ->
                SaveWriter().write(capture, directory, cancelled) { publication ->
                    atPublication.countDown()
                    check(release.await(10, TimeUnit.SECONDS))
                    if (cancel) publish(publication) else {
                        publish(publication)
                        published = publication.complete
                        Files.writeString(publication.complete.resolve("foreign-entry"), "SYNTHETIC_284")
                    }
                }
            }.use { f ->
                try {
                    f.trigger()
                    assertTrue(atPublication.await(5, TimeUnit.SECONDS))
                    val off = if (cancel) f.core.setEnabled(false) else null
                    release.countDown()
                    if (off != null) assertTrue(off.get(5, TimeUnit.SECONDS).accepted) else f.finishWriter()
                    val result = assertNotNull(f.core.snapshot().lastSave)
                    assertEquals(if (cancel) SavePhase.CANCELLED else SavePhase.FAILED, result.phase)
                    assertEquals(if (cancel) AutoSaveReason.CANCELLED else AutoSaveReason.SAVE_FAILED,
                        f.core.snapshot().autoSave.lastOutcome?.reason)
                    val retained = assertNotNull(result.retainedPath)
                    if (cancel) assertTrue(retained.fileName.toString().endsWith(".partial")) else assertEquals(published, retained)
                    val manifest = JsonParser.parseString(Files.readString(retained.resolve("session.json"))).asJsonObject
                    assertFalse(manifest["complete"].asBoolean)
                    assertEquals(result.autoEventId, manifest["auto_trigger"].asJsonObject["event_id"].asString)
                } finally { release.countDown() }
            }
        }
    }

    @Test
    fun offTimeoutKeepsPendingUntilTheActualWriterFinishes() {
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        CoreCrashFixture(root) { capture, directory, cancelled, publish ->
            entered.countDown()
            check(release.await(20, TimeUnit.SECONDS))
            SaveWriter().write(capture, directory, cancelled, publish)
        }.use { f ->
            try {
                f.trigger()
                assertTrue(entered.await(5, TimeUnit.SECONDS))
                assertFalse(f.core.setEnabled(false).get(12, TimeUnit.SECONDS).accepted)
                assertEquals(SavePhase.WRITING, f.core.snapshot().lastSave?.phase)
                assertEquals(AutoSaveReason.ACCEPTED_WRITING, f.core.snapshot().autoSave.lastOutcome?.reason)
                assertNotNull(f.field<FrozenCapture?>("pending"))
                release.countDown()
                f.finishWriter()
                assertEquals(SavePhase.CANCELLED, f.core.snapshot().lastSave?.phase)
                assertEquals(AutoSaveReason.CANCELLED, f.core.snapshot().autoSave.lastOutcome?.reason)
                assertTrue(f.core.setEnabled(false).get(5, TimeUnit.SECONDS).accepted)
            } finally { release.countDown() }
        }
    }
}

/** Real core/store/writer with synthetic wire records; no ADB or IDE is launched. */
private class CoreCrashFixture(
    root: Path,
    write: (FrozenCapture, Path, () -> Boolean, (SavePublication) -> Unit) -> SaveOutput,
) : AutoCloseable {
    val core = ReplayCore(ReplaySettings(saveDirectory = root, application = ApplicationTarget("com.example.target"),
        captureMode = CaptureMode.LOG_ONLY, replaySeconds = 30, autoSaveOnCrash = true), root.resolve("workspace"), root, write)
    val control: ScheduledExecutorService = field("control")
    val data: CrashFixture
    init {
        check(core.setEnabled(true).get(5, TimeUnit.SECONDS).accepted)
        data = onControl {
            ReplayCore::class.java.getDeclaredField("device").apply { isAccessible = true }
                .set(core, ReplayDevice("fixture", "Synthetic", DeviceKind.EMULATOR, true))
            CrashFixture(root, suppliedStore = field("store"), host = System.nanoTime() - 10_020_000_000,
                revision = core.snapshot().settingsRevision)
        }
    }
    @Suppress("UNCHECKED_CAST")
    fun <T> field(name: String): T = ReplayCore::class.java.getDeclaredField(name).apply { isAccessible = true }.get(core) as T
    fun <T> onControl(action: () -> T): T = control.submit<T> { action() }.get(5, TimeUnit.SECONDS)
    fun tick(relative: Long) {
        ReplayCore::class.java.getDeclaredMethod("autoSave", Long::class.javaPrimitiveType).apply { isAccessible = true }
            .invoke(core, data.at(relative))
    }
    fun trigger() = onControl { data.standard(); tick(10_020_000_000) }
    fun finishWriter() {
        field<Future<*>>("saveTask").get(5, TimeUnit.SECONDS)
        onControl { settle(field("saveAttempt")) }
    }
    fun settle(attempt: SaveAttempt) = ReplayCore::class.java.getDeclaredMethod("settleSave", SaveAttempt::class.java)
        .apply { isAccessible = true }.invoke(core, attempt)
    override fun close() { core.closeAsync().get(15, TimeUnit.SECONDS) }
}
