package io.github.shinma06.replaybuffer.core

import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.EOFException
import java.io.IOException
import java.io.InputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.attribute.PosixFilePermissions
import java.util.concurrent.atomic.AtomicLong
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertSame
import kotlin.test.assertTrue
import kotlin.concurrent.thread

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

    private fun logBytes(): ByteArray {
        val payload = byteArrayOf(4) + "Fixture\u0000message\u0000".toByteArray()
        return ByteBuffer.allocate(28 + payload.size).order(ByteOrder.LITTLE_ENDIAN)
            .putShort(payload.size.toShort()).putShort(28).putInt(123).putInt(124).putInt(1700000000).putInt(123456789)
            .putInt(0).putInt(10001).put(payload).array()
    }

    @Test
    fun diagnosticObservesExactlyReturnedFragmentsWithoutChangingParserResults() {
        val root = Files.createTempDirectory("replay-input-test-").toRealPath()
        try {
            val original = logBytes() + logBytes()
            val diagnostic = LogInputDiagnostic.open(root)
            val fragmented = object : ByteArrayInputStream(original) {
                override fun read(bytes: ByteArray, offset: Int, length: Int): Int = super.read(bytes, offset, minOf(3, length))
            }
            val input = diagnostic.input(fragmented)
            repeat(2) {
                val start = diagnostic.position()
                val actual = readLog(input)
                val expected = readLog(ByteArrayInputStream(logBytes()))
                assertEquals(expected.copy(raw = actual.raw), actual)
                assertContentEquals(expected.raw, actual.raw)
                diagnostic.parsed(start, it != 0) // Initial tail exclusion stays separately observable.
            }
            assertFailsWith<EOFException> { readLog(input) }
            assertEquals(original.size.toLong(), diagnostic.position())
            assertTrue(diagnostic.finish(3, "fixture"))
            assertContentEquals(original, Files.readAllBytes(root.resolve("input.bin.partial")))
            Files.list(root).use { paths -> paths.forEach {
                assertEquals(PosixFilePermissions.fromString("rw-------"), Files.getPosixFilePermissions(it))
            } }
        } finally { root.toFile().deleteRecursively() }
    }

    @Test
    fun diagnosticRetainsReturnedPrefixAndPropagatesTheOriginalReadException() {
        val root = Files.createTempDirectory("replay-input-exception-").toRealPath()
        try {
            val failure = IOException("test-only")
            val source = object : InputStream() {
                var first = true
                override fun read(): Int = throw failure
                override fun read(bytes: ByteArray, offset: Int, length: Int): Int {
                    if (!first) throw failure
                    first = false
                    logBytes().copyInto(bytes, offset, 0, 4)
                    return 4
                }
            }
            val diagnostic = LogInputDiagnostic.open(root)
            assertSame(failure, assertFailsWith<IOException> { readLog(diagnostic.input(source)) })
            assertFalse(diagnostic.finish(3, "fixture"))
            assertEquals("read_exception", diagnostic.failure())
            assertContentEquals(logBytes().copyOf(4), Files.readAllBytes(root.resolve("input.bin.partial")))
        } finally { root.toFile().deleteRecursively() }
    }

    @Test
    fun diagnosticLimitsAndExpectedShutdownNeverAlterReturnedInput() {
        for (limit in listOf("raw_limit", "metadata_limit", "read_limit", "duration_limit")) {
            val root = Files.createTempDirectory("replay-input-limit-").toRealPath()
            try {
                val time = AtomicLong()
                val diagnostic = LogInputDiagnostic.open(root, rawLimit = if (limit == "raw_limit") 3 else 1024,
                    metadataLimit = if (limit == "metadata_limit") 1 else 1024,
                    readLimit = if (limit == "read_limit") 1 else 32, now = time::get)
                if (limit == "duration_limit") time.set(241_000_000_000)
                assertContentEquals(readLog(ByteArrayInputStream(logBytes())).raw,
                    readLog(diagnostic.input(ByteArrayInputStream(logBytes()))).raw)
                assertFalse(diagnostic.finish(3, "fixture"))
                assertEquals(limit, diagnostic.failure())
                assertTrue(Files.size(root.resolve("input.bin.partial")) <= 1024)
                assertTrue(Files.size(root.resolve("receipt.bin.partial")) <= 1024)
            } finally { root.toFile().deleteRecursively() }
        }
        val root = Files.createTempDirectory("replay-input-close-").toRealPath()
        try {
            val diagnostic = LogInputDiagnostic.open(root)
            val failure = IOException("expected shutdown")
            val input = diagnostic.input(object : InputStream() { override fun read(): Int = throw failure }, { true })
            assertSame(failure, assertFailsWith<IOException> { input.read() })
            assertTrue(diagnostic.finish(3, "fixture"))
        } finally { root.toFile().deleteRecursively() }
    }

    @Test
    fun diagnosticRejectsUnsafeOrReusedRootsAndContainsDumpFailure() {
        val root = Files.createTempDirectory("replay-input-private-").toRealPath()
        try {
            val diagnostic = LogInputDiagnostic.open(root, persist = { _, _, _ -> throw IOException("write failed") })
            readLog(diagnostic.input(ByteArrayInputStream(logBytes())))
            assertFalse(diagnostic.finish(3, "fixture"))
            assertEquals("dump_failed", diagnostic.failure())
            assertFalse(diagnostic.finish(3, "fixture")) // A repeated product stop must not retry or throw.
            assertFalse(Files.exists(root.resolve("receipt.json")))
            assertFailsWith<Exception> { LogInputDiagnostic.open(root) }
            val public = Files.createDirectory(root.resolve("public"))
            Files.setPosixFilePermissions(public, PosixFilePermissions.fromString("rwxr-xr-x"))
            assertFailsWith<IllegalArgumentException> { LogInputDiagnostic.open(public) }
            val alias = root.resolve("alias")
            Files.createSymbolicLink(alias, public)
            assertFailsWith<IllegalArgumentException> { LogInputDiagnostic.open(alias) }
        } finally { root.toFile().deleteRecursively() }
        val existing = Files.createTempDirectory("replay-input-existing-").toRealPath()
        try {
            val diagnostic = LogInputDiagnostic.open(existing)
            Files.writeString(existing.resolve("input.bin.partial"), "preserve")
            readLog(diagnostic.input(ByteArrayInputStream(logBytes())))
            assertFalse(diagnostic.finish(3, "fixture"))
            assertEquals("preserve", Files.readString(existing.resolve("input.bin.partial")))
        } finally { existing.toFile().deleteRecursively() }
    }

    @Test
    fun absentOptInAndMultipleConnectionsNeverInventSharedInputEvidence() {
        val previous = System.getProperty(LogInputDiagnostic.PROPERTY)
        try {
            System.clearProperty(LogInputDiagnostic.PROPERTY)
            assertEquals(null, LogInputDiagnostic.optIn())
        } finally {
            if (previous != null) System.setProperty(LogInputDiagnostic.PROPERTY, previous)
        }
        val root = Files.createTempDirectory("replay-input-connections-").toRealPath()
        try {
            val diagnostic = LogInputDiagnostic.open(root)
            diagnostic.input(ByteArrayInputStream(logBytes()))
            val second = ByteArrayInputStream(logBytes())
            assertSame(second, diagnostic.input(second))
            readLog(second)
            assertFalse(diagnostic.finish(3, "fixture"))
            assertEquals("multiple_connections", diagnostic.failure())
        } finally { root.toFile().deleteRecursively() }
    }

    @Test
    fun diagnosticSerializesReaderAndSaveReceiptsAndKeepsStoppingFailuresPrivate() {
        val root = Files.createTempDirectory("replay-input-threads-").toRealPath()
        try {
            val diagnostic = LogInputDiagnostic.open(root)
            val source = ByteArrayInputStream((0 until 100).fold(byteArrayOf()) { bytes, _ -> bytes + logBytes() })
            val input = diagnostic.input(source)
            val failure = java.util.concurrent.atomic.AtomicReference<Throwable?>()
            val reader = thread {
                try { repeat(100) { val start = diagnostic.position(); readLog(input); diagnostic.parsed(start, true) } }
                catch (error: Throwable) { failure.set(error) }
            }
            val control = thread { repeat(100) { diagnostic.saveStarted(); diagnostic.frozen(null) } }
            reader.join(2000); control.join(2000)
            assertFalse(reader.isAlive || control.isAlive)
            assertEquals(null, failure.get())
            assertTrue(diagnostic.finish(3, "fixture"))
            val counts = mutableMapOf<Char, Int>()
            DataInputStream(Files.newInputStream(root.resolve("receipt.bin.partial"))).use { data ->
                while (data.available() > 0) {
                    val type = data.readUnsignedByte().toChar()
                    counts[type] = (counts[type] ?: 0) + 1
                    val size = when (type) { 'R' -> 28; 'P' -> 25; 'B', 'N' -> 8; else -> error("Unexpected receipt") }
                    assertEquals(size, data.skipBytes(size))
                }
            }
            assertEquals(100, counts['P']); assertEquals(100, counts['B']); assertEquals(100, counts['N'])
        } finally { root.toFile().deleteRecursively() }
    }
}
