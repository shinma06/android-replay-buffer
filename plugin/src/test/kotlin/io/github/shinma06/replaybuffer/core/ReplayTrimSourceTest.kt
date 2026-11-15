package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonParser
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import org.junit.jupiter.params.ParameterizedTest
import org.junit.jupiter.params.provider.ValueSource
import java.io.IOException
import java.nio.ByteBuffer
import java.nio.MappedByteBuffer
import java.nio.channels.FileChannel
import java.nio.channels.FileLock
import java.nio.channels.ReadableByteChannel
import java.nio.channels.WritableByteChannel
import java.nio.file.Files
import java.nio.file.Path
import java.security.MessageDigest
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith

class ReplayTrimSourceTest {
    @TempDir lateinit var root: Path

    private class ShortReads(val channel: FileChannel, val chunk: Int) : FileChannel() {
        var reads = 0
        override fun read(dst: ByteBuffer, position: Long): Int {
            reads++
            val limit = dst.limit()
            dst.limit(dst.position() + minOf(chunk, dst.remaining()))
            return try { channel.read(dst, position) } finally { dst.limit(limit) }
        }
        override fun implCloseChannel() = channel.close()
        override fun read(dst: ByteBuffer) = channel.read(dst)
        override fun read(dsts: Array<out ByteBuffer>, offset: Int, length: Int) = channel.read(dsts, offset, length)
        override fun write(src: ByteBuffer) = channel.write(src)
        override fun write(src: ByteBuffer, position: Long) = channel.write(src, position)
        override fun write(srcs: Array<out ByteBuffer>, offset: Int, length: Int) = channel.write(srcs, offset, length)
        override fun position() = channel.position()
        override fun position(value: Long): FileChannel = apply { channel.position(value) }
        override fun size() = channel.size()
        override fun truncate(size: Long): FileChannel = apply { channel.truncate(size) }
        override fun force(metaData: Boolean) = channel.force(metaData)
        override fun transferTo(position: Long, count: Long, target: WritableByteChannel) = channel.transferTo(position, count, target)
        override fun transferFrom(src: ReadableByteChannel, position: Long, count: Long) = channel.transferFrom(src, position, count)
        override fun map(mode: MapMode, position: Long, size: Long): MappedByteBuffer = channel.map(mode, position, size)
        override fun lock(position: Long, size: Long, shared: Boolean): FileLock = channel.lock(position, size, shared)
        override fun tryLock(position: Long, size: Long, shared: Boolean): FileLock? = channel.tryLock(position, size, shared)
    }

    private fun source(size: Int, chunk: Int, test: (ShortReads, ByteArray) -> Unit) {
        val bytes = ByteArray(size) { (it * 31).toByte() }
        val file = root.resolve("source-$size-$chunk")
        Files.write(file, bytes)
        ShortReads(FileChannel.open(file), chunk).use { test(it, bytes) }
        assertContentEquals(bytes, Files.readAllBytes(file))
    }

    @ParameterizedTest
    @ValueSource(ints = [65536, 1048613])
    fun exactConfigAndLargePacketReadsAdvanceOffsetsAndPreserveBytesAndHash(size: Int) {
        source(size + 11, 4093) { channel, bytes ->
            val actual = readTrimSource(channel, 11, size.toLong()) {}.let { ByteArray(it.remaining()).also(it::get) }
            val expected = bytes.copyOfRange(11, bytes.size)
            assertEquals(size, actual.size)
            assertContentEquals(expected, actual)
            assertContentEquals(MessageDigest.getInstance("SHA-256").digest(expected), MessageDigest.getInstance("SHA-256").digest(actual))
            assertEquals(0L, channel.position(), "positional reads must not move the channel cursor")
        }
    }

    @Test fun earlyEofIsNotAcceptedAsACompletePacket() = source(100, 7) { channel, _ ->
        assertFailsWith<IOException> { readTrimSource(channel, 11, 100) {} }
    }

    @Test fun zeroProgressStopsInsteadOfPublishingOrSpinning() = source(100, 0) { channel, _ ->
        assertFailsWith<IOException> { readTrimSource(channel, 0, 100) {} }
        assertEquals(1, channel.reads)
    }

    @Test fun cancellationBetweenShortReadsStopsBeforeTheNextRead() = source(100, 7) { channel, _ ->
        assertFailsWith<SaveCancelled> { readTrimSource(channel, 0, 100) { if (channel.reads == 3) throw SaveCancelled(null) } }
        assertEquals(3, channel.reads)
    }

    @Test fun priorSourcePlacementIsComposedWithNewPaddingWithoutScaling() {
        val row = JsonParser.parseString("""{"output_width":32,"output_height":32,"source_rect":{"x":4,"y":0,"width":24,"height":32,"padding_left":4,"padding_top":0,"padding_right":4,"padding_bottom":0}}""").asJsonObject
        val actual = trimSourceRectangle(row, mapOf("x" to 16, "y" to 16, "width" to 32, "height" to 32), 64, 64)
        assertEquals(JsonParser.parseString("""{"x":20,"y":16,"width":24,"height":32,"padding_left":20,"padding_top":16,"padding_right":20,"padding_bottom":16}"""), actual)
    }

    @Test fun normalizedInputWithoutPlacementIsRejectedRatherThanGuessed() {
        val row = JsonParser.parseString("""{"reencoded":true,"source_rect":null}""").asJsonObject
        assertFailsWith<IllegalArgumentException> {
            trimSourceRectangle(row, mapOf("x" to 0, "y" to 0, "width" to 64, "height" to 64), 64, 64)
        }
    }
}
