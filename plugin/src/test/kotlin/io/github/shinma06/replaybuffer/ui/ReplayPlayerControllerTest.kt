package io.github.shinma06.replaybuffer.ui

import com.google.gson.JsonParser
import com.intellij.mock.MockApplication
import com.intellij.mock.MockProject
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.util.Disposer
import io.github.shinma06.replaybuffer.core.ReplayFiles
import io.github.shinma06.replaybuffer.core.ReplayFrameCache
import io.github.shinma06.replaybuffer.core.ReplayFrameCursor
import io.github.shinma06.replaybuffer.core.ReplayVideoDecoder
import io.github.shinma06.replaybuffer.core.previewKeyframeUs
import io.github.shinma06.replaybuffer.core.SaveOutputFile
import io.github.shinma06.replaybuffer.core.SavedReplay
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path
import java.security.MessageDigest
import java.util.concurrent.CountDownLatch
import java.util.concurrent.ExecutorService
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicReference
import javax.swing.SwingUtilities
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

class ReplayPlayerControllerTest {
    @TempDir lateinit var root: Path

    @Test fun ordinaryOpenStillStartsInViewMode() = withController { controller ->
        assertEquals(PlayerMode.VIEW, controller.state.editor.mode)
        assertFalse(controller.state.playing)
    }

    @Test fun saveAndTrimStartsWithEditableFullRangeWithoutASecondToggle() = withController(startInTrim = true) { controller ->
        assertEquals(PlayerMode.TRIM, controller.state.editor.mode)
        assertFalse(controller.state.playing)
        val rules = controller.barModel().rules!!
        assertEquals(rules.fullInUs, controller.state.editor.inUs)
        assertEquals(rules.fullOutUs, controller.state.editor.outUs)
        SwingUtilities.invokeAndWait { controller.markIn(); controller.step(2); controller.markOut() }
        assertFalse(rules.rangeUnchanged(controller.state.editor))
        assertTrue(controller.barModel().buttons.single { it.id == PlayerButton.TRIM }.enabled)
        SwingUtilities.invokeAndWait { controller.cancel() }
        assertEquals(PlayerMode.VIEW, controller.state.editor.mode)
        // Cancel exits editing; the initial request must not force subsequent state back to TRIM.
        awaitPreview(controller)
        assertEquals(PlayerMode.VIEW, controller.state.editor.mode)
    }

    @Test fun trimRequestCannotBypassValidationOrPublishAfterDisposal() {
        val folder = replayFolder()
        val validManifest = Files.readString(folder.resolve(ReplayFiles.RECORD))
        for (dispose in listOf(false, true)) {
            Files.writeString(folder.resolve(ReplayFiles.RECORD), if (dispose) validManifest else "{}")
            val lifetime = Disposer.newDisposable()
            ApplicationManager.setApplication(MockApplication(lifetime), lifetime)
            lateinit var controller: ReplayPlayerController
            try {
                SwingUtilities.invokeAndWait {
                    controller = ReplayPlayerController(MockProject(null, lifetime), folder)
                    controller.enterTrim()
                    if (dispose) controller.dispose()
                }
                val worker = controller.javaClass.getDeclaredField("executor").apply { isAccessible = true }.get(controller) as ExecutorService
                if (dispose) assertTrue(worker.awaitTermination(30, TimeUnit.SECONDS))
                else worker.submit {}.get(30, TimeUnit.SECONDS)
                SwingUtilities.invokeAndWait {
                    assertEquals(PlayerMode.VIEW, controller.state.editor.mode)
                    if (dispose) assertTrue(controller.state.loading)
                    else assertNotNull(controller.state.error)
                    controller.enterTrim()
                    assertEquals(PlayerMode.VIEW, controller.state.editor.mode)
                }
            } finally {
                if (!dispose) SwingUtilities.invokeAndWait { controller.dispose() }
                Disposer.dispose(lifetime)
            }
        }
    }

    @Test fun scrubbingShowsOnlyIdrAndReleaseRequestsTheExactFrame() {
        val requests = java.util.Collections.synchronizedList(mutableListOf<Long>())
        val keys = java.util.Collections.synchronizedSet(mutableSetOf<Long>())
        withController(decoderFactory = { replay, cache ->
            replay.frames.filter { it["output_key"]?.asBoolean == true }.forEach {
                it["movie_pts_us"]?.takeIf { it.isJsonPrimitive }?.asLong?.let(keys::add)
            }
            val real = ReplayFrameCursor(replay, cache = cache)
            object : ReplayVideoDecoder {
                override fun seek(atUs: Long, checkActive: () -> Unit): ReplayFrameCursor.Decoded {
                    requests += atUs; return real.seek(atUs, checkActive)
                }
                override fun next(checkActive: () -> Unit) = real.next(checkActive)
                override fun close() = real.close()
            }
        }) { controller ->
            drain(controller)
            requests.clear()
            var exact = 0L
            SwingUtilities.invokeAndWait {
                exact = controller.state.timeline!!.spans.map { it.startUs }.first { it !in keys }
                controller.press(Grip.PLAYHEAD, exact)
            }
            drain(controller)
            assertTrue(requests.all { it in keys })
            SwingUtilities.invokeAndWait {
                assertTrue(controller.state.imageUs in keys || controller.state.image == null)
                controller.release()
            }
            drain(controller)
            assertTrue(exact in requests)
            SwingUtilities.invokeAndWait { assertEquals(exact, controller.state.imageUs) }
        }
    }

    @Test fun staleSeekYieldsToTheNewestExactPositionAndCannotPublishAfterIt() {
        val entered = CountDownLatch(1)
        val allowCheck = CountDownLatch(1)
        val cancelled = AtomicBoolean()
        val block = AtomicBoolean()
        withController(decoderFactory = { replay, cache ->
            val real = ReplayFrameCursor(replay, cache = cache)
            object : ReplayVideoDecoder {
                override fun seek(atUs: Long, checkActive: () -> Unit): ReplayFrameCursor.Decoded {
                    if (block.compareAndSet(true, false)) {
                        entered.countDown(); assertTrue(allowCheck.await(10, TimeUnit.SECONDS))
                        try { checkActive() } catch (failure: RuntimeException) { cancelled.set(true); throw failure }
                    }
                    return real.seek(atUs, checkActive)
                }
                override fun next(checkActive: () -> Unit) = real.next(checkActive)
                override fun close() = real.close()
            }
        }) { controller ->
            drain(controller); block.set(true)
            try {
                SwingUtilities.invokeAndWait { controller.step(2) }
                assertTrue(entered.await(10, TimeUnit.SECONDS))
                SwingUtilities.invokeAndWait { controller.step(3) }
                allowCheck.countDown()
                drain(controller)
                assertTrue(cancelled.get())
                SwingUtilities.invokeAndWait { assertEquals(controller.state.editor.playheadUs, controller.state.imageUs) }
            } finally { allowCheck.countDown() }
        }
    }

    private fun drain(controller: ReplayPlayerController) {
        val worker = controller.javaClass.getDeclaredField("executor").apply { isAccessible = true }.get(controller) as ExecutorService
        worker.submit {}.get(30, TimeUnit.SECONDS)
        SwingUtilities.invokeAndWait {}
    }

    @Test
    fun timelineFailureBeforeTransferClosesTheReplayPinsChildAndParent() {
        val folder = replayFolder()
        fun bytes() = Files.list(folder).use { paths ->
            paths.toList().associate { it.fileName.toString() to Files.readAllBytes(it).toList() }
        }
        val before = bytes()
        val lifetime = Disposer.newDisposable()
        ApplicationManager.setApplication(MockApplication(lifetime), lifetime)
        val project = MockProject(null, lifetime)
        val opened = AtomicReference<SavedReplay>()
        val controller = ReplayPlayerController(project, folder, openReplay = { path ->
            SavedReplay.open(path).also { replay ->
                opened.set(replay)
                // Fail the real timeline() after factory success, without changing the source files.
                replay.manifest.add("parts", com.google.gson.JsonArray().apply { add("timeline-boundary") })
            }
        })
        fun drain(controller: ReplayPlayerController) {
            val worker = controller.javaClass.getDeclaredField("executor").apply { isAccessible = true }
                .get(controller) as ExecutorService
            worker.submit {}.get(30, TimeUnit.SECONDS)
            SwingUtilities.invokeAndWait {}
        }
        try {
            drain(controller)
            val replay = assertNotNull(opened.get())
            @Suppress("UNCHECKED_CAST")
            val pins = SavedReplay::class.java.getDeclaredField("pins").apply { isAccessible = true }
                .get(replay) as Map<String, SaveOutputFile>
            assertTrue(ReplayFiles.RECORD in pins)
            assertTrue(pins.size > 1)
            assertTrue(pins.values.all { it.isClosed }, "pins must close before controller.dispose")
            assertFailsWith<IllegalStateException> { replay.entries() } // Input child closed.
            assertFailsWith<IllegalStateException> { replay.parent.entries() }
            SwingUtilities.invokeAndWait {
                assertFalse(controller.state.loading)
                assertTrue(assertNotNull(controller.state.error).contains("timeline-boundary"))
                assertNull(controller.state.timeline)
                assertNull(controller.javaClass.getDeclaredField("saved").apply { isAccessible = true }.get(controller))
            }
            assertEquals(before, bytes())
            val next = ReplayPlayerController(project, folder)
            try {
                drain(next)
                drain(next) // Drain the preview queued by the initial EDT publish.
                SwingUtilities.invokeAndWait {
                    assertFalse(next.state.loading)
                    assertNull(next.state.error)
                    assertNotNull(next.state.timeline)
                    assertNotNull(next.state.image)
                }
            } finally { SwingUtilities.invokeAndWait { next.dispose() } }
            assertEquals(before, bytes())
        } finally {
            SwingUtilities.invokeAndWait { controller.dispose() }
            opened.get()?.close() // Also release the intentionally leaking old-red variant.
            Disposer.dispose(lifetime)
        }
    }

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

    @Test
    fun pausingAHeldFrameResumesFromThePlaybackClock() = pauseWithinFrame(trim = false)

    @Test
    fun pausingTheFinalHeldFrameResumesInsideThatFrame() = pauseWithinFrame(trim = true)

    @Test
    fun pausingAnExpiredClockStaysInsideTheRange() = pauseWithinFrame(trim = true, overrun = true)

    private fun pauseWithinFrame(trim: Boolean, overrun: Boolean = false) = withController { controller ->
        val frame = if (trim) controller.state.timeline!!.spans.last { it.endUs - it.startUs > 1 }
            else controller.state.timeline!!.spans[3]
        editController(controller) { rules, state ->
            val range = if (trim) rules.enterTrim(state).copy(outUs = frame.endUs) else state
            rules.played(range, frame.startUs)
        }
        awaitPreview(controller)
        val until = if (trim) frame.endUs else controller.state.timeline!!.spans.last().endUs
        val clockUs = if (overrun) until + 50_000 else frame.startUs + (frame.endUs - frame.startUs) / 2
        val expected = minOf(clockUs, until - 1)
        withClockedPlayback(controller, frame.startUs, until, clockUs) {
            withPreviewBlocked(controller) {
                controller.togglePlay()
                assertFalse(controller.state.playing)
                assertEquals(expected, controller.state.editor.playheadUs)
                assertNull(controller.state.editor.completedUntilUs)
                assertEquals(frame.startUs, controller.state.imageUs)
                assertTrue(controller.barModel().buttons.single { it.id == PlayerButton.SAVE_FRAME }.enabled)
                controller.togglePlay()
                assertTrue(controller.state.playing)
                assertEquals(expected, sessionPosition(playback(controller), "fromUs"))
                assertEquals(until, sessionPosition(playback(controller), "untilUs"))
                controller.togglePlay()
            }
        }
    }

    @Test
    fun naturalViewCompletionKeepsItsMarkerAndRestartsAtTheBeginning() = finishPlayback(trim = false)

    @Test
    fun naturalTrimCompletionKeepsItsMarkerAndRestartsAtIn() = finishPlayback(trim = true)

    private fun finishPlayback(trim: Boolean) = withController { controller ->
        editController(controller) { rules, state ->
            if (trim) rules.played(rules.enterTrim(state).copy(inUs = 300_000, outUs = 900_000), 500_000)
            else rules.played(state, 500_000)
        }
        withPreviewBlocked(controller) {
            controller.togglePlay()
            val session = playback(controller)
            val until = sessionPosition(session, "untilUs")
            session.stop()
            @Suppress("UNCHECKED_CAST")
            val finished = session.javaClass.getDeclaredField("onFinished").apply { isAccessible = true }
                .get(session) as (String?) -> Unit
            finished(null)
            assertFalse(controller.state.playing)
            assertEquals(until - 1, controller.state.editor.playheadUs)
            assertEquals(until, controller.state.editor.completedUntilUs)
            controller.togglePlay()
            val restarted = playback(controller)
            assertEquals(if (trim) 300_000L else 0L, sessionPosition(restarted, "fromUs"))
            // A callback queued by the old session must not finish or rewind its replacement.
            val before = controller.state.editor
            finished(null)
            assertEquals(before, controller.state.editor)
            assertTrue(controller.state.playing)
            assertTrue(playback(controller) === restarted)
            controller.togglePlay()
        }
    }

    @Test
    fun cancellingTrimWhilePlayingRestoresEntryInsteadOfThePlaybackClock() = withController { controller ->
        editController(controller) { rules, state ->
            rules.played(rules.enterTrim(rules.played(state, 100_000)), 300_000)
        }
        withClockedPlayback(controller, 300_000, controller.state.editor.outUs, 650_000) {
            SwingUtilities.invokeAndWait {
                controller.cancel()
                assertFalse(controller.state.playing)
                assertEquals(PlayerMode.VIEW, controller.state.editor.mode)
                assertEquals(100_000, controller.state.editor.playheadUs)
            }
        }
    }

    /** A real session clock, held in onFrame so time advances only through the injected nanoTime. */
    private fun withClockedPlayback(controller: ReplayPlayerController, from: Long, until: Long, clockUs: Long,
        action: () -> Unit) {
        val nanos = AtomicLong()
        val shown = CountDownLatch(1)
        val release = CountDownLatch(1)
        val worker = AtomicReference<Thread>()
        val session = ReplayPlaybackSession(controller.folder, from, until,
            onFrame = { worker.set(Thread.currentThread()); shown.countDown(); release.await() },
            onFinished = { error("Held playback unexpectedly finished: $it") },
            onAudioUnavailable = { error(it) }, nanoTime = nanos::get,
            awaitAudio = { assertTrue(it.await(30, TimeUnit.SECONDS)); true })
        try {
            session.start()
            assertTrue(shown.await(30, TimeUnit.SECONDS))
            nanos.set((clockUs - from) * 1000)
            assertEquals(clockUs, session.nowUs())
            SwingUtilities.invokeAndWait {
                controller.javaClass.getDeclaredField("playback").apply { isAccessible = true }.set(controller, session)
                controller.javaClass.getDeclaredMethod("setState", PlayerState::class.java).apply { isAccessible = true }
                    .invoke(controller, controller.state.copy(playing = true))
            }
            action()
        } finally {
            session.stop()
            release.countDown()
            worker.get()?.let { it.join(30_000); assertFalse(it.isAlive) }
        }
    }

    private fun playback(controller: ReplayPlayerController): ReplayPlaybackSession =
        controller.javaClass.getDeclaredField("playback").apply { isAccessible = true }.get(controller) as ReplayPlaybackSession

    private fun sessionPosition(session: ReplayPlaybackSession, field: String): Long =
        session.javaClass.getDeclaredField(field).apply { isAccessible = true }.getLong(session)

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

    private fun replayFolder(gap: Boolean = false): Path {
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
        return folder
    }

    private fun withController(gap: Boolean = false, startInTrim: Boolean = false,
        decoderFactory: ((SavedReplay, ReplayFrameCache) -> ReplayVideoDecoder)? = null,
        test: (ReplayPlayerController) -> Unit) {
        val folder = replayFolder(gap)
        val lifetime = Disposer.newDisposable()
        ApplicationManager.setApplication(MockApplication(lifetime), lifetime)
        lateinit var controller: ReplayPlayerController
        SwingUtilities.invokeAndWait {
            controller = if (decoderFactory == null) ReplayPlayerController(MockProject(null, lifetime), folder)
                else ReplayPlayerController(MockProject(null, lifetime), folder, decoderFactory = decoderFactory)
            if (startInTrim) {
                assertTrue(controller.state.loading)
                controller.enterTrim()
            }
        }
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
