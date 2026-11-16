package io.github.shinma06.replaybuffer.core

import com.google.gson.GsonBuilder
import com.google.gson.JsonObject
import com.google.gson.JsonParser
import org.jcodec.codecs.h264.H264Encoder
import org.jcodec.codecs.h264.H264Utils
import org.jcodec.common.model.ColorSpace
import org.jcodec.common.model.Picture
import org.junit.jupiter.api.io.TempDir
import java.nio.ByteBuffer
import java.nio.file.Files
import java.nio.file.Path
import java.security.MessageDigest
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class ReplayTrimMetadataTest {
    @TempDir lateinit var root: Path
    private val gson = GsonBuilder().serializeNulls().create()

    private fun manifest(dir: Path) = JsonParser.parseString(Files.readString(dir.resolve(ReplayFiles.RECORD))).asJsonObject
    private fun rows(dir: Path, file: String) = Files.readAllLines(dir.resolve(file)).map { JsonParser.parseString(it).asJsonObject }
    private fun bytes(dir: Path) = Files.list(dir).use { files -> files.toList().associate { it.fileName.toString() to Files.readAllBytes(it) } }
    private fun assertUnchanged(dir: Path, expected: Map<String, ByteArray>) {
        val actual = bytes(dir)
        assertEquals(expected.keys, actual.keys)
        expected.forEach { (name, value) ->
            assertContentEquals(value, actual.getValue(name), name)
            assertContentEquals(MessageDigest.getInstance("SHA-256").digest(value),
                MessageDigest.getInstance("SHA-256").digest(actual.getValue(name)), name)
        }
    }

    private fun source(unknown: Set<Int> = emptySet(), unmappedTouches: Boolean = true, configure: (JsonObject) -> Unit = {}): Path {
        val parent = Files.createDirectory(root.resolve("saves"))
        val directory = CaptureStore(root.resolve("ring"), minFree = 0).use { store ->
            store.generation(1)
            store.session(VideoPacket.Session(32, 32), 1)
            val encoder = H264Encoder.createH264Encoder().apply { setKeyInterval(8) }
            for (n in 0 until 12) {
                val encoded = encoder.encodeFrame(Picture.create(32, 32, ColorSpace.YUV420J).apply { fill(20 + n) },
                    ByteBuffer.allocate(32 * 32 * 3)).data
                val data = ByteArray(encoded.remaining()).also { encoded.get(it) }
                if (n == 0) {
                    val config = H264Utils.splitFrame(ByteBuffer.wrap(data)).filter { it.get(0).toInt() and 31 in setOf(7, 8) }
                        .fold(byteArrayOf()) { out, nal -> out + byteArrayOf(0, 0, 0, 1) + ByteArray(nal.remaining()).also { nal.get(it) } }
                    store.frame(VideoPacket.Frame(0, true, false, config), 1)
                }
                assertTrue(store.frame(VideoPacket.Frame(1_000_000L + n * 100_000, false, n == 0, data), 1))
            }
            val capture = store.capture(ReplaySettings())!!
            val video = capture.video.mapIndexed { n, frame ->
                val t = 1_000_000_000L + n * 100_000_000L
                frame.copy(time = if (n in unknown) MappedTime(null, 0, Long.MAX_VALUE) else MappedTime(t, 0, 0))
            }
            val logs = listOf(800_000_000L, 1_200_000_000L, 1_500_000_000L, 2_200_000_000L, null).mapIndexed { n, t ->
                LogEntry("row$n", DeviceLog(1_700_000_000_000_000_000L + n, 12, 12, 10001, 0, 4,
                    "Fixture", "row $n", byteArrayOf(n.toByte())), 1,
                    MappedTime(t, 0, if (t == null) Long.MAX_VALUE else 0), n.toLong(), true, n.toLong())
            }
            val touches = listOf(900_000_000L, 1_300_000_000L, 1_500_000_000L, 1_900_000_000L, null).mapIndexed { n, t ->
                TouchEntry(TouchSample(t ?: 2_600_000_000L, if (n % 2 == 0) INJECTED_INPUT_DEVICE else "touchscreen",
                    0, TouchAction.DOWN, n, n), 1, MappedTime(t, 0, if (t == null) Long.MAX_VALUE else 0), n.toLong(), 32, 32)
            }.filter { unmappedTouches || it.time.sequence != null }
            val clocks = listOf(1_000_000_000L, 1_900_000_000L).map { t ->
                ClockSample(0, "fixture", t, t, 1_700_000_000_000_000_000L + t, t, 0, 1, 0)
            }
            val state = StreamSnapshot(StreamState.CAPTURING, 1.0)
            SaveWriter().write(capture.copy(video = video, logs = logs, touches = touches, start = 0, end = 3_000_000_000L,
                endUncertainty = 0, windowClockRegion = 0, clocks = clocks, gaps = emptyList(),
                touchState = state, injectedTouchState = state,
                states = listOf("video", "device_log", "app_log").associateWith { state }), parent, { false }) {
                Files.move(it.partial, it.complete)
            }.directory
        }
        val record = manifest(directory)
        configure(record)
        Files.writeString(directory.resolve(ReplayFiles.RECORD), gson.toJson(record))
        return directory
    }

    private fun trim(dir: Path, from: Long = 1_300_000, to: Long = 1_800_000) = ReplayTrim().trim(dir, from, to,
        { false }, { Files.move(it.partial, it.complete) })

    private fun withoutPosition(sample: JsonObject) = sample.deepCopy().apply { remove("t_ns") }

    private fun addRanges(record: JsonObject) {
        val samples = record["timebase"].asJsonObject["samples"].asJsonArray
        val ranges = listOf(100_000_000L to 1_100_000_000L, 1_200_000_000L to 1_450_000_000L,
            1_600_000_000L to 1_850_000_000L, 1_400_000_000L to null, 1_000_000_000L to 1_300_000_000L,
            1_800_000_000L to 2_000_000_000L)
        record.add("gaps", gson.toJsonTree(ranges.map { (a, b) -> mapOf("kind" to "video", "from_t_ns" to a.toString(),
            "to_t_ns" to b?.toString(), "duration_uncertain" to (b == null)) }))
        record.add("video_gap_scopes", gson.toJsonTree(ranges.mapIndexed { n, (a, b) -> mapOf("gap_index" to n,
            "from_t_ns" to a.toString(), "to_t_ns" to b?.toString(), "before_sample" to samples[0], "after_sample" to samples[1]) }))
        val range = mapOf("from_t_ns" to "1200000000", "to_t_ns" to "1850000000")
        record.add("video_missing_ranges", gson.toJsonTree(listOf(range)))
        record.add("video_tail", gson.toJsonTree(range + mapOf("source_t_ns" to "1400000000", "source_pts_us" to "1400000")))
        record["touches"].asJsonObject.add("gaps", gson.toJsonTree(listOf(range)))
        record["touches"].asJsonObject["injected"].asJsonObject.add("gaps", gson.toJsonTree(listOf(range)))
        record["timebase"].asJsonObject.add("clock_steps", gson.toJsonTree(listOf(range + mapOf("kind" to "unmeasured",
            "duration_uncertain" to false, "recovery_sample" to samples[1]))))
    }

    private fun assertUnpositionedRanges(record: JsonObject) {
        val timebase = record["timebase"].asJsonObject
        val ranges = listOf("gaps", "video_gap_scopes", "video_missing_ranges").flatMap { record[it].asJsonArray.toList() } +
            record["video_tail"] + timebase["clock_steps"].asJsonArray.toList() +
            record["touches"].asJsonObject["gaps"].asJsonArray.toList() +
            record["touches"].asJsonObject["injected"].asJsonObject["gaps"].asJsonArray.toList()
        ranges.forEach { element ->
            val row = element.asJsonObject
            assertTrue(row["from_t_ns"].isJsonNull)
            assertTrue(row["to_t_ns"].isJsonNull)
            if (row.has("duration_uncertain")) assertTrue(row["duration_uncertain"].asBoolean)
            listOf("before_sample", "after_sample", "recovery_sample").forEach { name ->
                row[name]?.asJsonObject?.let { assertTrue(it["t_ns"].isJsonNull) }
            }
        }
        assertTrue(record["video_tail"].asJsonObject["source_t_ns"].isJsonNull)
        assertEquals("1400000", record["video_tail"].asJsonObject["source_pts_us"].asString)
        timebase["boot_segments"].asJsonArray.forEach { segment ->
            assertTrue(segment.asJsonObject["first_sample_t_ns"].isJsonNull)
            assertTrue(segment.asJsonObject["last_sample_t_ns"].isJsonNull)
        }
    }

    @Test
    fun unknownStartDoesNotReuseTheSourceOriginForKnownRowsOrClockEvidence() {
        val source = source(setOf(3), configure = ::addRanges)
        val before = bytes(source)
        assertTrue(manifest(source)["timebase"].asJsonObject["known"].asBoolean)
        assertTrue(rows(source, ReplayFiles.VIDEO_FRAMES)[3]["t_ns"].isJsonNull)
        assertFalse(rows(source, ReplayFiles.VIDEO_FRAMES)[4]["t_ns"].isJsonNull)
        val output = trim(source)
        val record = manifest(output)
        val timebase = record["timebase"].asJsonObject
        assertFalse(timebase["known"].asBoolean)
        assertTrue(timebase["window_start_elapsed_ns"].isJsonNull)
        assertEquals("500000000", timebase["window_duration_ns"].asString)
        assertEquals(Long.MAX_VALUE.toString(), timebase["window_end_uncertainty_ns"].asString)
        assertUnpositionedRanges(record)
        assertEquals(5, record["gaps"].asJsonArray.size())
        val originalFrames = rows(source, ReplayFiles.VIDEO_FRAMES).associateBy { it["source_pts_us"].asString }
        rows(output, ReplayFiles.VIDEO_FRAMES).forEach { row ->
            for (key in listOf("t_ns", "display_t_ns", "confirmed_t_end_ns")) assertTrue(row[key].isJsonNull, key)
            assertFalse(row["clock_alignment_known"].asBoolean)
            assertEquals(Long.MAX_VALUE.toString(), row["t_uncertainty_ns"].asString)
            val original = originalFrames.getValue(row["source_pts_us"].asString)
            for (key in listOf("elapsed_ns", "boot_segment", "source_pts_us", "source_duration_us")) assertEquals(original[key], row[key], key)
        }
        for (file in listOf(ReplayFiles.LOGCAT_DEVICE, ReplayFiles.LOGCAT_APP, ReplayFiles.TOUCHES)) {
            val original = rows(source, file)
            val kept = rows(output, file)
            assertEquals(original.size - 1, kept.size)
            kept.forEach { row ->
                assertTrue(row["t_ns"].isJsonNull, file)
                assertEquals(Long.MAX_VALUE.toString(), row["t_uncertainty_ns"].asString)
                val identity = if (file == ReplayFiles.TOUCHES) "device_mono_ns" else "record_id"
                val old = original.single { it[identity] == row[identity] }
                for (key in listOf("epoch_ns", "elapsed_ns", "device_mono_ns", "boot_segment")) assertEquals(old[key], row[key], key)
            }
        }
        val oldSamples = manifest(source)["timebase"].asJsonObject["samples"].asJsonArray
        timebase["samples"].asJsonArray.forEachIndexed { n, sample ->
            assertTrue(sample.asJsonObject["t_ns"].isJsonNull)
            assertEquals(withoutPosition(oldSamples[n].asJsonObject), withoutPosition(sample.asJsonObject))
        }
        record["parts"].asJsonArray.forEach { part ->
            assertFalse(part.asJsonObject["clock_alignment_known"].asBoolean)
            for (key in listOf("t_start_ns", "t_end_ns", "confirmed_t_end_ns")) assertTrue(part.asJsonObject[key].isJsonNull)
        }
        assertEquals("1300000000", rows(source, ReplayFiles.TOUCHES)[1]["t_ns"].asString)
        assertEquals("0", rows(output, ReplayFiles.TOUCHES).single { it["device_mono_ns"].asString == "1300000000" }["movie_us"].asString)
        val outputBefore = bytes(output)
        val repeated = trim(output, 0, 500_000)
        assertFalse(manifest(repeated)["timebase"].asJsonObject["known"].asBoolean)
        assertUnpositionedRanges(manifest(repeated))
        assertTrue(rows(repeated, ReplayFiles.VIDEO_FRAMES).all { it["t_ns"].isJsonNull })
        assertEquals(rows(output, ReplayFiles.LOGCAT_DEVICE), rows(repeated, ReplayFiles.LOGCAT_DEVICE))
        assertUnchanged(source, before)
        assertUnchanged(output, outputBefore)
    }

    @Test
    fun gapProofSamplesFollowTheSameOriginAsTheTimebaseAfterClippingAndRetrim() {
        val source = source(configure = ::addRanges)
        val before = bytes(source)
        val output = trim(source)
        val record = manifest(output)
        val scopes = record["video_gap_scopes"].asJsonArray.map { it.asJsonObject }
        assertEquals(listOf(0, 1, 2), scopes.map { it["gap_index"].asInt })
        assertEquals(listOf("0", "300000000", "100000000"), scopes.map { it["from_t_ns"].asString })
        assertEquals(listOf("150000000", "500000000", null), scopes.map { it["to_t_ns"].takeUnless { it.isJsonNull }?.asString })
        val samples = record["timebase"].asJsonObject["samples"].asJsonArray
        val originalSamples = manifest(source)["timebase"].asJsonObject["samples"].asJsonArray
        samples.forEachIndexed { n, sample -> assertEquals(withoutPosition(originalSamples[n].asJsonObject), withoutPosition(sample.asJsonObject)) }
        scopes.forEach { scope ->
            assertEquals(samples[0], scope["before_sample"])
            assertEquals(samples[1], scope["after_sample"])
            assertEquals("-300000000", scope["before_sample"].asJsonObject["t_ns"].asString)
        }
        val outputBefore = bytes(output)
        val repeated = manifest(trim(output, 100_000, 400_000))
        val againSamples = repeated["timebase"].asJsonObject["samples"].asJsonArray
        repeated["video_gap_scopes"].asJsonArray.forEach { scope ->
            assertEquals(againSamples[0], scope.asJsonObject["before_sample"])
            assertEquals(againSamples[1], scope.asJsonObject["after_sample"])
            val index = scope.asJsonObject["gap_index"].asInt
            assertEquals(repeated["gaps"].asJsonArray[index].asJsonObject["from_t_ns"], scope.asJsonObject["from_t_ns"])
        }
        assertUnchanged(source, before)
        assertUnchanged(output, outputBefore)
    }

    @Test
    fun injectedCountCountsOnlyWrittenRowsIncludingUnmappedRowsAndCanReachZero() {
        val source = source()
        val before = bytes(source)
        val output = trim(source)
        fun check(dir: Path, expected: Int) {
            val taps = rows(dir, ReplayFiles.TOUCHES)
            val record = manifest(dir)
            assertEquals(expected, taps.count { it["source"].asString == "injected" })
            assertEquals(expected, record["touches"].asJsonObject["injected"].asJsonObject["count"].asInt)
            assertEquals(taps.size, record["counts"].asJsonObject["touches"].asInt)
            assertEquals(taps.count { !it["movie_us"].isJsonNull }, record["touches"].asJsonObject["in_movie_count"].asInt)
        }
        check(output, 2)
        val outputBefore = bytes(output)
        val repeated = trim(output, 0, 100_000)
        check(repeated, 1) // The unmapped injected row cannot be proven outside either window.
        assertUnchanged(source, before)
        assertUnchanged(output, outputBefore)
    }

    @Test
    fun injectedCountIsZeroWhenOnlyTouchscreenRowsSurvive() {
        val source = source(unmappedTouches = false)
        val before = bytes(source)
        val output = trim(source, 1_300_000, 1_400_000)
        val taps = rows(output, ReplayFiles.TOUCHES)
        assertEquals(1, taps.size)
        assertTrue(taps.all { it["source"].asString == "touchscreen" })
        assertEquals(0, manifest(output)["touches"].asJsonObject["injected"].asJsonObject["count"].asInt)
        assertUnchanged(source, before)
    }

    @Test
    fun unknownBothEndsRetainUnprovableRowsAndRangesOnRetrim() {
        val source = source(setOf(3, 7), configure = ::addRanges)
        val before = bytes(source)
        val output = trim(source)
        assertUnpositionedRanges(manifest(output))
        assertEquals(6, manifest(output)["gaps"].asJsonArray.size())
        for (file in listOf(ReplayFiles.LOGCAT_DEVICE, ReplayFiles.LOGCAT_APP, ReplayFiles.TOUCHES)) {
            assertEquals(rows(source, file).size, rows(output, file).size)
            assertTrue(rows(output, file).all { it["t_ns"].isJsonNull })
        }
        val outputBefore = bytes(output)
        val repeated = trim(output, 100_000, 400_000)
        assertFalse(manifest(repeated)["timebase"].asJsonObject["known"].asBoolean)
        assertUnpositionedRanges(manifest(repeated))
        assertUnchanged(source, before)
        assertUnchanged(output, outputBefore)
    }

    @Test
    fun knownOriginPreservesKnownRowsWithoutUpgradingAnUnknownMiddleFrame() {
        val source = source(setOf(5))
        val before = bytes(source)
        val output = trim(source)
        val timebase = manifest(output)["timebase"].asJsonObject
        assertTrue(timebase["known"].asBoolean)
        assertEquals("1300000000", timebase["window_start_elapsed_ns"].asString)
        assertEquals("500000000", timebase["window_duration_ns"].asString)
        val frames = rows(output, ReplayFiles.VIDEO_FRAMES)
        assertEquals("0", frames.first()["t_ns"].asString)
        assertTrue(frames[2]["t_ns"].isJsonNull)
        assertFalse(frames[2]["clock_alignment_known"].asBoolean)
        assertEquals("300000000", frames[3]["t_ns"].asString)
        assertTrue(frames[3]["clock_alignment_known"].asBoolean)
        val outputBefore = bytes(output)
        val repeated = trim(output, 0, 500_000)
        assertTrue(manifest(repeated)["timebase"].asJsonObject["known"].asBoolean)
        assertEquals(frames.map { it["t_ns"] }, rows(repeated, ReplayFiles.VIDEO_FRAMES).map { it["t_ns"] })
        assertUnchanged(source, before)
        assertUnchanged(output, outputBefore)
    }
}
