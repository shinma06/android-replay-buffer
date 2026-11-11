package io.github.shinma06.replaybuffer.core

import org.jcodec.common.model.Picture

/** A tap centre in source video frame pixels. */
internal data class OverlayDot(val x: Int, val y: Int)

/** Taps the saved MP4 draws: VIDEO draws all, DEVICE only those the device screen did not show (injected, or dots not on). */
internal fun overlayTouches(mode: TapDisplay, dots: TouchDots?, rows: List<TouchEntry>): List<TouchEntry> = when (mode) {
    TapDisplay.OFF -> emptyList()
    TapDisplay.VIDEO -> rows
    TapDisplay.DEVICE -> rows.filter { it.sample.injected || dots != TouchDots.APPLIED && dots != TouchDots.ALREADY_ON }
}

/**
 * Pointer positions over window time (sequence ns). A pointer is drawn while down and for [holdNs] after it lifts. A frame
 * shown until `until` also draws a pointer that first touches during that time, because a still screen produces no new
 * frame for a tap; every tap that starts in that time is drawn, up to [MAX_FRAME_TAPS] per pointer. A frame only draws taps of its own capture generation: a finger still down when the device
 * disconnected has no UP, and must not stay on the video after the reconnect. Likewise a reader gap ([gaps] for getevent,
 * [injectedGaps] for the injected-input reader) that starts after a pointer's last row may have lost its UP, so the
 * pointer is not drawn from that gap on. Rows without a certain time are left out.
 */
internal class TapOverlay(
    rows: List<TouchEntry>,
    private val gaps: List<TouchGap> = emptyList(),
    private val injectedGaps: List<TouchGap> = emptyList(),
    private val holdNs: Long = 100_000_000,
) {
    private val tracks = rows.filter { it.time.sequence != null && it.time.uncertainty != Long.MAX_VALUE }
        .groupBy { Triple(it.generation, it.sample.device, it.sample.pointer) }.values
        .map { track -> track.sortedBy { it.time.sequence!! } }

    /** Whether any drawn tap is visible in [from]..[to]. */
    fun within(from: Long, to: Long): Boolean = tracks.any { track ->
        track.any { val t = it.time.sequence!!; t <= to && t >= from - holdNs }
    }

    fun dots(sequence: Long, frameWidth: Int, frameHeight: Int, until: Long = sequence, generation: Long? = null): List<OverlayDot> = tracks.map { track ->
        if (generation != null && track.first().generation != generation) return@map emptyList()
        var low = 0
        var high = track.size - 1
        var found = -1
        while (low <= high) {
            val mid = (low + high) ushr 1
            if (track[mid].time.sequence!! <= sequence) { found = mid; low = mid + 1 } else high = mid - 1
        }
        val current = track.getOrNull(found)?.takeIf { it.sample.action != TouchAction.UP || sequence - it.time.sequence!! <= holdNs }
            ?.takeUnless { lost(it, sequence) }
        val later = track.subList(found + 1, track.size).takeWhile { it.time.sequence!! < until }
        val taps = later.filter { it.sample.action == TouchAction.DOWN }
        val rows = listOfNotNull(current) + taps.ifEmpty { listOfNotNull(later.firstOrNull().takeIf { current == null }) }
        rows.take(MAX_FRAME_TAPS).mapNotNull { project(it, frameWidth, frameHeight) }
    }.flatten().distinct()

    /** Whether reading stopped after [row] and by [sequence]; a gap with an unknown start cannot be ordered after it. */
    private fun lost(row: TouchEntry, sequence: Long): Boolean = (if (row.sample.injected) injectedGaps else gaps).any { gap ->
        gap.from != null && gap.from > row.time.sequence!! && gap.from <= sequence
    }

    private companion object {
        const val MAX_FRAME_TAPS = 16
    }

    private fun project(row: TouchEntry, frameWidth: Int, frameHeight: Int): OverlayDot? {
        if (row.width <= 0 || row.height <= 0 || frameWidth <= 0 || frameHeight <= 0) return null
        // Unknown rotation: only a frame in the natural orientation can be mapped.
        val rotation = row.rotation ?: if ((frameWidth > frameHeight) == (row.width > row.height)) 0 else return null
        val (sx, sy) = screenPoint(row.sample.x, row.sample.y, row.width, row.height, rotation)
        val screenWidth = if (rotation % 2 == 1) row.height else row.width
        val screenHeight = if (rotation % 2 == 1) row.width else row.height
        if ((frameWidth > frameHeight) != (screenWidth > screenHeight) && frameWidth != frameHeight && screenWidth != screenHeight) return null
        val x = (sx.toLong() * frameWidth / screenWidth).toInt().coerceIn(0, frameWidth - 1)
        val y = (sy.toLong() * frameHeight / screenHeight).toInt().coerceIn(0, frameHeight - 1)
        return OverlayDot(x, y)
    }
}

/**
 * End of the source time a frame stays on screen for drawing taps: the confirmed next frame of its run, else its own
 * held end, cut at the first video/clock gap that starts after it, so taps in an interruption or a later run stay off it.
 */
internal fun frameShownUntil(start: Long, confirmedNext: Long?, heldEnd: Long, gapStarts: List<Long>): Long =
    confirmedNext ?: gapStarts.filter { it > start }.fold(heldEnd, ::minOf).coerceAtLeast(start)

/** Dot radius in frame pixels, about the size of the device's own touch indicator. */
internal fun tapDotRadius(width: Int, height: Int): Int = maxOf(6, minOf(width, height) / 28)

/**
 * Draws translucent white dots with a dark rim into a YUV420 [picture] whose source frame occupies
 * [left],[top] [width]x[height]; JCodec planes store samples offset by -128.
 */
internal fun drawTapDots(picture: Picture, left: Int, top: Int, width: Int, height: Int, dots: List<OverlayDot>) {
    if (dots.isEmpty()) return
    val radius = tapDotRadius(width, height)
    val rim = maxOf(2, radius / 6)
    val pictureWidth = picture.width
    fun blend(value: Byte, target: Int, percent: Int): Byte {
        val v = value.toInt()
        return (v + (target - v) * percent / 100).coerceIn(-128, 127).toByte()
    }
    dots.forEach { dot ->
        val luma = picture.getPlaneData(0)
        for (y in maxOf(0, dot.y - radius)..minOf(height - 1, dot.y + radius)) {
            for (x in maxOf(0, dot.x - radius)..minOf(width - 1, dot.x + radius)) {
                val dx = x - dot.x
                val dy = y - dot.y
                val d2 = dx * dx + dy * dy
                if (d2 > radius * radius) continue
                val index = (y + top) * pictureWidth + x + left
                luma[index] = if (d2 >= (radius - rim) * (radius - rim)) blend(luma[index], -100, 70) else blend(luma[index], 110, 55)
            }
        }
        for (c in 1..2) {
            val plane = picture.getPlaneData(c)
            val chromaWidth = pictureWidth shr 1
            val r = radius shr 1
            val cx = dot.x shr 1
            val cy = dot.y shr 1
            for (y in maxOf(0, cy - r)..minOf((height shr 1) - 1, cy + r)) {
                for (x in maxOf(0, cx - r)..minOf((width shr 1) - 1, cx + r)) {
                    val dx = x - cx
                    val dy = y - cy
                    if (dx * dx + dy * dy > r * r) continue
                    val index = (y + (top shr 1)) * chromaWidth + x + (left shr 1)
                    plane[index] = blend(plane[index], 0, 60)
                }
            }
        }
    }
}
