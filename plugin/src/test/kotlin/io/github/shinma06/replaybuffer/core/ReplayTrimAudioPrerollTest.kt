package io.github.shinma06.replaybuffer.core

import com.google.gson.Gson
import com.google.gson.JsonParser
import io.github.shinma06.replaybuffer.ui.ReplayPlaybackSession
import org.jcodec.codecs.mpeg4.mp4.EsdsBox
import org.jcodec.common.io.NIOUtils
import org.jcodec.common.model.Packet
import org.jcodec.containers.mp4.MP4Packet
import org.jcodec.containers.mp4.MP4TrackType
import org.jcodec.containers.mp4.boxes.AudioSampleEntry
import org.jcodec.containers.mp4.boxes.NodeBox
import org.jcodec.containers.mp4.demuxer.AbstractMP4DemuxerTrack
import org.jcodec.containers.mp4.demuxer.MP4Demuxer
import org.jcodec.containers.mp4.muxer.MP4Muxer
import org.jcodec.containers.mp4.muxer.MP4MuxerTrack
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.io.ByteArrayOutputStream
import java.lang.reflect.Proxy
import java.nio.file.Files
import java.nio.file.Path
import java.security.MessageDigest
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import javax.sound.sampled.AudioFormat
import javax.sound.sampled.SourceDataLine
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class ReplayTrimAudioPrerollTest {
    @TempDir lateinit var root: Path
    private val rate = 48_000
    private val gapStart = 1_100_001L
    private val sourceSegments = listOf(PlaybackAudioSegment(0, 640_000, 0, 30),
        PlaybackAudioSegment(gapStart, 1_066_666, 30 * 1024, 50))

    @Test
    fun trimInsideAnAacPacketPreservesTheSourcePcm() {
        val source = fixture()
        val output = trim(source, 50_000, 300_000)
        assertContentEquals(oracle(source, 50_000, 300_000), play(output, 250_000))
        assertContentEquals(oracle(source, 60_000, 300_000), play(output, 240_000, fromUs = 10_000))
        assertContentEquals(oracle(source, 90_000, 300_000), play(output, 210_000, fromUs = 40_000))
    }

    @Test
    fun knownLimitationIndependentSegmentResetsAreNotIdentifiedBySchemaFour() {
        val source = fixture()
        val cold = oracle(source, 0, 2_000_000)
        val actual = play(source, 2_000_000)
        val boundary = (gapStart * rate / 1_000_000).toInt() * 2
        assertContentEquals(cold.copyOfRange(0, boundary), actual.copyOfRange(0, boundary))
        assertFalse(cold.copyOfRange(boundary, boundary + 2048).contentEquals(actual.copyOfRange(boundary, boundary + 2048)),
            "Known limitation: the unchanged player warms up across independently encoded segments")
        assertContentEquals(cold.copyOfRange(boundary + 2048, cold.size), actual.copyOfRange(boundary + 2048, actual.size))
    }

    @Test
    fun consecutivePacketsAcrossATwoMillisecondVideoHoleKeepTheirOverlap() {
        val source = fixture(listOf(PlaybackAudioSegment(0, 40_000, 0, 2),
            PlaybackAudioSegment(40_666, 100_000, 2 * 1024, 5)), independent = false)
        val stored = audio(source)
        val decoder = AacPcmDecoder(stored.config)
        val pcm = ByteArrayOutputStream().apply { stored.packets.forEach { write(decoder.decode(it)) } }.toByteArray()
        val expected = pcm.copyOfRange(0, 1920 * 2) + ByteArray(31 * 2) + pcm.copyOfRange(2048 * 2, (2048 + 4800) * 2)
        assertContentEquals(expected, play(source, 140_666))
        val output = trim(source, 0, 300_000)
        assertContentEquals(play(source, 300_000), play(output, 300_000))
        val again = trim(output, 0, 300_000)
        assertEquals(stored.packets.map(::hash), audio(again).packets.map(::hash))
        assertContentEquals(play(source, 300_000), play(again, 300_000))
    }

    @Test
    fun copiedPacketAndEditHideExactlyOnePrerollWithoutChangingTheAudibleCount() {
        val source = fixture()
        val output = trim(source, 50_000, 300_000)
        structure(source, output, (1 until 15).toList(), listOf(PlaybackAudioSegment(0, 250_000, 1376, 13)))
        val stored = audio(output)
        val decoder = AacPcmDecoder(stored.config)
        val pcm = ByteArrayOutputStream().apply { stored.packets.forEach { write(decoder.decode(it)) } }.toByteArray()
        assertContentEquals(oracle(source, 50_000, 300_000), pcm.copyOfRange(1376 * 2, (1376 + 12_000) * 2))
    }

    @Test
    fun theEditSkipsThePrerollAsWellAsTheIntraPacketOffset() {
        val output = trim(fixture(), 50_000, 300_000)
        assertEquals(listOf(250_000L to 1376L), audio(output).edits)
    }

    @Test
    fun packetBoundaryTrimAlsoKeepsThePrecedingPacket() {
        val source = fixture()
        val output = trim(source, 192_000, 500_000)
        structure(source, output, (8 until 24).toList(), listOf(PlaybackAudioSegment(0, 308_000, 1024, 15)))
        assertContentEquals(oracle(source, 192_000, 500_000), play(output, 308_000))
        val tone = trim(source, 42_667, 300_000)
        structure(source, tone, (1 until 15).toList(), listOf(PlaybackAudioSegment(0, 257_333, 1024, 13)))
        assertContentEquals(oracle(source, 42_667, 300_000), play(tone, 257_333))
    }

    @Test
    fun sourceStartsAndLeadingGapsDoNotInventPrerollFromAnotherSegment() {
        val source = fixture()
        for (from in listOf(0L, gapStart, gapStart + 10_000, 800_000L)) {
            val until = if (from == 0L) 400_000L else gapStart + 400_000
            val output = trim(source, from, until)
            val leading = maxOf(gapStart - from, 0).takeIf { from > 0 } ?: 0
            val media = if (from > gapStart) (from - gapStart) * rate / 1_000_000 else 0
            val first = if (from == 0L) 0 else 30
            structure(source, output, (first until first + 19).toList(),
                listOf(PlaybackAudioSegment(leading, until - from - leading, media, 19)))
            assertContentEquals(oracle(source, from, until), play(output, until - from))
        }
        val middle = trim(source, gapStart + 50_000, gapStart + 400_000)
        structure(source, middle, (31 until 49).toList(), listOf(PlaybackAudioSegment(0, 350_000, 1376, 17)))
        assertContentEquals(oracle(source, gapStart + 50_000, gapStart + 400_000), play(middle, 350_000))
    }

    @Test
    fun crossingAGapPreservesTheExistingPlayerBoundaryAndSilence() {
        val source = fixture()
        val output = trim(source, 60_000, 1_900_001)
        structure(source, output, (1 until 30).toList() + (30 until 68).toList(), listOf(
            PlaybackAudioSegment(0, 580_000, 1856, 28),
            PlaybackAudioSegment(1_040_001, 800_000, 29 * 1024, 38)))
        assertContentEquals(sourcePlaybackRange(source, 60_000, 1_900_001), play(output, 1_840_001))
    }

    @Test
    fun retrimReusesHiddenPrerollAndNeverBorrowsItForANewSourceSegment() {
        val source = fixture()
        val output = trim(source, 60_000, 1_900_001)
        val again = trim(output, 0, 1_840_001)
        assertEquals(audio(output).packets.map(::hash), audio(again).packets.map(::hash))
        assertEquals(audio(output).edits, audio(again).edits)
        assertContentEquals(sourcePlaybackRange(source, 60_000, 1_900_001), play(again, 1_840_001))
        val inside = trim(output, 0, 140_000)
        structure(source, inside, (1 until 10).toList(), listOf(PlaybackAudioSegment(0, 140_000, 1856, 8)))
        assertContentEquals(oracle(source, 60_000, 200_000), play(inside, 140_000))
        val later = trim(output, 20_000, 1_500_001)
        structure(source, later, (2 until 30).toList() + (30 until 52).toList(), listOf(
            PlaybackAudioSegment(0, 560_000, 1792, 27),
            PlaybackAudioSegment(1_020_001, 460_000, 28 * 1024, 22)))
        assertContentEquals(sourcePlaybackRange(source, 80_000, 1_560_001), play(later, 1_480_001))
        val nextSource = trim(output, 1_040_001, 1_440_001)
        structure(source, nextSource, (30 until 49).toList(), listOf(PlaybackAudioSegment(0, 400_000, 0, 19)))
        assertContentEquals(oracle(source, gapStart, gapStart + 400_000), play(nextSource, 400_000))
    }

    private fun hash(bytes: ByteArray) = MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }

    private fun sourcePlaybackRange(source: Path, from: Long, until: Long): ByteArray =
        play(source, until).copyOfRange((from * rate / 1_000_000).toInt() * 2, (until * rate / 1_000_000).toInt() * 2)

    private fun structure(source: Path, output: Path, expectedPackets: List<Int>, expectedSegments: List<PlaybackAudioSegment>) {
        val original = audio(source)
        val actual = audio(output)
        assertContentEquals(original.config, actual.config)
        assertEquals(expectedPackets.map { hash(original.packets[it]) }, actual.packets.map(::hash))
        SavedReplay.open(output).use { saved ->
            assertEquals(4, saved.manifest["schema"].asInt)
            ReplayAudioTrack.open(saved)!!.use { assertEquals(expectedSegments, it.segments) }
        }
        val edits = mutableListOf<Pair<Long, Long>>()
        var cursor = 0L
        for (segment in expectedSegments) {
            if (segment.movieStartUs > cursor) edits += segment.movieStartUs - cursor to -1L
            edits += segment.durationUs to segment.mediaStart
            cursor = segment.movieStartUs + segment.durationUs
        }
        assertEquals(edits, actual.edits)
    }

    private fun trim(source: Path, from: Long, until: Long): Path {
        val trim = ReplayTrim()
        val selection = trim.preflight(source, from, until)
        assertEquals(from, selection.startUs)
        assertEquals(until, selection.endUs)
        return trim.trim(source, from, until, { false }, { Files.move(it.partial, it.complete) })
    }

    private data class Audio(val config: ByteArray, val packets: List<ByteArray>, val edits: List<Pair<Long, Long>>)

    private fun audio(folder: Path): Audio = NIOUtils.readableChannel(folder.resolve("video.mp4").toFile()).use { input ->
        MP4Demuxer.createRawMP4Demuxer(input).use { demux ->
            val track = demux.audioTracks.single() as AbstractMP4DemuxerTrack
            val config = NIOUtils.toArray(NodeBox.findFirst(track.sampleEntries.single(), EsdsBox::class.java,
                EsdsBox.fourcc()).streamInfo)
            val packets = mutableListOf<ByteArray>()
            while (true) {
                val packet = track.nextFrame() as? MP4Packet ?: break
                assertEquals(packets.size * 1024L, packet.mediaPts)
                assertEquals(1024L, packet.duration)
                packets += NIOUtils.toArray(packet.data)
            }
            Audio(config, packets, track.edits.orEmpty().map { it.duration to it.mediaTime })
        }
    }

    // Decode each independent source segment from its own first packet, never from a trimmed output.
    private fun oracle(source: Path, from: Long, until: Long): ByteArray {
        val sourceAudio = audio(source)
        val out = ByteArrayOutputStream()
        var movie = from
        for (segment in sourceSegments) {
            val begin = maxOf(from, segment.movieStartUs)
            val end = minOf(until, segment.movieStartUs + segment.durationUs)
            if (begin >= end) continue
            if (begin > movie) out.write(ByteArray(((begin - movie) * rate / 1_000_000).toInt() * 2))
            val decoder = AacPcmDecoder(sourceAudio.config)
            val first = (segment.mediaStart / 1024).toInt()
            val pcm = sourceAudio.packets.subList(first, first + segment.samples).map(decoder::decode)
                .fold(ByteArrayOutputStream()) { bytes, packet -> bytes.apply { write(packet) } }.toByteArray()
            val skip = ((begin - segment.movieStartUs) * rate / 1_000_000).toInt()
            val count = ((end - begin) * rate / 1_000_000).toInt()
            out.write(pcm, skip * 2, count * 2)
            movie = end
        }
        val limit = ((until - from) * rate / 1_000_000).toInt() * 2
        return out.toByteArray().copyOf(limit)
    }

    private fun play(folder: Path, duration: Long, fromUs: Long = 0): ByteArray {
        val pcm = ByteArrayOutputStream()
        val finished = CountDownLatch(1)
        val closed = CountDownLatch(1)
        val failures = ConcurrentLinkedQueue<String>()
        var frameBytes = 0
        val drained = AtomicBoolean()
        val line = Proxy.newProxyInstance(SourceDataLine::class.java.classLoader,
            arrayOf(SourceDataLine::class.java)) { _, method, args -> when (method.name) {
                "open" -> { frameBytes = (args[0] as AudioFormat).frameSize; null }
                "start", "stop", "flush" -> null
                "write" -> {
                    pcm.write(args[0] as ByteArray, args[1] as Int, args[2] as Int)
                    args[2] as Int
                }
                "getMicrosecondPosition" -> if (drained.get()) pcm.size() / frameBytes * 1_000_000L / rate else 0L
                "drain" -> { drained.set(true); null }
                "close" -> { closed.countDown(); null }
                else -> error(method.name)
            } } as SourceDataLine
        val session = ReplayPlaybackSession(folder, fromUs, fromUs + duration, {},
            { it?.let(failures::add); finished.countDown() }, failures::add, audioOutput = { line })
        try {
            session.start()
            assertTrue(finished.await(10, TimeUnit.SECONDS), failures.toString())
            assertTrue(closed.await(10, TimeUnit.SECONDS), "line was not closed")
            assertTrue(failures.isEmpty(), failures.toString())
            return pcm.toByteArray()
        } finally { session.stop() }
    }

    private fun fixture(segments: List<PlaybackAudioSegment> = sourceSegments, independent: Boolean = true): Path {
        val folder = Files.createTempDirectory(root, "source-")
        val resource = "/replay-trim-parity/replay-20240101-000000-manual-feedface"
        val names = listOf("README.md", "replay.json", "video-frames.jsonl", "video-source.h264",
            "logcat-device.jsonl", "logcat-app.jsonl", "touches.jsonl")
        names.forEach { name -> javaClass.getResourceAsStream("$resource/$name")!!.use { Files.copy(it, folder.resolve(name)) } }
        val videoFile = Path.of(javaClass.getResource("$resource/video.mp4")!!.toURI())
        val audioFile = Path.of(javaClass.getResource("/aac-playback-sync/mono-no-pns.m4a")!!.toURI())
        // Repeat the existing synthetic IDR at exact cut boundaries, so video-frame rounding cannot affect the PCM oracle.
        val boundaries = listOf(0L, 20_000, 42_667, 50_000, 60_000, 80_000, 140_000, 192_000, 200_000,
            300_000, 400_000, 500_000, 640_000, 800_000, gapStart, gapStart + 10_000, gapStart + 50_000, gapStart + 400_000,
            1_560_001, 1_900_001, 2_000_000, 2_166_667, 2_200_002)
        val template = Files.readAllLines(folder.resolve("video-frames.jsonl")).first().let { JsonParser.parseString(it).asJsonObject }
        val rows = boundaries.zipWithNext().mapIndexed { i, (start, end) -> template.deepCopy().apply {
            addProperty("sample_index", i)
            addProperty("packet_ordinal", (i + 1).toString())
            listOf("source_pts_us", "source_display_start_us", "media_pts_us", "movie_pts_us", "presentation_pts_us")
                .forEach { addProperty(it, start.toString()) }
            addProperty("movie_end_us", end.toString())
            listOf("source_duration_us", "display_duration_us").forEach { addProperty(it, (end - start).toString()) }
            listOf("elapsed_ns", "t_ns", "display_t_ns").forEach { addProperty(it, (start * 1000).toString()) }
            addProperty("confirmed_t_end_ns", (end * 1000).toString())
        } }
        Files.writeString(folder.resolve("video-frames.jsonl"), rows.joinToString("\n", postfix = "\n"))
        NIOUtils.readableChannel(videoFile.toFile()).use { videoIn ->
            NIOUtils.readableChannel(audioFile.toFile()).use { audioIn ->
                NIOUtils.writableChannel(folder.resolve("video.mp4").toFile()).use { out ->
                    val mux = MP4Muxer.createMP4MuxerToChannel(out)
                    val videoSource = MP4Demuxer.createRawMP4Demuxer(videoIn).videoTrack as AbstractMP4DemuxerTrack
                    val video = mux.addTrack(MP4MuxerTrack(mux.nextTrackId, MP4TrackType.VIDEO))
                    videoSource.sampleEntries.forEach(video::addSampleEntry)
                    val idr = videoSource.nextFrame()
                    boundaries.zipWithNext().forEachIndexed { i, (start, end) ->
                        video.addFrameInternal(Packet.createPacket(idr.data.duplicate(), start, 1_000_000,
                            end - start, i.toLong(), Packet.FrameType.KEY, null), 1)
                    }
                    val audioSource = MP4Demuxer.createRawMP4Demuxer(audioIn).audioTracks.single() as AbstractMP4DemuxerTrack
                    val sound = mux.addTrack(MP4MuxerTrack(mux.nextTrackId, MP4TrackType.SOUND))
                    sound.addSampleEntry(audioSource.sampleEntries.single() as AudioSampleEntry)
                    var written = 0L
                    for ((i, segment) in segments.withIndex()) {
                        if (independent || i == 0) check(audioSource.gotoFrame(0))
                        repeat(segment.samples) {
                            sound.addFrameInternal(Packet.createPacket(audioSource.nextFrame().data, written * 1024,
                                rate, 1024, written, Packet.FrameType.KEY, null), 1)
                            written++
                        }
                    }
                    mux.finish()
                }
            }
        }
        val manifest = JsonParser.parseString(Files.readString(folder.resolve("replay.json"))).asJsonObject
        manifest["audio_track"].asJsonObject.addProperty("source",
            if (independent) "Synthetic mono-no-pns; independent AAC segments" else "Synthetic mono-no-pns; one continuous AAC stream")
        manifest["audio_track"].asJsonObject.add("segments", Gson().toJsonTree(segments.map { mapOf(
            "movie_start_us" to it.movieStartUs.toString(), "duration_us" to it.durationUs.toString(),
            "media_start_samples" to it.mediaStart.toString(), "samples" to it.samples) }))
        val parts = manifest["parts"].asJsonArray
        parts.remove(1)
        parts[0].asJsonObject.apply {
            addProperty("movie_end_us", "2200002")
            addProperty("duration_us", "2200002")
            addProperty("t_end_ns", "2200002000")
            addProperty("confirmed_t_end_ns", "2200002000")
        }
        manifest["video_run_boundaries"].asJsonArray.remove(0)
        val hashes = manifest["files_sha256"].asJsonObject
        for (name in listOf("video.mp4", "video-frames.jsonl")) hashes.addProperty(name, hash(Files.readAllBytes(folder.resolve(name))))
        Files.writeString(folder.resolve("replay.json"), manifest.toString())
        return folder
    }
}
