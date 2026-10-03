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
            assertTrue(capture.applySettings(store.settings(), unresolved).get(15, TimeUnit.SECONDS).accepted)
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
            val apply = capture.applySettings(next, unresolved)
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
            assertTrue(capture.applySettings(original, unresolved).get(15, TimeUnit.SECONDS).accepted)
            val changedDirectory = Files.createDirectory(directory.resolve("new-directory"))
            val changed = original.copy(retentionSeconds = 900, destination = changedDirectory.toString())
            store.apply(changed)
            Files.delete(changedDirectory)
            Files.createFile(changedDirectory)
            val result = capture.applySettings(changed, unresolved).get(15, TimeUnit.SECONDS)
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
            assertTrue(capture.applySettings(ReplaySettings(retentionSeconds = 60), unresolved).get(15, TimeUnit.SECONDS).accepted)
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
