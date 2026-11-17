package io.github.shinma06.replaybuffer.core

import org.jcodec.common.model.ColorSpace
import java.awt.Transparency
import java.awt.color.ColorSpace as AwtColorSpace
import java.awt.image.BufferedImage
import java.awt.image.ComponentColorModel
import java.awt.image.DataBuffer
import java.awt.image.DataBufferByte
import java.awt.image.Raster

/** Player-only decoder boundary. Exact PNG and trim continue to use the export path. */
internal interface ReplayVideoDecoder : AutoCloseable {
    fun seek(atUs: Long, checkActive: () -> Unit = {}): ReplayFrameCursor.Decoded
    /** Prepare reference state before the playback clock starts, even when the preview is cached. */
    fun seekForPlayback(atUs: Long, checkActive: () -> Unit = {}): ReplayFrameCursor.Decoded = seek(atUs, checkActive)
    fun next(checkActive: () -> Unit = {}): ReplayFrameCursor.Decoded?
}

/** Immutable after construction; the AWT view and Skia input share this one BGRA allocation. */
internal class ReplayPreviewImage internal constructor(val pixels: ByteArray, width: Int, height: Int) : BufferedImage(
    ComponentColorModel(AwtColorSpace.getInstance(AwtColorSpace.CS_sRGB), true, false,
        Transparency.TRANSLUCENT, DataBuffer.TYPE_BYTE),
    Raster.createInterleavedRaster(DataBufferByte(pixels, pixels.size), width, height,
        width * 4, 4, intArrayOf(2, 1, 0, 3), null), false, null,
) {
    companion object {
        /** Same integer coefficients, clipping and rounding as rgbImage / pinned JCodec 0.2.5. */
        fun convert(frame: MixedVideoNormalizer.DisplayFrame): ReplayPreviewImage {
            val p = frame.picture
            require(p.color == ColorSpace.YUV420 || p.color == ColorSpace.YUV420J) { "復号frameの色形式が非対応です" }
            val pixels = ByteArray(Math.multiplyExact(Math.multiplyExact(p.width, p.height), 4))
            val full = p.color == ColorSpace.YUV420J
            val bt709 = frame.matrixCoefficients == 1
            val y = p.getPlaneData(0); val cb = p.getPlaneData(1); val cr = p.getPlaneData(2)
            for (row in 0 until p.height) for (x in 0 until p.width) {
                val i = row * p.width + x
                val c = (row shr 1) * (p.width shr 1) + (x shr 1)
                val u = cb[c].toInt(); val v = cr[c].toInt()
                val r: Int; val g: Int; val b: Int
                if (bt709) {
                    val level = (y[i].toInt() + if (full) 128 else 112) * (if (full) 16384 else 19077) + 8192
                    r = (level + (if (full) 25802 else 29372) * v) shr 14
                    g = (level - (if (full) 3069 else 3494) * u - (if (full) 7670 else 8731) * v) shr 14
                    b = (level + (if (full) 30402 else 34610) * u) shr 14
                } else if (full) {
                    val level = (y[i].toInt() + 128) shl 10
                    r = (level + 1436 * v + 512) shr 10
                    g = (level - 352 * u - 731 * v + 512) shr 10
                    b = (level + 1815 * u + 512) shr 10
                } else {
                    val level = 298 * (y[i].toInt() + 112)
                    r = (level + 409 * v + 128) shr 8
                    g = (level - 100 * u - 208 * v + 128) shr 8
                    b = (level + 516 * u + 128) shr 8
                }
                val out = i * 4
                pixels[out] = b.coerceIn(0, 255).toByte()
                pixels[out + 1] = g.coerceIn(0, 255).toByte()
                pixels[out + 2] = r.coerceIn(0, 255).toByte()
                pixels[out + 3] = 255.toByte()
            }
            return ReplayPreviewImage(pixels, p.width, p.height)
        }
    }
}

/** Byte-bounded, shared only by cursors for one editor. Keys also carry the pinned source identity. */
internal class ReplayFrameCache(private val limitBytes: Long = 48L * 1024 * 1024) {
    private data class Key(val source: String, val index: Int)
    private val frames = LinkedHashMap<Key, ReplayFrameCursor.Decoded>(16, .75f, true)
    private var bytes = 0L
    private fun cost(frame: ReplayFrameCursor.Decoded) = frame.image.width.toLong() * frame.image.height * 4

    @Synchronized fun get(source: String, index: Int) = frames[Key(source, index)]

    @Synchronized fun put(source: String, frame: ReplayFrameCursor.Decoded) {
        val key = Key(source, frame.index)
        frames.remove(key)?.let { bytes -= cost(it) }
        val cost = cost(frame)
        if (cost > limitBytes) return
        while (bytes + cost > limitBytes && frames.isNotEmpty()) {
            val oldest = frames.entries.iterator().next()
            bytes -= cost(oldest.value); frames.remove(oldest.key)
        }
        frames[key] = frame; bytes += cost
    }

    @Synchronized fun clear() { frames.clear(); bytes = 0 }
    @Synchronized fun sizeBytes() = bytes
}

/** Only reference IDRs in the same segment / sample entry can stand in for a dragged position. */
internal fun previewKeyframeUs(saved: SavedReplay, atUs: Long): Long? {
    return saved.previewKeyframes.at(atUs)
}

/** Precomputed once: filmstrip painting / pointer input must not scan the entire recording. */
internal class ReplayPreviewIndex(rows: List<com.google.gson.JsonObject>) {
    private data class Span(val start: Long, val end: Long, val keyUs: Long?)
    private val spans = buildList {
        var segment: com.google.gson.JsonElement? = null
        var entry: com.google.gson.JsonElement? = null
        var keyUs: Long? = null
        for (row in rows) {
            if (segment != row["segment_index"] || entry != row["sample_entry"]) keyUs = null
            segment = row["segment_index"]; entry = row["sample_entry"]
            val start = row["movie_pts_us"]?.takeIf { it.isJsonPrimitive }?.asString?.toLongOrNull()
            val end = row["movie_end_us"]?.takeIf { it.isJsonPrimitive }?.asString?.toLongOrNull()
            if (row["output_key"]?.asBoolean == true) keyUs = start
            if (start != null && end != null) add(Span(start, end, keyUs))
        }
    }.sortedBy { it.start }

    fun at(atUs: Long): Long? {
        var low = 0; var high = spans.lastIndex
        while (low <= high) {
            val mid = (low + high) ushr 1
            val span = spans[mid]
            when {
                atUs < span.start -> high = mid - 1
                atUs >= span.end -> low = mid + 1
                else -> return span.keyUs
            }
        }
        return null
    }
}
