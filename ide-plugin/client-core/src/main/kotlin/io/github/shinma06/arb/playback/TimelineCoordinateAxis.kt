package io.github.shinma06.arb.playback

import kotlin.math.roundToLong

/** Frame starts and the exclusive media endpoint in nanoseconds. Empty PTS use a virtual CFR table. */
class TimelineCoordinateAxis(
    val count: Long,
    rawPtsNs: List<Long> = emptyList(),
    estimatedStepNs: Long = 33_333_333L,
    lastDurationNs: Long? = null,
    reportedEndNs: Long? = null,
) {
    init {
        require(count >= 0)
    }

    private val step = estimatedStepNs.coerceIn(1L, Long.MAX_VALUE / count.coerceAtLeast(1))
    private val starts = if (count > 0 && count <= 100_000 && rawPtsNs.size.toLong() == count) {
        LongArray(rawPtsNs.size).also { result ->
            val origin = rawPtsNs.first()
            for (i in 1 until result.size) {
                val value = rawPtsNs[i]
                val relative = when {
                    value <= origin -> 0L
                    origin < 0 && value > Long.MAX_VALUE + origin -> Long.MAX_VALUE
                    else -> value - origin
                }
                result[i] = relative.coerceIn(result[i - 1] + 1, Long.MAX_VALUE - (count - i))
            }
        }
    } else {
        null
    }

    // The daemon omits large tables but still reports the exact last start and end.
    private val virtualSpan = reportedEndNs?.takeIf {
        starts == null && count > 1 && lastDurationNs != null && lastDurationNs in 1..it &&
            it - lastDurationNs >= count - 1
    }?.minus(lastDurationNs ?: 0L)

    fun start(frame: Long): Long {
        if (count == 0L) return 0
        val i = frame.coerceIn(0, count - 1)
        starts?.let { return it[i.toInt()] }
        val span = virtualSpan ?: return i * step
        val intervals = count - 1
        val remainder = span % intervals
        val extra = if (remainder == 0L || i <= Long.MAX_VALUE / remainder) {
            i * remainder / intervals
        } else {
            (i.toBigInteger() * remainder.toBigInteger() / intervals.toBigInteger()).toLong()
        }
        return i * (span / intervals) + extra
    }

    val endNs: Long = if (count == 0L) 0 else {
        val last = start(count - 1)
        val inferred = if (count > 1) last - start(count - 2) else step
        last + (lastDurationNs?.takeIf { it > 0 } ?: inferred).coerceAtMost(Long.MAX_VALUE - last)
    }

    fun outTime(frame: Long): Long = if (frame >= count - 1) endNs else start(frame + 1)

    fun frameAt(time: Long): Long {
        if (count == 0L) return 0
        val t = time.coerceIn(0, endNs)
        if (virtualSpan != null) {
            var lo = 0L
            var hi = count - 1
            while (lo < hi) {
                val mid = lo + (hi - lo + 1) / 2
                if (start(mid) <= t) lo = mid else hi = mid - 1
            }
            return lo
        }
        val table = starts ?: return (t / step).coerceAtMost(count - 1)
        val i = table.binarySearch(t)
        return if (i >= 0) i.toLong() else (-i - 2).coerceAtLeast(0).toLong()
    }

    fun outFrame(time: Long): Long = frameAt(time.coerceAtLeast(1) - 1)

    fun fraction(time: Long): Double =
        if (endNs == 0L) 0.0 else time.coerceIn(0, endNs).toDouble() / endNs.toDouble()

    fun timeAtFraction(fraction: Double): Long =
        ((if (fraction.isNaN()) 0.0 else fraction.coerceIn(0.0, 1.0)) * endNs).roundToLong().coerceIn(0, endNs)

    fun pixel(time: Long, left: Float, width: Float): Float = (left + fraction(time) * width).toFloat()

    fun timeAtPixel(x: Float, left: Float, width: Float): Long =
        if (width <= 0f) 0 else timeAtFraction((x.toDouble() - left) / width)
}
