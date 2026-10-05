package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonParser
import org.jcodec.codecs.h264.H264Decoder
import org.jcodec.codecs.h264.H264Encoder
import org.jcodec.codecs.h264.H264Utils
import org.jcodec.common.io.NIOUtils
import org.jcodec.common.model.ColorSpace
import org.jcodec.common.model.Picture
import org.jcodec.containers.mp4.MP4Packet
import org.jcodec.containers.mp4.demuxer.MP4Demuxer
import java.nio.ByteBuffer
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.CancellationException
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class VideoClockRegionTest {
    @Test
    fun reconnectKeepsNewStreamPlayableWithTheClosedOldGenerationVideoLoss() {
        val root = Files.createTempDirectory("replay-reconnect-video-gap-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime() - 500_000_000
        val wall = 1_700_000_000_000_000_000L
        fun anchor(boot: String, elapsed: Long, at: Long, wallOffset: Long = 0) {
            assertTrue(store.clock.add(boot, listOf(elapsed, elapsed, wall + wallOffset + elapsed, elapsed),
                at, at, wall + at - host))
        }
        fun encoded(encoder: H264Encoder, number: Int): ByteArray {
            val data = encoder.encodeFrame(Picture.create(32, 32, ColorSpace.YUV420J).apply { fill(15 + number) },
                ByteBuffer.allocate(65536)).data
            return ByteArray(data.remaining()).also { data.get(it) }
        }
        fun config(bytes: ByteArray) = H264Utils.splitFrame(ByteBuffer.wrap(bytes))
            .filter { it.get(0).toInt() and 31 in setOf(7, 8) }
            .fold(byteArrayOf()) { result, nal -> result + byteArrayOf(0, 0, 0, 1) +
                ByteArray(nal.remaining()).also { nal.get(it) } }
        try {
            store.generation(1)
            store.session(VideoPacket.Session(32, 32), 1)
            anchor("old", 1_000_000_000, host)
            val oldIdr = encoded(H264Encoder.createH264Encoder(), 0)
            store.frame(VideoPacket.Frame(0, true, false, config(oldIdr)), 1)
            assertTrue(store.frame(VideoPacket.Frame(1_000_000, false, true, oldIdr), 1, host))
            anchor("old", 1_200_000_000, host + 200_000_000)
            store.clock.boundary()
            store.status("video", StreamState.RECOVERING, "connection ended", 1)
            store.generation(2) // Closes the old channel and rejects its late callbacks.
            store.session(VideoPacket.Session(32, 32), 2)
            val resumedHost = System.nanoTime()
            anchor("new", 1_000_000_000, resumedHost, 20_000_000_000)
            store.app("com.example.target", 10001, setOf(12), 2, uidExclusive = true)
            val encoder = H264Encoder.createH264Encoder().apply { setKeyInterval(10) }
            val decoder = H264Decoder()
            val expected = mutableMapOf<Long, List<ByteArray>>()
            for (n in 0..20) {
                val bytes = encoded(encoder, n)
                val pts = 1_000_000 + n * 100_000L
                if (n == 0) store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 2)
                expected[pts] = decoder.decodeFrame(ByteBuffer.wrap(bytes), Picture.create(32, 32, ColorSpace.YUV420J).data)
                    .data.map { it.copyOf() }
                assertTrue(store.frame(VideoPacket.Frame(pts, false, n % 10 == 0, bytes), 2, resumedHost + n * 100_000_000L))
            }
            for (event in 1..3) for (phase in listOf("REQUEST", "DRAW", "FRAME_COMMIT")) {
                val elapsed = 1_000_000_000 + event * 200_000_000L
                store.log(DeviceLog(wall + 20_000_000_000 + elapsed, 12, 12, 10001, 0, 4,
                    "Fixture", "$event $phase", byteArrayOf(event.toByte())), 2, resumedHost + elapsed - 1_000_000_000)
            }
            store.log(DeviceLog(wall + 999_000_000_000, 12, 12, 10001, 0, 4, "Fixture", "unsupported", byteArrayOf(0)),
                2, resumedHost + 2_000_000_000)
            anchor("new", 3_000_000_000, resumedHost + 2_000_000_000, 20_000_000_000)
            val capture = store.capture(ReplaySettings(replaySeconds = 2))!!
            val gap = capture.gaps.single { it.stream == "video" }
            assertEquals(1L, gap.generation)
            assertTrue(gap.fromNs != null && gap.toNs != null)
            assertEquals(Long.MAX_VALUE, gap.boundaryUncertaintyNs)
            assertEquals(21, capture.video.size)
            assertTrue(capture.video.all { it.generation == 2L && it.time.epoch == capture.windowClockEpoch &&
                it.time.sequence != null && it.time.uncertainty != Long.MAX_VALUE })
            assertFailsWith<CancellationException> { SaveWriter().write(capture, root, { true }) { a, b -> Files.move(a, b) } }
            val output = save(capture, root)
            val record = manifest(output)
            assertEquals(1, record["parts"].asJsonArray.size(), "Closed old video loss must not fragment the resumed stream")
            val part = record["parts"].asJsonArray.single().asJsonObject
            assertEquals("2000001", part["duration_us"].asString)
            assertTrue(part["clock_alignment_known"].asBoolean)
            assertEquals(listOf("video"), output.missingKinds) // Original historical loss is still explicit.
            assertEquals(Long.MAX_VALUE.toString(), record["gaps"].asJsonArray.single().asJsonObject["boundary_uncertainty_ns"].asString)
            assertTrue(record["gaps"].asJsonArray.single().asJsonObject["duration_uncertain"].asBoolean)
            val rows = Files.readAllLines(output.directory.resolve("frames.jsonl")).map { JsonParser.parseString(it).asJsonObject }
            assertEquals(capture.video.map { it.pts }, rows.filter { it["presented"].asBoolean }.map { it["source_pts_us"].asLong })
            NIOUtils.readableChannel(output.directory.resolve(part["file"].asString).toFile()).use { channel ->
                val track = MP4Demuxer.createMP4Demuxer(channel).videoTrack
                val partDecoder = H264Decoder()
                rows.forEach { row ->
                    val packet = track.nextFrame() ?: error("Missing resumed MP4 sample")
                    val picture = partDecoder.decodeFrame(packet.data, Picture.create(32, 32, ColorSpace.YUV420J).data)
                    expected.getValue(row["source_pts_us"].asLong).zip(picture.data).forEach { (a, b) -> assertContentEquals(a, b) }
                    assertEquals(row["media_pts_us"].asLong, (packet as MP4Packet).mediaPts)
                    assertEquals(row["display_duration_us"].asLong, packet.duration)
                }
                assertEquals(null, track.nextFrame())
            }
            val device = Files.readAllLines(output.directory.resolve("logcat-device.jsonl"))
            val app = Files.readAllLines(output.directory.resolve("logcat-app.jsonl"))
            assertEquals(10, device.size)
            assertEquals(9, app.size)
            assertTrue(app.all { it in device && JsonParser.parseString(it).asJsonObject["app_membership"].asBoolean })
            assertTrue(JsonParser.parseString(device.last()).asJsonObject["window_ns"].isJsonNull)
            anchor("new", 4_000_000_000, resumedHost + 3_000_000_000, 20_000_000_000)
            val retry = save(capture, root)
            assertEquals(record["parts"], manifest(retry)["parts"])
            assertEquals(record["gaps"], manifest(retry)["gaps"])
            for (file in listOf("frames.jsonl", "logcat-device.jsonl", "logcat-app.jsonl"))
                assertEquals(Files.readAllLines(output.directory.resolve(file)), Files.readAllLines(retry.directory.resolve(file)))
            assertEquals(gap, capture.gaps.single())
            store.release(capture.id)
        } finally {
            store.close()
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }

    @Test
    fun muxOldVideoLossRequiresClosedPositiveOlderOwnershipAndPreservesSourceGuards() {
        val first = VideoEntry(Path.of("unused"), 0, 1, 1_000_000, true, 32, 32, 2,
            MappedTime(1_000_000_000, 5, 1_000_000, 1_000_000_000), 1_000_000_000,
            byteArrayOf(1), 0, 1)
        val next = first.copy(pts = 1_100_000, key = false,
            time = first.time.copy(elapsed = 1_100_000_000, sequence = 1_100_000_000))
        val old = CaptureGap("video", 100_000_000, 200_000_000, "connection ended", Long.MAX_VALUE, 1, 2)
        assertFalse(first.continuousTo(next, listOf(old))) // Prune/retention still keep the unbounded loss.
        assertTrue(first.muxContinuousTo(next, listOf(old)))
        for (blocked in listOf(old.copy(toNs = null), old.copy(generation = 0), old.copy(generation = -1),
                old.copy(generation = 2), old.copy(generation = 3), old.copy(stream = "clock"))) {
            assertFalse(first.muxContinuousTo(next, listOf(blocked)))
        }
        assertFalse(first.muxContinuousTo(next, listOf(old, old.copy(generation = 2))))
        for (broken in listOf(next.copy(generation = 3), next.copy(session = 2), next.copy(width = 64),
                next.copy(height = 64), next.copy(config = byteArrayOf(2)), next.copy(pts = first.pts),
                next.copy(time = next.time.copy(epoch = 6)), next.copy(time = next.time.copy(sequence = null)),
                next.copy(time = next.time.copy(sequence = first.time.sequence)),
                next.copy(time = next.time.copy(uncertainty = Long.MAX_VALUE)))) {
            assertFalse(first.muxContinuousTo(broken, listOf(old)))
        }
        assertFalse(first.copy(time = first.time.copy(sequence = null)).muxContinuousTo(next, listOf(old)))
        assertFalse(first.copy(time = first.time.copy(uncertainty = Long.MAX_VALUE)).muxContinuousTo(next, listOf(old)))
    }

    @Test
    fun muxClockRegionDoesNotRelaxSourceBreaksOrUnprovenClockScopes() {
        val first = VideoEntry(Path.of("unused"), 0, 1, 1_000_000, true, 32, 32, 1,
            MappedTime(1_000_000_000, 0, 1_000_000, 1_000_000_000), 1_000_000_000,
            byteArrayOf(1), 0, 1)
        val next = first.copy(pts = 1_100_000, key = false,
            time = MappedTime(1_100_000_000, 0, 1_000_000, 1_100_000_000))
        val scope = CaptureGap("clock", 900_000_000, 1_200_000_000, "bounded", 1_000_000)
        assertFalse(first.continuousTo(next, listOf(scope))) // Retention's existing contract is unchanged.
        assertTrue(first.muxContinuousTo(next, listOf(scope)))
        for (broken in listOf(next.copy(generation = 2), next.copy(session = 2), next.copy(width = 64),
                next.copy(height = 64), next.copy(config = byteArrayOf(2)), next.copy(pts = first.pts),
                next.copy(time = next.time.copy(epoch = 1)), next.copy(time = next.time.copy(sequence = null)),
                next.copy(time = next.time.copy(sequence = first.time.sequence)),
                next.copy(time = next.time.copy(uncertainty = Long.MAX_VALUE)))) {
            assertFalse(first.muxContinuousTo(broken, listOf(scope)))
        }
        for (unproven in listOf(scope.copy(fromNs = null), scope.copy(toNs = null),
                scope.copy(boundaryUncertaintyNs = null), scope.copy(boundaryUncertaintyNs = Long.MAX_VALUE))) {
            assertFalse(first.muxContinuousTo(next, listOf(unproven)))
        }
        assertFalse(first.muxContinuousTo(next, listOf(scope, scope.copy(stream = "video", reason = "capacity drop"))))
        assertFalse(first.muxContinuousTo(next, listOf(scope.copy(fromNs = 1_050_000_000))))
        assertFalse(first.muxContinuousTo(next, listOf(scope.copy(toNs = 1_050_000_000))))
    }

    @Test
    fun denseVideoInsideABoundedClockGapKeepsSourceFramesPlayableAndRetryFrozen() {
        val root = Files.createTempDirectory("replay-dense-clock-gap-")
        try {
            val encoder = H264Encoder.createH264Encoder().apply { setKeyInterval(10) }
            val decoder = H264Decoder()
            val expected = mutableMapOf<Long, List<ByteArray>>()
            val packets = (0..40).map { n ->
                val buffer = encoder.encodeFrame(Picture.create(32, 32, ColorSpace.YUV420J).apply { fill(15 + n) },
                    ByteBuffer.allocate(65536)).data
                ByteArray(buffer.remaining()).also { buffer.get(it) }
            }
            val config = H264Utils.splitFrame(ByteBuffer.wrap(packets.first()))
                .filter { it.get(0).toInt() and 31 in setOf(7, 8) }
                .fold(byteArrayOf()) { bytes, nal -> bytes + byteArrayOf(0, 0, 0, 1) +
                    ByteArray(nal.remaining()).also { nal.get(it) } }
            val source = root.resolve("source.h264")
            Files.write(source, packets.fold(byteArrayOf()) { bytes, packet -> bytes + packet })
            var offset = 0L
            val frames = packets.mapIndexed { n, packet ->
                val pts = 1_000_000 + n * 100_000L
                expected[pts] = decoder.decodeFrame(ByteBuffer.wrap(packet), Picture.create(32, 32, ColorSpace.YUV420J).data)
                    .data.map { it.copyOf() }
                VideoEntry(source, offset, packet.size, pts, n % 10 == 0, 32, 32, 1,
                    MappedTime(pts * 1000, 0, 1_000_000, n * 100_000_000L), n * 100_000_000L,
                    config, n * 100_000_000L, 1).also { offset += packet.size }
            }
            val before = ClockSample(0, "fixture", 2_000_000_000, 2_000_000_000, 0, 2_000_000_000,
                0, 0, 0, -1_000_000_000, 0)
            val invalid = before.copy(before = 3_000_000_000, after = 3_000_000_000, mono = 3_000_000_000,
                received = 41_716_666)
            val recovery = before.copy(before = 4_000_000_000, after = 4_000_000_000, mono = 4_000_000_000)
            assertFalse(invalid.valid)
            val gap = CaptureGap("clock", 1_000_000_000, 3_000_000_000, "invalid RTT", Long.MAX_VALUE, 1, 0)
            val logSource = DeviceLog(1_700_000_002_500_000_000, 12, 12, 10001, 0, 4, "Fixture", "operation", byteArrayOf(1))
            val knownLog = LogEntry("known", logSource, 1, MappedTime(2_500_000_000, 0, 1_000_000, 1_500_000_000),
                1_500_000_000, true, 0)
            val unknownLog = knownLog.copy(id = "unknown", time = MappedTime(null, 0, Long.MAX_VALUE, null), app = null)
            val capture = FrozenCapture("fixed", "fixture", 1, 0, 4_100_000_000, 180, frames,
                listOf(knownLog, unknownLog), listOf(gap), listOf(before, invalid, recovery), emptyList(), ReplaySettings(),
                mapOf("video" to StreamSnapshot(StreamState.CAPTURING)), windowClockEpoch = 0,
                clockRecoveries = mapOf(gap to recovery))
            val output = save(capture, root)
            val manifest = manifest(output)
            val parts = manifest["parts"].asJsonArray
            // This fixture has one bounded clock-uncertain region and no acquisition/codec/epoch break.
            assertEquals(3, parts.size(), "Clock uncertainty must not create one part per source frame")
            val uncertain = parts.single { !it.asJsonObject["clock_alignment_known"].asBoolean }.asJsonObject
            assertTrue(uncertain["duration_us"].asLong >= 2_000_001)
            for (key in listOf("window_start_ns", "window_end_ns", "confirmed_window_end_ns")) assertTrue(uncertain[key].isJsonNull)
            assertEquals(listOf("video"), output.missingKinds)
            assertEquals(Long.MAX_VALUE.toString(), manifest["gaps"].asJsonArray.single().asJsonObject["boundary_uncertainty_ns"].asString)
            assertTrue(manifest["gaps"].asJsonArray.single().asJsonObject["duration_uncertain"].asBoolean)
            assertFalse(manifest["video_missing_ranges"].asJsonArray.isEmpty)
            val rows = Files.readAllLines(output.directory.resolve("frames.jsonl")).map { JsonParser.parseString(it).asJsonObject }
            assertEquals(frames.map { it.pts }, rows.filter { it["presented"].asBoolean }.map { it["source_pts_us"].asLong })
            val uncertainRows = rows.filter { it["part"].asString == uncertain["file"].asString }
            assertTrue(uncertainRows.all { it["window_ns"].isJsonNull && !it["clock_alignment_known"].asBoolean })
            assertTrue(uncertainRows.all { !it["mapped_window_ns"].isJsonNull })
            assertTrue(rows.all { it["uncertainty_ns"].asLong == 1_000_000L })
            val deviceLog = Files.readAllLines(output.directory.resolve("logcat-device.jsonl"))
            val appLog = Files.readAllLines(output.directory.resolve("logcat-app.jsonl"))
            assertEquals(2, deviceLog.size)
            assertEquals(listOf(deviceLog.first()), appLog)
            val unknownRow = JsonParser.parseString(deviceLog.last()).asJsonObject
            assertTrue(unknownRow["window_ns"].isJsonNull && unknownRow["app_membership"].isJsonNull)
            assertEquals(Long.MAX_VALUE.toString(), unknownRow["uncertainty_ns"].asString)
            parts.forEach { part ->
                val name = part.asJsonObject["file"].asString
                NIOUtils.readableChannel(output.directory.resolve(name).toFile()).use { channel ->
                    val track = MP4Demuxer.createMP4Demuxer(channel).videoTrack
                    val partDecoder = H264Decoder()
                    rows.filter { it["part"].asString == name }.forEach { row ->
                        val packet = track.nextFrame() ?: error("Missing MP4 sample")
                        val picture = partDecoder.decodeFrame(packet.data, Picture.create(32, 32, ColorSpace.YUV420J).data)
                        expected.getValue(row["source_pts_us"].asLong).zip(picture.data).forEach { (a, b) -> assertContentEquals(a, b) }
                        assertEquals(row["media_pts_us"].asLong, (packet as MP4Packet).mediaPts)
                        assertEquals(row["display_duration_us"].asLong, packet.duration)
                        assertEquals(1_000_000, packet.timescale)
                    }
                    assertEquals(null, track.nextFrame())
                }
            }
            val retry = save(capture, root)
            assertEquals(manifest["parts"], manifest(retry)["parts"])
            assertEquals(manifest["video_missing_ranges"], manifest(retry)["video_missing_ranges"])
            assertEquals(Files.readAllLines(output.directory.resolve("frames.jsonl")), Files.readAllLines(retry.directory.resolve("frames.jsonl")))
            assertEquals(deviceLog, Files.readAllLines(retry.directory.resolve("logcat-device.jsonl")))
            assertEquals(appLog, Files.readAllLines(retry.directory.resolve("logcat-app.jsonl")))
            val laterSample = recovery.copy(before = 5_000_000_000, after = 5_000_000_000, mono = 5_000_000_000)
            val later = save(capture.copy(clocks = capture.clocks + laterSample), root)
            assertEquals(manifest["parts"], manifest(later)["parts"])
            assertEquals(gap, capture.gaps.single())
            val outside = save(capture.copy(start = 3_200_000_000, seconds = 1), root)
            val outsideParts = manifest(outside)["parts"].asJsonArray
            assertEquals(1, outsideParts.size())
            assertTrue(outsideParts.single().asJsonObject["clock_alignment_known"].asBoolean)
        } finally {
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }

    private fun save(capture: FrozenCapture, root: Path) = SaveWriter().write(capture, root, { false }) { a, b -> Files.move(a, b) }
    private fun manifest(output: SaveOutput) = JsonParser.parseString(Files.readString(output.directory.resolve("session.json"))).asJsonObject
}
