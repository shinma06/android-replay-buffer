package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonParser
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.ByteBuffer
import java.nio.file.Files
import java.nio.file.Path
import java.security.MessageDigest
import java.time.ZonedDateTime
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

class ReplayTrimParityTest {
    @TempDir lateinit var root: Path

    @Test
    fun trimAndPngMatchTheSameByteDigestsOnEveryOs() {
        val source = Files.createDirectory(root.resolve("replay-20240101-000000-manual-feedface"))
        val names = listOf("README.md", "replay.json", "video.mp4", "video-source.h264", "video-frames.jsonl",
            "logcat-device.jsonl", "logcat-app.jsonl", "touches.jsonl")
        for (name in names) {
            resource("${source.fileName}/$name").use { Files.copy(it, source.resolve(name)) }
        }
        val before = names.associateWith { hash(Files.readAllBytes(source.resolve(it))) }
        val at = ZonedDateTime.parse("2024-01-02T03:04:05Z")
        val trimmed = ReplayTrim().trim(source, 200_001, 1_900_001, { false },
            { Files.move(it.partial, it.complete) }, at, "deadbeef")
        SavedReplay.open(trimmed).use { assertTrue(it.manifest["complete"].asBoolean) }
        val bytes = names.associateWith { Files.readAllBytes(trimmed.resolve(it)) }.toMutableMap()
        val videoHash = hash(bytes.getValue("video.mp4"))
        bytes["video.mp4"] = withoutMp4Dates(bytes.getValue("video.mp4"))
        val record = JsonParser.parseString(bytes.getValue("replay.json").toString(Charsets.UTF_8)).asJsonObject
        // Only the generated ID and hashes derived from MP4 creation/modification dates may differ.
        bytes["replay.json"] = bytes.getValue("replay.json").toString(Charsets.UTF_8)
            .replace(record["save_id"].asString, "generated-id")
            .replace(videoHash, hash(bytes.getValue("video.mp4"))).toByteArray(Charsets.UTF_8)
        val png = ReplayFrameExport.export(trimmed, 0, root, at, "deadbeef")
        bytes["frame.png"] = Files.readAllBytes(png)
        val actual = bytes.toSortedMap().entries.joinToString("\n", postfix = "\n") { (name, data) -> "$name ${hash(data)}" }
        val expected = resource("output.sha256").bufferedReader().use { it.readText() }
        assertEquals(expected, actual)
        assertEquals(before, names.associateWith { hash(Files.readAllBytes(source.resolve(it))) })
    }

    private fun resource(name: String) = assertNotNull(javaClass.getResourceAsStream("/replay-trim-parity/$name"))

    private fun hash(bytes: ByteArray) = MessageDigest.getInstance("SHA-256").digest(bytes)
        .joinToString("") { "%02x".format(it) }

    private fun withoutMp4Dates(input: ByteArray): ByteArray = input.copyOf().also { bytes ->
        val data = ByteBuffer.wrap(bytes)
        fun boxes(start: Int, end: Int) {
            var offset = start
            while (offset < end) {
                val shortLength = data.getInt(offset)
                val size = when (shortLength) {
                    1 -> data.getLong(offset + 8)
                    0 -> (end - offset).toLong()
                    else -> shortLength.toLong()
                }
                assertTrue(size >= 8 && size <= end - offset)
                val length = size.toInt()
                when (bytes.copyOfRange(offset + 4, offset + 8).toString(Charsets.US_ASCII)) {
                    "moov", "trak", "mdia" -> boxes(offset + 8, offset + length)
                    "mvhd", "tkhd", "mdhd" -> {
                        val count = if (bytes[offset + 8].toInt() == 1) 16 else 8
                        assertTrue(length >= 12 + count)
                        bytes.fill(0, offset + 12, offset + 12 + count)
                    }
                }
                offset += length
            }
        }
        boxes(0, bytes.size)
    }
}
