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
    private val idle = PlayerAvailability(loading = false, error = null, playheadInGap = false)

    private fun trimAt(playheadUs: Long) = rules.enterTrim(rules.initial().copy(playheadUs = playheadUs))

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
    fun markKeysUseTheShownFrameAndEnterTrimMode() {
        val view = rules.initial().copy(playheadUs = 300_000)
        val marked = rules.markIn(view)
        assertEquals(PlayerMode.TRIM, marked.mode)
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
        // CANCEL-01: discard edits, back to view mode at the playhead from before trim mode.
        val view = rules.initial().copy(playheadUs = 1_700_000)
        var trim = rules.enterTrim(view)
        trim = rules.endDrag(rules.dragTo(rules.beginDrag(trim, Grip.OUT, 600_000), 600_000))
        trim = rules.seek(trim, 200_000)
        val (cancelled, effect) = rules.cancel(trim)
        assertEquals(PlayerMode.VIEW, cancelled.mode)
        assertEquals(rules.fullInUs to rules.fullOutUs, cancelled.inUs to cancelled.outUs)
        assertEquals(1_700_000, cancelled.playheadUs)
        assertEquals(CancelEffect.STOP_PLAYBACK, effect)
        // CANCEL-02: unchanged range just exits.
        assertEquals(PlayerMode.VIEW, rules.cancel(rules.enterTrim(view)).first.mode)
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
        assertEquals(PlayerMode.TRIM, aborting.mode)
        assertEquals(CancelEffect.NONE, rules.cancel(aborting).second)
        val afterAbort = rules.taskFinished(aborting, TaskKind.TRIM, TaskOutcome.CANCELLED)
        assertEquals(PlayerMode.TRIM, afterAbort.mode)
        assertEquals(trim.inUs to trim.outUs, afterAbort.inUs to afterAbort.outUs)
        // Success returns to view mode; a PNG keeps the mode.
        assertEquals(PlayerMode.VIEW, rules.taskFinished(running, TaskKind.TRIM, TaskOutcome.SUCCESS).mode)
        assertEquals(PlayerMode.TRIM, rules.taskFinished(rules.taskStarted(trim, TaskKind.FRAME), TaskKind.FRAME, TaskOutcome.SUCCESS).mode)
        // Handles cannot be taken while a task runs.
        assertNull(rules.beginDrag(running, Grip.IN, 0).drag)
        // View mode without a task: nothing to cancel.
        assertEquals(view to CancelEffect.NONE, rules.cancel(view))
    }

    @Test
    fun buttonsLabelsAvailabilityAndReasons() {
        val view = playerButtons(rules, rules.initial(), idle)
        assertEquals(listOf("トリム…", "この位置をPNG保存"), view.map { it.label })
        assertTrue(view.all { it.enabled })
        val trim = trimAt(0)
        val unchanged = playerButtons(rules, trim, idle)
        assertEquals(listOf("トリミング", "この位置をPNG保存", "キャンセル"), unchanged.map { it.label })
        assertEquals("範囲が動画全体のままです", unchanged[0].reason)
        assertFalse(unchanged[0].enabled)
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
        assertNull(statusLine(null, view))
    }

    @Test
    fun pngInTrimModeWaitsUntilThePlayheadIsPlaced() {
        fun png(state: TrimEditor) = playerButtons(rules, state, idle).single { it.id == PlayerButton.SAVE_FRAME }
        // View mode keeps its behaviour: playhead shown, PNG usable in the neutral style.
        val view = rules.initial().copy(playheadUs = 300_000)
        assertTrue(rules.playheadVisible(view))
        assertTrue(png(view).enabled); assertFalse(png(view).accent)
        // Entering trim mode hides the playhead and disables PNG (neutral) with a reason.
        val entered = rules.enterTrim(view)
        assertFalse(rules.playheadVisible(entered))
        assertFalse(png(entered).enabled); assertFalse(png(entered).accent)
        assertEquals("再生位置を動かすと、その位置をPNG保存できます", png(entered).reason)
        assertEquals("範囲が動画全体のままです", statusLine(null, playerButtons(rules, entered, idle))) // the main action's reason comes first
        // Handle drags do not place the playhead, even when they pull it back inside the range.
        val narrowed = rules.endDrag(rules.beginDrag(entered, Grip.IN, 500_000))
        assertEquals(500_000, narrowed.playheadUs)
        assertFalse(rules.playheadVisible(narrowed))
        assertEquals("再生位置を動かすと、その位置をPNG保存できます", statusLine(null, playerButtons(rules, narrowed, idle)))
        // Taking the playhead shows it at once (with its bubble) and enables PNG in blue.
        val holding = rules.beginDrag(narrowed, Grip.PLAYHEAD, 700_000)
        assertTrue(rules.playheadVisible(holding))
        assertNotNull(rules.bubble(holding))
        val placed = rules.endDrag(holding)
        assertTrue(rules.playheadVisible(placed))
        assertTrue(png(placed).enabled); assertTrue(png(placed).accent); assertFalse(png(placed).primary)
        // Esc during that first playhead drag puts it back to hidden.
        assertFalse(rules.playheadVisible(rules.cancel(holding).first))
        // Arrow keys and playback place it too.
        assertTrue(rules.playheadVisible(rules.step(narrowed, 1)))
        assertTrue(rules.playheadVisible(rules.played(narrowed, 600_000)))
        // Once placed, a later handle drag that pulls it inside keeps it visible.
        val pulled = rules.endDrag(rules.beginDrag(placed, Grip.IN, 800_000))
        assertEquals(800_000, pulled.playheadUs)
        assertTrue(rules.playheadVisible(pulled))
        // While a task runs the button is disabled and neutral; the line stays as it was.
        val running = rules.taskStarted(placed, TaskKind.TRIM)
        assertFalse(png(running).enabled); assertFalse(png(running).accent)
        assertTrue(rules.playheadVisible(running))
        assertTrue(rules.playheadVisible(rules.taskFinished(rules.cancel(running).first, TaskKind.TRIM, TaskOutcome.CANCELLED)))
        // Cancel returns to view mode (always visible); re-entering starts hidden again.
        val back = rules.cancel(placed).first
        assertTrue(rules.playheadVisible(back))
        assertFalse(rules.playheadVisible(rules.enterTrim(back)))
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
        assertEquals(Grip.PLAYHEAD, geometry.hitTest(rules.initial(), 2f, 12f, 6f)) // no handles in view mode
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
