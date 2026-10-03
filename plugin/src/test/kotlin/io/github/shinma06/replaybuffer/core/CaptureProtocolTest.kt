package io.github.shinma06.replaybuffer.core

import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.EOFException
import java.nio.ByteBuffer
import java.nio.ByteOrder
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith

class CaptureProtocolTest {
    @Test
    fun fixedScrcpySessionIsTwelveBytesAndDoesNotConsumeTheNextFrame() {
        val bytes = ByteArrayOutputStream()
        DataOutputStream(bytes).use {
            it.writeInt(Int.MIN_VALUE + 1); it.writeInt(1920); it.writeInt(1080)
            it.writeLong((1L shl 61) or 123456); it.writeInt(4); it.write(byteArrayOf(0, 0, 1, 5))
        }
        val input = DataInputStream(ByteArrayInputStream(bytes.toByteArray()))
        assertEquals(VideoPacket.Session(1920, 1080), readVideo(input))
        val frame = readVideo(input) as VideoPacket.Frame
        assertEquals(123456, frame.pts)
        assertEquals(true, frame.key)
        assertContentEquals(byteArrayOf(0, 0, 1, 5), frame.bytes)
        assertEquals(0, input.available())
        assertFailsWith<EOFException> { readVideo(input) }
    }

    @Test
    fun oversizedOrBrokenPacketsAreRejectedBeforeAllocationAndBinaryLogsPreservePayload() {
        val bad = ByteBuffer.allocate(12).putLong(1).putInt(Int.MAX_VALUE).array()
        assertFailsWith<IllegalArgumentException> { readVideo(DataInputStream(ByteArrayInputStream(bad))) }
        val payload = byteArrayOf(6, 0, -1, 7)
        val bytes = ByteBuffer.allocate(28 + payload.size).order(ByteOrder.LITTLE_ENDIAN)
            .putShort(payload.size.toShort()).putShort(28).putInt(123).putInt(124).putInt(1700000000).putInt(123456789)
            .putInt(2).putInt(10001).put(payload).array()
        val log = readLog(ByteArrayInputStream(bytes))
        assertEquals(1700000000123456789, log.wall)
        assertEquals(10001, log.uid)
        assertEquals("binary", log.decodeStatus)
        assertContentEquals(payload, log.raw)
        assertFailsWith<EOFException> { readLog(ByteArrayInputStream(bytes.copyOf(bytes.size - 1))) }
        bytes[2] = 127
        assertFailsWith<IllegalArgumentException> { readLog(ByteArrayInputStream(bytes)) }
    }
}
