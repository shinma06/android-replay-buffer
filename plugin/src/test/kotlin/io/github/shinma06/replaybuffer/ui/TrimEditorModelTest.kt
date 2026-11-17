package io.github.shinma06.replaybuffer.ui

import io.github.shinma06.replaybuffer.core.ReplayTimeline
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

class TrimEditorModelTest {
    /** Frames of 100 ms at 0..900 ms, a gap 1.0..1.5 s, then frames 1.5..2.0 s. */
    private val spans = (0 until 10).map { ReplayTimeline.Span(it * 100_000L, (it + 1) * 100_000L) } +
        (0 until 5).map { ReplayTimeline.Span(1_500_000L + it * 100_000L, 1_600_000L + it * 100_000L) }
    private val rules = TrimRules(FrameGrid(spans), 2_000_000L)
    private val idle = PlayerAvailability(loading = false, error = null, playheadInGap = false, previewReady = true)

    private fun trimAt(playheadUs: Long) = rules.initial().copy(playheadUs = playheadUs)

    @Test
    fun handlesSnapToFrameBoundariesNeverCrossAndAvoidGaps() {
        var state = rules.beginDrag(trimAt(0), Grip.IN, 349_999)
        assertEquals(300_000, state.inUs) // nearest frame start, not an I-frame
        state = rules.dragTo(state, 360_000)
        assertEquals(400_000, state.inUs)
        state = rules.dragTo(state, 1_200_000) // inside the gap: nearest start is 1.0 s? no frame starts there
        assertTrue(spans.any { it.startUs == state.inUs })
        assertTrue(state.inUs !in 1_000_000 until 1_500_000)
        state = rules.endDrag(state)

        var out = rules.beginDrag(state, Grip.OUT, 1_240_000)
        assertTrue(spans.any { it.endUs == out.outUs }) // a frame end, never inside the gap
        out = rules.dragTo(out, 0) // dragged left past In: stops one frame after In
        assertEquals(spans.first { it.startUs == out.inUs }.endUs, out.outUs)
        out = rules.endDrag(out)
        assertTrue(out.inUs < out.outUs)

        val crossIn = rules.dragTo(rules.beginDrag(out, Grip.IN, 0), 2_000_000)
        assertEquals(out.inUs, crossIn.inUs) // In cannot pass the last kept frame
    }

    @Test
    fun markKeysUseTheShownFrame() {
        val view = rules.initial().copy(playheadUs = 300_000)
        val marked = rules.markIn(view)
        assertEquals(300_000, marked.inUs)
        val out = rules.markOut(rules.seek(marked, 1_650_000))
        assertEquals(1_700_000, out.outUs)
        assertEquals(1_500_000, rules.step(rules.seek(out, 900_000), 1).playheadUs) // stepping skips the gap
    }

    @Test
    fun playheadScrubSnapsAndStaysInsideTheRangeInTrimMode() {
        var state = rules.beginDrag(trimAt(0), Grip.IN, 300_000)
        state = rules.endDrag(state)
        assertEquals(300_000, state.playheadUs) // pulled inside the new range on release
        state = rules.beginDrag(state, Grip.PLAYHEAD, 150_000)
        assertEquals(300_000, state.playheadUs)
        state = rules.dragTo(state, 555_555)
        assertEquals(500_000, state.playheadUs)
        val view = rules.seek(rules.initial(), 1_250_000) // a gap snaps to the nearest frame
        assertTrue(rules.grid.indexAt(view.playheadUs) >= 0)
    }

    @Test
    fun timeBubbleOnlyWhileSomethingIsHeld() {
        val trim = trimAt(0)
        assertNull(rules.bubble(trim))
        assertNull(rules.bubble(rules.initial()))
        val inHeld = rules.beginDrag(trim, Grip.IN, 200_000)
        assertEquals(TimeBubble(Grip.IN, 200_000), rules.bubble(inHeld))
        assertEquals("0:00.20", rules.bubble(inHeld)!!.text)
        assertEquals(200_000, rules.displayUs(inHeld)) // preview shows the In frame
        assertNull(rules.bubble(rules.endDrag(inHeld)))
        val outHeld = rules.beginDrag(trim, Grip.OUT, 800_000)
        assertEquals(Grip.OUT, rules.bubble(outHeld)!!.grip)
        assertEquals(700_000, rules.displayUs(outHeld)) // preview shows the last kept frame
        val headHeld = rules.beginDrag(rules.initial(), Grip.PLAYHEAD, 1_600_000)
        assertEquals(TimeBubble(Grip.PLAYHEAD, 1_600_000), rules.bubble(headHeld))
        assertNull(rules.bubble(rules.cancel(headHeld).first)) // Esc drops the grip too
        assertEquals("6:09.22", bubbleTime(369_229_999))
    }

    @Test
    fun cancelSpec() {
        // CANCEL-01: reset only the range, keep the current frame and PNG placement.
        val full = rules.initial().copy(playheadUs = 1_700_000)
        var trim = full
        trim = rules.endDrag(rules.dragTo(rules.beginDrag(trim, Grip.OUT, 600_000), 600_000))
        trim = rules.seek(trim, 200_000)
        val (cancelled, effect) = rules.cancel(trim)
        assertEquals(rules.fullInUs to rules.fullOutUs, cancelled.inUs to cancelled.outUs)
        assertEquals(200_000, cancelled.playheadUs)
        assertEquals(CancelEffect.STOP_PLAYBACK, effect)
        // CANCEL-02: a full range is a no-op, including repeat cancellation after reset.
        assertEquals(full to CancelEffect.NONE, rules.cancel(full))
        assertEquals(cancelled to CancelEffect.NONE, rules.cancel(cancelled))
        // CANCEL-03: Esc during a drag restores the grab-time values and stays in trim mode.
        val dragging = rules.dragTo(rules.beginDrag(trim, Grip.IN, 300_000), 400_000)
        val (restored, dragEffect) = rules.cancel(dragging)
        assertEquals(trim.copy(drag = null), restored)
        assertEquals(CancelEffect.NONE, dragEffect)
        assertEquals(restored, rules.dragTo(restored, 100_000)) // later moves of the same press do nothing
        // CANCEL-04: while a task runs Cancel becomes 中止 and asks once; the range stays.
        val running = rules.taskStarted(trim, TaskKind.TRIM)
        val (aborting, abort) = rules.cancel(running)
        assertEquals(CancelEffect.ABORT_TASK, abort)
        assertTrue(aborting.cancelling)
        assertEquals(CancelEffect.NONE, rules.cancel(aborting).second)
        val afterAbort = rules.taskFinished(aborting, TaskKind.TRIM, TaskOutcome.CANCELLED)
        assertEquals(trim.inUs to trim.outUs, afterAbort.inUs to afterAbort.outUs)
        // Success resets the range and preserves placement; a PNG preserves all editing state.
        assertEquals(cancelled, rules.taskFinished(running, TaskKind.TRIM, TaskOutcome.SUCCESS))
        assertEquals(trim, rules.taskFinished(rules.taskStarted(trim, TaskKind.FRAME), TaskKind.FRAME, TaskOutcome.SUCCESS))
        // Handles cannot be taken while a task runs.
        assertNull(rules.beginDrag(running, Grip.IN, 0).drag)
        assertTrue(playerButtons(rules, cancelled, idle).last().enabled.not())
    }

    @Test
    fun naturalCompletionRewindsNextPlayEvenWhenTheTerminalTickWasNotShown() {
        val current = TrimRules(FrameGrid(listOf(
            ReplayTimeline.Span(0, 900_000), ReplayTimeline.Span(900_000, 1_000_000),
            ReplayTimeline.Span(1_000_000, 1_000_001)
        )), 1_000_001)
        val playing = current.played(current.initial(), 900_000)
        assertEquals(900_000L to 1_000_001L, current.playRange(playing))

        val finished = current.playbackFinished(playing, 1_000_001, failed = false)

        assertEquals(1_000_000, finished.playheadUs)
        assertEquals(0L to 1_000_001L, current.playRange(finished))
        assertTrue(current.grid.indexAt(finished.playheadUs) >= 0)
    }

    @Test
    fun naturalCompletionRewindsNextPlayToTheSelectedRange() {
        val playing = trimAt(500_000).copy(inUs = 300_000, outUs = 900_000)

        val finished = rules.playbackFinished(playing, 900_000, failed = false)

        assertEquals(899_999, finished.playheadUs)
        assertEquals(300_000L to 900_000L, rules.playRange(finished))
        assertEquals(8, rules.grid.indexAt(finished.playheadUs))
        assertEquals(playing, rules.playbackFinished(playing, 900_000, failed = true))
    }

    @Test
    fun seekingOrPausingOnALongFinalFrameDoesNotRewind() {
        val spans = listOf(ReplayTimeline.Span(0, 500_000), ReplayTimeline.Span(500_000, 1_000_000),
            ReplayTimeline.Span(1_000_000, 60_000_000))
        for (shortened in listOf(false, true)) {
            val movie = if (shortened) spans + ReplayTimeline.Span(60_000_000, 61_000_000) else spans
            val current = TrimRules(FrameGrid(movie), movie.last().endUs)
            val state = if (shortened) current.initial().copy(inUs = 500_000, outUs = 60_000_000) else current.initial()
            val sought = current.seek(state, 30_000_000)
            assertEquals(1_000_000L to 60_000_000L, current.playRange(sought), "shortened=$shortened seek")
            assertEquals(1_000_000L to 60_000_000L,
                current.playRange(current.played(state, 1_000_000)), "shortened=$shortened pause")
            val finished = current.playbackFinished(sought, 60_000_000, failed = false)
            val start = if (shortened) 500_000L else 0L
            assertEquals(start to 60_000_000L, current.playRange(finished), "shortened=$shortened completed")
            assertEquals(1_000_000L to 60_000_000L,
                current.playRange(current.seek(finished, 30_000_000)), "shortened=$shortened seek after completion")
        }
    }

    @Test
    fun aOneMicrosecondFinalFrameCanBeSoughtBeforeAndAfterCompletion() {
        val current = TrimRules(FrameGrid(listOf(ReplayTimeline.Span(0, 1_000_000),
            ReplayTimeline.Span(1_000_000, 1_000_001))), 1_000_001)
        val sought = current.seek(current.initial(), 1_000_000)
        val finished = current.playbackFinished(sought, 1_000_001, failed = false)
        assertEquals(1_000_000L to 1_000_001L, current.playRange(sought))
        assertEquals(0L to 1_000_001L, current.playRange(finished))
        assertEquals(1_000_000L to 1_000_001L, current.playRange(current.seek(finished, 1_000_000)))
        assertEquals(1_000_000L to 1_000_001L, current.playRange(current.step(finished, 0)))
        assertEquals(1_000_000L to 1_000_001L,
            current.playRange(current.endDrag(current.beginDrag(finished, Grip.PLAYHEAD, 1_000_000))))
    }

    @Test
    fun cancellingDragOrTrimRestoresWhetherPlaybackHadCompleted() {
        val current = TrimRules(FrameGrid(listOf(ReplayTimeline.Span(0, 500_000),
            ReplayTimeline.Span(500_000, 1_000_000), ReplayTimeline.Span(1_000_000, 60_000_000))), 60_000_000)
        val completed = current.playbackFinished(current.initial(), 60_000_000, failed = false)
        val dragging = current.beginDrag(completed, Grip.PLAYHEAD, 500_000)
        assertEquals(500_000L to 60_000_000L, current.playRange(dragging))
        assertEquals(completed, current.cancelDrag(dragging))
        val trim = current.seek(completed, 500_000)
        assertEquals(trim to CancelEffect.NONE, current.cancel(trim))
        assertEquals(500_000L to 60_000_000L, current.playRange(current.cancel(trim).first))
        val view = current.seek(current.initial(), 500_000)
        val endedTrim = current.playbackFinished(view, 60_000_000, failed = false)
        assertEquals(0L to 60_000_000L, current.playRange(current.cancel(endedTrim).first))
    }

    @Test
    fun completingAShorterTrimDoesNotMarkTheWholeMovieCompleted() {
        val current = TrimRules(FrameGrid(listOf(ReplayTimeline.Span(0, 500_000),
            ReplayTimeline.Span(500_000, 1_000_000), ReplayTimeline.Span(1_000_000, 60_000_000))), 60_000_000)
        val trim = current.markOut(current.seek(current.initial(), 500_000))
        val finished = current.playbackFinished(trim, 1_000_000, failed = false)
        assertEquals(0L to 1_000_000L, current.playRange(finished))
        val saved = current.taskFinished(current.taskStarted(finished, TaskKind.TRIM), TaskKind.TRIM, TaskOutcome.SUCCESS)
        assertEquals(999_999L to 60_000_000L, current.playRange(saved))
        assertEquals(500_000L to 60_000_000L, current.playRange(current.step(current.cancel(finished).first, 0)))
    }

    @Test
    fun buttonsLabelsAvailabilityAndReasons() {
        val trim = trimAt(0)
        val unchanged = playerButtons(rules, trim, idle)
        assertEquals(listOf("トリミング", "この位置をPNG保存", "キャンセル"), unchanged.map { it.label })
        assertEquals("範囲が動画全体のままです", unchanged[0].reason)
        assertFalse(unchanged[0].enabled)
        assertTrue(unchanged[1].enabled)
        assertFalse(unchanged[2].enabled)
        assertEquals("範囲が動画全体のままです", statusLine(null, unchanged))
        val edited = rules.markIn(rules.seek(trim, 300_000))
        assertTrue(playerButtons(rules, edited, idle)[0].enabled)
        val running = playerButtons(rules, rules.taskStarted(edited, TaskKind.TRIM), idle)
        assertEquals(listOf("保存中…", "この位置をPNG保存", "中止"), running.map { it.label })
        assertEquals(listOf(false, false, true), running.map { it.enabled })
        val aborting = playerButtons(rules, rules.cancel(rules.taskStarted(edited, TaskKind.TRIM)).first, idle)
        assertEquals("中止しています…", aborting[2].label)
        assertFalse(aborting[2].enabled)
        val gap = playerButtons(rules, edited, idle.copy(playheadInGap = true))
        assertEquals("この位置に表示frameがありません（欠落）", gap[1].reason)
        assertEquals("この位置に表示frameがありません（欠落）", statusLine(null, gap))
        assertEquals("保存しました", statusLine("保存しました", gap))
        assertEquals("読み込み中です", playerButtons(rules, edited, idle.copy(loading = true))[0].reason)
        val pending = playerButtons(rules, edited, idle.copy(previewReady = false))
        assertTrue(pending[0].enabled)
        assertFalse(pending[1].enabled)
        assertFalse(pending[1].accent)
        assertEquals("この位置の映像を準備しています", statusLine(null, pending))
    }

    @Test
    fun fullRangePngTaskOffersAnEnabledCancelButton() {
        val running = rules.taskStarted(rules.initial(), TaskKind.FRAME)
        val buttons = playerButtons(rules, running, idle)
        assertEquals(listOf(PlayerButton.TRIM, PlayerButton.SAVE_FRAME, PlayerButton.CANCEL), buttons.map { it.id })
        assertEquals(listOf(false, false, true), buttons.map { it.enabled })
        assertEquals("中止", buttons.last().label)
        assertFalse(buttons.last().primary)
    }

    @Test
    fun fullRangePngCancellationStaysVisibleDisabledAndRequestsOnlyOnce() {
        val running = rules.taskStarted(rules.initial(), TaskKind.FRAME)
        val (cancelling, effect) = rules.cancel(running)
        assertEquals(CancelEffect.ABORT_TASK, effect)
        assertEquals(cancelling to CancelEffect.NONE, rules.cancel(cancelling))
        val buttons = playerButtons(rules, cancelling, idle)
        assertEquals(listOf(PlayerButton.TRIM, PlayerButton.SAVE_FRAME, PlayerButton.CANCEL), buttons.map { it.id })
        assertEquals("中止しています…", buttons.last().label)
        assertEquals("中止しています…", buttons.last().reason)
        assertTrue(buttons.none { it.enabled })
    }

    @Test
    fun pngCompletionKeepsRangeAndPlayheadForEveryOutcomeIncludingLateCancellation() {
        for (outcome in TaskOutcome.entries) for (cancel in listOf(false, true)) {
            val before = rules.initial().copy(inUs = 300_000, outUs = 900_000, playheadUs = 500_000)
            val running = rules.taskStarted(before, TaskKind.FRAME)
            val pending = if (cancel) rules.cancel(running).first else running
            val finished = rules.taskFinished(pending, TaskKind.FRAME, outcome)
            assertEquals(before, finished, "$outcome / cancelling=$cancel")
            val buttons = playerButtons(rules, finished, idle)
            assertEquals(listOf(PlayerButton.TRIM, PlayerButton.SAVE_FRAME, PlayerButton.CANCEL), buttons.map { it.id })
            assertTrue(buttons.all { it.enabled })
        }
    }

    @Test
    fun pngIsAvailableFromTheInitialGenuinePreviewWithoutRangeEdits() {
        fun png(state: TrimEditor, availability: PlayerAvailability = idle) =
            playerButtons(rules, state, availability).single { it.id == PlayerButton.SAVE_FRAME }
        val initial = rules.initial()
        assertTrue(png(initial).enabled)
        assertTrue(png(initial).accent)
        assertFalse(png(initial).primary)
        assertFalse(png(initial, idle.copy(loading = true)).enabled)
        assertFalse(png(initial, idle.copy(error = "読めません")).enabled)
        assertFalse(png(initial, idle.copy(playheadInGap = true)).enabled)
        val pending = png(initial, idle.copy(previewReady = false))
        assertFalse(pending.enabled)
        assertFalse(pending.accent)
        assertEquals("この位置の映像を準備しています", pending.reason)

        val sought = rules.seek(initial, 300_000)
        val narrowed = rules.endDrag(rules.beginDrag(sought, Grip.IN, 700_000))
        assertEquals(700_000, narrowed.playheadUs)
        assertFalse(png(narrowed, idle.copy(previewReady = false)).enabled)
        assertTrue(png(narrowed).enabled)
        val holding = rules.beginDrag(narrowed, Grip.PLAYHEAD, 900_000)
        assertNotNull(rules.bubble(holding))
        assertEquals(narrowed, rules.cancel(holding).first)
        assertTrue(png(rules.cancel(holding).first).enabled)

        val reset = rules.cancel(narrowed).first
        assertEquals(initial.inUs to initial.outUs, reset.inUs to reset.outUs)
        assertEquals(narrowed.playheadUs, reset.playheadUs)
        assertTrue(png(reset).enabled)
        for (kind in TaskKind.entries) {
            val running = rules.taskStarted(narrowed, kind)
            assertFalse(png(running).enabled)
            assertFalse(png(running).accent)
            for (outcome in TaskOutcome.entries) {
                val finished = rules.taskFinished(rules.cancel(running).first, kind, outcome)
                assertEquals(if (kind == TaskKind.TRIM && outcome == TaskOutcome.SUCCESS) reset else narrowed, finished)
                assertTrue(png(finished).enabled)
            }
        }
        // A newly opened player has the same full selection and immediate PNG readiness.
        assertTrue(png(rules.initial()).enabled)
    }

    @Test
    fun buttonLayoutFollowsEditorWidth() {
        assertEquals(ControlLayout.STACKED, controlLayout(640f))
        assertEquals(ControlLayout.ROW, controlLayout(639f))
        val buttons = playerButtons(rules, trimAt(0), idle)
        assertEquals(listOf("この位置をPNG保存", "キャンセル", "トリミング"), rowOrder(buttons).map { it.label })
    }

    @Test
    fun oneFixedScaleStripWithoutZoom() {
        val geometry = StripGeometry(1000f, 2_000_000)
        assertEquals(500f, geometry.x(1_000_000))
        assertEquals(1_000_000, geometry.us(500f))
        assertEquals(0f, geometry.x(-5))
        assertEquals(1000f, geometry.x(9_000_000))
        // The mapping does not depend on the trim range or what is held.
        val narrowed = rules.endDrag(rules.beginDrag(trimAt(0), Grip.IN, 400_000))
        assertEquals(geometry.us(250f), StripGeometry(1000f, 2_000_000).us(250f))
        assertEquals(Grip.IN, geometry.hitTest(narrowed, geometry.x(400_000) + 5f, 12f, 6f))
        assertEquals(Grip.OUT, geometry.hitTest(narrowed, 995f, 12f, 6f))
        assertEquals(Grip.PLAYHEAD, geometry.hitTest(narrowed, 600f, 12f, 6f))
        assertEquals(Grip.IN, geometry.hitTest(rules.initial(), 2f, 12f, 6f)) // editable from the first load
    }

    @Test
    fun narrowRangesUseRenderedHandlesAndDragWithoutJumping() {
        val geometry = StripGeometry(1000f, 900_000_000)
        for (start in listOf(0L, 450_000_000L, 899_800_000L)) {
            val state = trimAt(0).copy(inUs = start, outUs = start + 100_000)
            val range = geometry.handleRange(state, 12f)
            assertTrue(range.start >= 0 && range.endInclusive <= 1000f)
            assertEquals(Grip.IN, geometry.hitTest(state, range.start + 6f, 12f, 6f))
            assertEquals(Grip.OUT, geometry.hitTest(state, range.endInclusive - 6f, 12f, 6f))
            for (grip in listOf(Grip.IN, Grip.OUT)) {
                val pressed = if (grip == Grip.IN) range.start + 6f else range.endInclusive - 6f
                val origin = if (grip == Grip.IN) state.inUs else state.outUs
                assertEquals(origin, geometry.dragUs(state, grip, pressed, pressed))
                assertEquals((origin - 9_000_000).coerceAtLeast(0), geometry.dragUs(state, grip, pressed, pressed - 10f))
            }
        }
    }

    @Test
    fun thumbnailsCoverTheWholeMovieAndAreReused() {
        assertEquals(4, thumbnailCount(100f, 27f))
        val slots = thumbnailSlots(rules.grid, 2_000_000, 4)
        assertEquals(listOf(200_000L, 700_000L, null, 1_700_000L), slots)
        val cache = ThumbnailCache<String>()
        assertEquals(listOf(200_000L, 700_000L, 1_700_000L), cache.missing(slots))
        cache.put(200_000, "a"); cache.put(700_000, "b")
        assertEquals(listOf(1_700_000L), cache.missing(slots))
        assertEquals("a", cache[200_000])
        val wider = thumbnailSlots(rules.grid, 2_000_000, 8) + 200_000L
        assertNotNull(wider.firstOrNull())
        assertFalse(cache.missing(wider).contains(200_000L)) // a new width reuses decoded frames
    }
}
