package io.github.shinma06.arb.ui

import io.github.shinma06.arb.model.CrashMarker
import io.github.shinma06.arb.model.GestureKind
import io.github.shinma06.arb.model.ScreenSpan
import io.github.shinma06.arb.model.TapRecord
import io.github.shinma06.arb.state.TimelineViewerState
import kotlin.math.roundToLong

/** Geometry shared by drawing and pointer hit tests. Coordinates are physical pixels. */
internal class StudioTimelineGeometry(
    private val state: TimelineViewerState,
    private val left: Float,
    private val width: Float,
    zoom: Float = 1f,
    viewportStartNs: Long = 0L,
) {
    private val axis get() = state.axis
    val zoom = zoom.coerceIn(1f, 8f)
    val span = if (this.zoom == 1f) axis.endNs else
        (axis.endNs.toDouble() / this.zoom).roundToLong().coerceAtLeast(1).coerceAtMost(axis.endNs)
    val maxStart = axis.endNs - span
    val start = viewportStartNs.coerceIn(0, maxStart)
    val end = start + span

    val inX get() = frame(state.inFrameIndex)
    val outX get() = at(axis.outTime(state.outFrameIndex))

    fun at(time: Long) = if (zoom == 1f || span == 0L) axis.pixel(time, left, width) else
        (left + (time.coerceIn(0, axis.endNs) - start).toDouble() / span * width).toFloat()
    fun timeAt(x: Float) = if (zoom == 1f) axis.timeAtPixel(x, left, width) else
        start + (((x.toDouble() - left) / width.coerceAtLeast(1f)).coerceIn(0.0, 1.0) * span).roundToLong().coerceAtMost(span)
    fun playhead(time: Long) = at(time)
    fun frame(index: Long) = at(axis.start(index))
    fun tickTime(index: Int, ticks: Int) = start + (span * (index.toDouble() / ticks)).roundToLong().coerceAtMost(span)
    fun tick(index: Int, ticks: Int) = at(tickTime(index, ticks))
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
