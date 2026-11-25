package io.github.shinma06.arb.ide

import androidx.compose.foundation.text.input.setTextAndPlaceCursorAtEnd
import com.intellij.openapi.options.ConfigurationException
import io.github.shinma06.arb.ipc.ArbdIoException
import io.github.shinma06.arb.model.ArbConfig
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.io.IOException
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import kotlin.test.assertContains
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/**
 * Settings Apply is a sequence of independent writers (daemon `config.update`, then the IDE-local
 * ones). These tests pin the contract when a later writer, or the daemon itself, fails: what was
 * persisted is stated, a failed value is never shown as saved, and a second Apply retries only the
 * failed part.
 */
class ArbSettingsPartialApplyTest {
    /** A persistent "world": daemon config + IDE-local settings, with injectable writer faults. */
    private class World {
        @Volatile var daemon: ArbConfig = ArbConfig()
        @Volatile var daemonUp = true
        @Volatile var customBinary = ""
        @Volatile var screenshot: Pair<Boolean, String> = true to ""
        @Volatile var tray = false
        @Volatile var trayWriterFails = false
        @Volatile var binaryWriterFails = false
        val daemonWrites = AtomicInteger()
        val trayWrites = AtomicInteger()
        val binaryWrites = AtomicInteger()
        val restartResult = AtomicReference<Result<Unit>>(Result.success(Unit))
        val restarts = AtomicInteger()
    }

    private fun configurable(world: World) = ArbSettingsConfigurable(
        project = null,
        daemonConfigReader = {
            if (!world.daemonUp) throw IOException("arbd unreachable")
            world.daemon
        },
        daemonConfigWriter = { _, draft ->
            if (!world.daemonUp) throw ArbdIoException("config.update", IOException("connection reset"))
            world.daemonWrites.incrementAndGet()
            val updated = world.daemon.copy(saveWindowSecs = draft.saveWindowSecs)
            world.daemon = updated
            updated
        },
        localBinaryReader = { world.customBinary },
        localBinaryWriter = {
            if (world.binaryWriterFails) throw IOException("settings store locked")
            world.binaryWrites.incrementAndGet()
            world.customBinary = it
        },
        localScreenshotReader = { world.screenshot },
        localScreenshotWriter = { inherit, dir -> world.screenshot = inherit to dir },
        localTrayReader = { world.tray },
        localTrayWriter = {
            if (world.trayWriterFails) throw IOException("tray service unavailable")
            world.trayWrites.incrementAndGet()
            world.tray = it
        },
        screenshotStatusResolver = { _, _ -> "" },
        daemonRestarter = {
            world.restarts.incrementAndGet()
            world.restartResult.get().getOrThrow()
        },
        backgroundUi = object : BackgroundUiRunner {
            override fun <T> run(work: () -> T, onEdt: (T) -> Unit) = onEdt(work())
        },
    )

    @Test
    fun daemonWriterFailureSavesNothingKeepsDraftModifiedAndRetryCompletes(@TempDir tmp: Path) {
        val world = World()
        val configurable = configurable(world)
        val form = configurable.createFormModel()
        form.saveWindowText.setTextAndPlaceCursorAtEnd("600")
        form.trayEnabled = true
        val binary = tmp.resolve("arbd").toAbsolutePath().toString()
        form.customArbdPathText.setTextAndPlaceCursorAtEnd(binary)
        assertTrue(configurable.isModified)

        world.daemonUp = false // the daemon disappears while Apply runs
        val error = assertFailsWith<ConfigurationException> { configurable.apply() }
        assertContains(error.message.orEmpty(), "arbd への保存を確認できません")
        assertEquals(0, world.daemonWrites.get())
        assertEquals(0, world.trayWrites.get(), "local writers must not run after the daemon writer failed")
        assertEquals(0, world.binaryWrites.get())
        assertEquals("", world.customBinary)
        assertFalse(world.tray)
        assertTrue(configurable.isModified, "the unsaved values must still count as modified")
        assertEquals("600", form.saveWindowText.text.toString(), "typed value stays visible, flagged unsaved")

        // The daemon comes back; a plain re-Apply persists everything exactly once.
        world.daemonUp = true
        configurable.apply()
        assertEquals(600, world.daemon.saveWindowSecs)
        assertTrue(world.tray)
        assertEquals(binary, world.customBinary)
        assertEquals(1, world.daemonWrites.get())
        assertFalse(configurable.isModified)
    }

    @Test
    fun laterLocalWriterFailureNamesSavedAndUnsavedAndRetriesOnlyTheFailedOne() {
        val world = World()
        val configurable = configurable(world)
        val form = configurable.createFormModel()
        form.saveWindowText.setTextAndPlaceCursorAtEnd("450")
        form.trayEnabled = true
        world.daemon = world.daemon.copy(deviceSerial = "device-B")
        world.trayWriterFails = true

        val error = assertFailsWith<ConfigurationException> { configurable.apply() }
        val message = error.message.orEmpty()
        assertContains(message, "保存済み: arbd 設定")
        assertContains(message, "未保存: トレイアイコン")
        assertContains(message, "tray service unavailable")
        assertEquals(450, world.daemon.saveWindowSecs, "the daemon part really was persisted")
        assertFalse(world.tray, "the failed value is not persisted")
        assertTrue(configurable.isModified, "only the failed tray change keeps the page modified")
        assertEquals("device-B", form.currentDraft().deviceSerial)
        assertTrue(form.trayEnabled, "the failed value stays on screen as an unsaved edit, never as saved")

        // Retry after the fault clears: the daemon is not written again, only the failed writer runs.
        world.trayWriterFails = false
        configurable.apply()
        assertEquals(1, world.daemonWrites.get(), "already-saved daemon settings are not re-sent")
        assertTrue(world.tray)
        assertEquals(1, world.trayWrites.get())
        assertFalse(configurable.isModified)
    }

    @Test
    fun resetAfterAPartialApplyShowsThePersistedValuesNotTheFailedOnes() {
        val world = World()
        val configurable = configurable(world)
        val form = configurable.createFormModel()
        form.saveWindowText.setTextAndPlaceCursorAtEnd("450")
        form.trayEnabled = true
        world.trayWriterFails = true
        assertFailsWith<ConfigurationException> { configurable.apply() }

        configurable.reset()
        assertEquals("450", form.saveWindowText.text.toString())
        assertFalse(form.trayEnabled, "after Reset the form shows what is really persisted")
        assertFalse(configurable.isModified)
    }

    @Test
    fun partialApplyMessageStatesNothingSavedWhenOnlyLocalWritesWereAttempted() {
        val text = ArbSettingsConfigurable.partialApplyMessage(emptyList(), listOf("トレイアイコン（x）"))
        assertContains(text, "保存済み: なし")
        assertContains(text, "未保存: トレイアイコン（x）")
    }

    @Test
    fun restartKeepsDraftAndRequiresSuccessfulConfigRead() {
        val world = World()
        val configurable = configurable(world)
        val form = configurable.createFormModel()
        form.saveWindowText.setTextAndPlaceCursorAtEnd("450")
        form.trayEnabled = true
        world.daemonUp = false
        configurable.restartDaemonAndReload()
        assertFalse(form.daemonOnline)
        assertEquals(450, form.currentDraft().saveWindowSecs)
        world.daemon = world.daemon.copy(videoMaxFps = 12, deviceSerial = "device-B")
        world.daemonUp = true
        configurable.restartDaemonAndReload()
        assertTrue(form.daemonOnline)
        assertTrue(form.trayEnabled)
        assertEquals(450, form.currentDraft().saveWindowSecs)
        assertEquals(12, form.currentDraft().videoMaxFps)
        assertEquals("device-B", form.currentDraft().deviceSerial)
    }

    @Test
    fun failedRestartAfterSavingTheBinaryPathSaysThePathIsSaved(@TempDir tmp: Path) {
        val world = World()
        val configurable = configurable(world)
        val form = configurable.createFormModel()
        val binary = tmp.resolve("arbd").toAbsolutePath().toString()
        form.customArbdPathText.setTextAndPlaceCursorAtEnd(binary)
        world.restartResult.set(Result.failure(IOException("spawn failed")))

        configurable.restartDaemonAndReload()
        assertEquals(binary, world.customBinary, "the path is persisted before the restart is attempted")
        assertFalse(form.daemonOnline)
        assertContains(form.daemonBannerMessage, "spawn failed")
        assertContains(form.daemonBannerMessage, "バイナリパスの変更は保存済み")
    }

    @Test
    fun failedBinaryPathWriteIsReportedAndDoesNotRestart(@TempDir tmp: Path) {
        val world = World()
        world.binaryWriterFails = true
        val configurable = configurable(world)
        val form = configurable.createFormModel()
        form.customArbdPathText.setTextAndPlaceCursorAtEnd(tmp.resolve("arbd").toAbsolutePath().toString())
        configurable.restartDaemonAndReload()
        assertTrue(form.daemonOnline)
        assertTrue(form.isBannerVisible)
        assertContains(form.daemonBannerMessage, "保存できませんでした")
        assertContains(form.daemonBannerMessage, "settings store locked")
        assertEquals(0, world.restarts.get(), "no restart onto a path that could not be saved")
    }
}
