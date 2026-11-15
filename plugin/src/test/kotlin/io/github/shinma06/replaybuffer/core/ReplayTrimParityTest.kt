package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonParser
import org.jcodec.common.io.NIOUtils
import org.jcodec.containers.mp4.MP4Packet
import org.jcodec.containers.mp4.demuxer.AbstractMP4DemuxerTrack
import org.jcodec.containers.mp4.demuxer.MP4Demuxer
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.ByteBuffer
import java.nio.file.Files
import java.nio.file.Path
import java.security.MessageDigest
import java.time.ZonedDateTime
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

class ReplayTrimParityTest {
    @TempDir lateinit var root: Path

    private val names = listOf("README.md", "replay.json", "video.mp4", "video-source.h264", "video-frames.jsonl",
        "logcat-device.jsonl", "logcat-app.jsonl", "touches.jsonl")

    private fun fixture(): Path {
        val source = Files.createDirectory(root.resolve("replay-20240101-000000-manual-feedface"))
        for (name in names) resource("${source.fileName}/$name").use { Files.copy(it, source.resolve(name)) }
        return source
    }

    @Test
    fun zeroUncertaintyFactsOnBothWindowBoundariesAreKept() {
        val source = fixture()
        for (from in listOf(300_000L, 500_000L)) {
            val trimmed = ReplayTrim().trim(source, from, 800_000, { false }, { Files.move(it.partial, it.complete) })
            val logs = Files.readAllLines(trimmed.resolve("logcat-device.jsonl")).map { JsonParser.parseString(it).asJsonObject }
            if (from == 300_000L) assertTrue(logs.any { it["record_id"].asString == "1" && it["t_ns"].asString == "0" })
            assertTrue(logs.any { it["record_id"].asString == "2" && it["t_ns"].asString.toLong() == (800_000 - from) * 1000 })
            if (from == 500_000L) {
                val touches = Files.readAllLines(trimmed.resolve("touches.jsonl")).map { JsonParser.parseString(it).asJsonObject }
                assertTrue(touches.any { it["t_ns"].isJsonPrimitive && it["t_ns"].asString == "0" })
            }
        }
    }

    @Test
    fun trimAndPngMatchTheSameByteDigestsOnEveryOs() {
        val source = fixture()
        val before = names.associateWith { hash(Files.readAllBytes(source.resolve(it))) }
        val sourceAudio = audioPackets(source, listOf(
            PlaybackAudioSegment(0, 640_000, 0, 30),
            PlaybackAudioSegment(1_100_001, 1_066_666, 30 * 1024, 50),
        ), 1.706666)
        val at = ZonedDateTime.parse("2024-01-02T03:04:05Z")
        val trimmed = ReplayTrim().trim(source, 200_001, 1_900_001, { false },
            { Files.move(it.partial, it.complete) }, at, "deadbeef")
        SavedReplay.open(trimmed).use { assertTrue(it.manifest["complete"].asBoolean) }
        assertEquals(sourceAudio.subList(9, 30) + sourceAudio.subList(30, 68), audioPackets(trimmed, listOf(
            PlaybackAudioSegment(0, 440_000, 384, 21),
            PlaybackAudioSegment(900_001, 800_000, 21 * 1024, 38),
        ), 1.24))
        val bytes = names.associateWith { Files.readAllBytes(trimmed.resolve(it)) }.toMutableMap()
        val videoHash = hash(bytes.getValue("video.mp4"))
        bytes["video.mp4"] = withoutMp4Dates(bytes.getValue("video.mp4"))
        val record = JsonParser.parseString(bytes.getValue("replay.json").toString(Charsets.UTF_8)).asJsonObject
        val boundary = record["video_run_boundaries"].asJsonArray.single().asJsonObject
        assertTrue(boundary["actual_gap_duration_us"].isJsonNull)
        assertEquals("0", boundary["authored_gap_duration_us"].asString)
        assertEquals(hash(Files.readAllBytes(source.resolve("replay.json"))),
            record["trim"].asJsonObject["source_replay_sha256"].asString)
        // Only generated IDs and hashes derived from MP4 creation/modification dates may differ.
        bytes["replay.json"] = bytes.getValue("replay.json").toString(Charsets.UTF_8)
            .replace(record["save_id"].asString, "generated-id")
            .replace(videoHash, hash(bytes.getValue("video.mp4"))).toByteArray(Charsets.UTF_8)
        val png = ReplayFrameExport.export(trimmed, 0, root, at, "deadbeef")
        bytes["frame.png"] = withoutGeneratedPngId(Files.readAllBytes(png), record["save_id"].asString)
        val trimmedBefore = names.associateWith { hash(Files.readAllBytes(trimmed.resolve(it))) }
        val retrimmed = ReplayTrim().trim(trimmed, 100_001, 1_500_001, { false },
            { Files.move(it.partial, it.complete) }, at, "cafebabe")
        assertEquals(sourceAudio.subList(14, 30) + sourceAudio.subList(30, 59), audioPackets(retrimmed, listOf(
            PlaybackAudioSegment(0, 340_000, 64, 16),
            PlaybackAudioSegment(800_001, 600_000, 16 * 1024, 29),
        ), 0.94))
        bytes["retrim-video.mp4"] = withoutMp4Dates(Files.readAllBytes(retrimmed.resolve("video.mp4")))
        val retrimmedRecord = JsonParser.parseString(Files.readString(retrimmed.resolve("replay.json"))).asJsonObject
        bytes["retrim-audio.json"] = retrimmedRecord["audio_track"].toString().toByteArray(Charsets.UTF_8)
        val actual = bytes.toSortedMap().entries.joinToString("\n", postfix = "\n") { (name, data) -> "$name ${hash(data)}" }
        val expected = resource("output.sha256").bufferedReader().use { it.readText() }
        assertEquals(expected, actual)
        assertEquals(before, names.associateWith { hash(Files.readAllBytes(source.resolve(it))) })
        assertEquals(trimmedBefore, names.associateWith { hash(Files.readAllBytes(trimmed.resolve(it))) })
    }

    private fun audioPackets(dir: Path, expected: List<PlaybackAudioSegment>, availableSeconds: Double): List<String> {
        SavedReplay.open(dir).use { saved ->
            assertTrue(saved.manifest["complete"].asBoolean)
            val info = saved.manifest["audio_track"].asJsonObject
            assertEquals("video.mp4", info["file"].asString)
            assertEquals("AAC-LC", info["codec"].asString)
            assertEquals(48_000, info["sample_rate"].asInt)
            assertEquals(1, info["channels"].asInt)
            val coverage = saved.manifest["coverage"].asJsonObject["audio"].asJsonObject
            assertEquals("CAPTURING", coverage["state"].asString)
            assertTrue(coverage["requested"].asBoolean)
            assertTrue(coverage["reason"].isJsonNull)
            assertEquals(availableSeconds, coverage["availableSeconds"].asDouble, 1e-9)
            assertNotNull(ReplayAudioTrack.open(saved)).use { audio ->
                assertContentEquals(byteArrayOf(0x11, 0x88.toByte()), audio.config)
                assertEquals(48_000, audio.sampleRate)
                assertEquals(1, audio.channels)
                assertEquals(expected, audio.segments)
                assertEquals(listOf(AudioChunk.Silence(22_080)), audio.schedule(0).filterIsInstance<AudioChunk.Silence>())
            }
        }
        return NIOUtils.readableChannel(dir.resolve("video.mp4").toFile()).use { input ->
            MP4Demuxer.createRawMP4Demuxer(input).use { demux ->
                assertEquals(1, demux.videoTracks.size)
                val audio = demux.audioTracks.single() as AbstractMP4DemuxerTrack
                val (first, second) = expected
                assertEquals(listOf(first.durationUs to first.mediaStart,
                    (second.movieStartUs - first.durationUs) to -1L,
                    second.durationUs to second.mediaStart), audio.edits.map { it.duration to it.mediaTime })
                assertTrue(audio.edits.all { it.rate == 1f })
                val decoder = AacPcmDecoder(byteArrayOf(0x11, 0x88.toByte()))
                val packets = mutableListOf<String>()
                while (true) {
                    val packet = audio.nextFrame() as? MP4Packet ?: break
                    assertEquals(48_000, packet.timescale)
                    assertEquals(packets.size * 1024L, packet.mediaPts)
                    assertEquals(1024L, packet.duration)
                    val data = ByteArray(packet.data.remaining()).also { packet.data.duplicate().get(it) }
                    assertEquals(2048, decoder.decode(data).size)
                    packets += hash(data)
                }
                assertEquals(expected.sumOf { it.samples }, packets.size)
                packets
            }
        }
    }

    private fun resource(name: String) = assertNotNull(javaClass.getResourceAsStream("/replay-trim-parity/$name"))

    private fun hash(bytes: ByteArray) = MessageDigest.getInstance("SHA-256").digest(bytes)
        .joinToString("") { "%02x".format(it) }

    private fun withoutGeneratedPngId(input: ByteArray, saveId: String): ByteArray {
        val buffer = ByteBuffer.wrap(input).apply { position(8) }
        var normalized: ByteArray? = null
        while (buffer.hasRemaining()) {
            val offset = buffer.position()
            val size = buffer.int
            val type = ByteArray(4).also { buffer.get(it) }
            val data = ByteArray(size).also { buffer.get(it) }
            val crc = buffer.int
            if (String(type, Charsets.US_ASCII) == "tEXt" &&
                String(data, Charsets.ISO_8859_1).startsWith("arb:source_save_id\u0000")) {
                assertEquals(null, normalized)
                assertEquals("arb:source_save_id\u0000$saveId", String(data, Charsets.ISO_8859_1))
                assertEquals(java.util.zip.CRC32().apply { update(type); update(data) }.value.toInt(), crc)
                assertEquals("02df0c6fe5b29f841f515a8a7cb34d216de1e4d13e30afb1dce896d11df9cf9d",
                    hash(input.copyOfRange(0, offset) + input.copyOfRange(buffer.position(), input.size)))
                val stable = "arb:source_save_id\u0000generated-id".toByteArray(Charsets.ISO_8859_1)
                val chunk = ByteBuffer.allocate(12 + stable.size).putInt(stable.size).put(type).put(stable)
                    .putInt(java.util.zip.CRC32().apply { update(type); update(stable) }.value.toInt()).array()
                normalized = input.copyOfRange(0, offset) + chunk + input.copyOfRange(buffer.position(), input.size)
            }
        }
        return assertNotNull(normalized)
    }

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
