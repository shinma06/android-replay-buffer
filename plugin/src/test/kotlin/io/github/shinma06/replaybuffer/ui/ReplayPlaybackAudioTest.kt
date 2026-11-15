package io.github.shinma06.replaybuffer.ui

import com.google.gson.JsonParser
import net.sourceforge.jaad.aac.Decoder
import net.sourceforge.jaad.aac.SampleBuffer
import org.jcodec.codecs.mpeg4.mp4.EsdsBox
import org.jcodec.common.io.NIOUtils
import org.jcodec.common.model.Packet
import org.jcodec.containers.mp4.MP4TrackType
import org.jcodec.containers.mp4.boxes.AudioSampleEntry
import org.jcodec.containers.mp4.boxes.NodeBox
import org.jcodec.containers.mp4.demuxer.AbstractMP4DemuxerTrack
import org.jcodec.containers.mp4.demuxer.MP4Demuxer
import org.jcodec.containers.mp4.muxer.MP4Muxer
import org.jcodec.containers.mp4.muxer.MP4MuxerTrack
import org.junit.jupiter.api.io.TempDir
import java.io.ByteArrayOutputStream
import java.lang.reflect.Proxy
import java.nio.file.Files
import java.nio.file.Path
import java.security.MessageDigest
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import javax.sound.sampled.AudioFormat
import javax.sound.sampled.SourceDataLine
import kotlin.math.abs
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class ReplayPlaybackAudioTest {
    @TempDir lateinit var root: Path

    @Test
    fun monoPcmAndTheOpenedLineAgreeOnTheNumberOfFrames() {
        val folder = fixture()
        NIOUtils.readableChannel(folder.resolve("video.mp4").toFile()).use { input ->
            val track = MP4Demuxer.createRawMP4Demuxer(input).audioTracks.single() as AbstractMP4DemuxerTrack
            val entry = track.sampleEntries.single() as AudioSampleEntry
            assertEquals(2, entry.channelCount.toInt())
            val config = NIOUtils.toArray(NodeBox.findFirst(entry, EsdsBox::class.java, EsdsBox.fourcc()).streamInfo)
            val buffer = SampleBuffer().apply { isBigEndian = false }
            Decoder(config).decodeFrame(NIOUtils.toArray(track.nextFrame().data), buffer)
            assertEquals(1, buffer.channels)
            assertEquals(48_000, buffer.sampleRate)
            assertEquals(16, buffer.bitsPerSample)
            assertEquals(2048, buffer.data.size)
            val output = play(folder)
            assertEquals(1024, buffer.data.size / output.format.frameSize)
            assertEquals(1, output.format.channels)
            assertEquals(48_000f, output.format.sampleRate)
            assertEquals(AudioFormat.Encoding.PCM_SIGNED, output.format.encoding)
            assertEquals(false, output.format.isBigEndian)
        }
    }

    @Test
    fun beepSpacingIsOneSecondBothWithAndWithoutTheEncoderPrimingEdit() {
        for (mediaStart in listOf(0L, 1024L)) {
            val output = play(fixture(mediaStart = mediaStart))
            val onsets = onsets(output)
            assertEquals(8, onsets.size, "mediaStart=$mediaStart: $onsets")
            onsets.zipWithNext().forEach { (a, b) ->
                assertTrue(abs(b - a - 1.0) < .015, "mediaStart=$mediaStart: $onsets")
            }
            assertEquals(8 * 48_000, output.pcm.size / output.format.frameSize)
        }
    }

    @Test
    fun seekInsideAPacketAndAnAudioGapUsePcmFramesNotContainerChannels() {
        val output = play(fixture(gap = true), fromUs = 1_025_000)
        val reference = decodedPcm("/aac-lc-escape/beeps.m4a")
        val seekSample = 1024 + 1_025_000L * 48_000 / 1_000_000
        val firstEnd = 1024 + 3 * 48_000
        val secondEnd = firstEnd + 4_500_000L * 48_000 / 1_000_000
        val expected = reference.copyOfRange(seekSample.toInt() * 2, firstEnd * 2) + ByteArray(24_000 * 2) +
            reference.copyOfRange(firstEnd * 2, secondEnd.toInt() * 2)
        assertContentEquals(expected, output.pcm)
        val onsets = onsets(output)
        assertEquals(6, onsets.size, onsets.toString())
        for ((actual, expected) in onsets.zip(listOf(.975, 2.475, 3.475, 4.475, 5.475, 6.475)))
            assertTrue(abs(actual - expected) < .015, onsets.toString())
        val pcm = output.pcm
        assertTrue(pcm.copyOfRange(94_800 * 2, 118_800 * 2).all { it == 0.toByte() })
        assertEquals(334_800, pcm.size / output.format.frameSize)
    }

    @Test
    fun stereoAt44100IsNotForcedToMonoOr48000() {
        val output = play(fixture(resource = "/aac-playback-sync/stereo-44100.m4a", rate = 44_100))
        assertEquals(2, output.format.channels)
        assertEquals(44_100f, output.format.sampleRate)
        assertEquals(8 * 44_100, output.pcm.size / output.format.frameSize)
        val onsets = onsets(output)
        assertEquals(8, onsets.size, onsets.toString())
        onsets.zipWithNext().forEach { (a, b) -> assertTrue(abs(b - a - 1.0) < .015, onsets.toString()) }
    }

    private fun play(folder: Path, fromUs: Long = 0): Output {
        val output = Output()
        val finished = CountDownLatch(1)
        val failures = ConcurrentLinkedQueue<String>()
        val session = ReplayPlaybackSession(folder, fromUs, 8_000_000, {},
            { failure -> failure?.let(failures::add); finished.countDown() }, failures::add,
            audioOutput = { output.line })
        try {
            session.start()
            assertTrue(finished.await(10, TimeUnit.SECONDS), "playback did not finish: $failures")
            assertTrue(output.closed.await(10, TimeUnit.SECONDS), "audio line was not closed")
            assertTrue(failures.isEmpty(), failures.toString())
            return output
        } finally { session.stop() }
    }

    private class Output {
        lateinit var format: AudioFormat
        private val bytes = ByteArrayOutputStream()
        val pcm: ByteArray get() = bytes.toByteArray()
        val closed = CountDownLatch(1)
        @Volatile private var drained = false
        val line = Proxy.newProxyInstance(SourceDataLine::class.java.classLoader, arrayOf(SourceDataLine::class.java)) { _, method, args ->
            when (method.name) {
                "open" -> { format = args[0] as AudioFormat; null }
                "start", "stop", "flush" -> null
                "write" -> {
                    val count = args[2] as Int
                    assertEquals(0, (args[1] as Int) % format.frameSize)
                    assertEquals(0, count % format.frameSize)
                    bytes.write(args[0] as ByteArray, args[1] as Int, count)
                    count
                }
                "getMicrosecondPosition" -> if (drained) (bytes.size() / format.frameSize * 1_000_000L / format.sampleRate.toInt()) else 0L
                "drain" -> { drained = true; null }
                "close" -> { closed.countDown(); null }
                else -> error("Unexpected audio line call: ${method.name}")
            }
        } as SourceDataLine
    }

    private fun onsets(output: Output): List<Double> {
        val pcm = output.pcm
        val frames = pcm.size / output.format.frameSize
        val rate = output.format.sampleRate.toInt()
        val block = rate / 100
        val starts = mutableListOf<Double>()
        var lastLoud = -rate
        for (start in 0 until frames - block step block) {
            var energy = 0.0
            for (frame in start until start + block) {
                val i = frame * output.format.frameSize
                val value = (pcm[i].toInt() and 255) or (pcm[i + 1].toInt() shl 8)
                energy += value.toDouble() * value
            }
            val loud = energy / block > 1_000_000
            if (loud) {
                if (start - lastLoud > rate / 5) starts += start.toDouble() / rate
                lastLoud = start
            }
        }
        return starts
    }

    private fun decodedPcm(resource: String): ByteArray =
        NIOUtils.readableChannel(Path.of(javaClass.getResource(resource)!!.toURI()).toFile()).use { input ->
            val track = MP4Demuxer.createRawMP4Demuxer(input).audioTracks.single() as AbstractMP4DemuxerTrack
            val config = NIOUtils.toArray(NodeBox.findFirst(track.sampleEntries.single(), EsdsBox::class.java, EsdsBox.fourcc()).streamInfo)
            val decoder = Decoder(config)
            val buffer = SampleBuffer().apply { isBigEndian = false }
            val bytes = ByteArrayOutputStream()
            while (true) {
                val packet = track.nextFrame() ?: break
                decoder.decodeFrame(NIOUtils.toArray(packet.data), buffer)
                bytes.write(buffer.data)
            }
            bytes.toByteArray()
        }

    private fun fixture(resource: String = "/aac-lc-escape/beeps.m4a", rate: Int = 48_000,
                        mediaStart: Long = 1024, gap: Boolean = false): Path {
        val folder = Files.createTempDirectory(root, "replay-")
        val videoRoot = "/replay-trim-parity/replay-20240101-000000-manual-feedface"
        val sourceVideo = Path.of(javaClass.getResource("$videoRoot/video.mp4")!!.toURI())
        val sourceAudio = Path.of(javaClass.getResource(resource)!!.toURI())
        NIOUtils.readableChannel(sourceVideo.toFile()).use { videoInput ->
            NIOUtils.readableChannel(sourceAudio.toFile()).use { audioInput ->
                NIOUtils.writableChannel(folder.resolve("video.mp4").toFile()).use { output ->
                    val mux = MP4Muxer.createMP4MuxerToChannel(output)
                    val videoSource = MP4Demuxer.createRawMP4Demuxer(videoInput).videoTrack as AbstractMP4DemuxerTrack
                    val video = mux.addTrack(MP4MuxerTrack(mux.nextTrackId, MP4TrackType.VIDEO))
                    video.addSampleEntry(videoSource.sampleEntries.first())
                    video.addFrameInternal(Packet.createPacket(videoSource.nextFrame().data, 0, 1_000_000,
                        8_000_000, 0, Packet.FrameType.KEY, null), 1)
                    val audioSource = MP4Demuxer.createRawMP4Demuxer(audioInput).audioTracks.single() as AbstractMP4DemuxerTrack
                    val audio = mux.addTrack(MP4MuxerTrack(mux.nextTrackId, MP4TrackType.SOUND))
                    audio.addSampleEntry(audioSource.sampleEntries.single())
                    var index = 0L
                    while (true) {
                        val packet = audioSource.nextFrame() ?: break
                        audio.addFrameInternal(Packet.createPacket(packet.data, index * 1024, rate,
                            1024, index, Packet.FrameType.KEY, null), 1)
                        index++
                    }
                    mux.finish()
                }
            }
        }
        val row = javaClass.getResource("$videoRoot/video-frames.jsonl")!!.readText().lineSequence().first()
            .let { JsonParser.parseString(it).asJsonObject }
        row.addProperty("movie_pts_us", "0")
        row.addProperty("movie_end_us", "8000000")
        Files.writeString(folder.resolve("video-frames.jsonl"), "$row\n")
        fun segment(start: Long, duration: Long, media: Long): String {
            val packets = (media % 1024 + duration * rate / 1_000_000 + 1023) / 1024
            return """{"movie_start_us":"$start","duration_us":"$duration","media_start_samples":"$media","samples":$packets}"""
        }
        val segments = if (gap) listOf(segment(0, 3_000_000, mediaStart), segment(3_500_000, 4_500_000, mediaStart + 3L * rate))
            else listOf(segment(0, 8_000_000, mediaStart))
        fun sha(name: String) = MessageDigest.getInstance("SHA-256").digest(Files.readAllBytes(folder.resolve(name)))
            .joinToString("") { "%02x".format(it) }
        Files.writeString(folder.resolve("replay.json"), """{
            "schema":4,"complete":true,"files":{"video":"video.mp4","video_frames":"video-frames.jsonl"},
            "parts":[{"movie_end_us":"8000000"}],
            "files_sha256":{"video.mp4":"${sha("video.mp4")}","video-frames.jsonl":"${sha("video-frames.jsonl")}"},
            "audio_track":{"sample_rate":$rate,"segments":[${segments.joinToString(",")}]}
        }""")
        return folder
    }
}
