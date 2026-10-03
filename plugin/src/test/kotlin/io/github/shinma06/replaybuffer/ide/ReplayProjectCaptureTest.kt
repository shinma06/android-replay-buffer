package io.github.shinma06.replaybuffer.ide

import io.github.shinma06.replaybuffer.core.ApplicationMode
import io.github.shinma06.replaybuffer.core.CaptureState
import io.github.shinma06.replaybuffer.core.SavePhase
import io.github.shinma06.replaybuffer.core.SaveSnapshot
import io.github.shinma06.replaybuffer.settings.AppSelectionMode
import io.github.shinma06.replaybuffer.settings.ReplaySettings
import io.github.shinma06.replaybuffer.settings.ReplaySettingsState
import io.github.shinma06.replaybuffer.settings.ReplaySettingsStore
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

/** Exercises the actual core with no SDK/adb, IDE, ToolWindow, or device. */
class ReplayProjectCaptureTest {
    @TempDir lateinit var directory: Path
    private val unresolved = AndroidReplayEnvironment(null, "SDK未設定", ApplicationSelection(null, "fixture", "実行対象未選択"))

    @Test
    fun `persisted enabled restores without a panel and Apply preserves the latest toggle`() {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val store = ReplaySettingsStore()
        store.loadState(ReplaySettingsState(enabled = true, retentionSeconds = 60, appSelection = AppSelectionMode.MANUAL, manualPackage = "com.manual.app"))
        val capture = ReplayProjectCapture(scope, store, {}, {})
        try {
            assertTrue(capture.applySettings(store.settings(), unresolved, 0).get(15, TimeUnit.SECONDS).accepted)
            val restored = capture.view.snapshot!!
            assertTrue(restored.enabled)
            assertEquals(CaptureState.WAITING, restored.captureState)
            assertEquals(60, restored.settings.replaySeconds)
            assertNull(restored.settings.adbPath)
            assertNull(restored.settings.saveDirectory)
            assertEquals("com.manual.app", restored.settings.application.packageName)
            assertEquals(ApplicationMode.MANUAL, restored.settings.application.mode)
            val disable = capture.setEnabled(false)
            val next = store.settings().copy(retentionSeconds = 900, destination = directory.toString())
            store.apply(next)
            val apply = capture.applySettings(next, unresolved, 0)
            assertTrue(disable.get(15, TimeUnit.SECONDS).accepted)
            assertTrue(apply.get(15, TimeUnit.SECONDS).accepted)
            assertFalse(store.enabled)
            assertFalse(capture.view.snapshot!!.enabled)
            assertEquals(900, capture.view.snapshot!!.settings.replaySeconds)
            assertEquals(0, capture.view.pending)
            assertFalse(capture.save().get(15, TimeUnit.SECONDS).accepted)
            assertNotNull(capture.view.message)
        } finally {
            capture.close()
            capture.termination.get(15, TimeUnit.SECONDS)
            scope.cancel()
        }
    }

    @Test
    fun `invalid startup settings reconcile persisted enabled and preserve settings for correction`() {
        for (destination in listOf(Files.createFile(directory.resolve("file")), directory.resolve("missing"))) {
            val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
            val store = ReplaySettingsStore()
            store.loadState(ReplaySettingsState(enabled = true, destination = destination.toString()))
            val capture = ReplayProjectCapture(scope, store, {}, {})
            try {
                val settings = store.settings()
                val rejected = capture.applySettings(settings, unresolved, 0).get(15, TimeUnit.SECONDS)
                assertFalse(rejected.accepted)
                assertFalse(store.enabled)
                assertFalse(capture.view.snapshot!!.enabled)
                assertEquals(CaptureState.DISABLED, capture.view.snapshot!!.captureState)
                assertEquals(settings, store.settings())
                assertTrue(capture.view.message!!.startsWith("保存済み設定を反映できません"))
                // Correction does not silently restart capture; explicit ON still works without a save path.
                store.apply(settings.copy(destination = ""))
                assertTrue(capture.applySettings(store.settings(), unresolved, 0).get(15, TimeUnit.SECONDS).accepted)
                assertFalse(capture.view.snapshot!!.enabled)
                assertTrue(capture.setEnabled(true).get(15, TimeUnit.SECONDS).accepted)
                assertTrue(store.enabled)
                assertTrue(capture.view.snapshot!!.enabled)
                assertNull(capture.view.snapshot!!.settings.saveDirectory)
            } finally {
                capture.close()
                capture.termination.get(15, TimeUnit.SECONDS)
                scope.cancel()
            }
        }
    }

    @Test
    fun `startup failure queued before a later ON cannot roll back the users persisted intent`() {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val store = ReplaySettingsStore()
        store.loadState(ReplaySettingsState(enabled = true, destination = directory.resolve("missing").toString()))
        val blocked = java.util.concurrent.CountDownLatch(1)
        val release = java.util.concurrent.CountDownLatch(1)
        val hold = AtomicBoolean(false)
        lateinit var capture: ReplayProjectCapture
        capture = ReplayProjectCapture(scope, store, {
            if (hold.get() && capture.view.snapshot?.enabled == false && hold.compareAndSet(true, false)) {
                blocked.countDown()
                assertTrue(release.await(15, TimeUnit.SECONDS))
            }
        }, {})
        try {
            // Hold an earlier real core operation so failed restoration queues before ON.
            assertTrue(capture.setEnabled(true).get(15, TimeUnit.SECONDS).accepted)
            hold.set(true)
            val off = capture.setEnabled(false)
            assertTrue(blocked.await(15, TimeUnit.SECONDS))
            val rejected = capture.applySettings(store.settings(), unresolved, 0)
            val on = capture.setEnabled(true)
            assertTrue(store.enabled)
            release.countDown()
            assertTrue(off.get(15, TimeUnit.SECONDS).accepted)
            assertFalse(rejected.get(15, TimeUnit.SECONDS).accepted)
            assertTrue(on.get(15, TimeUnit.SECONDS).accepted)
            assertTrue(store.enabled)
            assertTrue(capture.view.snapshot!!.enabled)
        } finally {
            release.countDown()
            capture.close()
            capture.termination.get(15, TimeUnit.SECONDS)
            scope.cancel()
        }
    }

    @Test
    fun `old resolved settings resumed after newer settings cannot revert the actual core`() {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val store = ReplaySettingsStore()
        val capture = ReplayProjectCapture(scope, store, {}, {})
        try {
            val old = ReplaySettings(retentionSeconds = 60, destination = Files.createDirectory(directory.resolve("old")).toString())
            val oldEnvironment = unresolved.copy(application = ApplicationSelection("com.old.app", "old", null))
            store.apply(old)
            capture.expectSettings(1)
            // The old resolver has produced its result, but pauses before calling Capture.
            val next = old.copy(retentionSeconds = 900, destination = Files.createDirectory(directory.resolve("new")).toString())
            val nextEnvironment = unresolved.copy(application = ApplicationSelection("com.new.app", "new", null))
            store.apply(next)
            capture.expectSettings(2)
            assertTrue(capture.applySettings(next, nextEnvironment, 2).get(15, TimeUnit.SECONDS).accepted)
            val applied = capture.view
            assertEquals(next.toCoreSettings(nextEnvironment), applied.snapshot!!.settings)
            // Cancellation alone cannot stop this late non-suspending call from the old resolver.
            assertFalse(capture.applySettings(old, oldEnvironment, 1).get(15, TimeUnit.SECONDS).accepted)
            assertEquals(next, store.settings())
            assertEquals(applied, capture.view)
        } finally {
            capture.close()
            capture.termination.get(15, TimeUnit.SECONDS)
            scope.cancel()
        }
    }

    @Test
    fun `revision changed in the submission callback rejects queued old automatic application`() {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val store = ReplaySettingsStore()
        val settings = ReplaySettings(retentionSeconds = 900)
        val oldEnvironment = unresolved.copy(application = ApplicationSelection("com.old.app", "old", null))
        val nextEnvironment = unresolved.copy(application = ApplicationSelection("com.new.app", "new", null))
        val replace = AtomicBoolean(false)
        lateinit var capture: ReplayProjectCapture
        capture = ReplayProjectCapture(scope, store, {
            if (replace.compareAndSet(true, false)) {
                capture.expectSettings(2)
                // This callback is outside the owner's lock: the new real core operation can finish.
                assertTrue(capture.applySettings(settings, nextEnvironment, 2).get(15, TimeUnit.SECONDS).accepted)
            }
        }, {})
        try {
            store.apply(settings)
            assertTrue(capture.applySettings(settings, unresolved, 0).get(15, TimeUnit.SECONDS).accepted)
            capture.expectSettings(1)
            replace.set(true)
            assertFalse(capture.applySettings(settings, oldEnvironment, 1).get(15, TimeUnit.SECONDS).accepted)
            assertEquals(settings.toCoreSettings(nextEnvironment), capture.view.snapshot!!.settings)
            assertEquals(0, capture.view.pending)
            assertNull(capture.view.message)
        } finally {
            capture.close()
            capture.termination.get(15, TimeUnit.SECONDS)
            scope.cancel()
        }
    }

    @Test
    fun `rapid toggles are ordered and closed owner cannot alter persistent enabled`() {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val store = ReplaySettingsStore()
        val capture = ReplayProjectCapture(scope, store, {}, {})
        try {
            val operations = listOf(capture.setEnabled(true), capture.setEnabled(false), capture.setEnabled(true), capture.setEnabled(false))
            operations.forEach { assertTrue(it.get(15, TimeUnit.SECONDS).accepted) }
            assertFalse(store.enabled)
            assertFalse(capture.view.snapshot!!.enabled)
            assertEquals(0, capture.view.pending)
            capture.close()
            capture.termination.get(15, TimeUnit.SECONDS)
            assertFalse(capture.setEnabled(true).get(15, TimeUnit.SECONDS).accepted)
            assertFalse(store.enabled)
        } finally {
            capture.close()
            capture.termination.get(15, TimeUnit.SECONDS)
            scope.cancel()
        }
    }

    @Test
    fun `a destination replaced after Apply reports saved settings failure and keeps actual capture settings`() {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val store = ReplaySettingsStore()
        val capture = ReplayProjectCapture(scope, store, {}, {})
        try {
            val original = ReplaySettings(retentionSeconds = 60, destination = directory.toString())
            store.apply(original)
            assertTrue(capture.applySettings(original, unresolved, 0).get(15, TimeUnit.SECONDS).accepted)
            val changedDirectory = Files.createDirectory(directory.resolve("new-directory"))
            val changed = original.copy(retentionSeconds = 900, destination = changedDirectory.toString())
            store.apply(changed)
            Files.delete(changedDirectory)
            Files.createFile(changedDirectory)
            val result = capture.applySettings(changed, unresolved, 0).get(15, TimeUnit.SECONDS)
            assertFalse(result.accepted)
            assertTrue(result.reason!!.startsWith("保存済み設定を反映できません"))
            assertEquals(changed, store.settings())
            assertEquals(60, capture.view.snapshot!!.settings.replaySeconds)
            assertEquals(directory, capture.view.snapshot!!.settings.saveDirectory)
            assertEquals(0, capture.view.pending)
        } finally {
            capture.close()
            capture.termination.get(15, TimeUnit.SECONDS)
            scope.cancel()
        }
    }

    @Test
    fun `older revisions and late terminal saves cannot replace the displayed request or its notification path`() {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val store = ReplaySettingsStore()
        val notifications = CopyOnWriteArrayList<SaveSnapshot>()
        val capture = ReplayProjectCapture(scope, store, {}, notifications::add)
        try {
            assertTrue(capture.applySettings(ReplaySettings(retentionSeconds = 60), unresolved, 0).get(15, TimeUnit.SECONDS).accepted)
            val base = capture.view.snapshot!!
            val first = base.copy(revision = base.revision + 100, save = SaveSnapshot(SavePhase.COMPLETED, "request-a", directory = directory.resolve("first"), missingKinds = listOf("video")))
            val second = first.copy(revision = first.revision + 1, save = SaveSnapshot(SavePhase.COMPLETED, "request-b", directory = directory.resolve("second")))
            capture.acceptSnapshot(first)
            capture.acceptSnapshot(second)
            capture.acceptSnapshot(first)
            capture.acceptSnapshot(second.copy(revision = second.revision + 1))
            assertEquals("request-b", capture.view.snapshot!!.save.requestId)
            assertEquals(listOf("request-a", "request-b"), notifications.map { it.requestId })
            assertEquals(directory.resolve("first"), notifications.first().directory)
            assertEquals(listOf("video"), notifications.first().missingKinds)
            capture.close()
            capture.termination.get(15, TimeUnit.SECONDS)
            capture.acceptSnapshot(first.copy(revision = second.revision + 10))
            assertEquals("request-b", capture.view.snapshot!!.save.requestId)
            assertEquals(2, notifications.size)
        } finally {
            capture.close()
            capture.termination.get(15, TimeUnit.SECONDS)
            scope.cancel()
        }
    }

    @Test
    fun `manual settings remain authoritative and clearing override restores resolved automatic application`() {
        val env = unresolved.copy(application = ApplicationSelection("com.auto.debug", "debug", null))
        val manual = ReplaySettings(appSelection = AppSelectionMode.MANUAL, manualPackage = "com.manual.app")
        assertEquals("com.manual.app", manual.toCoreSettings(env).validate().application.packageName)
        val automatic = manual.copy(appSelection = AppSelectionMode.AUTOMATIC).toCoreSettings(env).validate()
        assertEquals("com.auto.debug", automatic.application.packageName)
        assertEquals(ApplicationMode.AUTO, automatic.application.mode)
        assertNull(automatic.saveDirectory)
        assertEquals("実行対象未選択", manual.copy(appSelection = AppSelectionMode.AUTOMATIC).toCoreSettings(unresolved).application.unresolvedReason)
    }

    @Test
    fun `close completes even when the project scope was already canceled before initialization`() {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        scope.cancel()
        val capture = ReplayProjectCapture(scope, ReplaySettingsStore(), {}, {})
        capture.close()
        capture.termination.get(15, TimeUnit.SECONDS)
    }
}
