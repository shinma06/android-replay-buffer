package io.github.shinma06.replaybuffer.ui

import io.github.shinma06.replaybuffer.core.ReplayTimeline
import java.util.Locale
import kotlin.math.abs
import kotlin.math.ceil

/** Player screen mode: plain viewing, or QuickTime-style trim with the range frame on the filmstrip. */
internal enum class PlayerMode { VIEW, TRIM }

/** What the pointer holds on the filmstrip. */
internal enum class Grip { IN, OUT, PLAYHEAD }

/** Background work started from the player. */
internal enum class TaskKind { TRIM, FRAME }

internal enum class TaskOutcome { SUCCESS, CANCELLED, FAILED }

/** Values captured when a grip was taken, restored by Esc while still dragging (CANCEL-03). */
internal data class TrimDrag(val grip: Grip, val inUs: Long, val outUs: Long, val playheadUs: Long)

/** UI state of the trim player. All transitions live in [TrimRules]; files are never touched here. */
internal data class TrimEditor(
    val mode: PlayerMode = PlayerMode.VIEW,
    val inUs: Long = 0,
    val outUs: Long = 0,
    val playheadUs: Long = 0,
    /** Playhead when trim mode was entered; Cancel returns here (CANCEL-01). */
    val entryPlayheadUs: Long = 0,
    val drag: TrimDrag? = null,
    val task: TaskKind? = null,
    val cancelling: Boolean = false,
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
        return spans.indices.minByOrNull { i ->
            val span = spans[i]
            if (us < span.startUs) span.startUs - us else us - span.endUs + 1
        } ?: -1
    }

    /** Span whose start boundary is closest to [us]. */
    fun nearestStartIndex(us: Long): Int = spans.indices.minByOrNull { abs(spans[it].startUs - us) } ?: -1

    /** Span whose end boundary is closest to [us]. */
    fun nearestEndIndex(us: Long): Int = spans.indices.minByOrNull { abs(spans[it].endUs - us) } ?: -1

    /** Index of the span that starts at [startUs] (an In boundary). */
    fun indexStartingAt(startUs: Long): Int = spans.indexOfFirst { it.startUs == startUs }

    /** Index of the span that ends at [endUs] (an Out boundary). */
    fun indexEndingAt(endUs: Long): Int = spans.indexOfLast { it.endUs == endUs }
}

/**
 * Pure transitions of the trim player (drag, snap, marks, cancel, task lifecycle). The controller
 * owns decoding, playback and background tasks and only applies these results.
 */
internal class TrimRules(val grid: FrameGrid, val durationUs: Long) {
    val fullInUs: Long get() = grid.spans.firstOrNull()?.startUs ?: 0
    val fullOutUs: Long get() = grid.spans.lastOrNull()?.endUs ?: durationUs

    fun initial(): TrimEditor = TrimEditor(inUs = fullInUs, outUs = fullOutUs, playheadUs = fullInUs, entryPlayheadUs = fullInUs)

    fun rangeUnchanged(state: TrimEditor): Boolean = state.inUs == fullInUs && state.outUs == fullOutUs

    private fun inIndex(state: TrimEditor) = grid.indexStartingAt(state.inUs).takeIf { it >= 0 } ?: 0
    private fun outIndex(state: TrimEditor) = grid.indexEndingAt(state.outUs).takeIf { it >= 0 } ?: grid.spans.lastIndex

    /** Clamps a playhead to a frame start inside the range in trim mode, or anywhere in view mode. */
    private fun clampPlayhead(state: TrimEditor, us: Long): Long {
        if (grid.isEmpty) return 0
        var index = grid.nearestIndex(us)
        if (state.mode == PlayerMode.TRIM) index = index.coerceIn(inIndex(state), outIndex(state))
        return grid.spans[index].startUs
    }

    fun enterTrim(state: TrimEditor): TrimEditor {
        if (state.mode == PlayerMode.TRIM || grid.isEmpty || state.task != null) return state
        val entered = state.copy(mode = PlayerMode.TRIM, inUs = fullInUs, outUs = fullOutUs, entryPlayheadUs = state.playheadUs)
        return entered.copy(playheadUs = clampPlayhead(entered, state.playheadUs))
    }

    /** `I`: the frame shown at the playhead becomes In (enters trim mode first). */
    fun markIn(state: TrimEditor): TrimEditor {
        val trim = enterTrim(state).takeIf { it.mode == PlayerMode.TRIM && it.drag == null } ?: return state
        val index = grid.nearestIndex(trim.playheadUs).coerceAtMost(outIndex(trim))
        return trim.copy(inUs = grid.spans[index].startUs)
    }

    /** `O`: the frame shown at the playhead becomes the last kept frame (enters trim mode first). */
    fun markOut(state: TrimEditor): TrimEditor {
        val trim = enterTrim(state).takeIf { it.mode == PlayerMode.TRIM && it.drag == null } ?: return state
        val index = grid.nearestIndex(trim.playheadUs).coerceAtLeast(inIndex(trim))
        return trim.copy(outUs = grid.spans[index].endUs)
    }

    fun beginDrag(state: TrimEditor, grip: Grip, us: Long): TrimEditor {
        if (grid.isEmpty || state.drag != null) return state
        if (grip != Grip.PLAYHEAD && (state.mode != PlayerMode.TRIM || state.task != null)) return state
        val taken = state.copy(drag = TrimDrag(grip, state.inUs, state.outUs, state.playheadUs))
        return dragTo(taken, us)
    }

    /** Moves the held element to the frame boundary nearest [us]; In and Out never cross, one frame stays. */
    fun dragTo(state: TrimEditor, us: Long): TrimEditor {
        val drag = state.drag ?: return state
        return when (drag.grip) {
            Grip.IN -> state.copy(inUs = grid.spans[grid.nearestStartIndex(us).coerceAtMost(outIndex(state))].startUs)
            Grip.OUT -> state.copy(outUs = grid.spans[grid.nearestEndIndex(us).coerceAtLeast(inIndex(state))].endUs)
            Grip.PLAYHEAD -> state.copy(playheadUs = clampPlayhead(state, us))
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
        return state.copy(inUs = drag.inUs, outUs = drag.outUs, playheadUs = drag.playheadUs, drag = null)
    }

    /** The Cancel / 中止 button and Esc (3.2). */
    fun cancel(state: TrimEditor): Pair<TrimEditor, CancelEffect> = when {
        state.drag != null -> cancelDrag(state) to CancelEffect.NONE
        state.task != null && state.cancelling -> state to CancelEffect.NONE
        state.task != null -> state.copy(cancelling = true) to CancelEffect.ABORT_TASK
        state.mode == PlayerMode.TRIM -> state.copy(mode = PlayerMode.VIEW, inUs = fullInUs, outUs = fullOutUs,
            playheadUs = state.entryPlayheadUs) to CancelEffect.STOP_PLAYBACK
        else -> state to CancelEffect.NONE
    }

    fun step(state: TrimEditor, delta: Int): TrimEditor {
        if (grid.isEmpty || state.drag != null) return state
        val current = grid.nearestIndex(state.playheadUs)
        var index = (current + delta).coerceIn(0, grid.spans.lastIndex)
        if (state.mode == PlayerMode.TRIM) index = index.coerceIn(inIndex(state), outIndex(state))
        return state.copy(playheadUs = grid.spans[index].startUs)
    }

    /** Keyboard/click seek outside a drag. */
    fun seek(state: TrimEditor, us: Long): TrimEditor =
        if (state.drag != null) state else state.copy(playheadUs = clampPlayhead(state, us))

    /** A frame published by real-time playback. */
    fun played(state: TrimEditor, us: Long): TrimEditor = if (state.drag != null) state else state.copy(playheadUs = us)

    /** Where playback starts and must stop: from the playhead (or the range start at the end) to Out / the end. */
    fun playRange(state: TrimEditor): Pair<Long, Long>? {
        if (grid.isEmpty) return null
        val (from, until) = if (state.mode == PlayerMode.TRIM) state.inUs to state.outUs else fullInUs to fullOutUs
        val lastStart = grid.spans[if (state.mode == PlayerMode.TRIM) outIndex(state) else grid.spans.lastIndex].startUs
        return (if (state.playheadUs >= lastStart || state.playheadUs < from) from else state.playheadUs) to until
    }

    fun taskStarted(state: TrimEditor, kind: TaskKind): TrimEditor = state.copy(task = kind, cancelling = false)

    fun taskFinished(state: TrimEditor, kind: TaskKind, outcome: TaskOutcome): TrimEditor {
        val idle = state.copy(task = null, cancelling = false)
        return if (kind == TaskKind.TRIM && outcome == TaskOutcome.SUCCESS)
            idle.copy(mode = PlayerMode.VIEW, inUs = fullInUs, outUs = fullOutUs)
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
internal enum class PlayerButton { ENTER_TRIM, TRIM, SAVE_FRAME, CANCEL }

internal data class ButtonSpec(
    val id: PlayerButton,
    val label: String,
    val primary: Boolean,
    val enabled: Boolean,
    /** Shown in the status line / tooltip while disabled. */
    val reason: String? = null,
)

/** Inputs besides [TrimEditor] that decide button availability. */
internal data class PlayerAvailability(val loading: Boolean, val error: String?, val playheadInGap: Boolean)

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
        else -> null
    }
    val saveFrame = ButtonSpec(PlayerButton.SAVE_FRAME, "この位置をPNG保存", false, frameReason == null, frameReason)
    if (state.mode == PlayerMode.VIEW) {
        return listOf(ButtonSpec(PlayerButton.ENTER_TRIM, "トリム…", true, blocked == null, blocked), saveFrame)
    }
    val trimReason = blocked ?: if (rules.rangeUnchanged(state)) "範囲が動画全体のままです" else null
    val trimLabel = if (state.task == TaskKind.TRIM) "保存中…" else "トリミング"
    val cancel = when {
        state.task != null && state.cancelling -> ButtonSpec(PlayerButton.CANCEL, "中止しています…", false, false, "中止しています…")
        state.task != null -> ButtonSpec(PlayerButton.CANCEL, "中止", false, true)
        else -> ButtonSpec(PlayerButton.CANCEL, "キャンセル", false, true)
    }
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

    /**
     * What a press at [x] takes. Handles sit inside the frame edges ([handlePx] wide) and win over the
     * playhead; anywhere else grabs the playhead and jumps it there.
     */
    fun hitTest(state: TrimEditor, x: Float, handlePx: Float, slopPx: Float): Grip {
        if (state.mode != PlayerMode.TRIM) return Grip.PLAYHEAD
        val left = x(state.inUs)
        val right = x(state.outUs)
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
internal class ThumbnailCache<T> {
    private val images = LinkedHashMap<Long, T>()

    operator fun get(startUs: Long): T? = synchronized(images) { images[startUs] }

    fun put(startUs: Long, image: T) = synchronized(images) { images[startUs] = image }

    /** Frame starts of [slots] not decoded yet, ascending and without duplicates. */
    fun missing(slots: List<Long?>): List<Long> = synchronized(images) {
        slots.filterNotNull().distinct().filterNot(images::containsKey).sorted()
    }

    val size: Int get() = synchronized(images) { images.size }
}

/** The bar's one status line: a result message first, else why the primary (or PNG) button is disabled. */
internal fun statusLine(message: String?, buttons: List<ButtonSpec>): String? = message
    ?: buttons.firstOrNull { it.primary }?.reason
    ?: buttons.firstOrNull { it.id == PlayerButton.SAVE_FRAME }?.reason
