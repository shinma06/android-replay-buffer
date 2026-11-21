package io.github.shinma06.replaybuffer.ui

import io.github.shinma06.replaybuffer.core.ReplayTimeline
import java.util.Locale
import kotlin.math.abs
import kotlin.math.ceil

/** What the pointer holds on the filmstrip. */
internal enum class Grip { IN, OUT, PLAYHEAD }

/** Background work started from the player. */
internal enum class TaskKind { TRIM, FRAME }

internal enum class TaskOutcome { SUCCESS, CANCELLED, FAILED }

/** Values captured when a grip was taken, restored by Esc while still dragging (CANCEL-03). */
internal data class TrimDrag(val grip: Grip, val inUs: Long, val outUs: Long, val playheadUs: Long,
    val completedUntilUs: Long?)

/** UI state of the trim player. All transitions live in [TrimRules]; files are never touched here. */
internal data class TrimEditor(
    val inUs: Long = 0,
    val outUs: Long = 0,
    val playheadUs: Long = 0,
    val drag: TrimDrag? = null,
    val task: TaskKind? = null,
    val cancelling: Boolean = false,
    /** Natural completion is distinct from seeking to the last (possibly long-held) frame. */
    val completedUntilUs: Long? = null,
)

/** Effect the controller must run after [TrimRules.cancel]. */
internal enum class CancelEffect { NONE, ABORT_TASK, STOP_PLAYBACK }

/** The time bubble: shown only while something is held (AC-15). */
internal data class TimeBubble(val grip: Grip, val us: Long) {
    val text: String get() = bubbleTime(us)
}

/** `m:ss.cc` with hundredths truncated, as in QuickTime's bubble. */
internal fun bubbleTime(us: Long): String {
    val centis = us.coerceAtLeast(0) / 10_000
    return String.format(Locale.ROOT, "%d:%02d.%02d", centis / 6000, centis / 100 % 60, centis % 100)
}

/** Presented frame spans in movie order, used to snap every position to a frame boundary (TRIM-M01). */
internal class FrameGrid(spans: List<ReplayTimeline.Span>) {
    val spans: List<ReplayTimeline.Span> = spans.sortedBy { it.startUs }
    private val starts = LongArray(this.spans.size) { this.spans[it].startUs }
    private val startBoundaries = this.spans.indices.fold(mutableListOf<Pair<Long, Int>>()) { values, index ->
        val startUs = this.spans[index].startUs
        if (values.lastOrNull()?.first != startUs) values += startUs to index
        values
    }
    private val startBoundaryTimes = LongArray(startBoundaries.size) { startBoundaries[it].first }
    private val startBoundarySpans = IntArray(startBoundaries.size) { startBoundaries[it].second }
    private val endBoundaries = this.spans.indices.sortedWith(
        compareBy<Int> { this.spans[it].endUs }.thenBy { it }).fold(mutableListOf<Pair<Long, Int>>()) { values, index ->
        val endUs = this.spans[index].endUs
        if (values.lastOrNull()?.first != endUs) values += endUs to index
        values
    }
    private val endBoundaryTimes = LongArray(endBoundaries.size) { endBoundaries[it].first }
    private val endBoundarySpans = IntArray(endBoundaries.size) { endBoundaries[it].second }
    private val lastIndexByEnd = HashMap<Long, Int>().apply {
        this@FrameGrid.spans.forEachIndexed { index, span -> put(span.endUs, index) }
    }

    val isEmpty: Boolean get() = spans.isEmpty()

    /** Index of the span shown at [us], or -1 in a gap. */
    fun indexAt(us: Long): Int {
        var low = 0
        var high = spans.lastIndex
        while (low <= high) {
            val mid = (low + high) ushr 1
            val span = spans[mid]
            when {
                us < span.startUs -> high = mid - 1
                us >= span.endUs -> low = mid + 1
                else -> return mid
            }
        }
        return -1
    }

    /** The span shown at [us], or the nearest one when [us] is in a gap. */
    fun nearestIndex(us: Long): Int {
        indexAt(us).takeIf { it >= 0 }?.let { return it }
        if (isEmpty) return -1
        val next = lowerBound(starts, us)
        if (next == 0) return 0
        if (next == spans.size) return spans.lastIndex
        val previous = next - 1
        val beforeDistance = us - spans[previous].endUs + 1
        val afterDistance = spans[next].startUs - us
        return if (beforeDistance <= afterDistance) previous else next
    }

    /** In an interior gap, the first step selects the adjacent frame in that direction. */
    fun stepIndex(us: Long, delta: Int): Int {
        if (isEmpty) return -1
        var current = indexAt(us)
        if (current < 0) {
            val next = upperBound(starts, us)
            current = when {
                next in 1 until spans.size && delta > 0 -> next - 1
                next in 1 until spans.size && delta < 0 -> next
                else -> nearestIndex(us) // Keep before-first, terminal and zero-delta behaviour.
            }
        }
        return (current + delta).coerceIn(0, spans.lastIndex)
    }

    /** Span whose start boundary is closest to [us]. */
    fun nearestStartIndex(us: Long): Int = nearestBoundary(startBoundaryTimes, startBoundarySpans, us)

    /** Span whose end boundary is closest to [us]. */
    fun nearestEndIndex(us: Long): Int = nearestBoundary(endBoundaryTimes, endBoundarySpans, us)

    /** Index of the span that starts at [startUs] (an In boundary). */
    fun indexStartingAt(startUs: Long): Int = lowerBound(starts, startUs).takeIf { it < starts.size && starts[it] == startUs } ?: -1

    /** Index of the span that ends at [endUs] (an Out boundary). */
    fun indexEndingAt(endUs: Long): Int = lastIndexByEnd[endUs] ?: -1

    private fun nearestBoundary(times: LongArray, indices: IntArray, us: Long): Int {
        if (times.isEmpty()) return -1
        val next = lowerBound(times, us)
        if (next == 0) return indices[0]
        if (next == times.size) return indices.last()
        val before = next - 1
        val beforeDistance = us - times[before]
        val afterDistance = times[next] - us
        return if (beforeDistance <= afterDistance) indices[before] else indices[next]
    }

    private fun lowerBound(values: LongArray, target: Long): Int {
        var low = 0
        var high = values.size
        while (low < high) {
            val mid = (low + high) ushr 1
            if (values[mid] < target) low = mid + 1 else high = mid
        }
        return low
    }

    private fun upperBound(values: LongArray, target: Long): Int {
        var low = 0
        var high = values.size
        while (low < high) {
            val mid = (low + high) ushr 1
            if (values[mid] <= target) low = mid + 1 else high = mid
        }
        return low
    }
}

/**
 * Pure transitions of the trim player (drag, snap, marks, cancel, task lifecycle). The controller
 * owns decoding, playback and background tasks and only applies these results.
 */
internal class TrimRules(val grid: FrameGrid, val durationUs: Long) {
    val fullInUs: Long get() = grid.spans.firstOrNull()?.startUs ?: 0
    val fullOutUs: Long get() = grid.spans.lastOrNull()?.endUs ?: durationUs

    fun initial(): TrimEditor = TrimEditor(inUs = fullInUs, outUs = fullOutUs, playheadUs = fullInUs)

    fun rangeUnchanged(state: TrimEditor): Boolean = state.inUs == fullInUs && state.outUs == fullOutUs

    private fun inIndex(state: TrimEditor) = grid.indexStartingAt(state.inUs).takeIf { it >= 0 } ?: 0
    private fun outIndex(state: TrimEditor) = grid.indexEndingAt(state.outUs).takeIf { it >= 0 } ?: grid.spans.lastIndex

    /** Clamps a playhead to a frame start inside the selected range. */
    private fun clampPlayhead(state: TrimEditor, us: Long): Long {
        if (grid.isEmpty) return 0
        val index = grid.nearestIndex(us).coerceIn(inIndex(state), outIndex(state))
        return grid.spans[index].startUs
    }

    /** `I`: the frame shown at the playhead becomes In. */
    fun markIn(state: TrimEditor): TrimEditor {
        if (grid.isEmpty || state.task != null || state.drag != null) return state
        val index = grid.nearestIndex(state.playheadUs).coerceAtMost(outIndex(state))
        return state.copy(inUs = grid.spans[index].startUs)
    }

    /** `O`: the frame shown at the playhead becomes the last kept frame. */
    fun markOut(state: TrimEditor): TrimEditor {
        if (grid.isEmpty || state.task != null || state.drag != null) return state
        val index = grid.nearestIndex(state.playheadUs).coerceAtLeast(inIndex(state))
        return state.copy(outUs = grid.spans[index].endUs)
    }

    fun beginDrag(state: TrimEditor, grip: Grip, us: Long): TrimEditor {
        if (grid.isEmpty || state.drag != null) return state
        if (grip != Grip.PLAYHEAD && state.task != null) return state
        val taken = state.copy(drag = TrimDrag(grip, state.inUs, state.outUs, state.playheadUs, state.completedUntilUs))
        return dragTo(taken, us)
    }

    /** Moves the held element to the frame boundary nearest [us]; In and Out never cross, one frame stays. */
    fun dragTo(state: TrimEditor, us: Long): TrimEditor {
        val drag = state.drag ?: return state
        return when (drag.grip) {
            Grip.IN -> state.copy(inUs = grid.spans[grid.nearestStartIndex(us).coerceAtMost(outIndex(state))].startUs)
            Grip.OUT -> state.copy(outUs = grid.spans[grid.nearestEndIndex(us).coerceAtLeast(inIndex(state))].endUs)
            Grip.PLAYHEAD -> state.copy(playheadUs = clampPlayhead(state, us), completedUntilUs = null)
        }
    }

    /** Pointer released: the bubble goes away and the playhead is pulled back inside the range. */
    fun endDrag(state: TrimEditor): TrimEditor {
        if (state.drag == null) return state
        val released = state.copy(drag = null)
        return released.copy(playheadUs = clampPlayhead(released, released.playheadUs))
    }

    /** Esc while dragging: restore the values from before the grab and drop the grip (CANCEL-03). */
    fun cancelDrag(state: TrimEditor): TrimEditor {
        val drag = state.drag ?: return state
        return state.copy(inUs = drag.inUs, outUs = drag.outUs, playheadUs = drag.playheadUs,
            completedUntilUs = drag.completedUntilUs, drag = null)
    }

    /** The Cancel / 中止 button and Esc (3.2). */
    fun cancel(state: TrimEditor): Pair<TrimEditor, CancelEffect> = when {
        state.drag != null -> cancelDrag(state) to CancelEffect.NONE
        state.task != null && state.cancelling -> state to CancelEffect.NONE
        state.task != null -> state.copy(cancelling = true) to CancelEffect.ABORT_TASK
        rangeUnchanged(state) -> state to CancelEffect.NONE
        else -> resetSelection(state) to CancelEffect.STOP_PLAYBACK
    }

    /** Reset only the selection; retain the current frame, with no stale range-end marker. */
    private fun resetSelection(state: TrimEditor): TrimEditor =
        state.copy(inUs = fullInUs, outUs = fullOutUs, completedUntilUs = null)

    fun step(state: TrimEditor, delta: Int): TrimEditor {
        if (grid.isEmpty || state.drag != null) return state
        val index = grid.stepIndex(state.playheadUs, delta).coerceIn(inIndex(state), outIndex(state))
        return state.copy(playheadUs = grid.spans[index].startUs, completedUntilUs = null)
    }

    /** Keyboard/click seek outside a drag. */
    fun seek(state: TrimEditor, us: Long): TrimEditor =
        if (state.drag != null) state else state.copy(playheadUs = clampPlayhead(state, us), completedUntilUs = null)

    /** A frame published by real-time playback. */
    fun played(state: TrimEditor, us: Long): TrimEditor = if (state.drag != null) state else
        state.copy(playheadUs = us, completedUntilUs = null)

    fun playbackFinished(state: TrimEditor, untilUs: Long, failed: Boolean): TrimEditor =
        if (failed || state.drag != null) state else played(state, untilUs - 1).copy(completedUntilUs = untilUs)

    /** Where playback starts and must stop: from the playhead (or the range start at the end) to Out / the end. */
    fun playRange(state: TrimEditor): Pair<Long, Long>? {
        if (grid.isEmpty) return null
        val (from, until) = state.inUs to state.outUs
        return (if (state.completedUntilUs == until || state.playheadUs < from || state.playheadUs >= until) from else state.playheadUs) to until
    }

    fun taskStarted(state: TrimEditor, kind: TaskKind): TrimEditor = state.copy(task = kind, cancelling = false)

    fun taskFinished(state: TrimEditor, kind: TaskKind, outcome: TaskOutcome): TrimEditor {
        val idle = state.copy(task = null, cancelling = false)
        return if (kind == TaskKind.TRIM && outcome == TaskOutcome.SUCCESS)
            resetSelection(idle)
        else idle
    }

    /** Movie time of the frame the video area shows: the held handle's edge frame, else the playhead. */
    fun displayUs(state: TrimEditor): Long = when (state.drag?.grip) {
        Grip.IN -> state.inUs
        Grip.OUT -> grid.spans.getOrNull(outIndex(state))?.startUs ?: state.playheadUs
        else -> state.playheadUs
    }

    /** Null unless a handle or the playhead is held: no time text is shown otherwise. */
    fun bubble(state: TrimEditor): TimeBubble? = when (state.drag?.grip) {
        null -> null
        Grip.IN -> TimeBubble(Grip.IN, state.inUs)
        Grip.OUT -> TimeBubble(Grip.OUT, state.outUs)
        Grip.PLAYHEAD -> TimeBubble(Grip.PLAYHEAD, state.playheadUs)
    }
}

/** Identifies a button in the right-hand column. */
internal enum class PlayerButton { TRIM, SAVE_FRAME, CANCEL }

internal data class ButtonSpec(
    val id: PlayerButton,
    val label: String,
    val primary: Boolean,
    val enabled: Boolean,
    /** Shown in the status line while disabled. */
    val reason: String? = null,
    /** Drawn in the blue accent (primary) style without being the main action. */
    val accent: Boolean = primary,
)

/** Inputs besides [TrimEditor] that decide button availability. */
internal data class PlayerAvailability(val loading: Boolean, val error: String?, val playheadInGap: Boolean,
    val previewReady: Boolean)

/** Button column, top to bottom (stacked layout), per 3.1. */
internal fun playerButtons(rules: TrimRules, state: TrimEditor, availability: PlayerAvailability): List<ButtonSpec> {
    val blocked = when {
        availability.loading -> "読み込み中です"
        availability.error != null -> "この保存は切り取れません: ${availability.error}"
        state.task != null -> "保存中です"
        else -> null
    }
    val frameReason = when {
        availability.loading -> "読み込み中です"
        availability.error != null -> blocked
        state.task != null -> "保存中です"
        availability.playheadInGap -> "この位置に表示frameがありません（欠落）"
        !availability.previewReady -> "この位置の映像を準備しています"
        else -> null
    }
    // PNG is a first-class edit action as soon as its genuine current preview is ready.
    val saveFrame = ButtonSpec(PlayerButton.SAVE_FRAME, "この位置をPNG保存", false, frameReason == null, frameReason,
        accent = frameReason == null)
    val cancel = when {
        state.task != null && state.cancelling -> ButtonSpec(PlayerButton.CANCEL, "中止しています…", false, false, "中止しています…")
        state.task != null -> ButtonSpec(PlayerButton.CANCEL, "中止", false, true)
        else -> ButtonSpec(PlayerButton.CANCEL, "キャンセル", false,
            blocked == null && state.drag == null && !rules.rangeUnchanged(state),
            blocked ?: if (rules.rangeUnchanged(state)) "範囲が動画全体のままです" else null)
    }
    val trimReason = blocked ?: if (rules.rangeUnchanged(state)) "範囲が動画全体のままです" else null
    val trimLabel = if (state.task == TaskKind.TRIM) "保存中…" else "トリミング"
    return listOf(ButtonSpec(PlayerButton.TRIM, trimLabel, true, trimReason == null, trimReason), saveFrame, cancel)
}

/** Where the button column goes for an editor width. */
internal enum class ControlLayout { STACKED, ROW }

internal const val STACKED_MIN_WIDTH_DP = 640f

internal fun controlLayout(widthDp: Float): ControlLayout = if (widthDp >= STACKED_MIN_WIDTH_DP) ControlLayout.STACKED else ControlLayout.ROW

/** Row layout order: secondary buttons first, primary at the right end (IntelliJ dialog order). */
internal fun rowOrder(buttons: List<ButtonSpec>): List<ButtonSpec> = buttons.filterNot { it.primary } + buttons.filter { it.primary }

/**
 * The one filmstrip: the whole movie from 0 to [durationUs] across [widthPx] at a fixed scale, with
 * no zoom or scroll, so a strip position always means the same movie time (AC-13).
 */
internal class StripGeometry(val widthPx: Float, val durationUs: Long) {
    fun x(us: Long): Float = if (durationUs <= 0 || widthPx <= 0f) 0f
        else (us.toDouble() / durationUs * widthPx).toFloat().coerceIn(0f, widthPx)

    fun us(x: Float): Long = if (widthPx <= 0f) 0 else (x.coerceIn(0f, widthPx).toDouble() / widthPx * durationUs).toLong()

    fun handleRange(state: TrimEditor, handlePx: Float): ClosedFloatingPointRange<Float> {
        val minimum = (2 * handlePx).coerceIn(0f, widthPx.coerceAtLeast(0f))
        val left = x(state.inUs).coerceAtMost(widthPx - minimum)
        return left..x(state.outUs).coerceAtLeast(left + minimum).coerceAtMost(widthPx)
    }

    fun dragUs(state: TrimEditor, grip: Grip, pressedX: Float, currentX: Float): Long {
        if (grip == Grip.PLAYHEAD || widthPx <= 0f) return us(currentX)
        val origin = if (grip == Grip.IN) state.inUs else state.outUs
        return (origin + (currentX - pressedX).toDouble() / widthPx * durationUs).toLong().coerceIn(0, durationUs)
    }

    /**
     * What a press at [x] takes. Handles sit inside the frame edges ([handlePx] wide) and win over the
     * playhead; anywhere else grabs the playhead and jumps it there.
     */
    fun hitTest(state: TrimEditor, x: Float, handlePx: Float, slopPx: Float): Grip {
        val range = handleRange(state, handlePx)
        val left = range.start
        val right = range.endInclusive
        val onIn = x >= left - slopPx && x <= left + handlePx + slopPx
        val onOut = x >= right - handlePx - slopPx && x <= right + slopPx
        return when {
            onIn && onOut -> if (abs(x - left) <= abs(x - right)) Grip.IN else Grip.OUT
            onIn -> Grip.IN
            onOut -> Grip.OUT
            else -> Grip.PLAYHEAD
        }
    }
}

/** Thumbnail count for a strip width: enough frames of [thumbWidthPx] to cover it. */
internal fun thumbnailCount(stripWidthPx: Float, thumbWidthPx: Float): Int =
    if (stripWidthPx <= 0f || thumbWidthPx <= 0f) 0 else ceil(stripWidthPx / thumbWidthPx).toInt().coerceIn(1, 400)

/** For each of [count] equal slots, the start of the frame shown at the slot centre, or null in a gap. */
internal fun thumbnailSlots(grid: FrameGrid, durationUs: Long, count: Int): List<Long?> = List(count) { slot ->
    val centre = ((slot + 0.5) / count * durationUs).toLong()
    grid.indexAt(centre).takeIf { it >= 0 }?.let { grid.spans[it].startUs }
}

/** Thumbnails by frame start, kept for the open save so a new strip width reuses decoded frames (AC-20). */
internal class ThumbnailCache<T>(private val maxEntries: Int = 400, private val maxBytes: Long = Long.MAX_VALUE,
    private val weight: (T) -> Long = { 1L }) {
    private val images = LinkedHashMap<Long, T>(16, .75f, true)
    private var bytes = 0L

    operator fun get(startUs: Long): T? = synchronized(images) { images[startUs] }

    fun put(startUs: Long, image: T) = synchronized(images) {
        images.remove(startUs)?.let { bytes -= weight(it) }
        val size = weight(image)
        if (size > maxBytes) return@synchronized
        images[startUs] = image
        bytes += size
        while (images.size > maxEntries || bytes > maxBytes) images.remove(images.keys.first())?.let { bytes -= weight(it) }
    }

    /** Frame starts of [slots] not decoded yet, ascending and without duplicates. */
    fun missing(slots: List<Long?>): List<Long> = synchronized(images) {
        // Touch visible entries before adding missing ones, so a resize evicts the old strip first.
        slots.filterNotNull().distinct().filter { images[it] == null }.sorted()
    }

    val size: Int get() = synchronized(images) { images.size }
}

/** The bar's one status line: a result message first, else why the primary (or PNG) button is disabled. */
internal fun statusLine(message: String?, buttons: List<ButtonSpec>): String? = message
    ?: buttons.firstOrNull { it.primary }?.reason
    ?: buttons.firstOrNull { it.id == PlayerButton.SAVE_FRAME }?.reason
