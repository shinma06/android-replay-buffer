package io.github.shinma06.replaybuffer.core

import com.google.gson.Gson
import com.google.gson.JsonObject
import java.nio.ByteBuffer
import java.nio.channels.ReadableByteChannel
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.TimeUnit
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class SavedReplayTest {
    @Test
    fun utf8LinesAreReadAcrossBufferBoundariesAndTheChannelIsClosed() {
        val channel = SmallReads("日本語😀\n\n最終行".toByteArray())
        val lines = mutableListOf<String>()
        readReplayLines(channel, lines::add)
        assertEquals(listOf("日本語😀", "", "最終行"), lines)
        assertFalse(channel.isOpen)
    }

    @Test
    fun aConsumerFailureClosesTheChannelBeforeItEscapes() {
        val channel = SmallReads("先頭\n次\n".toByteArray())
        val failure = IllegalStateException("stop")
        assertEquals(failure, assertFailsWith<IllegalStateException> {
            readReplayLines(channel) { throw failure }
        })
        assertFalse(channel.isOpen)
    }

    @Test
    fun emptyFilesFinalNewlinesAndMalformedUtf8HaveStableLineSemantics() {
        val cases = listOf(
            byteArrayOf() to emptyList(),
            "\n".toByteArray() to listOf(""),
            "a\r\nb\n\n".toByteArray() to listOf("a", "b", ""),
            byteArrayOf(0xc3.toByte(), 0x28) to listOf("\uFFFD(")
        )
        for ((bytes, expected) in cases) {
            val lines = mutableListOf<String>()
            val channel = SmallReads(bytes)
            readReplayLines(channel, lines::add)
            assertEquals(expected, lines)
            assertFalse(channel.isOpen)
        }
    }

    @Test
    fun largeJsonlCanBeConsumedInASmallHeap() {
        val directory = Files.createTempDirectory("saved-reader-").toRealPath()
        try {
            Files.write(directory.resolve(ReplayFiles.VIDEO), byteArrayOf())
            Files.writeString(directory.resolve(ReplayFiles.VIDEO_FRAMES),
                "{\"presented\":true,\"movie_pts_us\":\"0\",\"movie_end_us\":\"1\"}\n")
            val row = "{\"t_ns\":\"0\",\"message\":\"日本語😀${"x".repeat(4096)}\"}\n".toByteArray(Charsets.UTF_8)
            val count = 16_384
            Files.newOutputStream(directory.resolve(ReplayFiles.LOGCAT_DEVICE)).buffered().use { output ->
                repeat(count) { output.write(row) }
            }
            val hashes = JsonObject()
            for (name in listOf(ReplayFiles.VIDEO, ReplayFiles.VIDEO_FRAMES, ReplayFiles.LOGCAT_DEVICE)) {
                val digest = java.security.MessageDigest.getInstance("SHA-256")
                Files.newInputStream(directory.resolve(name)).use { input ->
                    val buffer = ByteArray(8192)
                    while (true) {
                        val size = input.read(buffer)
                        if (size < 0) break
                        digest.update(buffer, 0, size)
                    }
                }
                hashes.addProperty(name, digest.digest().joinToString("") { "%02x".format(it) })
            }
            val record = JsonObject().apply {
                addProperty("schema", ReplayFiles.SCHEMA)
                addProperty("complete", true)
                add("files", JsonObject().apply {
                    addProperty("video", ReplayFiles.VIDEO)
                    addProperty("video_frames", ReplayFiles.VIDEO_FRAMES)
                })
                add("files_sha256", hashes)
            }
            Files.writeString(directory.resolve(ReplayFiles.RECORD), Gson().toJson(record))

            fun location(resource: java.net.URL, suffix: String): String =
                if (resource.protocol == "jar") Path.of(java.net.URI.create(resource.toString().removePrefix("jar:").substringBefore("!/"))).toString()
                else Path.of(resource.toURI()).toString().removeSuffix(suffix.replace('/', java.io.File.separatorChar))
            val locations = listOf(SavedReplayReadFixture::class.java, SavedReplay::class.java,
                kotlin.Unit::class.java, Gson::class.java, com.sun.jna.Native::class.java).map {
                val suffix = "/${it.name.replace('.', '/')}.class"
                location(it.getResource(suffix)!!, suffix)
            } + location(SavedReplay::class.java.getResource("/replay/deps/identity.properties")!!, "/replay/deps/identity.properties")
            val jna = System.getProperties().stringPropertyNames().filter { it.startsWith("jna.") }.sorted()
                .map { "-D$it=${System.getProperty(it)}" }
            val process = ProcessBuilder(listOf(Path.of(System.getProperty("java.home"), "bin", "java").toString()) + jna + listOf(
                "-Xmx32m", "-cp", locations.distinct().joinToString(java.io.File.pathSeparator),
                SavedReplayReadFixture::class.java.name, directory.toString(), count.toString()))
                .redirectErrorStream(true).start()
            try {
                assertTrue(process.waitFor(60, TimeUnit.SECONDS), "reader fixture did not finish")
                val output = process.inputStream.bufferedReader().use { it.readText() }
                assertEquals(0, process.exitValue(), output)
                assertTrue(output.trimEnd().endsWith("rows=$count"), output)
            } finally {
                if (process.isAlive) process.destroyForcibly().waitFor()
                process.inputStream.close()
                process.outputStream.close()
            }
        } finally {
            directory.toFile().deleteRecursively()
        }
    }

    private class SmallReads(private val bytes: ByteArray) : ReadableByteChannel {
        private var offset = 0
        private var open = true
        override fun isOpen(): Boolean = open
        override fun close() { open = false }
        override fun read(destination: ByteBuffer): Int {
            check(open)
            if (offset == bytes.size) return -1
            val size = minOf(3, destination.remaining(), bytes.size - offset)
            destination.put(bytes, offset, size)
            offset += size
            return size
        }
    }
}

internal object SavedReplayReadFixture {
    @JvmStatic
    fun main(args: Array<String>) {
        var count = 0
        SavedReplay.open(Path.of(args[0])).use { saved ->
            saved.forEachLine(ReplayFiles.LOGCAT_DEVICE) { line ->
                check(line.contains("日本語😀"))
                count++
            }
        }
        check(count == args[1].toInt())
        println("rows=$count")
    }
}
