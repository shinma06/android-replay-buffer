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
import java.util.concurrent.CancellationException
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class CaptureStoreSaveTest {
    @Test
    fun longVfrIntervalsKeepOnePartAndWindowCutRetainsItsDisplayedFrame() {
        val root = Files.createTempDirectory("replay-long-vfr-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime()
        val bytes = sample()
        try {
            store.generation(1); store.session(VideoPacket.Session(32, 32), 1)
            anchor(store.clock, 1_000_000_000, host)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            store.frame(VideoPacket.Frame(1_000_000, false, true, bytes), 1, host)
            // No new image for five seconds; clocks remain valid without claiming video arrivals.
            for (second in 2L..6L) anchor(store.clock, second * 1_000_000_000, host + (second - 1) * 1_000_000_000)
            store.frame(VideoPacket.Frame(6_000_000, false, true, bytes), 1, host + 5_000_000_000)
            store.prune(2)
            val capture = store.capture(ReplaySettings(replaySeconds = 2))!!
            assertEquals(3_000_000_000, capture.start)
            assertEquals(listOf(1_000_000L, 6_000_000L), capture.video.map { it.pts })
            val output = SaveWriter().write(capture, root, { false }) { a, b -> Files.move(a, b) }
            val manifest = JsonParser.parseString(Files.readString(output.directory.resolve("session.json"))).asJsonObject
            assertEquals(1, manifest["parts"].asJsonArray.size())
            assertTrue(manifest["video_missing_ranges"].asJsonArray.isEmpty)
            val part = manifest["parts"].asJsonArray.single().asJsonObject
            assertEquals("0", part["window_start_ns"].asString)
            assertEquals("3000000", part["edit_start_us"].asString)
            NIOUtils.readableChannel(output.directory.resolve("video-001.mp4").toFile()).use { channel ->
                val frame = MP4Demuxer.createRawMP4Demuxer(channel).videoTrack.nextFrame()
                assertEquals(5_000_000, frame.duration)
            }
            for (gap in listOf(
                CaptureGap("video", 2_000_000_000, 2_100_000_000, "lost", 0),
                CaptureGap("clock", null, null, "unknown clock"),
            )) {
                val broken = SaveWriter().write(capture.copy(gaps = listOf(gap)), root, { false }) { a, b -> Files.move(a, b) }
                val brokenManifest = JsonParser.parseString(Files.readString(broken.directory.resolve("session.json"))).asJsonObject
                assertFalse(brokenManifest["video_missing_ranges"].asJsonArray.isEmpty)
            }
            store.release(capture.id)
        } finally {
            store.close()
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }

    @Test
    fun noNewFrameWithinTheWindowRetainsTheLastFrameAndItsDecodeGopWithinByteLimits() {
        val root = Files.createTempDirectory("replay-static-cut-")
        val bytes = sample()
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime()
        try {
            store.generation(1); store.session(VideoPacket.Session(32, 32), 1)
            anchor(store.clock, 1_000_000_000, host)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            store.frame(VideoPacket.Frame(1_000_000, false, true, bytes), 1, host)
            store.frame(VideoPacket.Frame(1_100_000, false, false, bytes), 1, host + 100_000_000)
            for (second in 2L..15L) anchor(store.clock, second * 1_000_000_000, host + (second - 1) * 1_000_000_000)
            store.prune(2)
            val capture = store.capture(ReplaySettings(replaySeconds = 2))!!
            assertEquals(12_000_000_000, capture.start)
            assertEquals(listOf(1_000_000L, 1_100_000L), capture.video.map { it.pts })
            assertTrue(capture.video.all { Files.exists(it.file) })
            assertTrue(capture.gaps.isEmpty())
            val fixedTail = capture.videoTail()!!
            assertEquals(12_000_000_000, fixedTail.fromNs)
            assertEquals(14_000_000_000, fixedTail.toNs)
            assertTrue(fixedTail.displayHeld)
            val output = SaveWriter().write(capture, root, { false }) { a, b -> Files.move(a, b) }
            assertTrue(output.missingKinds.isEmpty())
            val manifest = JsonParser.parseString(Files.readString(output.directory.resolve("session.json"))).asJsonObject
            assertTrue(manifest["complete"].asBoolean)
            assertFalse(manifest["video_tail"].asJsonObject["new_frame_confirmed"].asBoolean)
            assertTrue(manifest["video_tail"].asJsonObject["display_held"].asBoolean)
            assertEquals("2000000", manifest["parts"].asJsonArray.single().asJsonObject["duration_us"].asString)
            assertTrue(manifest["parts"].asJsonArray.single().asJsonObject["confirmed_window_end_ns"].isJsonNull)
            assertEquals(2, Files.readAllLines(output.directory.resolve("frames.jsonl")).size)
            val unknown = capture.copy(endUncertainty = Long.MAX_VALUE)
            assertFalse(unknown.videoTail()!!.displayHeld)
            assertEquals(null, unknown.videoTail()!!.fromNs)
            val broken = capture.copy(start = 0, gaps = listOf(CaptureGap("video", 5_000_000_000, 6_000_000_000, "known loss", 0)))
            assertEquals(5_000_000_000, broken.videoTail()!!.toNs)
            val brokenOutput = SaveWriter().write(broken, root, { false }) { a, b -> Files.move(a, b) }
            assertEquals(listOf("video"), brokenOutput.missingKinds)
            val longEnd = Int.MAX_VALUE.toLong() * 1000 + 20_000_000_000
            val longWindow = capture.copy(start = longEnd - 2_000_000_000, end = longEnd)
            val longOutput = SaveWriter().write(longWindow, root, { false }) { a, b -> Files.move(a, b) }
            assertTrue(longOutput.missingKinds.isEmpty())
            val longManifest = JsonParser.parseString(Files.readString(longOutput.directory.resolve("session.json"))).asJsonObject
            val longPart = longManifest["parts"].asJsonArray.single().asJsonObject
            assertTrue(longPart["media_timeline_clipped"].asBoolean)
            assertEquals("2000000", longPart["duration_us"].asString)
            assertEquals(capture.video.map { it.pts.toString() }, Files.readAllLines(longOutput.directory.resolve("frames.jsonl")).map {
                JsonParser.parseString(it).asJsonObject["source_pts_us"].asString
            })
            anchor(store.clock, 16_000_000_000, host + 15_000_000_000)
            store.frame(VideoPacket.Frame(16_000_000, false, true, bytes), 1, host + 15_000_000_000)
            assertEquals(fixedTail, capture.videoTail())
            store.release(capture.id)
            store.session(VideoPacket.Session(32, 32), 1)
            anchor(store.clock, 20_000_000_000, host + 19_000_000_000)
            store.prune(2)
            assertFalse(store.hasData()) // A new session cannot use the old static image as its own.
        } finally {
            store.close()
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }

    @Test
    fun unknownWindowStillReclaimsByteEvictedGopsWhilePreservingPinnedBytes() {
        val root = Files.createTempDirectory("replay-unknown-cap-fixture-")
        val bytes = sample()
        val store = CaptureStore(root.resolve("ring"), videoLimit = bytes.size.toLong(), minFree = 0)
        val host = System.nanoTime()
        try {
            store.generation(1); store.session(VideoPacket.Session(32, 32), 1)
            anchor(store.clock, 1_000_000_000, host)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            store.frame(VideoPacket.Frame(1_000_000, false, true, bytes), 1, host)
            val pin = store.capture(ReplaySettings())!!
            val pinnedFile = pin.video.single().file
            val hash = sha256(pinnedFile)
            store.clock.add("2", listOf(100_000_000, 100_000_000, 1_700_000_020_000_000_000, 100_000_000),
                host + 20_000_000_000, host + 20_000_000_000, 100_000_000_000)
            store.generation(2); store.session(VideoPacket.Session(32, 32), 2)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 2)
            for (n in 1..20) {
                store.frame(VideoPacket.Frame(n * 100_000L, false, true, bytes), 2, host + 20_000_000_000)
                store.prune(1)
                assertEquals(2, Files.list(store.directory).use { it.count() })
                assertEquals(hash, sha256(pinnedFile))
            }
            store.release(pin.id)
            store.prune(1)
            assertFalse(Files.exists(pinnedFile))
            assertEquals(1, Files.list(store.directory).use { it.count() })
            assertEquals(bytes.size.toLong(), Files.list(store.directory).use { paths -> paths.mapToLong { Files.size(it) }.sum() })
        } finally { store.close(); Files.delete(root) }
    }

    @Test
    fun unknownBootBridgeRetainsOldRecordsAndExportsUnknownWindowCoordinates() {
        val root = Files.createTempDirectory("replay-unknown-boot-fixture-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime()
        val bytes = sample()
        try {
            store.generation(1); store.session(VideoPacket.Session(32, 32), 1)
            anchor(store.clock, 1_000_000_000, host)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            store.frame(VideoPacket.Frame(1_000_000, false, true, bytes), 1, host)
            store.log(DeviceLog(1_700_000_001_000_000_000, 12, 12, 10001, 0, 4, "Fixture", "old boot", byteArrayOf(1)), 1, host)
            store.status("video", StreamState.RECOVERING, "prior known gap", 1)
            store.status("video", StreamState.CAPTURING, null, 1)
            store.clock.add("2", listOf(100_000_000, 100_000_000, 1_700_000_020_000_000_000, 100_000_000),
                host + 20_000_000_000, host + 20_000_000_000, 100_000_000_000)
            store.prune(1)
            val capture = store.capture(ReplaySettings(replaySeconds = 1))!!
            assertFalse(capture.windowKnown)
            assertTrue(capture.gaps.any { it.reason == "prior known gap" })
            assertTrue(capture.states.getValue("video").gaps.any { it.reason == "prior known gap" })
            assertTrue(store.streams(1).getValue("video").gaps.any { it.reason == "prior known gap" })
            assertEquals(1, capture.video.size)
            assertEquals(1, capture.logs.size)
            val output = SaveWriter().write(capture, root, { false }) { a, b -> Files.move(a, b) }
            val manifest = JsonParser.parseString(Files.readString(output.directory.resolve("session.json"))).asJsonObject
            assertFalse(manifest["window_clock_known"].asBoolean)
            assertEquals(store.clock.currentEpoch(), manifest["window_clock_epoch"].asInt)
            assertTrue(manifest["parts"].asJsonArray.single().asJsonObject["window_start_ns"].isJsonNull)
            for (name in listOf("frames.jsonl", "logcat-device.jsonl")) {
                assertTrue(JsonParser.parseString(Files.readAllLines(output.directory.resolve(name)).single()).asJsonObject["window_ns"].isJsonNull)
            }
        } finally {
            store.close()
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }

    @Test
    fun nullTargetAndRecoveredClockAndLogGapsRemainImmutableOnlyWhileInWindow() {
        val root = Files.createTempDirectory("replay-gap-fixture-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime()
        try {
            store.generation(1)
            store.app(null, null, emptySet(), 1)
            assertEquals(StreamState.UNAVAILABLE, store.streams()["app_log"]?.state)
            anchor(store.clock, 1_000_000_000, host)
            store.app("com.example.app", 10001, setOf(12), 1, true)
            store.log(DeviceLog(1_700_000_001_000_000_000, 12, 12, 10001, 0, 4, "a", "a", byteArrayOf(1)), 1, host)
            store.status("device_log", StreamState.RECOVERING, "isolated log interruption", 1)
            store.clockStatus(false, 1)
            anchor(store.clock, 1_100_000_000, host + 100_000_000)
            store.status("device_log", StreamState.CAPTURING, null, 1)
            store.clockStatus(true, 1)
            val recovered = store.streams(1, 100_000_000)
            val appGaps = recovered.getValue("app_log").gaps
            assertEquals(setOf("device_log", "clock"), appGaps.map { it.stream }.toSet())
            assertEquals(listOf("clock"), recovered.getValue("video").gaps.map { it.stream })
            assertFailsWith<UnsupportedOperationException> { (appGaps as MutableList).clear() }
            anchor(store.clock, 6_000_000_000, host + 5_000_000_000)
            assertTrue(store.streams(1, 5_000_000_000).values.all { it.gaps.isEmpty() })
            store.freeze()
            val fixedEnd = store.end()
            store.clock.add("2", listOf(100_000_000, 100_000_000, 1_700_000_020_000_000_000, 100_000_000),
                host + 20_000_000_000, host + 20_000_000_000, 100_000_000_000)
            assertEquals(Long.MAX_VALUE, store.clock.endUncertainty())
            assertEquals(fixedEnd, store.end())
            assertTrue(store.streams(1).values.all { it.gaps.isEmpty() }) // Frozen known window is not replaced by the new unknown clock.
            store.resume()
            assertEquals(2, store.streams(1).getValue("app_log").gaps.size)
            assertEquals(2, appGaps.size)
        } finally { store.close(); Files.delete(root) }
    }

    private fun sample(): ByteArray {
        val picture = Picture.create(32, 32, ColorSpace.YUV420J).apply { fill(15) }
        val data = H264Encoder.createH264Encoder().encodeIDRFrame(picture, ByteBuffer.allocate(65536))
        return ByteArray(data.remaining()).also { data.get(it) }
    }

    private fun config(bytes: ByteArray): ByteArray = H264Utils.splitFrame(ByteBuffer.wrap(bytes))
        .filter { it.get(0).toInt() and 31 in setOf(7, 8) }
        .fold(byteArrayOf()) { result, nal -> result + byteArrayOf(0, 0, 0, 1) + ByteArray(nal.remaining()).also { nal.get(it) } }

    private fun anchor(clock: CaptureClock, elapsed: Long, host: Long) {
        clock.add("1", listOf(elapsed, elapsed, 1_700_000_000_000_000_000 + elapsed, elapsed), host, host)
    }

    @Test
    fun pinnedRequestSurvivesPruneAndFailedSaveThenExportsMatchingLogsAndOriginalVfrPts() {
        val root = Files.createTempDirectory("replay-save-fixture-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime()
        val bytes = sample()
        try {
            store.generation(1)
            store.session(VideoPacket.Session(32, 32), 1)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            anchor(store.clock, 1_000_000_000, host)
            store.app("com.example.app", 10001, setOf(12), 1)
            for (pts in listOf(0L, 100_000L, 350_000L)) {
                anchor(store.clock, 1_000_000_000 + pts * 1000, host + pts * 1000)
                store.frame(VideoPacket.Frame(1_000_000 + pts, false, true, bytes), 1, host + pts * 1000)
            }
            store.log(DeviceLog(1_700_000_001_350_000_000, 12, 12, 10001, 0, 4, "Fixture", "hello\nworld", byteArrayOf(1, 2)), 1, host + 350_000_000)
            anchor(store.clock, 1_400_000_000, host + 400_000_000)
            store.status("video", StreamState.RECOVERING, "schema gap fixture", 1)
            store.status("video", StreamState.CAPTURING, null, 1)
            val capture = store.capture(ReplaySettings())!!
            assertEquals(0, capture.start)
            assertEquals(400_000_000, capture.end)
            assertEquals(true, capture.logs.single().app)
            assertFailsWith<IllegalStateException> { store.capture(ReplaySettings()) }
            assertFailsWith<SaveFailure> { SaveWriter().write(capture, root.resolve("missing"), { false }) { a, b -> Files.move(a, b) } }
            anchor(store.clock, 10_000_000_000, host + 9_000_000_000)
            store.prune(1)
            assertTrue(capture.video.all { Files.exists(it.file) })
            val output = SaveWriter().write(capture, root, { false }) { a, b -> Files.move(a, b) }
            assertEquals(listOf("video"), output.missingKinds) // The explicit gap remains a known interruption.
            val completed = output.directory
            val manifest = JsonParser.parseString(Files.readString(completed.resolve("session.json"))).asJsonObject
            assertFalse(manifest["files_sha256"].asJsonObject.has("session.json"))
            assertEquals(setOf("state", "availableSeconds", "reason"), manifest["coverage"].asJsonObject["video"].asJsonObject.keySet())
            val gap = manifest["gaps"].asJsonArray.single().asJsonObject
            assertTrue(gap["from_ns"].asJsonPrimitive.isString && gap["to_ns"].asJsonPrimitive.isString)
            assertTrue(manifest["build"].asJsonObject["source"].asString.matches(Regex("[a-f0-9]{40}")))
            manifest["files_sha256"].asJsonObject.entrySet().forEach { (name, value) -> assertEquals(value.asString, sha256(completed.resolve(name))) }
            val device = Files.readAllLines(completed.resolve("logcat-device.jsonl"))
            assertEquals(device, Files.readAllLines(completed.resolve("logcat-app.jsonl")))
            assertEquals(capture.logs.single().id, JsonParser.parseString(device.single()).asJsonObject["record_id"].asString)
            NIOUtils.readableChannel(completed.resolve("video-001.mp4").toFile()).use { channel ->
                val mux = MP4Demuxer.createRawMP4Demuxer(channel)
                val track = mux.videoTrack
                for ((pts, duration) in listOf(0L to 100_000L, 100_000L to 250_000L, 350_000L to 50_000L)) {
                    val frame = track.nextFrame()
                    assertEquals(pts, frame.pts)
                    assertEquals(duration, frame.duration)
                    assertEquals(1_000_000, frame.timescale)
                }
            }
            val logOnly = SaveWriter().write(capture.copy(video = emptyList()), root, { false }) { a, b -> Files.move(a, b) }
            assertEquals(listOf("video"), logOnly.missingKinds)
            val hash = sha256(completed.resolve("session.json"))
            assertFailsWith<CancellationException> { SaveWriter().write(capture, root, { true }) { a, b -> Files.move(a, b) } }
            assertEquals(hash, sha256(completed.resolve("session.json")))
            store.release(capture.id)
            assertTrue(capture.video.all { !Files.exists(it.file) || it.file == capture.video.last().file })
        } finally {
            store.close()
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }

    @Test
    fun cutPrerollAndDelayedBoundaryPartsUseNonnegativeEditsAndActualWindowOffsets() {
        val root = Files.createTempDirectory("replay-edit-fixture-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime()
        val bytes = sample()
        try {
            store.generation(1); store.session(VideoPacket.Session(32, 32), 1)
            anchor(store.clock, 1_000_000_000, host)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            for (pts in listOf(0L, 100_000L, 350_000L)) {
                anchor(store.clock, 1_000_000_000 + pts * 1000, host + pts * 1000)
                store.frame(VideoPacket.Frame(1_000_000 + pts, false, true, bytes), 1, host + pts * 1000)
            }
            anchor(store.clock, 1_400_000_000, host + 400_000_000)
            val original = store.capture(ReplaySettings())!!
            val cut = SaveWriter().write(original.copy(start = 50_000_000), root, { false }) { a, b -> Files.move(a, b) }
            val cutJson = JsonParser.parseString(Files.readString(cut.directory.resolve("session.json"))).asJsonObject
            val part = cutJson["parts"].asJsonArray.single().asJsonObject
            assertEquals("0", part["window_start_ns"].asString)
            assertEquals("50000", part["edit_start_us"].asString)
            NIOUtils.readableChannel(cut.directory.resolve("video-001.mp4").toFile()).use { channel ->
                val track = MP4Demuxer.createRawMP4Demuxer(channel).videoTrack
                assertEquals(listOf(0L, 50_000L, 300_000L), (1..3).map { track.nextFrame().pts })
            }
            val indices = Files.readAllLines(cut.directory.resolve("frames.jsonl")).map { JsonParser.parseString(it).asJsonObject }
            assertEquals(listOf("0", "50000", "300000"), indices.map { it["presentation_pts_us"].asString })
            val delayed = original.copy(start = 0, video = original.video.drop(1))
            val delayedOutput = SaveWriter().write(delayed, root, { false }) { a, b -> Files.move(a, b) }
            val delayedPart = JsonParser.parseString(Files.readString(delayedOutput.directory.resolve("session.json"))).asJsonObject["parts"].asJsonArray.single().asJsonObject
            assertEquals("0", delayedPart["edit_start_us"].asString)
            assertEquals("100000000", delayedPart["window_start_ns"].asString)
            assertEquals(listOf("video"), delayedOutput.missingKinds)
            val lateFrame = original.video.first().let { it.copy(pts = it.pts + 1_000_000,
                time = it.time.copy(elapsed = it.time.elapsed!! + 1_000_000_000, sequence = 1_000_000_000)) }
            val late = SaveWriter().write(original.copy(start = 0, end = 1_200_000_000, video = listOf(lateFrame)), root, { false }) { a, b -> Files.move(a, b) }
            val latePart = JsonParser.parseString(Files.readString(late.directory.resolve("session.json"))).asJsonObject["parts"].asJsonArray.single().asJsonObject
            assertEquals("0", latePart["edit_start_us"].asString)
            assertEquals("1000000000", latePart["window_start_ns"].asString)
            for (boundary in listOf("session", "clock", "config")) {
                val second = original.video.last().let { frame -> when (boundary) {
                    "session" -> frame.copy(session = frame.session + 1)
                    "clock" -> frame.copy(time = frame.time.copy(epoch = frame.time.epoch + 1))
                    else -> frame.copy(config = byteArrayOf(0) + frame.config)
                } }
                val split = SaveWriter().write(original.copy(video = listOf(original.video.first(), second)), root, { false }) { a, b -> Files.move(a, b) }
                val parts = JsonParser.parseString(Files.readString(split.directory.resolve("session.json"))).asJsonObject["parts"].asJsonArray
                assertEquals(2, parts.size())
                assertEquals("0", parts[1].asJsonObject["edit_start_us"].asString)
                assertEquals("350000000", parts[1].asJsonObject["window_start_ns"].asString)
            }
            store.release(original.id)
            anchor(store.clock, 2_050_000_000, host + 1_050_000_000)
            store.prune(1)
            val realCut = store.capture(ReplaySettings(replaySeconds = 1))!!
            assertEquals(50_000_000, realCut.start)
            assertEquals(1_000_000, realCut.video.first().pts)
            store.release(realCut.id)
        } finally {
            store.close()
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }

    @Test
    fun uniqueUidFollowsNewPidImmediatelyAndSharedUidRemainsUnknown() {
        val root = Files.createTempDirectory("replay-app-fixture-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime()
        try {
            store.generation(1); anchor(store.clock, 1_000_000_000, host)
            store.app("com.example.app", 10001, setOf(12), 1, uidExclusive = true)
            anchor(store.clock, 2_000_000_000, host + 1_000_000_000)
            store.log(DeviceLog(1_700_000_002_000_000_000, 99, 99, 10001, 0, 4, "app", "new PID", byteArrayOf(1)), 1, host + 1_000_000_000)
            val unique = store.capture(ReplaySettings())!!
            assertEquals(true, unique.logs.single().app)
            store.release(unique.id)
            store.app("com.example.app", 10001, setOf(12), 1, uidExclusive = false)
            store.log(DeviceLog(1_700_000_002_000_000_000, 100, 100, 10001, 0, 4, "app", "shared UID", byteArrayOf(2)), 1, host + 1_000_000_000)
            val shared = store.capture(ReplaySettings())!!
            assertEquals(null, shared.logs.last().app)
            store.release(shared.id)
        } finally { store.close(); Files.delete(root) }
    }

    @Test
    fun byteCeilingsRecordSequenceLossAndPreserveThePinnedOriginalBytes() {
        val root = Files.createTempDirectory("replay-cap-fixture-")
        val bytes = sample()
        val store = CaptureStore(root.resolve("ring"), videoLimit = bytes.size.toLong(), logLimit = 258, minFree = 0)
        val host = System.nanoTime()
        try {
            store.generation(1); store.session(VideoPacket.Session(32, 32), 1)
            anchor(store.clock, 1_000_000_000, host)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            store.frame(VideoPacket.Frame(1_000_000, false, true, bytes), 1, host)
            val pin = store.capture(ReplaySettings())!!
            val pinnedFile = pin.video.single().file
            val hash = sha256(pinnedFile)
            store.log(DeviceLog(1_700_000_001_000_000_000, 12, 12, 10001, 0, 4, "a", "a", byteArrayOf(1, 2)), 1, host)
            anchor(store.clock, 2_000_000_000, host + 1_000_000_000)
            store.frame(VideoPacket.Frame(2_000_000, false, true, bytes), 1, host + 1_000_000_000)
            store.log(DeviceLog(1_700_000_002_000_000_000, 12, 12, 10001, 0, 4, "b", "b", byteArrayOf(3, 4)), 1, host + 1_000_000_000)
            store.prune(180)
            assertEquals(hash, sha256(pinnedFile))
            store.release(pin.id)
            val live = store.capture(ReplaySettings())!!
            assertEquals(1, live.video.size)
            assertEquals(1, live.logs.size)
            assertTrue(live.gaps.any { it.stream == "video" && it.fromNs == 0L && it.generation == 1L })
            assertTrue(live.gaps.any { it.stream == "device_log" && it.fromNs == 0L })
            store.release(live.id)
            assertFalse(Files.exists(pinnedFile))
            assertFailsWith<IllegalArgumentException> {
                store.frame(VideoPacket.Frame(0, true, false, ByteArray(ReplaySettings.MAX_CONFIG_PACKET_BYTES + 1)), 1)
            }
        } finally { store.close(); Files.delete(root) }
    }

    @Test
    fun completeDisconnectFreezesPruneAndLowDiskStopsBeforeWritingPacket() {
        val root = Files.createTempDirectory("replay-ring-fixture-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime()
        val bytes = sample()
        try {
            store.generation(1); store.session(VideoPacket.Session(32, 32), 1)
            anchor(store.clock, 1_000_000_000, host)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            store.frame(VideoPacket.Frame(1_000_000, false, true, bytes), 1, host)
            store.freeze()
            val frozen = store.end()
            anchor(store.clock, 11_000_000_000, host + 10_000_000_000)
            store.prune(1)
            assertEquals(frozen, store.end())
            assertTrue(store.hasData())
            val fixed = store.capture(ReplaySettings(replaySeconds = 1))!!
            assertEquals(frozen, fixed.end)
            assertTrue(fixed.windowKnown)
            assertEquals(0, fixed.windowClockEpoch)
            store.release(fixed.id)
            store.resume(); store.prune(1)
            assertFalse(store.hasData())
            assertEquals(10_000_000_000, store.end())
        } finally { store.close(); Files.delete(root) }
        val lowRoot = Files.createTempDirectory("replay-low-disk-")
        CaptureStore(lowRoot.resolve("ring"), minFree = Long.MAX_VALUE).use { low ->
            low.generation(1); low.session(VideoPacket.Session(32, 32), 1)
            low.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            assertFailsWith<IllegalStateException> { low.frame(VideoPacket.Frame(1, false, true, bytes), 1) }
            assertFalse(low.hasData())
        }
        Files.delete(lowRoot)
    }
}
