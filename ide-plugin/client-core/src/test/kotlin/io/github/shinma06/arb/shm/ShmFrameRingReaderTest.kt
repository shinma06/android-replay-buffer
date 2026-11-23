package io.github.shinma06.arb.shm

import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.lang.invoke.MethodHandles
import java.lang.invoke.VarHandle
import java.nio.ByteOrder
import java.nio.MappedByteBuffer
import java.nio.channels.FileChannel
import java.nio.file.Path
import java.nio.file.StandardOpenOption
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

class ShmFrameRingReaderTest {
    private val longLe: VarHandle =
        MethodHandles.byteBufferViewVarHandle(LongArray::class.java, ByteOrder.LITTLE_ENDIAN)
    private val intLe: VarHandle =
        MethodHandles.byteBufferViewVarHandle(IntArray::class.java, ByteOrder.LITTLE_ENDIAN)

    private fun createRawRingFile(path: Path, slotCapacity: Int): Pair<FileChannel, MappedByteBuffer> {
        val alignedStride = ShmFrameRingReader.alignUp(slotCapacity)
        val totalSize = ShmFrameRingReader.HEADER_REGION_BYTES + alignedStride * ShmFrameRingReader.SHM_SLOT_COUNT
        val ch = FileChannel.open(
            path,
            StandardOpenOption.CREATE,
            StandardOpenOption.TRUNCATE_EXISTING,
            StandardOpenOption.READ,
            StandardOpenOption.WRITE,
        )
        val mapped = ch.map(FileChannel.MapMode.READ_WRITE, 0L, totalSize.toLong())
        mapped.order(ByteOrder.LITTLE_ENDIAN)
        mapped.put(0, ShmFrameRingReader.RING_MAGIC)
        mapped.putInt(8, ShmFrameRingReader.RING_VERSION)
        mapped.putInt(12, ShmFrameRingReader.SHM_SLOT_COUNT)
        mapped.putLong(16, slotCapacity.toLong())
        intLe.setRelease(mapped, 24, ShmFrameRingReader.NO_PUBLISHED_SLOT)
        longLe.setRelease(mapped, 32, 0L)
        return ch to mapped
    }

    private fun publishSlot(
        mapped: MappedByteBuffer,
        slotCapacity: Int,
        slotIdx: Int,
        generation: Long,
        frameIndex: Long,
        isApproximate: Boolean,
        width: Int,
        height: Int,
        stride: Int,
        pixelFormat: ShmPixelFormat,
        boottimeNs: Long,
        pixels: ByteArray,
    ) {
        val alignedStride = ShmFrameRingReader.alignUp(slotCapacity)
        val slotBase = ShmFrameRingReader.RING_HEADER_BYTES + slotIdx * ShmFrameRingReader.SLOT_HEADER_BYTES
        val seqBefore = longLe.getOpaque(mapped, slotBase) as Long
        // Mark odd (writing)
        longLe.setRelease(mapped, slotBase, seqBefore + 1L)
        VarHandle.releaseFence()

        val payloadOffset = ShmFrameRingReader.HEADER_REGION_BYTES + slotIdx * alignedStride
        mapped.put(payloadOffset, pixels)

        mapped.putLong(slotBase + 8, generation)
        mapped.putLong(slotBase + 16, frameIndex)
        mapped.putLong(slotBase + 24, boottimeNs)
        mapped.putInt(slotBase + 32, width)
        mapped.putInt(slotBase + 36, height)
        mapped.putInt(slotBase + 40, stride)
        mapped.putInt(slotBase + 44, pixelFormat.wire)
        mapped.putInt(slotBase + 48, pixels.size)
        mapped.putInt(slotBase + 52, if (isApproximate) ShmFrameRingReader.FLAG_IS_APPROXIMATE else 0)

        // Mark even (stable)
        longLe.setRelease(mapped, slotBase, seqBefore + 2L)
        intLe.setRelease(mapped, 24, slotIdx)
        longLe.setRelease(mapped, 32, generation)
    }

    @Test
    fun readsEmptyApproximateExactAndDecodesRgbaAndBgraWithStride(@TempDir tmp: Path) {
        val ringPath = tmp.resolve("viewport.shm")
        val slotCapacity = 256
        val (writerCh, mapped) = createRawRingFile(ringPath, slotCapacity)

        writerCh.use {
            ShmFrameRingReader.open(ringPath).use { reader ->
                assertEquals(0L, reader.latestGeneration())
                assertNull(reader.readLatest(), "empty ring must return null")

                // 2x2 RGBA8 with padded stride = 12 (8 pixel bytes + 4 pad bytes per row)
                val rgbaPixels = ByteArray(24)
                // (0,0) red, (1,0) green, (0,1) blue, (1,1) white
                byteArrayOf(0xFF.toByte(), 0x10, 0x20, 0xFF.toByte()).copyInto(rgbaPixels, 0)
                byteArrayOf(0x10, 0xEE.toByte(), 0x30, 0xFF.toByte()).copyInto(rgbaPixels, 4)
                byteArrayOf(0x05, 0x15, 0xDD.toByte(), 0xFF.toByte()).copyInto(rgbaPixels, 12)
                byteArrayOf(0xFF.toByte(), 0xFF.toByte(), 0xFF.toByte(), 0xFF.toByte()).copyInto(rgbaPixels, 16)

                publishSlot(
                    mapped = mapped,
                    slotCapacity = slotCapacity,
                    slotIdx = 0,
                    generation = 1L,
                    frameIndex = 12L,
                    isApproximate = true,
                    width = 2,
                    height = 2,
                    stride = 12,
                    pixelFormat = ShmPixelFormat.RGBA8,
                    boottimeNs = 123_456_789L,
                    pixels = rgbaPixels,
                )

                val approx = assertNotNull(reader.readLatest())
                assertEquals(1L, approx.meta.generation)
                assertEquals(12L, approx.meta.frameIndex)
                assertTrue(approx.meta.isApproximate)
                assertEquals(123_456_789L, approx.meta.boottimeNs)
                assertEquals(ShmPixelFormat.RGBA8, approx.meta.pixelFormat)

                val imgRgba = approx.toBufferedImage()
                assertEquals(0xFFFF1020.toInt(), imgRgba.getRGB(0, 0))
                assertEquals(0xFF10EE30.toInt(), imgRgba.getRGB(1, 0))
                assertEquals(0xFF0515DD.toInt(), imgRgba.getRGB(0, 1))
                assertEquals(0xFFFFFFFF.toInt(), imgRgba.getRGB(1, 1))

                // Now publish exact settled frame in slot 1 as BGRA8
                val bgraPixels = byteArrayOf(
                    0x44, 0x33, 0x22, 0xFF.toByte(), // B=44, G=33, R=22, A=FF -> ARGB 0xFF223344
                )
                publishSlot(
                    mapped = mapped,
                    slotCapacity = slotCapacity,
                    slotIdx = 1,
                    generation = 2L,
                    frameIndex = 15L,
                    isApproximate = false,
                    width = 1,
                    height = 1,
                    stride = 4,
                    pixelFormat = ShmPixelFormat.BGRA8,
                    boottimeNs = 200_000_000L,
                    pixels = bgraPixels,
                )

                val exact = assertNotNull(reader.readLatest())
                assertEquals(2L, exact.meta.generation)
                assertEquals(15L, exact.meta.frameIndex)
                assertFalse(exact.meta.isApproximate)
                assertEquals(0xFF223344.toInt(), exact.toBufferedImage().getRGB(0, 0))
            }
        }
    }

    @Test
    fun seqlockPreventsTornFramesUnderConcurrentWriter(@TempDir tmp: Path) {
        // Run multiple back-to-back stress rounds to prove zero flakiness under contention.
        repeat(5) { round ->
            val ringPath = tmp.resolve("concurrent-$round.shm")
            val width = 32
            val height = 32
            val stride = width * 4
            val slotCapacity = stride * height
            val (writerCh, mapped) = createRawRingFile(ringPath, slotCapacity)

            writerCh.use {
                val stop = AtomicBoolean(false)
                val firstPublished = CountDownLatch(1)
                val writerThread = Thread(
                    {
                        var gen = 1L
                        var slot = 0
                        val payload = ByteArray(slotCapacity)
                        while (!stop.get()) {
                            val fillByte = (gen and 0xFFL).toByte()
                            payload.fill(fillByte)
                            publishSlot(
                                mapped = mapped,
                                slotCapacity = slotCapacity,
                                slotIdx = slot,
                                generation = gen,
                                frameIndex = gen,
                                isApproximate = (gen % 2L == 1L),
                                width = width,
                                height = height,
                                stride = stride,
                                pixelFormat = ShmPixelFormat.RGBA8,
                                boottimeNs = gen * 1_000L,
                                pixels = payload,
                            )
                            if (gen == 1L) {
                                firstPublished.countDown()
                            }
                            gen++
                            slot = (slot + 1) % ShmFrameRingReader.SHM_SLOT_COUNT
                            // Pace every generation (a tight writer starves the reader on 2-vCPU
                            // Windows runners). 20us still overlaps writes with reads many times.
                            java.util.concurrent.locks.LockSupport.parkNanos(20_000L)
                        }
                    },
                    "shm-test-writer-$round",
                ).apply { start() }

                try {
                    assertTrue(firstPublished.await(5, TimeUnit.SECONDS), "writer did not publish initial frame")
                    ShmFrameRingReader.open(ringPath).use { reader ->
                        var observed = 0
                        var lastGen = -1L
                        var distinctGenerations = 0
                        val deadlineNs = System.nanoTime() + TimeUnit.SECONDS.toNanos(30)
                        while ((observed < 200 || distinctGenerations < 2) && System.nanoTime() < deadlineNs) {
                            val frame = reader.readLatest() ?: continue
                            val expectedByte = (frame.meta.generation and 0xFFL).toByte()
                            assertEquals(frame.meta.generation, frame.meta.frameIndex)
                            assertTrue(
                                frame.pixels.all { it == expectedByte },
                                "Seqlock must never return a torn frame (round=$round, gen=${frame.meta.generation})",
                            )
                            if (frame.meta.generation != lastGen) {
                                distinctGenerations++
                                lastGen = frame.meta.generation
                            }
                            observed++
                        }
                        assertTrue(observed >= 200, "Expected >=200 consistent concurrent reads in round $round, got $observed")
                        assertTrue(
                            distinctGenerations >= 2,
                            "Expected concurrent writer progress across multiple generations, got $distinctGenerations",
                        )
                    }
                } finally {
                    stop.set(true)
                    writerThread.join(5_000L)
                    assertFalse(writerThread.isAlive, "writerThread must stop cleanly")
                }
            }
        }
    }

    @Test
    fun throwsWhenSeqlockRemainsStuckOdd(@TempDir tmp: Path) {
        val ringPath = tmp.resolve("stuck-odd.shm")
        val slotCapacity = 64
        val (writerCh, mapped) = createRawRingFile(ringPath, slotCapacity)

        writerCh.use {
            publishSlot(
                mapped = mapped,
                slotCapacity = slotCapacity,
                slotIdx = 0,
                generation = 1L,
                frameIndex = 1L,
                isApproximate = false,
                width = 1,
                height = 1,
                stride = 4,
                pixelFormat = ShmPixelFormat.RGBA8,
                boottimeNs = 1_000L,
                pixels = byteArrayOf(1, 2, 3, 4),
            )
            // Simulate a crashed writer leaving slot 0's seqlock odd (write in progress).
            val slot0Base = ShmFrameRingReader.RING_HEADER_BYTES
            val seq = longLe.getOpaque(mapped, slot0Base) as Long
            longLe.setRelease(mapped, slot0Base, seq + 1L)

            ShmFrameRingReader.open(ringPath).use { reader ->
                val ex = assertFailsWith<ShmRingException> { reader.readLatest() }
                assertTrue(ex.message!!.contains("seqlock"), "Unexpected error: ${ex.message}")
            }
        }
    }

    @Test
    fun rejectsCorruptMagicAndTruncatedFiles(@TempDir tmp: Path) {
        val badMagic = tmp.resolve("bad-magic.shm")
        val (ch, mapped) = createRawRingFile(badMagic, 64)
        mapped.put(0, "BADMAGIC".toByteArray())
        ch.close()
        assertFailsWith<ShmRingException> { ShmFrameRingReader.open(badMagic) }

        val truncated = tmp.resolve("truncated.shm")
        FileChannel.open(
            truncated,
            StandardOpenOption.CREATE,
            StandardOpenOption.WRITE,
            StandardOpenOption.TRUNCATE_EXISTING,
        ).use { it.write(java.nio.ByteBuffer.allocate(32)) }
        assertFailsWith<ShmRingException> { ShmFrameRingReader.open(truncated) }
    }
}
