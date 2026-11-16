package io.github.shinma06.replaybuffer.ui

import com.google.gson.JsonParser
import com.intellij.mock.MockApplication
import com.intellij.mock.MockProject
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.util.Disposer
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path
import java.security.MessageDigest
import java.util.concurrent.CountDownLatch
import java.util.concurrent.ExecutorService
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import javax.swing.SwingUtilities
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

class ReplayPlayerControllerTest {
    @TempDir lateinit var root: Path

    @Test
    fun markingOutPausesBeforeChangingThePlaybackRange() = markWhilePlaying(out = true)

    @Test
    fun markingInPausesBeforeChangingThePlaybackRange() = markWhilePlaying(out = false)

    @Test
    fun closingTheTabLeavesTheBackgroundSaveTokenUncancelled() {
        val cancel = AtomicBoolean()
        withController { controller ->
            controller.javaClass.getDeclaredField("taskCancel").apply { isAccessible = true }.set(controller, cancel)
        }
        assertFalse(cancel.get())
    }

    @Test
    fun viewPngCancelButtonAndEscapeUseTheExistingTokenOnlyOnce() = withController { controller ->
        val token = controller.javaClass.getDeclaredField("taskCancel").apply { isAccessible = true }
        val edit = controller.javaClass.getDeclaredMethod("edit", Function2::class.java).apply { isAccessible = true }
        SwingUtilities.invokeAndWait {
            val before = controller.state.editor
            for (buttonFirst in listOf(true, false)) {
                val start: (TrimRules, TrimEditor) -> TrimEditor = { rules, _ -> rules.taskStarted(before, TaskKind.FRAME) }
                edit.invoke(controller, start)
                val cancel = AtomicBoolean()
                token.set(controller, cancel)
                if (buttonFirst) controller.button(PlayerButton.CANCEL) else controller.cancel()
                assertTrue(cancel.get())
                assertEquals(before.copy(task = TaskKind.FRAME, cancelling = true), controller.state.editor)
                val button = controller.barModel().buttons.single { it.id == PlayerButton.CANCEL }
                assertEquals("中止しています…", button.label)
                assertFalse(button.enabled)
                // Reset the probe so a second abort request would be observable.
                cancel.set(false)
                controller.button(PlayerButton.CANCEL)
                controller.cancel()
                assertFalse(cancel.get())
            }
        }
    }

    @Test
    fun returningToTheShownFrameRejectsAnObsoleteSeekResult() = withController { controller ->
        val worker = controller.javaClass.getDeclaredField("executor").apply { isAccessible = true }.get(controller) as ExecutorService
        val held = CountDownLatch(1)
        val release = CountDownLatch(1)
        worker.execute { held.countDown(); release.await() }
        assertTrue(held.await(30, TimeUnit.SECONDS))
        try {
            SwingUtilities.invokeAndWait {
                controller.step(1)
                controller.step(-1)
            }
        } finally { release.countDown() }
        worker.submit {}.get(30, TimeUnit.SECONDS)
        SwingUtilities.invokeAndWait {
            assertEquals(controller.state.editor.playheadUs, controller.state.imageUs)
        }
    }

    @Test
    fun pngButtonWaitsForTheSeekPreview() = pendingPreview(handle = false, direct = false)

    @Test
    fun directPngExportWaitsForTheSeekPreview() = pendingPreview(handle = false, direct = true)

    @Test
    fun pngButtonWaitsForTheReleasedHandlePreview() = pendingPreview(handle = true, direct = false)

    @Test
    fun directPngExportWaitsForTheReleasedHandlePreview() = pendingPreview(handle = true, direct = true)

    private fun pendingPreview(handle: Boolean, direct: Boolean) = withController { controller ->
        if (handle) {
            SwingUtilities.invokeAndWait {
                controller.button(PlayerButton.ENTER_TRIM)
                controller.step(3)
                controller.press(Grip.OUT, controller.state.timeline!!.spans[8].endUs)
            }
            awaitPreview(controller)
        }
        withPreviewBlocked(controller) {
            val shownUs = controller.state.imageUs
            if (handle) controller.release() else controller.step(3)
            assertNotNull(controller.state.image)
            assertEquals(shownUs, controller.state.imageUs)
            assertTrue(controller.state.editor.playheadUs != shownUs)
            if (direct) {
                controller.exportPng()
                assertNull(controller.state.editor.task)
            } else assertFalse(controller.barModel().buttons.single { it.id == PlayerButton.SAVE_FRAME }.enabled)
        }
        SwingUtilities.invokeAndWait {
            assertEquals(controller.state.editor.playheadUs, controller.state.imageUs)
            assertTrue(controller.barModel().buttons.single { it.id == PlayerButton.SAVE_FRAME }.enabled)
        }
    }

    @Test
    fun pngRequiresAnImageAfterSeekingOutOfAGap() = withController(gap = true) { controller ->
        pauseAtGap(controller)
        withPreviewBlocked(controller) {
            assertNull(controller.state.image)
            assertEquals("この位置に表示frameがありません（欠落）",
                controller.barModel().buttons.single { it.id == PlayerButton.SAVE_FRAME }.reason)
            controller.exportPng()
            assertNull(controller.state.editor.task)
            controller.step(1)
            assertNull(controller.state.image)
            assertFalse(controller.barModel().buttons.single { it.id == PlayerButton.SAVE_FRAME }.enabled)
            controller.exportPng()
            assertNull(controller.state.editor.task)
        }
    }

    @Test
    fun naturalCompletionKeepsPngEnabledForTheSameFinalFrame() = withController { controller ->
        // The fixture ends with a 1 us terminal tick; use the last held frame as trim Out.
        val last = controller.state.timeline!!.spans.last { it.endUs - it.startUs > 1 }
        editController(controller) { rules, state ->
            rules.played(rules.enterTrim(state).copy(outUs = last.endUs), last.startUs)
        }
        awaitPreview(controller)
        withPreviewBlocked(controller) {
            editController(controller) { rules, state -> rules.playbackFinished(state, last.endUs, failed = false) }
            assertEquals(last.startUs, controller.state.imageUs)
            assertEquals(last.endUs - 1, controller.state.editor.playheadUs)
            assertTrue(controller.state.imageUs != controller.state.editor.playheadUs)
            assertTrue(controller.barModel().buttons.single { it.id == PlayerButton.SAVE_FRAME }.enabled)
        }
    }

    private fun editController(controller: ReplayPlayerController, transform: (TrimRules, TrimEditor) -> TrimEditor) {
        val edit = controller.javaClass.getDeclaredMethod("edit", Function2::class.java).apply { isAccessible = true }
        if (SwingUtilities.isEventDispatchThread()) edit.invoke(controller, transform)
        else SwingUtilities.invokeAndWait { edit.invoke(controller, transform) }
    }

    private fun awaitPreview(controller: ReplayPlayerController) {
        val worker = controller.javaClass.getDeclaredField("executor").apply { isAccessible = true }.get(controller) as ExecutorService
        worker.submit {}.get(30, TimeUnit.SECONDS)
        SwingUtilities.invokeAndWait {}
    }

    private fun withPreviewBlocked(controller: ReplayPlayerController, action: () -> Unit) {
        val worker = controller.javaClass.getDeclaredField("executor").apply { isAccessible = true }.get(controller) as ExecutorService
        val held = CountDownLatch(1)
        val release = CountDownLatch(1)
        worker.execute { held.countDown(); release.await() }
        assertTrue(held.await(30, TimeUnit.SECONDS))
        try { SwingUtilities.invokeAndWait(action) }
        finally { release.countDown() }
        awaitPreview(controller)
    }

    @Test
    fun cancellingTrimBackToAVideoGapClearsTheOtherTimesImage() = withController(gap = true) { controller ->
        val gapUs = pauseAtGap(controller)
        SwingUtilities.invokeAndWait { controller.button(PlayerButton.ENTER_TRIM) }
        val worker = controller.javaClass.getDeclaredField("executor").apply { isAccessible = true }.get(controller) as ExecutorService
        worker.submit {}.get(30, TimeUnit.SECONDS)
        SwingUtilities.invokeAndWait {
            assertNotNull(controller.state.image)
            controller.cancel()
            assertEquals(gapUs, controller.state.editor.playheadUs)
            assertNull(controller.state.image)
            assertEquals(-1L, controller.state.imageUs)
        }
    }

    @Test
    fun aPendingTrimPreviewCannotRestoreAnImageAfterReturningToAGap() = withController(gap = true) { controller ->
        val gapUs = pauseAtGap(controller)
        val worker = controller.javaClass.getDeclaredField("executor").apply { isAccessible = true }.get(controller) as ExecutorService
        val held = CountDownLatch(1)
        val release = CountDownLatch(1)
        worker.execute { held.countDown(); release.await() }
        assertTrue(held.await(30, TimeUnit.SECONDS))
        try {
            SwingUtilities.invokeAndWait {
                controller.button(PlayerButton.ENTER_TRIM)
                controller.cancel()
            }
        } finally { release.countDown() }
        worker.submit {}.get(30, TimeUnit.SECONDS)
        SwingUtilities.invokeAndWait {
            assertEquals(gapUs, controller.state.editor.playheadUs)
            assertNull(controller.state.image)
            assertEquals(-1L, controller.state.imageUs)
        }
    }

    private fun pauseAtGap(controller: ReplayPlayerController): Long {
        val gapUs = controller.state.timeline!!.spans.zipWithNext().first { (a, b) -> a.endUs < b.startUs }.first.endUs
        val edit = controller.javaClass.getDeclaredMethod("edit", Function2::class.java).apply { isAccessible = true }
        val pause: (TrimRules, TrimEditor) -> TrimEditor = { rules, state -> rules.played(state, gapUs) }
        SwingUtilities.invokeAndWait { edit.invoke(controller, pause) }
        return gapUs
    }

    private fun markWhilePlaying(out: Boolean) = withController { controller ->
        SwingUtilities.invokeAndWait {
            controller.step(3)
            val position = controller.state.editor.playheadUs
            val lastFrame = controller.state.timeline!!.spans.first { it.startUs == position }
            controller.togglePlay()
            assertTrue(controller.state.playing)
            if (out) controller.markOut() else controller.markIn()
            assertFalse(controller.state.playing)
            assertEquals(PlayerMode.TRIM, controller.state.editor.mode)
            assertEquals(position, controller.state.editor.playheadUs)
            if (out) assertEquals(lastFrame.endUs, controller.state.editor.outUs)
            else assertEquals(position, controller.state.editor.inUs)
            controller.togglePlay()
            assertTrue(controller.state.playing)
            controller.togglePlay()
        }
    }

    private fun withController(gap: Boolean = false, test: (ReplayPlayerController) -> Unit) {
        val folder = Files.createDirectory(root.resolve("replay-20240101-000000-manual-feedface"))
        for (name in listOf("README.md", "replay.json", "video.mp4", "video-source.h264", "video-frames.jsonl",
            "logcat-device.jsonl", "logcat-app.jsonl", "touches.jsonl")) {
            assertNotNull(javaClass.getResourceAsStream("/replay-trim-parity/${folder.fileName}/$name")).use {
                Files.copy(it, folder.resolve(name))
            }
        }
        val manifest = folder.resolve("replay.json")
        val record = JsonParser.parseString(Files.readString(manifest)).asJsonObject
        record.remove("audio_track")
        if (gap) {
            val frames = folder.resolve("video-frames.jsonl")
            val rows = Files.readAllLines(frames).map { JsonParser.parseString(it).asJsonObject }
            rows[rows.size / 2].apply {
                addProperty("presented", false)
                addProperty("preroll", true)
                add("movie_pts_us", com.google.gson.JsonNull.INSTANCE)
                add("movie_end_us", com.google.gson.JsonNull.INSTANCE)
            }
            Files.writeString(frames, rows.joinToString("\n", postfix = "\n"))
            val hash = MessageDigest.getInstance("SHA-256").digest(Files.readAllBytes(frames)).joinToString("") { "%02x".format(it) }
            record.getAsJsonObject("files_sha256").addProperty("video-frames.jsonl", hash)
        }
        Files.writeString(manifest, record.toString())
        val lifetime = Disposer.newDisposable()
        ApplicationManager.setApplication(MockApplication(lifetime), lifetime)
        val controller = ReplayPlayerController(MockProject(null, lifetime), folder)
        try {
            SwingUtilities.invokeAndWait {}
            val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(30)
            while (true) {
                var ready = false
                SwingUtilities.invokeAndWait {
                    assertNull(controller.state.error)
                    ready = !controller.state.loading && controller.state.image != null
                }
                if (ready) break
                check(System.nanoTime() < deadline) { "Replay did not load" }
                Thread.sleep(10)
            }
            test(controller)
        } finally {
            SwingUtilities.invokeAndWait { controller.dispose() }
            Disposer.dispose(lifetime)
        }
    }
}
