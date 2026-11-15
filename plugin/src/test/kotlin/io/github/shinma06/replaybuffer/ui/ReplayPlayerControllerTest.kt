package io.github.shinma06.replaybuffer.ui

import com.google.gson.JsonParser
import com.intellij.mock.MockApplication
import com.intellij.mock.MockProject
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.util.Disposer
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.CountDownLatch
import java.util.concurrent.ExecutorService
import java.util.concurrent.TimeUnit
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

    private fun withController(test: (ReplayPlayerController) -> Unit) {
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
