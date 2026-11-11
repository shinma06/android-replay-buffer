package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonParser
import org.jcodec.codecs.h264.H264Decoder
import org.jcodec.codecs.h264.H264Encoder
import org.jcodec.codecs.h264.H264Utils
import org.jcodec.common.io.NIOUtils
import org.jcodec.common.model.ColorSpace
import org.jcodec.common.model.Picture
import org.jcodec.containers.mp4.MP4Packet
import org.jcodec.containers.mp4.MP4Util
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
    fun maximumHistoryWindowSelectionDoesNotAllocateAnAnchorListForEveryGap() {
        val clocks = List(4096) { n ->
            val time = n * 2_000_000L
            ClockSample(0, "boot", time, time, time, time, 0, 0, 0)
        }
        val gaps = List(4096) { n ->
            val from = (n % 4095) * 2_000_000L
            CaptureGap(listOf("video", "device_log", "app_log", "clock")[n % 4], from + 100, from + 500,
                "recovered history", Long.MAX_VALUE, 1, 0)
        }
        val memory = java.lang.management.ManagementFactory.getThreadMXBean() as com.sun.management.ThreadMXBean
        assertTrue(memory.isThreadAllocatedMemorySupported)
        val wasEnabled = memory.isThreadAllocatedMemoryEnabled
        memory.isThreadAllocatedMemoryEnabled = true
        try {
            val thread = Thread.currentThread().threadId()
            val before = memory.getThreadAllocatedBytes(thread)
            val started = System.nanoTime()
            val selected = gaps.inWindow(20_000_000_000, 21_000_000_000, 1_000_000, clocks)
            val elapsed = System.nanoTime() - started
            val allocated = memory.getThreadAllocatedBytes(thread) - before
            println("window-history gaps=4096 clocks=4096 allocated_bytes=$allocated elapsed_ns=$elapsed")
            assertTrue(selected.isEmpty())
            assertTrue(allocated < 16 * 1024 * 1024, "Window selection allocated $allocated bytes for bounded history")
            assertTrue(gaps.all { it.boundaryUncertaintyNs == Long.MAX_VALUE })
        } finally {
            memory.isThreadAllocatedMemoryEnabled = wasEnabled
        }
    }

    @Test
    fun videoLossEnvelopeRequiresBothFiniteNormalAnchorsAndStillBlocksRealLoss() {
        val before = ClockSample(0, "old", 4_000_000_000, 4_000_000_000, 0, 4_000_000_000,
            0, 0, -1_000_000_000)
        val after = before.copy(region = 2, boot = "new", before = 7_000_000_000, after = 7_000_000_000, mono = 7_000_000_000)
        val gap = CaptureGap("video", 3_500_000_000, 5_500_000_000, "connection ended", Long.MAX_VALUE, 1, 1)
        val scope = listOf(gap).gapScopes(listOf(before, after)).single()
        assertEquals(3_000_000_000L, scope.fromNs)
        assertEquals(6_000_000_000L, scope.toNs)
        assertEquals(1_000_000L, scope.boundaryUncertaintyNs)
        val lastBefore = before.copy(before = 3_999_000_000, after = 4_001_000_000)
        val firstAfter = after.copy(before = 6_999_000_000, after = 7_001_000_000)
        val proofs = mutableMapOf<CaptureGap, Pair<ClockSample, ClockSample>>()
        val duplicateScope = listOf(gap).gapScopes(listOf(before, lastBefore, firstAfter, after), videoProofs = proofs).single()
        assertEquals(2_000_000L, duplicateScope.boundaryUncertaintyNs)
        assertEquals(lastBefore to firstAfter, proofs[gap])
        assertEquals(Long.MAX_VALUE, gap.boundaryUncertaintyNs) // History and overlapping warnings keep the original uncertainty.
        for (kind in listOf("video", "device_log", "app_log")) {
            val loss = gap.copy(stream = kind)
            assertTrue(listOf(loss).inWindow(7_000_000_000, 8_000_000_000, 1_000_000, listOf(before, after)).isEmpty())
            assertEquals(listOf(loss), listOf(loss).inWindow(6_001_500_000, 8_000_000_000, 1_000_000, listOf(before, after)))
            assertEquals(listOf(loss), listOf(loss).inWindow(7_000_000_000, 8_000_000_000, Long.MAX_VALUE, listOf(before, after)))
        }
        val first = VideoEntry(Path.of("unused"), 0, 1, 1_000_000, true, 32, 32, 1,
            MappedTime(1_000_000_000, 0, 1_000_000, 1_000_000_000), 1_000_000_000, byteArrayOf(1), 0, 1)
        fun frame(at: Long) = first.copy(pts = at / 1000, time = first.time.copy(elapsed = at, sequence = at))
        assertFalse(first.continuousTo(frame(1_100_000_000), listOf(gap)))
        assertTrue(first.continuousTo(frame(1_100_000_000), listOf(scope)))
        assertTrue(frame(7_000_000_000).continuousTo(frame(7_100_000_000), listOf(scope)))
        assertFalse(frame(4_000_000_000).continuousTo(frame(4_100_000_000), listOf(scope)))
        assertFalse(frame(2_000_000_000).continuousTo(frame(7_000_000_000), listOf(scope)))
        assertFalse(frame(2_998_500_000).continuousTo(frame(2_999_000_000), listOf(scope))) // Both error margins count.
        for (unproven in listOf(gap.copy(fromNs = null), gap.copy(toNs = null), gap.copy(generation = 0),
                gap.copy(generation = -1), gap.copy(fromNs = 6_000_000_000))) {
            assertEquals(unproven, listOf(unproven).gapScopes(listOf(before, after)).single())
            assertEquals(listOf(unproven), listOf(unproven).inWindow(7_000_000_000, 8_000_000_000, 1_000_000, listOf(before, after)))
        }
        // Another boot segment has only an approximate place on the axis, so its read cannot bound the loss.
        for (unproven in listOf(after.copy(segment = 1), after.copy(segmentOffset = Long.MAX_VALUE),
                after.copy(received = 100_000_000), after.copy(sent = 1),
                after.copy(after = after.before - 1), after.copy(before = -1, after = -1),
                after.copy(after = after.before + 2_000_001))) {
            assertEquals(gap, listOf(gap).gapScopes(listOf(before, unproven)).single())
            assertEquals(listOf(gap), listOf(gap).inWindow(7_000_000_000, 8_000_000_000, 1_000_000, listOf(before, unproven)))
        }
        assertEquals(gap, listOf(gap).gapScopes(listOf(before)).single())
        assertEquals(gap, listOf(gap).gapScopes(listOf(after)).single())
        val known = gap.copy(boundaryUncertaintyNs = 1_000_000)
        assertEquals(known, listOf(known).gapScopes(listOf(before, after)).single())
    }

    @Test
    fun denseVideoInsideAResolvedClockGapStaysAlignedPlayableAndRetryFrozen() {
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
                    config, n * 100_000_000L, 1, decodeRun = 1, packetOrdinal = n + 1L).also { offset += packet.size }
            }
            val before = ClockSample(0, "fixture", 2_000_000_000, 2_000_000_000, 0, 2_000_000_000,
                0, 0, -1_000_000_000)
            val invalid = before.copy(before = 3_000_000_000, after = 3_002_000_001, mono = 3_000_000_000)
            val recovery = before.copy(before = 4_000_000_000, after = 4_000_000_000, mono = 4_000_000_000)
            assertFalse(invalid.valid)
            val gap = CaptureGap("clock", 1_000_000_000, 3_000_000_000, "invalid read width", Long.MAX_VALUE, 1, 0)
            val logSource = DeviceLog(1_700_000_002_500_000_000, 12, 12, 10001, 0, 4, "Fixture", "operation", byteArrayOf(1))
            val knownLog = LogEntry("known", logSource, 1, MappedTime(2_500_000_000, 0, 1_000_000, 1_500_000_000),
                1_500_000_000, true, 0)
            val unknownLog = knownLog.copy(id = "unknown", time = MappedTime(null, 0, Long.MAX_VALUE, null), app = null)
            val capture = FrozenCapture("fixed", "fixture", 1, 0, 4_100_000_000, 180, frames,
                listOf(knownLog, unknownLog), listOf(gap), listOf(before, invalid, recovery), emptyList(), ReplaySettings(),
                mapOf("video" to StreamSnapshot(StreamState.CAPTURING)), windowClockRegion = 0,
                clockRecoveries = mapOf(gap to recovery))
            val output = save(capture, root)
            val manifest = manifest(output)
            val parts = manifest["parts"].asJsonArray
            // The recovery read proves the clock outage continuous: no acquisition/codec/region break and no loss.
            assertEquals(1, parts.size(), "A clock measurement outage never changes the source run")
            val part = parts.single().asJsonObject
            assertTrue(part["clock_alignment_known"].asBoolean)
            assertTrue(part["duration_us"].asLong >= 2_000_001)
            for (key in listOf("t_start_ns", "t_end_ns", "confirmed_t_end_ns")) assertFalse(part[key].isJsonNull)
            assertEquals(emptyList(), output.missingKinds)
            assertTrue(manifest["gaps"].asJsonArray.isEmpty, "A resolved clock outage is not a stream gap")
            assertTrue(manifest["timebase"].asJsonObject["clock_steps"].asJsonArray.isEmpty)
            assertTrue(manifest["video_missing_ranges"].asJsonArray.isEmpty)
            val rows = Files.readAllLines(output.directory.resolve("video-frames.jsonl")).map { JsonParser.parseString(it).asJsonObject }
            assertEquals(frames.map { it.pts }, rows.filter { it["presented"].asBoolean }.map { it["source_pts_us"].asLong })
            assertTrue(rows.all { it["clock_alignment_known"].asBoolean && !it["t_ns"].isJsonNull })
            assertTrue(rows.all { it["t_uncertainty_ns"].asLong == 1_000_000L })
            val deviceLog = Files.readAllLines(output.directory.resolve("logcat-device.jsonl"))
            val appLog = Files.readAllLines(output.directory.resolve("logcat-app.jsonl"))
            assertEquals(2, deviceLog.size)
            assertEquals(listOf(deviceLog.first()), appLog)
            val unknownRow = JsonParser.parseString(deviceLog.last()).asJsonObject
            assertTrue(unknownRow["t_ns"].isJsonNull && unknownRow["app_membership"].isJsonNull)
            assertEquals(Long.MAX_VALUE.toString(), unknownRow["t_uncertainty_ns"].asString)
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
            assertEquals(Files.readAllLines(output.directory.resolve("video-frames.jsonl")), Files.readAllLines(retry.directory.resolve("video-frames.jsonl")))
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

    private fun save(capture: FrozenCapture, root: Path) = SaveWriter().write(capture, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
    private fun manifest(output: SaveOutput) = JsonParser.parseString(Files.readString(output.directory.resolve("replay.json"))).asJsonObject
}
