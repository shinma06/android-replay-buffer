package io.github.shinma06.replaybuffer.core

import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import java.nio.channels.Channels
import java.nio.channels.ClosedByInterruptException
import java.nio.channels.ClosedChannelException
import java.nio.channels.ReadableByteChannel
import java.nio.channels.WritableByteChannel
import java.nio.file.Files
import java.nio.file.Path
import java.security.MessageDigest
import java.util.HexFormat
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import org.junit.jupiter.api.condition.EnabledOnOs
import org.junit.jupiter.api.Timeout
import org.junit.jupiter.api.condition.OS

class SaveOutputChannelTest {
    private val root: Path = Files.createTempDirectory("replay-save-channel-").toRealPath()

    @AfterTest
    fun cleanUp() {
        Files.walk(root).use { it.sorted(java.util.Comparator.reverseOrder()).forEach(Files::deleteIfExists) }
    }

    private fun bytes(text: String) = ByteBuffer.wrap(text.toByteArray())

    @Test
    fun channelWritesTheHeldFileAfterItsNameIsSwapped() {
        SaveOutputDirectory.open(root).use { directory ->
            directory.createFile("out").use { output ->
                Files.move(root.resolve("out"), root.resolve("moved"))
                Files.writeString(root.resolve("out"), "decoy")
                output.channel().use { it.write(bytes("held")) }
                assertEquals("held", Files.readString(root.resolve("moved")))
                assertEquals("decoy", Files.readString(root.resolve("out")))
                val expected = HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256").digest("held".toByteArray()))
                assertEquals(expected, output.hash())
                assertFailsWith<IllegalStateException> { directory.verifyFile("out", output) }
                directory.verifyFile("moved", output)
            }
        }
    }

    @Test
    fun channelBehavesLikeAFileChannelOnEveryOs() {
        SaveOutputDirectory.open(root).use { directory ->
            directory.createFile("out").use { output ->
                output.channel().use { channel ->
                    assertEquals(11, channel.write(bytes("hello world")))
                    assertEquals(11, channel.size())
                    assertEquals(11, channel.position())
                    val word = ByteBuffer.allocate(5)
                    assertEquals(5, channel.read(word, 6))
                    assertEquals("world", String(word.array()))
                    assertEquals(11, channel.position())
                    assertEquals(-1, channel.read(ByteBuffer.allocate(1)))
                    channel.truncate(5)
                    assertEquals(5, channel.size())
                    assertEquals(5, channel.position())
                    channel.truncate(9)
                    assertEquals(5, channel.size())
                    assertEquals(1, channel.write(bytes("!"), 7))
                    assertEquals(8, channel.size())
                    assertEquals(5, channel.position())
                    assertEquals(4, channel.write(arrayOf(bytes("ab"), ByteBuffer.allocateDirect(2).put("cd".toByteArray()).flip())))
                    channel.force(true)
                    val head = ByteBuffer.allocateDirect(3)
                    val tail = ByteBuffer.allocate(20)
                    channel.position(0)
                    assertEquals(9, channel.read(arrayOf(head, tail)))
                    assertEquals(-1, channel.read(arrayOf(ByteBuffer.allocate(1))))
                    assertContentEquals("hel".toByteArray(), ByteArray(3).also { head.flip().get(it) })
                    assertEquals("loabcd", String(tail.array(), 0, tail.position()))
                    val copied = ByteArrayOutputStream()
                    assertEquals(9, channel.transferTo(0, 100, Channels.newChannel(copied)))
                    assertEquals("helloabcd", copied.toString())
                    assertEquals(3, channel.transferFrom(Channels.newChannel(ByteArrayInputStream("XYZ".toByteArray())), 1, 100))
                    assertEquals(1, channel.transferFrom(Channels.newChannel(ByteArrayInputStream("Q".toByteArray())), 12, 1))
                    channel.truncate(9)
                }
                assertEquals("hXYZoabcd", Files.readString(root.resolve("out")))
                val channel = output.channel()
                channel.close()
                assertFailsWith<ClosedChannelException> { channel.write(bytes("x")) }
                output.channel().use { interrupted ->
                    Thread.currentThread().interrupt()
                    try { assertFailsWith<ClosedByInterruptException> { interrupted.write(bytes("x")) } }
                    finally { Thread.interrupted() }
                    assertFalse(interrupted.isOpen)
                }
                assertEquals("hXYZoabcd", Files.readString(root.resolve("out")))
            }
        }
    }

    /** Like a non-blocking socket: accepts [limit] bytes, then makes no progress. */
    private class Stalling(private val limit: Int) : WritableByteChannel, ReadableByteChannel {
        val written = ByteArrayOutputStream()
        private var read = 0
        override fun write(src: ByteBuffer): Int {
            val count = minOf(src.remaining(), limit - written.size())
            repeat(count) { written.write(src.get().toInt()) }
            return count
        }
        override fun read(dst: ByteBuffer): Int {
            val count = minOf(dst.remaining(), limit - read)
            repeat(count) { dst.put(('a'.code + read++ % 26).toByte()) }
            return count
        }
        override fun isOpen() = true
        override fun close() {}
    }

    @Test
    @Timeout(30)
    fun transfersStopWhenTheOtherChannelMakesNoProgress() {
        SaveOutputDirectory.open(root).use { directory ->
            directory.createFile("out").use { output ->
                output.channel().use { channel ->
                    channel.write(bytes("hello world"))
                    val target = Stalling(3)
                    assertEquals(3, channel.transferTo(0, 11, target))
                    assertEquals("hel", target.written.toString())
                    assertEquals(2, channel.transferFrom(Stalling(2), 11, 10))
                    assertEquals(13, channel.size())
                    assertEquals(11, channel.position())
                }
                assertEquals("hello worldab", Files.readString(root.resolve("out")))
            }
        }
    }

    /** Reopening by handle cannot observe a swap, so feed adopt() a different file directly. */
    @Test
    @EnabledOnOs(OS.WINDOWS)
    fun windowsRejectsAChannelThatIsNotTheHeldFile() {
        SaveOutputDirectory.open(root).use { directory ->
            directory.createFile("held").close()
            directory.createFile("other").close()
            val held = WindowsSaveHandles.openFile(directory.descriptor, "held")
            try {
                val other = WindowsSaveHandles.openFile(directory.descriptor, "other")
                val failure = assertFailsWith<IllegalStateException> { WindowsSaveHandles.adopt(held, other, true, false) }
                assertEquals("保存出力の実体が変更されています", failure.message)
                val same = WindowsSaveHandles.openFile(directory.descriptor, "held")
                WindowsSaveHandles.adopt(held, same, true, false).use { assertEquals(0, it.size()) }
            } finally { WindowsSaveHandles.close(held) }
        }
    }
}
