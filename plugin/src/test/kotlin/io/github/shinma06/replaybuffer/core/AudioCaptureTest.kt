package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonParser
import org.jcodec.codecs.h264.H264Encoder
import org.jcodec.codecs.h264.H264Utils
import org.jcodec.common.io.NIOUtils
import org.jcodec.common.model.ColorSpace
import org.jcodec.common.model.Picture
import org.jcodec.containers.mp4.demuxer.MP4Demuxer
import java.nio.ByteBuffer
import java.nio.file.Files
import java.nio.file.Path
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

class AudioCaptureTest {
    @TempDir lateinit var root: Path

    private val config = byteArrayOf(0x11, 0x90.toByte()) // AAC-LC, 48 kHz, stereo

    @Test
    fun audioSpecificConfigAcceptsOnlyPlainAacLc() {
        assertEquals(48_000 to 2, aacConfig(config))
        assertEquals(44_100 to 1, aacConfig(byteArrayOf(0x12, 0x08)))
        assertNull(aacConfig(byteArrayOf(0x29, 0x90.toByte()))) // HE-AAC object type 5
        assertNull(aacConfig(byteArrayOf(0x11)))
    }

    @Test
    fun audioFollowsTheMovieTimeOfTheShownFrameAndSkipsVideoHoles() {
        // Two presented intervals: 0..1 s of sequence time at movie 0, then 3..4 s at movie 1 s (the 1..3 s hole is not in the movie).
        val anchors = mapOf(1 to listOf(MovieAnchor(1, 0, 1_000_000, 0), MovieAnchor(1, 3_000_000_000, 1_000_000, 1_000_000)))
        val frame = 1024 * 1_000_000_000L / 48_000
        val rows = (0 until 200).map { i -> entry(i * frame) }
        val plan = assertNotNull(planAudio(rows, anchors, 2_000_000) { true })
        assertEquals(2, plan.segments.size)
        assertEquals(0L, plan.segments[0].movieStartUs)
        // The first unit after the hole starts at 3.008 s, i.e. 8 ms into the second presented interval.
        assertTrue(plan.segments[1].movieStartUs in 1_000_000L..1_021_333L)
        assertTrue(plan.segments.all { it.durationUs in 970_000..1_010_000 })
        assertEquals(plan.segments.sumOf { it.count }, plan.samples.size)
        assertTrue(plan.samples.all { it.time.sequence!! < 1_000_000_000 || it.time.sequence >= 3_000_000_000 })
        assertEquals(plan.segments[1].mediaStart, plan.segments[0].count * AAC_FRAME_SAMPLES)
        assertTrue(plan.durationUs <= 2_000_000)
    }

    @Test
    fun theLastUnitBeforeAVideoHoleIsCutAtTheHole() {
        val frame = 1024 * 1_000_000_000L / 48_000
        val rows = (0 until 200).map { i -> entry(i * frame) }
        // Video ends at 1 s and resumes at 3 s: the unit at 0.981333 s must not play into the hole.
        val wide = assertNotNull(planAudio(rows, mapOf(1 to listOf(MovieAnchor(1, 0, 1_000_000, 0),
            MovieAnchor(1, 3_000_000_000, 1_000_000, 1_000_000))), 2_000_000) { true })
        assertEquals(1_000_000L, wide.segments[0].movieStartUs + wide.segments[0].durationUs)
        assertEquals(1_000_000 + (141 * frame - 3_000_000_000) / 1000, wide.segments[1].movieStartUs)
        assertEquals(1_000_000L, wide.edits[0].duration)
        // A 2 ms hole has no unit inside it; the units on either side still never share an edit across it.
        val narrow = assertNotNull(planAudio(rows, mapOf(1 to listOf(MovieAnchor(1, 0, 1_000_000, 0),
            MovieAnchor(1, 1_002_000_000, 1_000_000, 1_000_000))), 2_000_000) { true })
        assertEquals(2, narrow.segments.size)
        assertEquals(1_000_000L, narrow.segments[0].movieStartUs + narrow.segments[0].durationUs)
        assertEquals(1_000_000 + (47 * frame - 1_002_000_000) / 1000, narrow.segments[1].movieStartUs)
        // Frames that continue in both movie and device time form one span.
        assertEquals(1, displayedSpans(mapOf(1 to listOf(MovieAnchor(1, 0, 500_000, 0),
            MovieAnchor(1, 500_000_000, 500_000, 500_000)))).size)
    }

    @Test
    fun driftBeyondTheToleranceStartsANewEditWithoutOverlap() {
        val anchors = mapOf(1 to listOf(MovieAnchor(1, 0, 10_000_000, 0)))
        val frame = 1024 * 1_000_000_000L / 48_000
        val rows = (0 until 10).map { entry(it * frame) } + (10 until 20).map { entry(it * frame + 100_000_000) }
        val plan = assertNotNull(planAudio(rows, anchors, 10_000_000) { true })
        assertEquals(2, plan.segments.size)
        val (a, b) = plan.segments
        assertTrue(b.movieStartUs >= a.movieStartUs + a.durationUs)
        assertEquals(3, plan.edits.size) // audio, empty edit, audio
        assertEquals(-1L, plan.edits[1].mediaTime)
        // Unaligned or unmapped rows are never placed.
        assertTrue(planAudio(rows, anchors, 10_000_000) { false }!!.samples.isEmpty())
    }

    @Test
    fun logOnlyAndSettingOffKeepNoAudio() {
        for ((mode, requested) in listOf(CaptureMode.LOG_ONLY to true, CaptureMode.VIDEO_AND_LOGS to false)) {
            CaptureStore(root.resolve("ring-$mode-$requested"), minFree = 0, captureMode = mode, audioRequested = requested).use { store ->
                val host = System.nanoTime() + 1_000_000_000
                store.generation(1)
                sample(store, host, 1)
                store.audioSession(1)
                store.audio(AudioPacket(0, true, config), 1)
                store.audio(AudioPacket(1_100_000, false, byteArrayOf(1)), 1, host + 100_000_000)
                store.status("device_log", StreamState.CAPTURING, null, 1)
                store.log(DeviceLog(1_700_000_001_100_000_000, 12, 12, 10001, 0, 4, "Fixture", "row", byteArrayOf(1)), 1, host + 100_000_000)
                sample(store, host, 2)
                val pinned = assertNotNull(store.capture(ReplaySettings(captureMode = mode, recordAudio = requested), null, host + 1_000_000_000))
                assertTrue(pinned.audio.isEmpty())
                assertNull(pinned.audioState)
                val coverage = audioCoverage(pinned, null)
                assertEquals(StreamState.NOT_REQUESTED, coverage["state"])
                assertEquals(false, coverage["requested"])
                store.release(pinned.id)
            }
        }
    }

    @Test
    fun savedMp4HasAnAacTrackAlignedWithTheVideoAndCoverage() {
        CaptureStore(root.resolve("ring"), minFree = 0, audioRequested = true).use { store ->
            val host = System.nanoTime() + 1_000_000_000
            store.generation(1)
            sample(store, host, 1)
            store.session(VideoPacket.Session(32, 32), 1)
            val encoder = H264Encoder.createH264Encoder().apply { setKeyInterval(30) }
            for (n in 0 until 10) {
                val picture = Picture.create(32, 32, ColorSpace.YUV420J).apply { fill(20 + n) }
                val encoded = encoder.encodeFrame(picture, ByteBuffer.allocate(32 * 32 * 3)).data
                val bytes = ByteArray(encoded.remaining()).also { encoded.duplicate().get(it) }
                if (n == 0) {
                    val ps = H264Utils.splitFrame(ByteBuffer.wrap(bytes)).filter { it.get(0).toInt() and 31 in setOf(7, 8) }
                        .fold(byteArrayOf()) { result, nal -> result + byteArrayOf(0, 0, 0, 1) + ByteArray(nal.remaining()).also { nal.get(it) } }
                    store.frame(VideoPacket.Frame(0, true, false, ps), 1)
                }
                store.frame(VideoPacket.Frame(1_000_000L + n * 100_000, false, n == 0, bytes), 1, host + n * 100_000_000L)
            }
            store.audioSession(1)
            store.audio(AudioPacket(0, true, config), 1)
            val unit = 1024 * 1_000_000L / 48_000
            for (i in 0 until 40) store.audio(AudioPacket(1_000_000 + i * unit, false, byteArrayOf(0x21, i.toByte())), 1, host + i * unit * 1000)
            sample(store, host, 2)
            val pinned = assertNotNull(store.capture(ReplaySettings(recordAudio = true), null, host + 1_000_000_000))
            assertEquals(StreamState.CAPTURING, pinned.audioState?.state)
            assertEquals(40, pinned.audio.size)
            val output = SaveWriter().write(pinned, root, { false }) { Files.move(it.partial, it.complete) }
            val json = JsonParser.parseString(Files.readString(output.directory.resolve("session.json"))).asJsonObject
            val coverage = json["coverage"].asJsonObject["audio"].asJsonObject
            assertEquals("CAPTURING", coverage["state"].asString)
            assertTrue(coverage["requested"].asBoolean)
            assertTrue(coverage["reason"].isJsonNull)
            assertTrue(coverage["availableSeconds"].asDouble > 0.8)
            val track = json["audio_track"].asJsonObject
            assertEquals(48_000, track["sample_rate"].asInt)
            val segment = track["segments"].asJsonArray.single().asJsonObject
            val firstFrame = JsonParser.parseString(Files.readAllLines(output.directory.resolve("frames.jsonl")).first()).asJsonObject
            // Audio and the first presented frame share the same device instant, so they start together on the movie timeline.
            assertEquals(firstFrame["movie_pts_us"].asString, segment["movie_start_us"].asString)
            NIOUtils.readableChannel(output.directory.resolve("video-001.mp4").toFile()).use { channel ->
                val demuxer = MP4Demuxer.createMP4Demuxer(channel)
                assertEquals(1, demuxer.videoTracks.size)
                val audio = demuxer.audioTracks.single()
                assertEquals(segment["samples"].asInt, audio.meta.totalFrames)
                val first = audio.nextFrame()
                val data = ByteArray(first.data.remaining()).also { first.data.get(it) } // The demuxer prepends an ADTS header.
                assertContentEquals(byteArrayOf(0x21, 0), data.copyOfRange(data.size - 2, data.size))
            }
            assertTrue(Files.readString(output.directory.resolve("README.txt")).contains("端末の再生音"))
            store.release(pinned.id)
        }
    }

    @Test
    fun unavailableAudioKeepsVideoAndSavesTheReason() {
        CaptureStore(root.resolve("ring"), minFree = 0, audioRequested = true).use { store ->
            store.generation(1)
            store.audioStatus(StreamState.UNAVAILABLE, "端末のAndroidが13未満のため再生音を取得できません（動画のみ記録しています）", 1)
            store.audioSession(1) // A later attempt cannot clear the latched reason within this generation.
            val capture = FrozenCapture("id", "seq", 1, 0, 1, 1, emptyList(), emptyList(), emptyList(), emptyList(), emptyList(),
                ReplaySettings(recordAudio = true), emptyMap(), audioState = StreamSnapshot(StreamState.UNAVAILABLE, reason = "理由"))
            val coverage = audioCoverage(capture, null)
            assertEquals(StreamState.UNAVAILABLE, coverage["state"])
            assertEquals("理由", coverage["reason"])
            assertEquals(true, coverage["requested"])
        }
    }

    @Test
    fun logsOnlySaveExcludesAudioEvenWhenRecorded() {
        CaptureStore(root.resolve("ring-logs"), minFree = 0, audioRequested = true).use { store ->
            val host = System.nanoTime() + 1_000_000_000
            store.generation(1)
            sample(store, host, 1)
            store.status("device_log", StreamState.CAPTURING, null, 1)
            store.audioSession(1)
            store.audio(AudioPacket(0, true, config), 1)
            for (i in 0 until 20) store.audio(AudioPacket(1_000_000 + i * 21_333L, false, byteArrayOf(1)), 1, host + i * 21_333_000L)
            store.log(DeviceLog(1_700_000_001_100_000_000, 12, 12, 10001, 0, 4, "Fixture", "row", byteArrayOf(1)), 1, host + 100_000_000)
            sample(store, host, 2)
            val pinned = assertNotNull(store.capture(ReplaySettings(recordAudio = true), null, host + 1_000_000_000,
                request = SaveRequest(SaveContent.LOGS_ONLY, SaveOrigin.LOGCAT_TOOLBAR)))
            assertTrue(pinned.audio.isEmpty())
            assertNull(pinned.audioState)
            val output = SaveWriter().write(pinned, root, { false }) { Files.move(it.partial, it.complete) }
            val json = JsonParser.parseString(Files.readString(output.directory.resolve("session.json"))).asJsonObject
            val audio = json["coverage"].asJsonObject["audio"].asJsonObject
            assertEquals("EXCLUDED", audio["state"].asString)
            assertEquals(false, audio["requested"].asBoolean)
            assertTrue(audio["excluded"].asBoolean)
            assertTrue(json["audio_track"].isJsonNull)
            assertTrue(Files.notExists(output.directory.resolve("video-001.mp4")))
            store.release(pinned.id)
        }
    }

    private fun entry(sequence: Long) = AudioEntry(sequence / 1000, byteArrayOf(1), config, MappedTime(sequence, 1, 0, sequence), 1, 1, 0)

    private fun sample(store: CaptureStore, host: Long, second: Long) {
        val elapsed = second * 1_000_000_000
        val received = host + (second - 1) * 1_000_000_000
        assertTrue(store.clock.add("1", listOf(elapsed, elapsed, 1_700_000_000_000_000_000 + elapsed, elapsed), received, received))
        store.clockStatus(true, 1)
    }
}
