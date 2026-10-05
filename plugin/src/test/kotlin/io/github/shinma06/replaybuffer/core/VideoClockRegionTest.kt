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
    fun sourceOnlyProofRequiresClosedCutsOutsideTheEntireDecoderGop() {
        val root = Files.createTempDirectory("replay-source-proof-")
        try {
            val file = root.resolve("gop.h264")
            Files.write(file, ByteArray(15))
            val frames = (0..4).map { n -> VideoEntry(file, n * 3L, 3, 1_000_000 + n * 100_000L,
                n == 0, 32, 32, 1, if (n < 2) MappedTime(1_000_000_000 + n * 100_000_000L, 0, 1_000_000)
                else MappedTime(null, 0, Long.MAX_VALUE, null), 0, byteArrayOf(1), n.toLong(), 1) }
            val gap = CaptureGap("video", 1L, 2L, "cut", Long.MAX_VALUE, 1, 1)
            val before = VideoSourcePosition(file, 15, 1, 1, 4)
            val after = VideoSourcePosition(root.resolve("new.h264"), 0, 2, 2, 5)
            val cut = VideoCut(before, after)
            val capture = FrozenCapture("id", "sequence", 2, 0, 2_000_000_000, 10, frames, emptyList(), listOf(gap),
                emptyList(), emptyList(), ReplaySettings(), emptyMap(), videoCuts = mapOf(gap to cut))
            assertEquals(setOf(file), capture.sourceOnlyGops())
            assertTrue(capture.copy(videoCuts = emptyMap()).sourceOnlyGops().isEmpty())
            assertTrue(capture.copy(gaps = emptyList()).sourceOnlyGops().isEmpty())
            for (unproven in listOf(gap.copy(toNs = null), gap.copy(fromNs = null), gap.copy(generation = 0),
                    gap.copy(fromNs = 3), gap.copy(boundaryUncertaintyNs = 0), gap.copy(reason = "different key"))) {
                assertTrue(capture.copy(gaps = listOf(unproven)).sourceOnlyGops().isEmpty())
            }
            for (unproven in listOf(VideoCut(null, after), VideoCut(before),
                    cut.copy(before = before.copy(generation = 0)), cut.copy(after = after.copy(session = 0)),
                    cut.copy(before = before.copy(offset = -1)), cut.copy(after = after.copy(host = 3)),
                    cut.copy(before = before.copy(offset = Long.MAX_VALUE)), cut.copy(before = before.copy(host = 3)),
                    VideoCut(before.copy(offset = 9), before.copy(offset = 9, host = 5)), // Inside visible run.
                    VideoCut(before.copy(offset = 3), before.copy(offset = 3, host = 5)), // Inside decoder prefix.
                    VideoCut(before.copy(offset = 0), before.copy(offset = 3, host = 5)), // Deleted IDR bytes.
                    VideoCut(before.copy(offset = 15), before.copy(offset = 14, host = 5)),
                    VideoCut(before.copy(host = Long.MIN_VALUE), after.copy(host = Long.MAX_VALUE)))) {
                assertTrue(capture.copy(videoCuts = mapOf(gap to unproven)).sourceOnlyGops().isEmpty())
            }
            val target = frames[3]
            for (broken in listOf(target.copy(offset = target.offset + 1), target.copy(size = 0),
                    target.copy(generation = 2), target.copy(session = 2), target.copy(width = 64), target.copy(height = 64),
                    target.copy(config = byteArrayOf(2)), target.copy(time = target.time.copy(epoch = 2)),
                    target.copy(pts = frames[2].pts), target.copy(pts = Long.MAX_VALUE), target.copy(host = 1),
                    target.copy(offset = Long.MAX_VALUE), target.copy(file = root.resolve("different-gop.h264")))) {
                assertTrue(capture.copy(video = frames.map { if (it === target) broken else it }).sourceOnlyGops().isEmpty())
            }
            assertTrue(capture.copy(video = frames.drop(1)).sourceOnlyGops().isEmpty())
            assertTrue(capture.copy(video = frames.take(3)).sourceOnlyGops().isEmpty()) // Only one unknown sample.
            Files.write(file, ByteArray(14))
            assertTrue(capture.sourceOnlyGops().isEmpty())
        } finally {
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }

    @Test
    fun actualVideoInterruptionInsideTheGopDoesNotAuthorizeUnknownPresentation() {
        val root = Files.createTempDirectory("replay-internal-source-cut-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime() - 1_000_000_000
        val wall = 1_700_000_000_000_000_000L
        fun anchor(boot: String, elapsed: Long, at: Long) {
            assertTrue(store.clock.add(boot, listOf(elapsed, elapsed, wall + elapsed, elapsed), at, at, wall + at - host))
        }
        try {
            store.generation(1); store.session(VideoPacket.Session(32, 32), 1)
            anchor("old", 1_000_000_000, host)
            store.frame(VideoPacket.Frame(0, true, false, byteArrayOf(1)), 1)
            store.frame(VideoPacket.Frame(1_000_000, false, true, byteArrayOf(1)), 1, host)
            store.frame(VideoPacket.Frame(1_300_000, false, false, byteArrayOf(2)), 1, host + 300_000_000)
            assertFailsWith<IllegalArgumentException> {
                store.frame(VideoPacket.Frame(1_350_000, false, false, ByteArray(32 * 1024 * 1024)), 1, host + 350_000_000)
            } // DeviceCapture's existing failure path reports RECOVERING after the storage guard rejects this packet.
            store.status("video", StreamState.RECOVERING, "actual interruption", 1)
            store.frame(VideoPacket.Frame(1_400_000, false, false, byteArrayOf(3)), 1, host + 400_000_000)
            store.clock.boundary()
            store.status("video", StreamState.RECOVERING, "connection ended", 1)
            store.generation(2); store.session(VideoPacket.Session(32, 32), 2)
            val resumed = System.nanoTime()
            anchor("new", 1_000_000_000, resumed)
            store.frame(VideoPacket.Frame(0, true, false, byteArrayOf(1)), 2)
            store.frame(VideoPacket.Frame(1_000_000, false, true, byteArrayOf(4)), 2, resumed)
            val capture = store.capture(ReplaySettings(replaySeconds = 10))!!
            val old = capture.video.filter { it.generation == 1L }
            assertTrue(old.drop(1).all { it.sourceTimeUnknown() })
            val internal = capture.gaps.first { it.stream == "video" }
            val cut = capture.videoCuts.getValue(internal)
            assertEquals(old[1].file, cut.before!!.file)
            assertEquals(old[1].offset + old[1].size, cut.before.offset)
            assertEquals(old[2].offset, cut.after!!.offset)
            assertTrue(capture.sourceOnlyGops().isEmpty()) // Full prefix/run crosses the actual byte cut.
            assertTrue(capture.copy(videoCuts = capture.videoCuts - internal).sourceOnlyGops().isEmpty())
        } finally {
            store.close()
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }

    @Test
    fun retainedLossAndGapReplacementCannotInventSourceCutProof() {
        val root = Files.createTempDirectory("replay-source-cut-lifetime-")
        val store = CaptureStore(root.resolve("ring"), videoLimit = 2, minFree = 0)
        try {
            store.generation(1); store.session(VideoPacket.Session(32, 32), 1)
            val host = System.nanoTime() - 2_000_000_000
            store.clock.add("old", listOf(1_000_000_000L, 1_000_000_000L, 1_700_000_001_000_000_000L, 1_000_000_000L), host, host)
            store.frame(VideoPacket.Frame(0, true, false, byteArrayOf(1)), 1)
            store.frame(VideoPacket.Frame(1_000_000, false, true, byteArrayOf(1)), 1, host)
            store.frame(VideoPacket.Frame(1_100_000, false, false, byteArrayOf(2)), 1, host + 100_000_000)
            val pinned = store.capture(ReplaySettings())!!
            store.frame(VideoPacket.Frame(1_200_000, false, false, byteArrayOf(3)), 1, host + 200_000_000)
            assertTrue(pinned.videoCuts.isEmpty()) // Live eviction cannot change the pinned proof.
            assertTrue(Files.exists(pinned.video.first().file))
            store.release(pinned.id)
            val lossCapture = store.capture(ReplaySettings())!!
            val loss = lossCapture.gaps.single { it.stream == "video" }
            val cut = lossCapture.videoCuts.getValue(loss)
            assertEquals(0L, cut.before!!.offset) // Removed IDR, not the current source end at byte 3.
            assertEquals(1L, cut.after!!.offset)
            assertTrue(lossCapture.sourceOnlyGops().isEmpty())
            store.release(lossCapture.id)
            store.status("video", StreamState.RECOVERING, "ended", 1)
            store.status("video", StreamState.CAPTURING, null, 1) // No fully indexed recovery packet.
            val replaced = store.capture(ReplaySettings())!!
            assertTrue(replaced.videoCuts.keys.none { it.reason == "ended" })
            store.release(replaced.id)
            store.clock.boundary()
            store.frame(VideoPacket.Frame(1_300_000, false, false, byteArrayOf(4)), 1, host + 300_000_000)
            store.frame(VideoPacket.Frame(1_400_000, false, false, byteArrayOf(5)), 1, host + 400_000_000)
            store.frame(VideoPacket.Frame(1_500_000, false, false, byteArrayOf(6)), 1, host + 500_000_000)
            store.frame(VideoPacket.Frame(1_600_000, false, false, byteArrayOf(7)), 1, host + 600_000_000)
            val coalesced = store.capture(ReplaySettings())!!
            val unknownLoss = coalesced.gaps.last { it.stream == "video" && it.reason.contains("上限") }
            assertEquals(null, unknownLoss.toNs)
            assertTrue(unknownLoss !in coalesced.videoCuts) // Duplicate null-time loss invalidated the association.
            val oldFile = coalesced.video.first().file
            store.release(coalesced.id)
            store.generation(2); store.session(VideoPacket.Session(32, 32), 2)
            store.frame(VideoPacket.Frame(0, true, false, byteArrayOf(1)), 2)
            store.frame(VideoPacket.Frame(1_000_000, false, true, byteArrayOf(1)), 2, host + 700_000_000)
            store.frame(VideoPacket.Frame(1_100_000, false, false, byteArrayOf(2)), 2, host + 800_000_000)
            store.prune(10)
            assertFalse(Files.exists(oldFile))
            val pruned = store.capture(ReplaySettings())!!
            assertTrue(pruned.videoCuts.values.none { it.before?.file == oldFile || it.after?.file == oldFile })
        } finally {
            store.close()
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }

    @Test
    fun trailingFramesAfterTheLastOldAnchorKeepTheirObservedSourceIntervals() {
        val root = Files.createTempDirectory("replay-trailing-unknown-source-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime() - 3_000_000_000
        val wall = 1_700_000_000_000_000_000L
        val bytes = mutableMapOf<Pair<Long, Long>, ByteArray>()
        val pixels = mutableMapOf<Pair<Long, Long>, List<ByteArray>>()
        fun anchor(boot: String, elapsed: Long, at: Long, wallOffset: Long = 0) {
            assertTrue(store.clock.add(boot, listOf(elapsed, elapsed, wall + wallOffset + elapsed, elapsed),
                at, at, wall + at - host))
        }
        fun acquire(owner: Long, pts: List<Long>, at: Long) {
            val encoder = H264Encoder.createH264Encoder().apply { setKeyInterval(100) }
            val decoder = H264Decoder()
            pts.forEachIndexed { n, point ->
                val data = encoder.encodeFrame(Picture.create(32, 32, ColorSpace.YUV420J).apply { fill(20 + n) },
                    ByteBuffer.allocate(65536)).data
                val encoded = ByteArray(data.remaining()).also { data.get(it) }
                if (n == 0) {
                    val config = H264Utils.splitFrame(ByteBuffer.wrap(encoded))
                        .filter { it.get(0).toInt() and 31 in setOf(7, 8) }
                        .fold(byteArrayOf()) { result, nal -> result + byteArrayOf(0, 0, 0, 1) +
                            ByteArray(nal.remaining()).also { nal.get(it) } }
                    store.frame(VideoPacket.Frame(0, true, false, config), owner)
                }
                bytes[owner to point] = encoded
                pixels[owner to point] = decoder.decodeFrame(ByteBuffer.wrap(encoded),
                    Picture.create(32, 32, ColorSpace.YUV420J).data).data.map { it.copyOf() }
                assertTrue(store.frame(VideoPacket.Frame(point, false, n == 0, encoded), owner,
                    at + (point - pts.first()) * 1000))
            }
        }
        try {
            store.generation(1)
            store.session(VideoPacket.Session(32, 32), 1)
            anchor("old", 1_000_000_000, host)
            val unknownPts = listOf(1_300_000L, 1_400_000L, 1_570_000L, 1_880_000L, 1_980_000L, 2_180_000L, 2_430_000L)
            acquire(1, listOf(1_000_000L, 1_100_000L) + unknownPts, host)
            anchor("old", 1_200_000_000, host + 200_000_000) // Last normal anchor is before the trailing P frames.
            store.clock.boundary()
            store.clockStatus(false, 1)
            store.status("video", StreamState.RECOVERING, "connection ended", 1)
            store.generation(2)
            store.session(VideoPacket.Session(32, 32), 2)
            val resumed = System.nanoTime()
            anchor("new", 1_000_000_000, resumed, 20_000_000_000)
            store.clockStatus(true, 2)
            acquire(2, listOf(1_000_000L, 1_200_000L), resumed)
            store.app("com.example.target", 10001, setOf(12), 2, uidExclusive = true)
            store.log(DeviceLog(wall + 20_000_000_000 + 1_250_000_000, 12, 12, 10001, 0, 4,
                "Fixture", "new event", byteArrayOf(1)), 2, resumed + 250_000_000)
            store.log(DeviceLog(wall + 999_000_000_000, 12, 12, 10001, 0, 4,
                "Fixture", "unsupported", byteArrayOf(2)), 2, resumed + 250_000_000)
            anchor("new", 1_300_000_000, resumed + 300_000_000, 20_000_000_000)
            val capture = store.capture(ReplaySettings(replaySeconds = 10))!!
            val unknown = capture.video.filter { it.generation == 1L && it.pts in unknownPts }
            assertEquals(7, unknown.size)
            assertTrue(unknown.all { it.time.elapsed == null && it.time.sequence == null && it.time.uncertainty == Long.MAX_VALUE })
            val frozenCuts = capture.videoCuts.toMap()
            assertFalse(unknown[0].muxContinuousTo(unknown[1], capture.gaps)) // The logical guard still rejects null/MAX.
            val output = save(capture, root)
            val record = manifest(output)
            val parts = record["parts"].asJsonArray.map { it.asJsonObject }
            val sourceParts = parts.filter { it["generation"].asLong == 1L && !it["clock_alignment_known"].asBoolean }
            assertEquals(1, sourceParts.size, "A proved trailing same-GOP source run must not become seven one-tick files")
            val part = sourceParts.single()
            val withoutProof = save(capture.copy(videoCuts = emptyMap()), root)
            val withoutRecord = manifest(withoutProof)
            assertEquals(7, withoutRecord["parts"].asJsonArray.count {
                it.asJsonObject["generation"].asLong == 1L && !it.asJsonObject["clock_alignment_known"].asBoolean
            })
            assertEquals(parts.filter { it["clock_alignment_known"].asBoolean },
                withoutRecord["parts"].asJsonArray.map { it.asJsonObject }.filter { it["clock_alignment_known"].asBoolean }
                    .mapIndexed { i, known -> known.deepCopy().apply { add("file", parts.filter { it["clock_alignment_known"].asBoolean }[i]["file"]) } })
            for (field in listOf("gaps", "video_gap_scopes", "video_clock_gap_scopes", "video_missing_ranges"))
                assertEquals(withoutRecord[field], record[field])
            val single = save(capture.copy(video = capture.video.filter { !it.sourceTimeUnknown() || it === unknown.first() }), root)
            val singlePart = manifest(single)["parts"].asJsonArray.single {
                it.asJsonObject["generation"].asLong == 1L && !it.asJsonObject["clock_alignment_known"].asBoolean
            }.asJsonObject
            assertEquals("1", singlePart["duration_us"].asString) // No cadence/hold is invented for one sample.
            assertEquals("1130001", part["duration_us"].asString)
            for (field in listOf("window_start_ns", "window_end_ns", "confirmed_window_end_ns")) assertTrue(part[field].isJsonNull)
            val rows = Files.readAllLines(output.directory.resolve("frames.jsonl")).map { JsonParser.parseString(it).asJsonObject }
            val visible = rows.filter { it["part"] == part["file"] && it["presented"].asBoolean }
            assertEquals(unknownPts, visible.map { it["source_pts_us"].asLong })
            assertEquals(listOf(100_000L, 170_000L, 310_000L, 100_000L, 200_000L, 250_000L),
                visible.dropLast(1).map { it["source_duration_us"].asLong })
            assertTrue(visible.last()["source_duration_us"].isJsonNull)
            assertEquals(1L, visible.last()["display_duration_us"].asLong)
            visible.forEach { row ->
                for (field in listOf("elapsed_ns", "window_ns", "mapped_window_ns")) assertTrue(row[field].isJsonNull)
                assertEquals(Long.MAX_VALUE.toString(), row["uncertainty_ns"].asString)
                assertFalse(row["clock_alignment_known"].asBoolean)
            }
            for (frame in capture.video) assertContentEquals(bytes.getValue(frame.generation to frame.pts),
                Files.readAllBytes(frame.file).copyOfRange(frame.offset.toInt(), frame.offset.toInt() + frame.size))
            for (media in parts) {
                NIOUtils.readableChannel(output.directory.resolve(media["file"].asString).toFile()).use { channel ->
                    val track = MP4Demuxer.createMP4Demuxer(channel).videoTrack
                    val decoder = H264Decoder()
                    rows.filter { it["part"] == media["file"] }.forEach { row ->
                        val packet = track.nextFrame() ?: error("Missing source/preroll sample")
                        fun vcl(data: ByteBuffer) = H264Utils.splitFrame(data.duplicate())
                            .filter { it.get(0).toInt() and 31 in setOf(1, 5) }
                            .map { nal -> ByteArray(nal.remaining()).also { nal.get(it) } }
                        val originalVcl = vcl(ByteBuffer.wrap(bytes.getValue(media["generation"].asLong to row["source_pts_us"].asLong)))
                        val exportedVcl = vcl(packet.data)
                        assertEquals(originalVcl.size, exportedVcl.size)
                        originalVcl.zip(exportedVcl).forEach { (a, b) -> assertContentEquals(a, b) }
                        val picture = decoder.decodeFrame(packet.data, Picture.create(32, 32, ColorSpace.YUV420J).data)
                        pixels.getValue(media["generation"].asLong to row["source_pts_us"].asLong).zip(picture.data)
                            .forEach { (a, b) -> assertContentEquals(a, b) }
                        assertEquals(row["media_pts_us"].asLong, (packet as MP4Packet).mediaPts)
                        assertEquals(row["display_duration_us"].asLong, packet.duration)
                    }
                    assertEquals(null, track.nextFrame())
                }
            }
            assertTrue(output.missingKinds.contains("video"))
            assertEquals(Long.MAX_VALUE.toString(), record["gaps"].asJsonArray.single { it.asJsonObject["kind"].asString == "video" }
                .asJsonObject["boundary_uncertainty_ns"].asString)
            val device = Files.readAllLines(output.directory.resolve("logcat-device.jsonl"))
            val app = Files.readAllLines(output.directory.resolve("logcat-app.jsonl"))
            assertEquals(2, device.size)
            assertEquals(1, app.size)
            assertTrue(app.single() in device)
            assertTrue(JsonParser.parseString(device.last()).asJsonObject["window_ns"].isJsonNull)
            assertFailsWith<CancellationException> { SaveWriter().write(capture, root, { true }) { a, b -> Files.move(a, b) } }
            anchor("new", 2_000_000_000, resumed + 1_000_000_000, 20_000_000_000)
            store.status("video", StreamState.RECOVERING, "later live interruption", 2)
            acquire(2, listOf(2_000_000L, 2_100_000L), resumed + 1_000_000_000)
            assertEquals(frozenCuts, capture.videoCuts)
            val retry = save(capture, root)
            for (field in listOf("parts", "gaps", "video_gap_scopes", "video_clock_gap_scopes", "video_missing_ranges"))
                assertEquals(record[field], manifest(retry)[field])
            assertEquals(Files.readAllLines(output.directory.resolve("frames.jsonl")), Files.readAllLines(retry.directory.resolve("frames.jsonl")))
            for (name in listOf("logcat-device.jsonl", "logcat-app.jsonl"))
                assertEquals(Files.readAllLines(output.directory.resolve(name)), Files.readAllLines(retry.directory.resolve(name)))
        } finally {
            store.close()
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }

    @Test
    fun reconnectRetainsPlayableOldFramesBeforeTheSameGenerationUncertainVideoLoss() {
        val root = Files.createTempDirectory("replay-old-stream-video-gap-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime() - 4_000_000_000
        val wall = 1_700_000_000_000_000_000L
        fun anchor(boot: String, elapsed: Long, at: Long, wallOffset: Long = 0) {
            assertTrue(store.clock.add(boot, listOf(elapsed, elapsed, wall + wallOffset + elapsed, elapsed),
                at, at, wall + at - host))
        }
        val expected = mutableMapOf<Pair<Long, Long>, List<ByteArray>>()
        fun acquire(owner: Long, at: Long, shade: Int) {
            val encoder = H264Encoder.createH264Encoder().apply { setKeyInterval(10) }
            val decoder = H264Decoder()
            for (n in 0..20) {
                val data = encoder.encodeFrame(Picture.create(32, 32, ColorSpace.YUV420J).apply { fill(shade + n) },
                    ByteBuffer.allocate(65536)).data
                val bytes = ByteArray(data.remaining()).also { data.get(it) }
                if (n == 0) {
                    val config = H264Utils.splitFrame(ByteBuffer.wrap(bytes))
                        .filter { it.get(0).toInt() and 31 in setOf(7, 8) }
                        .fold(byteArrayOf()) { result, nal -> result + byteArrayOf(0, 0, 0, 1) +
                            ByteArray(nal.remaining()).also { nal.get(it) } }
                    store.frame(VideoPacket.Frame(0, true, false, config), owner)
                } else if (n % 10 != 0) {
                    assertTrue(H264Utils.splitFrame(ByteBuffer.wrap(bytes)).any { it.get(0).toInt() and 31 == 1 })
                }
                val pts = 1_000_000 + n * 100_000L
                expected[owner to pts] = decoder.decodeFrame(ByteBuffer.wrap(bytes),
                    Picture.create(32, 32, ColorSpace.YUV420J).data).data.map { it.copyOf() }
                assertTrue(store.frame(VideoPacket.Frame(pts, false, n % 10 == 0, bytes), owner, at + n * 100_000_000L))
            }
        }
        try {
            store.generation(1)
            store.session(VideoPacket.Session(32, 32), 1)
            anchor("old", 1_000_000_000, host)
            acquire(1, host, 15)
            anchor("old", 4_000_000_000, host + 3_000_000_000) // Normal evidence after all old frames.
            store.clock.boundary()
            store.clockStatus(false, 1)
            store.status("video", StreamState.RECOVERING, "connection ended", 1)
            store.generation(2)
            store.session(VideoPacket.Session(32, 32), 2)
            val resumedHost = System.nanoTime()
            anchor("new", 1_000_000_000, resumedHost, 20_000_000_000)
            store.clockStatus(true, 2)
            store.app("com.example.target", 10001, setOf(12), 2, uidExclusive = true)
            acquire(2, resumedHost, 45)
            for (event in 1..3) for (phase in listOf("REQUEST", "DRAW", "FRAME_COMMIT")) {
                val elapsed = 1_000_000_000 + event * 200_000_000L
                store.log(DeviceLog(wall + 20_000_000_000 + elapsed, 12, 12, 10001, 0, 4,
                    "Fixture", "$event $phase", byteArrayOf(event.toByte())), 2, resumedHost + elapsed - 1_000_000_000)
            }
            store.log(DeviceLog(wall + 999_000_000_000, 12, 12, 10001, 0, 4, "Fixture", "unsupported", byteArrayOf(0)),
                2, resumedHost + 2_000_000_000)
            anchor("new", 5_000_000_000, resumedHost + 4_000_000_000, 20_000_000_000)
            val capture = store.capture(ReplaySettings(replaySeconds = 10))!!
            val videoGap = capture.gaps.single { it.stream == "video" }
            assertEquals(1L, videoGap.generation)
            assertTrue(videoGap.fromNs != null && videoGap.toNs != null)
            assertEquals(Long.MAX_VALUE, videoGap.boundaryUncertaintyNs)
            val oldFrames = capture.video.filter { it.generation == 1L }
            assertEquals(21, oldFrames.size)
            assertTrue(oldFrames.all { it.time.sequence != null && it.time.uncertainty != Long.MAX_VALUE && it.time.epoch == 0 })
            assertTrue(oldFrames.last().time.sequence!! < capture.clocks.last { it.epoch == 0 }.let { it.elapsed + it.sequenceOffset!! })
            assertFailsWith<CancellationException> { SaveWriter().write(capture, root, { true }) { a, b -> Files.move(a, b) } }
            val output = save(capture, root)
            val record = manifest(output)
            val parts = record["parts"].asJsonArray.map { it.asJsonObject }
            val oldParts = parts.filter { it["generation"].asLong == 1L }
            assertEquals(1, oldParts.size, "The same-owner loss after old frames must not split them into micro-parts")
            assertEquals("2000001", oldParts.single()["duration_us"].asString)
            assertTrue(oldParts.single()["clock_alignment_known"].asBoolean)
            assertEquals(listOf("video"), output.missingKinds)
            assertEquals(Long.MAX_VALUE.toString(), record["gaps"].asJsonArray.single { it.asJsonObject["kind"].asString == "video" }
                .asJsonObject["boundary_uncertainty_ns"].asString)
            val videoScope = record["video_gap_scopes"].asJsonArray.single().asJsonObject
            assertTrue(videoScope["derived_from_valid_samples"].asBoolean)
            assertEquals(1L, videoScope["generation"].asLong)
            assertTrue(videoScope["from_ns"].asLong > oldFrames.last().time.sequence!!)
            assertTrue(videoScope["to_ns"].asLong >= videoGap.toNs)
            for (field in listOf("before_sample", "after_sample"))
                assertTrue(record["clock_samples"].asJsonArray.any { it == videoScope[field] })
            val rows = Files.readAllLines(output.directory.resolve("frames.jsonl")).map { JsonParser.parseString(it).asJsonObject }
            for (part in parts) {
                val owner = part["generation"].asLong
                NIOUtils.readableChannel(output.directory.resolve(part["file"].asString).toFile()).use { channel ->
                    val track = MP4Demuxer.createMP4Demuxer(channel).videoTrack
                    val decoder = H264Decoder()
                    rows.filter { it["part"].asString == part["file"].asString }.forEach { row ->
                        val packet = track.nextFrame() ?: error("Missing retained old/new MP4 sample")
                        val picture = decoder.decodeFrame(packet.data, Picture.create(32, 32, ColorSpace.YUV420J).data)
                        expected.getValue(owner to row["source_pts_us"].asLong).zip(picture.data).forEach { (a, b) -> assertContentEquals(a, b) }
                        assertEquals(row["media_pts_us"].asLong, (packet as MP4Packet).mediaPts)
                        assertEquals(row["display_duration_us"].asLong, packet.duration)
                    }
                    assertEquals(null, track.nextFrame())
                }
                val sourcePts = capture.video.filter { it.generation == owner }.map { it.pts }
                val presented = rows.filter { it["presented"].asBoolean && parts.single { part -> part["file"] == it["part"] }["generation"].asLong == owner }
                    .map { it["source_pts_us"].asLong }
                assertEquals(sourcePts, presented) // Presented source frames exactly once, excluding decoder preroll.
            }
            val cut = save(capture.copy(start = 1_150_000_000), root)
            val cutOld = manifest(cut)["parts"].asJsonArray.single { it.asJsonObject["generation"].asLong == 1L }.asJsonObject
            assertEquals("150000", cutOld["edit_start_us"].asString)
            assertEquals("850001", cutOld["duration_us"].asString)
            assertEquals("0", cutOld["window_start_ns"].asString)
            assertEquals(1, cutOld["preroll_samples"].asInt)
            val cutRows = Files.readAllLines(cut.directory.resolve("frames.jsonl")).map { JsonParser.parseString(it).asJsonObject }
                .filter { it["part"].asString == cutOld["file"].asString }
            assertEquals(oldFrames.filter { it.pts >= 2_100_000 }.map { it.pts },
                cutRows.filter { it["presented"].asBoolean }.map { it["source_pts_us"].asLong })
            assertTrue(cutRows.first()["preroll"].asBoolean)
            assertEquals("-50000000", cutRows.first { it["presented"].asBoolean }["window_ns"].asString)
            NIOUtils.readableChannel(cut.directory.resolve(cutOld["file"].asString).toFile()).use { channel ->
                val track = MP4Demuxer.createMP4Demuxer(channel).videoTrack
                val decoder = H264Decoder()
                cutRows.forEach { row ->
                    val packet = track.nextFrame() ?: error("Missing old cut sample or decoder preroll")
                    val picture = decoder.decodeFrame(packet.data, Picture.create(32, 32, ColorSpace.YUV420J).data)
                    expected.getValue(1L to row["source_pts_us"].asLong).zip(picture.data).forEach { (a, b) -> assertContentEquals(a, b) }
                }
                assertEquals(null, track.nextFrame())
            }
            val device = Files.readAllLines(output.directory.resolve("logcat-device.jsonl"))
            val app = Files.readAllLines(output.directory.resolve("logcat-app.jsonl"))
            assertEquals(10, device.size)
            assertEquals(9, app.size)
            assertTrue(app.all { it in device && JsonParser.parseString(it).asJsonObject["app_membership"].asBoolean })
            assertTrue(JsonParser.parseString(device.last()).asJsonObject["window_ns"].isJsonNull)
            anchor("new", 6_000_000_000, resumedHost + 5_000_000_000, 20_000_000_000)
            val retry = save(capture, root)
            for (field in listOf("parts", "gaps", "video_clock_gap_scopes", "video_gap_scopes", "video_missing_ranges"))
                assertEquals(record[field], manifest(retry)[field])
            for (file in listOf("frames.jsonl", "logcat-device.jsonl", "logcat-app.jsonl"))
                assertEquals(Files.readAllLines(output.directory.resolve(file)), Files.readAllLines(retry.directory.resolve(file)))
            assertEquals(videoGap, capture.gaps.single { it.stream == "video" })
            store.release(capture.id)
        } finally {
            store.close()
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }

    @Test
    fun videoLossEnvelopeRequiresBothFiniteNormalAnchorsAndStillBlocksRealLoss() {
        val before = ClockSample(0, "old", 4_000_000_000, 4_000_000_000, 0, 4_000_000_000,
            0, 0, 0, -1_000_000_000, 0)
        val after = before.copy(epoch = 2, boot = "new", before = 7_000_000_000, after = 7_000_000_000, mono = 7_000_000_000)
        val gap = CaptureGap("video", 3_500_000_000, 5_500_000_000, "connection ended", Long.MAX_VALUE, 1, 1)
        val scope = listOf(gap).videoScopes(listOf(before, after)).single()
        assertEquals(3_000_000_000L, scope.fromNs)
        assertEquals(6_000_000_000L, scope.toNs)
        assertEquals(1_000_000L, scope.boundaryUncertaintyNs)
        assertEquals(Long.MAX_VALUE, gap.boundaryUncertaintyNs) // Original retention/UI/coverage boundary is unchanged.
        val first = VideoEntry(Path.of("unused"), 0, 1, 1_000_000, true, 32, 32, 1,
            MappedTime(1_000_000_000, 0, 1_000_000, 1_000_000_000), 1_000_000_000, byteArrayOf(1), 0, 1)
        fun frame(at: Long) = first.copy(pts = at / 1000, time = first.time.copy(elapsed = at, sequence = at))
        assertFalse(first.continuousTo(frame(1_100_000_000), listOf(gap)))
        assertTrue(first.muxContinuousTo(frame(1_100_000_000), listOf(scope)))
        assertTrue(frame(7_000_000_000).muxContinuousTo(frame(7_100_000_000), listOf(scope)))
        assertFalse(frame(4_000_000_000).muxContinuousTo(frame(4_100_000_000), listOf(scope)))
        assertFalse(frame(2_000_000_000).muxContinuousTo(frame(7_000_000_000), listOf(scope)))
        assertFalse(frame(2_998_500_000).muxContinuousTo(frame(2_999_000_000), listOf(scope))) // Both error margins count.
        for (unproven in listOf(gap.copy(fromNs = null), gap.copy(toNs = null), gap.copy(generation = 0),
                gap.copy(generation = -1), gap.copy(fromNs = 6_000_000_000))) {
            assertEquals(unproven, listOf(unproven).videoScopes(listOf(before, after)).single())
        }
        for (unproven in listOf(after.copy(sequenceOffset = null), after.copy(bridgeError = Long.MAX_VALUE),
                after.copy(bridgeError = 20_000_001), after.copy(sequenceOffset = Long.MAX_VALUE),
                after.copy(received = 100_000_000), after.copy(sent = 1),
                after.copy(after = after.before - 1), after.copy(before = -1, after = -1),
                after.copy(after = after.before + 2_000_001))) {
            assertEquals(gap, listOf(gap).videoScopes(listOf(before, unproven)).single())
        }
        assertEquals(gap, listOf(gap).videoScopes(listOf(before)).single())
        assertEquals(gap, listOf(gap).videoScopes(listOf(after)).single())
        val known = gap.copy(boundaryUncertaintyNs = 1_000_000)
        assertEquals(known, listOf(known).videoScopes(listOf(before, after)).single())
    }

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
