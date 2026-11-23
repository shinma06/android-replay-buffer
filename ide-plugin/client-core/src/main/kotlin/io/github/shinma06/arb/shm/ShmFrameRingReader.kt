package io.github.shinma06.arb.shm

import java.awt.image.BufferedImage
import java.io.Closeable
import java.io.IOException
import java.lang.invoke.MethodHandles
import java.lang.invoke.VarHandle
import java.nio.ByteOrder
import java.nio.MappedByteBuffer
import java.nio.channels.FileChannel
import java.nio.charset.StandardCharsets
import java.nio.file.Path
import java.nio.file.StandardOpenOption

enum class ShmPixelFormat(val wire: Int) {
    BGRA8(1),
    RGBA8(2),
    RGB24(3),
    NV12(4);

    fun requiredBufferBytes(width: Int, height: Int, stride: Int): Int? {
        if (width <= 0 || height <= 0 || stride <= 0) return null
        val minRowBytes: Long = when (this) {
            BGRA8, RGBA8 -> width.toLong() * 4L
            RGB24 -> width.toLong() * 3L
            NV12 -> width.toLong()
        }
        if (stride.toLong() < minRowBytes) return null
        val totalRows: Long = when (this) {
            BGRA8, RGBA8, RGB24 -> height.toLong()
            NV12 -> height.toLong() + ((height.toLong() + 1L) / 2L)
        }
        val total = stride.toLong() * totalRows
        if (total > Int.MAX_VALUE.toLong()) return null
        return total.toInt()
    }

    companion object {
        fun fromWire(raw: Int): ShmPixelFormat? = entries.firstOrNull { it.wire == raw }
    }
}

data class ShmFrameMeta(
    val generation: Long,
    val frameIndex: Long,
    val isApproximate: Boolean,
    val width: Int,
    val height: Int,
    val stride: Int,
    val pixelFormat: ShmPixelFormat,
    val boottimeNs: Long,
)

data class ShmFrame(
    val meta: ShmFrameMeta,
    val pixels: ByteArray,
) {
    /**
     * Converts the decoded pixel payload into a standard Swing/AWT [BufferedImage] (`TYPE_INT_ARGB`).
     */
    fun toBufferedImage(): BufferedImage {
        val w = meta.width
        val h = meta.height
        val stride = meta.stride
        val image = BufferedImage(w, h, BufferedImage.TYPE_INT_ARGB)
        val argb = IntArray(w * h)

        when (meta.pixelFormat) {
            ShmPixelFormat.RGBA8 -> {
                for (y in 0 until h) {
                    val rowBase = y * stride
                    val dstBase = y * w
                    for (x in 0 until w) {
                        val p = rowBase + x * 4
                        val r = pixels[p].toInt() and 0xFF
                        val g = pixels[p + 1].toInt() and 0xFF
                        val b = pixels[p + 2].toInt() and 0xFF
                        val a = pixels[p + 3].toInt() and 0xFF
                        argb[dstBase + x] = (a shl 24) or (r shl 16) or (g shl 8) or b
                    }
                }
            }

            ShmPixelFormat.BGRA8 -> {
                for (y in 0 until h) {
                    val rowBase = y * stride
                    val dstBase = y * w
                    for (x in 0 until w) {
                        val p = rowBase + x * 4
                        val b = pixels[p].toInt() and 0xFF
                        val g = pixels[p + 1].toInt() and 0xFF
                        val r = pixels[p + 2].toInt() and 0xFF
                        val a = pixels[p + 3].toInt() and 0xFF
                        argb[dstBase + x] = (a shl 24) or (r shl 16) or (g shl 8) or b
                    }
                }
            }

            ShmPixelFormat.RGB24 -> {
                for (y in 0 until h) {
                    val rowBase = y * stride
                    val dstBase = y * w
                    for (x in 0 until w) {
                        val p = rowBase + x * 3
                        val r = pixels[p].toInt() and 0xFF
                        val g = pixels[p + 1].toInt() and 0xFF
                        val b = pixels[p + 2].toInt() and 0xFF
                        argb[dstBase + x] = (0xFF shl 24) or (r shl 16) or (g shl 8) or b
                    }
                }
            }

            ShmPixelFormat.NV12 -> {
                val uvBase = stride * h
                for (y in 0 until h) {
                    val yRow = y * stride
                    val uvRow = uvBase + (y / 2) * stride
                    val dstBase = y * w
                    for (x in 0 until w) {
                        val yVal = (pixels[yRow + x].toInt() and 0xFF) - 16
                        val uvOffset = uvRow + (x and 0x7FFF_FFFE)
                        val uVal = (pixels[uvOffset].toInt() and 0xFF) - 128
                        val vVal = (pixels[uvOffset + 1].toInt() and 0xFF) - 128
                        val c = if (yVal < 0) 0 else yVal
                        val r = ((298 * c + 409 * vVal + 128) shr 8).coerceIn(0, 255)
                        val g = ((298 * c - 100 * uVal - 208 * vVal + 128) shr 8).coerceIn(0, 255)
                        val b = ((298 * c + 516 * uVal + 128) shr 8).coerceIn(0, 255)
                        argb[dstBase + x] = (0xFF shl 24) or (r shl 16) or (g shl 8) or b
                    }
                }
            }
        }

        image.setRGB(0, 0, w, h, argb, 0, w)
        return image
    }

    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is ShmFrame) return false
        return meta == other.meta && pixels.contentEquals(other.pixels)
    }

    override fun hashCode(): Int = 31 * meta.hashCode() + pixels.contentHashCode()
}

class ShmRingException(message: String) : IOException(message)

/**
 * Lock-free read-only consumer for `arbd`'s triple-buffered shared-memory frame ring.
 *
 * Matches `core/crates/arb-os/src/shm.rs` layout **bit-for-bit**:
 * - `RawRingHeader` (64 bytes @ offset 0):
 *   - `0..8`   magic `"ARBSHM01"`
 *   - `8..12`  version `1` (u32 LE)
 *   - `12..16` slot_count `3` (u32 LE)
 *   - `16..24` slot_capacity (u64 LE)
 *   - `24..28` published_slot (AtomicU32 LE, `0xFFFF_FFFF` when empty)
 *   - `28..32` _pad0
 *   - `32..40` published_generation (AtomicU64 LE)
 *   - `40..64` _reserved
 * - `RawSlotHeader[3]` (64 bytes each @ offsets 64, 128, 192):
 *   - `+0..+8`   seq (AtomicU64 LE seqlock: odd=writing, even=stable)
 *   - `+8..+16`  generation (u64 LE)
 *   - `+16..+24` frame_index (i64 LE)
 *   - `+24..+32` boottime_ns (i64 LE)
 *   - `+32..+36` width (u32 LE)
 *   - `+36..+40` height (u32 LE)
 *   - `+40..+44` stride (u32 LE)
 *   - `+44..+48` pixel_format (u32 LE)
 *   - `+48..+52` payload_len (u32 LE)
 *   - `+52..+56` flags (u32 LE, bit 0 = `FLAG_IS_APPROXIMATE`)
 *   - `+56..+64` _reserved
 * - Payload slots @ offset `256 + slotIdx * alignUp(slotCapacity, 64)`.
 */
class ShmFrameRingReader private constructor(
    val path: Path,
    private val channel: FileChannel,
    private val mapped: MappedByteBuffer,
    val slotCapacity: Int,
    private val alignedSlotStride: Int,
) : Closeable {
    fun latestGeneration(): Long = LONG_LE.getAcquire(mapped, OFF_PUBLISHED_GENERATION) as Long

    /**
     * Atomically reads the latest published frame using the slot seqlock so torn frames are never returned.
     * Returns `null` if no frame has been published yet.
     */
    @Throws(ShmRingException::class)
    fun readLatest(): ShmFrame? {
        for (attempt in 0 until MAX_SEQLOCK_RETRIES) {
            val publishedGen = LONG_LE.getAcquire(mapped, OFF_PUBLISHED_GENERATION) as Long
            if (publishedGen == 0L) {
                return null
            }
            val publishedSlot = INT_LE.getAcquire(mapped, OFF_PUBLISHED_SLOT) as Int
            if (publishedSlot == NO_PUBLISHED_SLOT) {
                return null
            }
            if (publishedSlot !in 0 until SHM_SLOT_COUNT) {
                throw ShmRingException("published_slot $publishedSlot out of range 0..<$SHM_SLOT_COUNT")
            }

            val slotBase = RING_HEADER_BYTES + publishedSlot * SLOT_HEADER_BYTES
            val seq1 = LONG_LE.getAcquire(mapped, slotBase) as Long
            if ((seq1 and 1L) != 0L) {
                Thread.onSpinWait()
                continue
            }

            val generation = mapped.getLong(slotBase + SLOT_OFF_GENERATION)
            val frameIndex = mapped.getLong(slotBase + SLOT_OFF_FRAME_INDEX)
            val boottimeNs = mapped.getLong(slotBase + SLOT_OFF_BOOTTIME_NS)
            val width = mapped.getInt(slotBase + SLOT_OFF_WIDTH)
            val height = mapped.getInt(slotBase + SLOT_OFF_HEIGHT)
            val stride = mapped.getInt(slotBase + SLOT_OFF_STRIDE)
            val pixelFormatRaw = mapped.getInt(slotBase + SLOT_OFF_PIXEL_FORMAT)
            val payloadLen = mapped.getInt(slotBase + SLOT_OFF_PAYLOAD_LEN)
            val flags = mapped.getInt(slotBase + SLOT_OFF_FLAGS)

            if (payloadLen < 0 || payloadLen > slotCapacity) {
                VarHandle.acquireFence()
                val recheckSeq = LONG_LE.getOpaque(mapped, slotBase) as Long
                if (recheckSeq != seq1) {
                    Thread.onSpinWait()
                    continue
                }
                throw ShmRingException(
                    "slot payload_len ($payloadLen) exceeds slot_capacity ($slotCapacity)",
                )
            }

            val payloadOffset = HEADER_REGION_BYTES + publishedSlot * alignedSlotStride
            val pixels = ByteArray(payloadLen)
            mapped.get(payloadOffset, pixels, 0, payloadLen)

            VarHandle.acquireFence()
            val seq2 = LONG_LE.getOpaque(mapped, slotBase) as Long
            if (seq1 != seq2) {
                Thread.onSpinWait()
                continue
            }

            val pixelFormat = ShmPixelFormat.fromWire(pixelFormatRaw)
                ?: throw ShmRingException("Unknown pixel_format discriminant $pixelFormatRaw")
            val requiredBytes = pixelFormat.requiredBufferBytes(width, height, stride)
                ?: throw ShmRingException("Corrupt frame dimensions ${width}x$height stride=$stride")
            if (payloadLen < requiredBytes) {
                throw ShmRingException("Slot payload ($payloadLen) smaller than required ($requiredBytes)")
            }

            return ShmFrame(
                meta = ShmFrameMeta(
                    generation = generation,
                    frameIndex = frameIndex,
                    isApproximate = (flags and FLAG_IS_APPROXIMATE) != 0,
                    width = width,
                    height = height,
                    stride = stride,
                    pixelFormat = pixelFormat,
                    boottimeNs = boottimeNs,
                ),
                pixels = pixels,
            )
        }
        throw ShmRingException("Shared-memory frame seqlock did not stabilize under contention")
    }

    override fun close() {
        channel.close()
    }

    companion object {
        const val SHM_SLOT_COUNT: Int = 3
        const val RING_VERSION: Int = 1
        const val CACHE_LINE_BYTES: Int = 64
        const val RING_HEADER_BYTES: Int = 64
        const val SLOT_HEADER_BYTES: Int = 64
        const val HEADER_REGION_BYTES: Int = RING_HEADER_BYTES + SHM_SLOT_COUNT * SLOT_HEADER_BYTES
        const val FLAG_IS_APPROXIMATE: Int = 1 shl 0
        const val NO_PUBLISHED_SLOT: Int = -1 // 0xFFFF_FFFF
        const val MAX_SEQLOCK_RETRIES: Int = 64

        val RING_MAGIC: ByteArray = "ARBSHM01".toByteArray(StandardCharsets.US_ASCII)

        private const val OFF_VERSION = 8
        private const val OFF_SLOT_COUNT = 12
        private const val OFF_SLOT_CAPACITY = 16
        private const val OFF_PUBLISHED_SLOT = 24
        private const val OFF_PUBLISHED_GENERATION = 32

        private const val SLOT_OFF_GENERATION = 8
        private const val SLOT_OFF_FRAME_INDEX = 16
        private const val SLOT_OFF_BOOTTIME_NS = 24
        private const val SLOT_OFF_WIDTH = 32
        private const val SLOT_OFF_HEIGHT = 36
        private const val SLOT_OFF_STRIDE = 40
        private const val SLOT_OFF_PIXEL_FORMAT = 44
        private const val SLOT_OFF_PAYLOAD_LEN = 48
        private const val SLOT_OFF_FLAGS = 52

        private val LONG_LE: VarHandle =
            MethodHandles.byteBufferViewVarHandle(LongArray::class.java, ByteOrder.LITTLE_ENDIAN)
        private val INT_LE: VarHandle =
            MethodHandles.byteBufferViewVarHandle(IntArray::class.java, ByteOrder.LITTLE_ENDIAN)

        fun alignUp(value: Int, align: Int = CACHE_LINE_BYTES): Int {
            val rem = value % align
            return if (rem == 0) value else Math.addExact(value, align - rem)
        }

        @Throws(IOException::class)
        fun open(path: Path): ShmFrameRingReader {
            val channel = FileChannel.open(path, StandardOpenOption.READ)
            try {
                val fileSize = channel.size()
                if (fileSize < HEADER_REGION_BYTES.toLong()) {
                    throw ShmRingException("SHM file too small for header: $fileSize < $HEADER_REGION_BYTES")
                }
                if (fileSize > Int.MAX_VALUE.toLong()) {
                    throw ShmRingException("SHM file size exceeds MappedByteBuffer limit: $fileSize")
                }
                val mapped = channel.map(FileChannel.MapMode.READ_ONLY, 0L, fileSize)
                mapped.order(ByteOrder.LITTLE_ENDIAN)

                val magic = ByteArray(8)
                mapped.get(0, magic, 0, 8)
                if (!magic.contentEquals(RING_MAGIC)) {
                    throw ShmRingException("Unexpected SHM ring magic bytes")
                }

                val version = mapped.getInt(OFF_VERSION)
                if (version != RING_VERSION) {
                    throw ShmRingException("Unsupported SHM ring version $version (expected $RING_VERSION)")
                }

                val slotCount = mapped.getInt(OFF_SLOT_COUNT)
                if (slotCount != SHM_SLOT_COUNT) {
                    throw ShmRingException("Unexpected SHM slot count $slotCount (expected $SHM_SLOT_COUNT)")
                }

                val slotCapacityLong = mapped.getLong(OFF_SLOT_CAPACITY)
                if (slotCapacityLong <= 0L || slotCapacityLong > Int.MAX_VALUE.toLong()) {
                    throw ShmRingException("Invalid SHM slot_capacity: $slotCapacityLong")
                }
                val slotCapacity = slotCapacityLong.toInt()
                val alignedStride = alignUp(slotCapacity, CACHE_LINE_BYTES)
                val requiredBytes = HEADER_REGION_BYTES.toLong() + alignedStride.toLong() * SHM_SLOT_COUNT.toLong()
                if (fileSize < requiredBytes) {
                    throw ShmRingException("SHM backing file truncated: $fileSize < $requiredBytes")
                }

                return ShmFrameRingReader(path, channel, mapped, slotCapacity, alignedStride)
            } catch (e: Throwable) {
                runCatching { channel.close() }
                throw e
            }
        }
    }
}
