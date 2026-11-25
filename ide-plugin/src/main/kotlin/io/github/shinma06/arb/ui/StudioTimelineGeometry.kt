package io.github.shinma06.arb.ui

import io.github.shinma06.arb.model.CrashMarker
import io.github.shinma06.arb.model.GestureKind
import io.github.shinma06.arb.model.ScreenSpan
import io.github.shinma06.arb.model.TapRecord
import io.github.shinma06.arb.state.TimelineViewerState

/** Geometry shared by drawing and pointer hit tests. Coordinates are physical pixels. */
internal class StudioTimelineGeometry(
    private val state: TimelineViewerState,
    private val left: Float,
    private val width: Float,
) {
    private val axis get() = state.axis
    val inX get() = frame(state.inFrameIndex)
    val outX get() = at(axis.outTime(state.outFrameIndex))

    fun at(time: Long) = axis.pixel(time, left, width)
    fun timeAt(x: Float) = axis.timeAtPixel(x, left, width)
    fun playhead(time: Long) = at(time)
    fun frame(index: Long) = at(axis.start(index))
    fun tick(index: Int, ticks: Int) = at(axis.timeAtFraction(index.toDouble() / ticks))
    fun event(time: Long, bootId: String?) = state.eventTime(time, bootId)?.let(::at)
    fun screen(span: ScreenSpan, end: Boolean = false) = state.screenTime(span, end)?.let(::at)
    fun crash(marker: CrashMarker) = state.crashTime(marker)?.let(::at)
    fun tap(record: TapRecord) = state.tapTime(record)?.let(::at)

    fun swipeEnd(record: TapRecord): Float? {
        if (record.gestureKind != GestureKind.SWIPE) return null
        val duration = record.endTOffsetNs ?: return null
        val start = state.tapTime(record) ?: return null
        return at(start + duration.coerceIn(0, axis.endNs - start))
    }
}
