package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonObject
import com.google.gson.JsonParser
import org.jcodec.codecs.h264.H264Encoder
import org.jcodec.codecs.h264.H264Utils
import org.jcodec.common.model.ColorSpace
import org.jcodec.common.model.Picture
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.ByteBuffer
import java.nio.file.Files
import java.nio.file.Path
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

/** T1: freeze schema 4's trim formula independently of schema 5's future interval-union calculation. */
class ReplayTrimCoverageBaselineTest {
    @TempDir lateinit var root: Path
    private val host = 4_000_000_000_000_000_000L

    private fun manifest(dir: Path) = JsonParser.parseString(Files.readString(dir.resolve(ReplayFiles.RECORD))).asJsonObject
    private fun rows(dir: Path) = Files.readAllLines(dir.resolve(ReplayFiles.VIDEO_FRAMES)).map { JsonParser.parseString(it).asJsonObject }
    private fun files(dir: Path) = Files.list(dir).use { paths -> paths.toList().associate { it.fileName.toString() to Files.readAllBytes(it) } }
    private fun coverage(record: JsonObject, kind: String) = record["coverage"].asJsonObject[kind].asJsonObject["availableSeconds"].asDouble

    @Test
    fun schemaFourAvailableSecondsForFixedTrimInputs() {
        CaptureStore(root.resolve("ring"), minFree = 0).use { store ->
            store.generation(1)
            store.session(VideoPacket.Session(32, 32), 1)
            assertTrue(store.clock.add("baseline", listOf(1_000_000_000L, 1_000_000_000L,
                1_700_000_000_000_000_000L, 1_000_000_000L), host, host))
            val encoder = H264Encoder.createH264Encoder().apply { setKeyInterval(8) }
            // Small inputs assembled in this test, following ReplayTrimTest's existing in-memory encoder pattern.
            repeat(16) { n ->
                val picture = Picture.create(32, 32, ColorSpace.YUV420J).apply { fill(20 + n) }
                val encoded = encoder.encodeFrame(picture, ByteBuffer.allocate(32 * 32 * 3)).data
                val bytes = ByteArray(encoded.remaining()).also { encoded.get(it) }
                if (n == 0) {
                    val config = H264Utils.splitFrame(ByteBuffer.wrap(bytes)).filter { it.get(0).toInt() and 31 in setOf(7, 8) }
                        .fold(byteArrayOf()) { acc, nal -> acc + byteArrayOf(0, 0, 0, 1) + ByteArray(nal.remaining()).also { nal.get(it) } }
                    store.frame(VideoPacket.Frame(0, true, false, config), 1, host)
                }
                assertTrue(store.frame(VideoPacket.Frame(1_000_000 + n * 100_000L, false, n % 8 == 0, bytes),
                    1, host + n * 100_000_000L))
            }
            val capture = assertNotNull(store.capture(ReplaySettings(), host = host + 1_500_000_000))
            val video = capture.video.map { frame ->
                val t = frame.pts * 1000
                frame.copy(time = MappedTime(t, 0, 0, t))
            }
            val logs = video.mapIndexed { n, frame ->
                LogEntry("row-$n", DeviceLog(1_700_000_000_000_000_000L + n * 100_000_000L, 12, 12, 10001,
                    0, 4, "Baseline", "row $n", byteArrayOf(1)), 1, frame.time, frame.retainedAt, true, frame.host)
            }
            val cases = listOf(
                emptyList<Pair<Long?, Long?>>() to 0.8,
                listOf(1_600_000_000L to 1_800_000_000L) to 0.6,
                listOf(null to 1_800_000_000L) to 0.8, // Unknown start subtracts zero.
                listOf(1_500_000_000L to 1_800_000_000L, 1_700_000_000L to 2_000_000_000L) to 0.2,
            )
            for ((i, case) in cases.withIndex()) {
                val (ranges, expected) = case
                val gaps = ranges.map { (from, to) -> CaptureGap("device_log", from, to, "baseline", 0, 1, 0) } +
                    CaptureGap("clock", 1_500_000_000L, 1_600_000_000L, "clock unknown", 0, 1, 0)
                val source = SaveWriter().write(capture.copy(id = "trim-baseline-$i", video = video, logs = logs,
                    start = 0, end = 3_000_000_000, endUncertainty = 0, gaps = gaps,
                    states = listOf("video", "device_log", "app_log").associateWith { StreamSnapshot(StreamState.CAPTURING, 180.0) }),
                    root, { false }) { Files.move(it.partial, it.complete) }.directory
                val before = files(source)
                val frames = rows(source)
                val output = ReplayTrim().trim(source, frames[4]["movie_pts_us"].asString.toLong(),
                    frames[11]["movie_end_us"].asString.toLong(), { false }, { Files.move(it.partial, it.complete) })
                val record = manifest(output)
                assertEquals(4, manifest(source)["schema"].asInt)
                assertEquals(4, record["schema"].asInt)
                assertEquals("800000000", record["timebase"].asJsonObject["window_duration_ns"].asString)
                assertEquals(0.7, coverage(record, "video"), 1e-9, "case $i")
                assertEquals(expected, coverage(record, "device_log"), 1e-9, "case $i")
                assertEquals(0.8, coverage(record, "app_log"), 1e-9, "case $i")
                assertEquals(0.0, coverage(record, "audio"), 1e-9, "case $i")
                val after = files(source)
                assertEquals(before.keys, after.keys)
                before.forEach { (name, bytes) -> assertContentEquals(bytes, after.getValue(name), name) }
            }
        }
    }
}
