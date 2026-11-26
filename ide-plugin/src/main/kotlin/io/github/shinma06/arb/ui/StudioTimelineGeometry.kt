package io.github.shinma06.arb.ui

import io.github.shinma06.arb.model.CrashMarker
import io.github.shinma06.arb.model.GestureKind
import io.github.shinma06.arb.model.ScreenSpan
import io.github.shinma06.arb.model.TapRecord
import io.github.shinma06.arb.state.TimelineViewerState
import kotlin.math.abs
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

    val inX get() = at(state.inTimeNs)
    val outX get() = at(state.outTimeNs)

    fun at(time: Long) = if (zoom == 1f || span == 0L) axis.pixel(time, left, width) else
        (left + (time.coerceIn(0, axis.endNs) - start).toDouble() / span * width).toFloat()
    fun timeAt(x: Float) = if (zoom == 1f) axis.timeAtPixel(x, left, width) else
        start + (((x.toDouble() - left) / width.coerceAtLeast(1f)).coerceIn(0.0, 1.0) * span).roundToLong().coerceAtMost(span)
    fun playhead(time: Long) = at(time)

    /** Whether [x] lies on the track; panned-out items are neither drawn nor hit. */
    fun visible(x: Float) = x in left..(left + width)

    /** Marker-track hits; the gutters only hit at 1x, where nothing lies outside the track. */
    fun markerHittable(x: Float) = zoom == 1f || visible(x)

    enum class Hit { IN, OUT, PLAYHEAD }

    fun hit(x: Float, y: Float, handleBand: ClosedFloatingPointRange<Float>, radius: Float): Hit =
        when (if (y in handleBand) trimEdgeAt(x, radius) else null) {
            TrimEdge.IN -> Hit.IN
            TrimEdge.OUT -> Hit.OUT
            null -> Hit.PLAYHEAD
        }

    fun trimEdgeAt(x: Float, hitPx: Float): TrimEdge? {
        val inDistance = inX.takeIf(::visible)?.let { abs(x - it) } ?: Float.POSITIVE_INFINITY
        val outDistance = outX.takeIf(::visible)?.let { abs(x - it) } ?: Float.POSITIVE_INFINITY
        return when {
            minOf(inDistance, outDistance) > hitPx -> null
            inDistance < outDistance || (inDistance == outDistance && x <= inX) -> TrimEdge.IN
            else -> TrimEdge.OUT
        }
    }

    /** Nearest pin time within [radius] of [x]. */
    fun pinAt(x: Float, radius: Float, times: List<Long>): Long? =
        times.map { at(it) to it }.filter { visible(it.first) }.minByOrNull { abs(it.first - x) }?.takeIf { abs(it.first - x) <= radius }?.second
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
