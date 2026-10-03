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
            assertTrue(output.missingKinds.isEmpty())
            val completed = output.directory
            val manifest = JsonParser.parseString(Files.readString(completed.resolve("session.json"))).asJsonObject
            assertFalse(manifest["files_sha256"].asJsonObject.has("session.json"))
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
