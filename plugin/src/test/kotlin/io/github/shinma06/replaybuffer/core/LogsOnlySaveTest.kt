package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonParser
import org.jcodec.codecs.h264.H264Encoder
import org.jcodec.codecs.h264.H264Utils
import org.jcodec.common.model.ColorSpace
import org.jcodec.common.model.Picture
import java.nio.ByteBuffer
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.ScheduledExecutorService
import java.util.concurrent.TimeUnit
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

class LogsOnlySaveTest {
    @TempDir lateinit var root: Path

    @Test
    fun logsOnlyRequestDuringVideoCaptureSavesLogsWithoutMovieAndLeavesVideoRunning() {
        CaptureStore(root.resolve("ring"), minFree = 0).use { store ->
            val host = System.nanoTime() + 60_000_000_000
            val bytes = sample()
            store.generation(1)
            store.session(VideoPacket.Session(32, 32), 1)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            store.status("video", StreamState.CAPTURING, null, 1)
            store.status("device_log", StreamState.CAPTURING, null, 1)
            frameAndLog(store, host, 1, bytes)
            store.app("com.fixture.app", 10001, setOf(12), 1, true)
            for (second in 2L..4L) frameAndLog(store, host, second, bytes)
            val request = SaveRequest(SaveContent.LOGS_ONLY, SaveOrigin.LOGCAT_TOOLBAR)
            assertTrue(request.excludesVideo(CaptureMode.VIDEO_AND_LOGS))
            assertFalse(request.excludesVideo(CaptureMode.LOG_ONLY))
            val logs = assertNotNull(store.capture(ReplaySettings(), request = request))
            assertTrue(logs.video.isEmpty())
            assertTrue(logs.touches.isEmpty())
            assertNull(logs.touchState)
            assertTrue(logs.gaps.none { it.stream == "video" })
            assertTrue(logs.logs.isNotEmpty())
            assertEquals(StreamState.CAPTURING, logs.states.getValue("video").state)
            // Video packets keep arriving while the logs-only request is pinned and written.
            frameAndLog(store, host, 5, bytes)
            val output = SaveWriter().write(logs, root, { false }) { Files.move(it.partial, it.complete) }
            assertTrue(output.missingKinds.isEmpty())
            val folder = output.directory
            Files.list(folder).use { files -> assertTrue(files.noneMatch { it.fileName.toString().endsWith(".mp4") }) }
            assertFalse(Files.exists(folder.resolve("touches.jsonl")))
            assertTrue(Files.readAllLines(folder.resolve("logcat-device.jsonl")).isNotEmpty())
            assertTrue(Files.readAllLines(folder.resolve("logcat-app.jsonl")).isNotEmpty())
            val json = JsonParser.parseString(Files.readString(folder.resolve("replay.json"))).asJsonObject
            assertEquals("VIDEO_AND_LOGS", json["capture_mode"].asString)
            val saveRequest = json["save_request"].asJsonObject
            assertEquals("LOGS_ONLY", saveRequest["content"].asString)
            assertEquals("LOGCAT_TOOLBAR", saveRequest["origin"].asString)
            assertTrue(saveRequest["video_excluded"].asBoolean)
            val video = json["coverage"].asJsonObject["video"].asJsonObject
            assertEquals("EXCLUDED", video["state"].asString)
            assertTrue(video["excluded"].asBoolean)
            assertFalse(video["requested"].asBoolean)
            assertEquals("CAPTURING", video["capture_state"].asString)
            assertTrue(json["files"].asJsonObject["video"].isJsonNull)
            assertTrue(Files.readString(folder.resolve("README.md")).contains("動画は意図的に除外"))
            store.release(logs.id)
            frameAndLog(store, host, 6, bytes)
            val full = assertNotNull(store.capture(ReplaySettings()))
            assertTrue(full.video.isNotEmpty())
            assertEquals(SaveRequest(), full.request)
            store.release(full.id)
        }
    }

    @Test
    fun logsOnlyRequestDuringVideoCaptureSavesAQuietLogWindowWhileAllStillNeedsContent() {
        CaptureStore(root.resolve("ring"), minFree = 0).use { store ->
            val host = System.nanoTime() + 60_000_000_000
            val bytes = sample()
            store.generation(1)
            store.session(VideoPacket.Session(32, 32), 1)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            store.status("video", StreamState.CAPTURING, null, 1)
            frameAndLog(store, host, 1, bytes, log = false)
            store.status("device_log", StreamState.CAPTURING, null, 1)
            store.app("com.fixture.app", 10001, setOf(12), 1, true)
            for (second in 2L..4L) frameAndLog(store, host, second, bytes, log = false)
            val request = SaveRequest(SaveContent.LOGS_ONLY, SaveOrigin.LOGCAT_TOOLBAR)
            val quiet = assertNotNull(store.capture(ReplaySettings(), request = request))
            assertTrue(quiet.empty)
            assertTrue(quiet.logs.isEmpty())
            assertTrue(quiet.start < quiet.end)
            val output = SaveWriter().write(quiet, root, { false }) { Files.move(it.partial, it.complete) }
            Files.list(output.directory).use { files -> assertTrue(files.noneMatch { it.fileName.toString().endsWith(".mp4") }) }
            assertEquals(0L, Files.size(output.directory.resolve("logcat-device.jsonl")))
            val json = JsonParser.parseString(Files.readString(output.directory.resolve("replay.json"))).asJsonObject
            assertEquals("EXCLUDED", json["coverage"].asJsonObject["video"].asJsonObject["state"].asString)
            assertEquals("CAPTURING", json["coverage"].asJsonObject["device_log"].asJsonObject["state"].asString)
            store.release(quiet.id)
            // The regular save of the same window keeps needing content: here it carries the video.
            val full = assertNotNull(store.capture(ReplaySettings()))
            assertTrue(full.video.isNotEmpty())
            store.release(full.id)
            // A quiet window proves nothing for the logs-only request once a log reader is unhealthy.
            store.status("device_log", StreamState.RECOVERING, "reader failed", 1)
            assertNull(store.capture(ReplaySettings(), request = request))
        }
    }

    @Test
    fun allSaveDuringVideoCaptureStillRejectsAWindowWithoutVideoOrLogs() {
        CaptureStore(root.resolve("ring"), minFree = 0).use { store ->
            val host = System.nanoTime() + 60_000_000_000
            store.generation(1)
            store.status("video", StreamState.CAPTURING, null, 1)
            store.status("device_log", StreamState.CAPTURING, null, 1)
            store.app("com.fixture.app", 10001, setOf(12), 1, true)
            for (second in 1L..4L) {
                val elapsed = second * 1_000_000_000
                val received = host + (second - 1) * 1_000_000_000
                assertTrue(store.clock.add("1", listOf(elapsed, elapsed, 1_700_000_000_000_000_000 + elapsed, elapsed), received, received))
                store.clockStatus(true, 1)
            }
            assertNull(store.capture(ReplaySettings()))
        }
    }

    @Test
    fun replayCoreLogsOnlySaveStartsAtLogReadinessBeforeAnyFrameOrLineWhileAllStaysUnavailable() {
        val output = Files.createDirectories(root.resolve("out"))
        val core = ReplayCore(ReplaySettings(saveDirectory = output, application = ApplicationTarget("com.fixture.app"),
            captureMode = CaptureMode.VIDEO_AND_LOGS, replaySeconds = 30), root.resolve("workspace"))
        try {
            assertTrue(core.setEnabled(true).get(5, TimeUnit.SECONDS).accepted)
            fun <T> field(name: String): T = @Suppress("UNCHECKED_CAST")
                (ReplayCore::class.java.getDeclaredField(name).apply { isAccessible = true }.get(core) as T)
            field<ScheduledExecutorService>("control").submit {
                ReplayCore::class.java.getDeclaredField("device").apply { isAccessible = true }
                    .set(core, ReplayDevice("fixture", "Synthetic", DeviceKind.EMULATOR, true))
                val store = field<CaptureStore>("store")
                val base = System.nanoTime() - 3_000_000_000
                fun anchor(second: Long) {
                    val elapsed = 100_000_000_000 + second * 1_000_000_000
                    val received = base + second * 1_000_000_000
                    assertTrue(store.clock.add("1", listOf(elapsed, elapsed, 1_700_000_000_000_000_000 + elapsed, elapsed), received, received))
                    store.clockStatus(true, 1)
                }
                store.generation(1)
                store.status("video", StreamState.WAITING, null, 1)
                anchor(0)
                store.status("device_log", StreamState.CAPTURING, null, 1)
                store.app("com.fixture.app", 10001, setOf(12), 1, true)
                for (second in 1L..3L) anchor(second)
            }.get(5, TimeUnit.SECONDS)
            Thread.sleep(50)
            // Neither a frame nor a log row exists, so the regular save keeps rejecting the window.
            val all = core.save().get(5, TimeUnit.SECONDS)
            assertFalse(all.accepted)
            assertEquals("保存できる取得データがありません", all.reason)
            assertFalse(core.snapshot().canSave(SaveContent.ALL))
            assertTrue(core.snapshot().canSave(SaveContent.LOGS_ONLY))
            assertNull(core.snapshot().saveDisabledReason(SaveContent.LOGS_ONLY))
            val logs = core.save(SaveRequest(SaveContent.LOGS_ONLY, SaveOrigin.LOGCAT_TOOLBAR)).get(5, TimeUnit.SECONDS)
            assertTrue(logs.accepted, logs.reason)
            val saved = assertNotNull(logs.saveResult).toCompletableFuture().get(15, TimeUnit.SECONDS)
            assertEquals(SavePhase.COMPLETED, saved.phase, saved.error)
            assertEquals(SaveContent.LOGS_ONLY, saved.content)
            val directory = assertNotNull(saved.directory)
            Files.list(directory).use { files -> assertTrue(files.noneMatch { it.fileName.toString().endsWith(".mp4") }) }
            assertEquals(0L, Files.size(directory.resolve("logcat-device.jsonl")))
            val json = JsonParser.parseString(Files.readString(directory.resolve("replay.json"))).asJsonObject
            assertEquals("VIDEO_AND_LOGS", json["capture_mode"].asString)
            assertEquals("LOGCAT_TOOLBAR", json["save_request"].asJsonObject["origin"].asString)
            assertEquals("EXCLUDED", json["coverage"].asJsonObject["video"].asJsonObject["state"].asString)
            assertTrue(core.snapshot().enabled)
        } finally { core.closeAsync().get(15, TimeUnit.SECONDS) }
    }

    private fun frameAndLog(store: CaptureStore, host: Long, second: Long, bytes: ByteArray, log: Boolean = true) {
        val elapsed = second * 1_000_000_000
        val received = host + (second - 1) * 1_000_000_000
        assertTrue(store.clock.add("1", listOf(elapsed, elapsed, 1_700_000_000_000_000_000 + elapsed, elapsed), received, received))
        store.clockStatus(true, 1)
        assertTrue(store.frame(VideoPacket.Frame(second * 1_000_000, false, true, bytes), 1, received))
        if (log) store.log(DeviceLog(1_700_000_000_000_000_000 + elapsed, 12, 12, 10001, 0, 4, "Fixture", "row-$second", byteArrayOf(1)), 1, received)
    }

    private fun sample(): ByteArray {
        val picture = Picture.create(32, 32, ColorSpace.YUV420J).apply { fill(15) }
        val data = H264Encoder.createH264Encoder().encodeIDRFrame(picture, ByteBuffer.allocate(65536))
        return ByteArray(data.remaining()).also { data.get(it) }
    }

    private fun config(bytes: ByteArray): ByteArray = H264Utils.splitFrame(ByteBuffer.wrap(bytes))
        .filter { it.get(0).toInt() and 31 in setOf(7, 8) }
        .fold(byteArrayOf()) { result, nal -> result + byteArrayOf(0, 0, 0, 1) + ByteArray(nal.remaining()).also { nal.get(it) } }
}
