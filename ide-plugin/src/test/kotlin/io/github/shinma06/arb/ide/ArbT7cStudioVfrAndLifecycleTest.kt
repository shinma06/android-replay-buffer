package io.github.shinma06.arb.ide

import com.google.gson.JsonArray
import com.google.gson.JsonObject
import com.google.gson.JsonParser
import com.intellij.icons.AllIcons
import io.github.shinma06.arb.ipc.ArbPaths
import io.github.shinma06.arb.ipc.ArbdProcessManager
import io.github.shinma06.arb.ipc.ArbdRpcClient
import io.github.shinma06.arb.ipc.HostOs
import io.github.shinma06.arb.ipc.LocalIpcTransport
import io.github.shinma06.arb.model.AdbDeviceInfo
import io.github.shinma06.arb.model.AdbDeviceState
import io.github.shinma06.arb.model.ArbConfig
import io.github.shinma06.arb.model.ArbdRpcException
import io.github.shinma06.arb.model.CaptureMode
import io.github.shinma06.arb.model.CaptureStatusView
import io.github.shinma06.arb.model.RecoverySummaryView
import io.github.shinma06.arb.model.SystemStatusResponse
import io.github.shinma06.arb.playback.AudioSink
import io.github.shinma06.arb.playback.PcmFormat
import io.github.shinma06.arb.playback.PlaybackClock
import io.github.shinma06.arb.playback.PlaybackMode
import io.github.shinma06.arb.shm.ShmFrameRingReader
import io.github.shinma06.arb.shm.ShmPixelFormat
import io.github.shinma06.arb.state.ArbFormatters
import io.github.shinma06.arb.state.CaptureBarReducer
import io.github.shinma06.arb.state.CaptureBarState
import io.github.shinma06.arb.state.SourceJumpTarget
import io.github.shinma06.arb.ui.ArbToolWindowController
import io.github.shinma06.arb.ui.CaptureToggleState
import io.github.shinma06.arb.ui.persistCaptureEnabled
import io.github.shinma06.arb.ui.toToolbarSnapshot
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.InputStream
import java.io.OutputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong
import javax.swing.SwingUtilities
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * T-7C fail-first regression tests covering:
 * - H8: Real VFR `frame_pts_ns` wiring from `media.open_viewport` into `ArbBundleFileEditor` -> `PlaybackController`
 * - M1: Atomic viewport ownership closing leaked viewports when tab disposes mid-`open_viewport`
 * - M2: Decoupled video-first opening + bounded cursor-paged log fetching with adaptive halving on 4 MiB cap
 * - Item 7: Always-on background capture armed UI (`OFF` / `ON・待機中` / `ON・REC`) across 0-device & disconnects
 * - Item 8: Settings ON/OFF toggle for OS menu bar / system tray (`trayEnabled`, default `false`, managed lifecycle)
 */
class ArbT7cStudioVfrAndLifecycleTest {
    private val noopJumpResolver = object : SourceJumpResolver {
        override fun resolveAndNavigate(target: SourceJumpTarget): Boolean = false
    }

    private class VirtualClock(initialNs: Long = 1_000_000_000L) : PlaybackClock {
        private val now = AtomicLong(initialNs)
        override fun nowNs(): Long = now.get()
        fun advanceMs(ms: Long) {
            now.addAndGet(ms * 1_000_000L)
        }
    }

    private class FakeAudioSink : AudioSink {
        override val isAvailable: Boolean = true
        override fun open(format: PcmFormat): Boolean = true
        override fun start() = Unit
        override fun stop() = Unit
        override fun flush() = Unit
        override fun write(pcm: ByteArray, offset: Int, length: Int): Int = length
        override fun close() = Unit
    }

    private class FakeManagedProcess : Process() {
        @Volatile
        private var alive: Boolean = true
        val destroyed = AtomicBoolean(false)
        val destroyedForcibly = AtomicBoolean(false)

        override fun getOutputStream(): OutputStream = ByteArrayOutputStream()
        override fun getInputStream(): InputStream = ByteArrayInputStream(ByteArray(0))
        override fun getErrorStream(): InputStream = ByteArrayInputStream(ByteArray(0))
        override fun waitFor(): Int = if (alive) 1 else 0
        override fun waitFor(timeout: Long, unit: TimeUnit): Boolean = !alive
        override fun exitValue(): Int = if (alive) throw IllegalThreadStateException("running") else 0
        override fun destroy() {
            destroyed.set(true)
            alive = false
        }
        override fun destroyForcibly(): Process {
            destroyedForcibly.set(true)
            alive = false
            return this
        }
        override fun isAlive(): Boolean = alive
    }

    private fun awaitCondition(timeoutMs: Long = 5_000L, msg: String = "condition timed out", check: () -> Boolean) {
        val deadline = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < deadline) {
            if (check()) return
            Thread.sleep(15L)
        }
        assertTrue(check(), msg)
    }

    /** Creates a valid triple-buffered ARB SHM file with a 2x2 RGBA8 frame already published in slot 0. */
    private fun writeValidShmFile(path: Path, frameIndex: Long = 0L): Path {
        val slotCapacity = 64
        val alignedStride = ShmFrameRingReader.alignUp(slotCapacity, ShmFrameRingReader.CACHE_LINE_BYTES)
        val totalSize = ShmFrameRingReader.HEADER_REGION_BYTES + alignedStride * ShmFrameRingReader.SHM_SLOT_COUNT
        val buf = ByteBuffer.allocate(totalSize).order(ByteOrder.LITTLE_ENDIAN)
        // Ring header (64 bytes)
        buf.position(0)
        buf.put(ShmFrameRingReader.RING_MAGIC)
        buf.putInt(8, ShmFrameRingReader.RING_VERSION)
        buf.putInt(12, ShmFrameRingReader.SHM_SLOT_COUNT)
        buf.putLong(16, slotCapacity.toLong())
        buf.putInt(24, 0) // published_slot = 0
        buf.putLong(32, 1L) // published_generation = 1
        // Slot 0 header @ offset 64
        val slotBase = ShmFrameRingReader.RING_HEADER_BYTES
        buf.putLong(slotBase + 0, 2L) // even seqlock = stable
        buf.putLong(slotBase + 8, 1L) // generation
        buf.putLong(slotBase + 16, frameIndex) // frame_index
        buf.putLong(slotBase + 24, 10_000_000_000L) // boottime_ns
        buf.putInt(slotBase + 32, 2) // width
        buf.putInt(slotBase + 36, 2) // height
        buf.putInt(slotBase + 40, 8) // stride = 2 * 4
        buf.putInt(slotBase + 44, ShmPixelFormat.RGBA8.wire)
        buf.putInt(slotBase + 48, 16) // payload_len = 2 * 2 * 4
        buf.putInt(slotBase + 52, 0) // flags = exact
        // Payload slot 0 @ offset 256
        val payloadOffset = ShmFrameRingReader.HEADER_REGION_BYTES
        for (i in 0 until 16) {
            buf.put(payloadOffset + i, 0x7F.toByte())
        }
        Files.write(path, buf.array())
        return path
    }

    /** Creates a valid 48 kHz 16-bit LE stereo WAV file of [durationSeconds] seconds. */
    private fun writeValidWavFile(path: Path, durationSeconds: Int): Path {
        val sampleRate = 48_000
        val channels = 2
        val bitsPerSample = 16
        val blockAlign = channels * (bitsPerSample / 8)
        val byteRate = sampleRate * blockAlign
        val dataBytes = sampleRate * durationSeconds * blockAlign
        val buf = ByteBuffer.allocate(44 + dataBytes).order(ByteOrder.LITTLE_ENDIAN)
        buf.put("RIFF".toByteArray(Charsets.US_ASCII))
        buf.putInt(36 + dataBytes)
        buf.put("WAVE".toByteArray(Charsets.US_ASCII))
        buf.put("fmt ".toByteArray(Charsets.US_ASCII))
        buf.putInt(16)
        buf.putShort(1) // PCM
        buf.putShort(channels.toShort())
        buf.putInt(sampleRate)
        buf.putInt(byteRate)
        buf.putShort(blockAlign.toShort())
        buf.putShort(bitsPerSample.toShort())
        buf.put("data".toByteArray(Charsets.US_ASCII))
        buf.putInt(dataBytes)
        Files.write(path, buf.array())
        return path
    }

    private class StubTransport(
        private val maxResponseBytes: Int = Int.MAX_VALUE,
        private val handler: (method: String, params: JsonObject) -> String,
    ) : LocalIpcTransport {
        override val endpoint: String = "mock://t7c"
        val methodsCalled = CopyOnWriteArrayList<String>()
        val closed = AtomicBoolean(false)
        private val responses = LinkedBlockingQueue<String>()

        override fun writeLine(jsonLine: String) {
            check(!closed.get()) { "transport closed" }
            val obj = JsonParser.parseString(jsonLine).asJsonObject
            val id = obj.get("id").asLong
            val method = obj.get("method").asString
            val params = obj.get("params")?.takeIf { it.isJsonObject }?.asJsonObject ?: JsonObject()
            methodsCalled += method
            val resultJson = handler(method, params)
            if (resultJson.startsWith(RPC_ERROR_PREFIX)) {
                val message = resultJson.removePrefix(RPC_ERROR_PREFIX)
                responses.put("""{"jsonrpc":"2.0","id":$id,"error":{"code":-32602,"message":"$message"}}""")
                return
            }
            val fullLine = """{"jsonrpc":"2.0","id":$id,"result":$resultJson}"""
            val byteLen = fullLine.toByteArray(Charsets.UTF_8).size
            if (byteLen > maxResponseBytes) {
                responses.put(
                    """{"jsonrpc":"2.0","id":$id,"error":{"code":${ArbBundleFileEditor.RPC_ERR_RESPONSE_TOO_LARGE},"message":"response of $byteLen bytes exceeds the $maxResponseBytes-byte IPC frame limit; narrow the request (limit / start_ns / end_ns)"}}""",
                )
            } else {
                responses.put(fullLine)
            }
        }

        override fun readLine(): String? = responses.take()
        override fun close() {
            closed.set(true)
        }
    }

    private fun bundleOpenResultJson(
        bundleId: String,
        frameCount: Int,
        startNs: Long,
        endNs: Long,
        hasAudio: Boolean = false,
    ): String {
        val audioPathPart = if (hasAudio) ""","audio_path":"/tmp/$bundleId/audio.m4a"""" else ""
        val audioStreamPart = if (hasAudio) {
            ""","audio":{"status":"present","av_offset_status":"calibrated","av_offset_ms":50,"tail_may_be_incomplete":false}"""
        } else {
            ""
        }
        return """{
            "bundle_id":"$bundleId",
            "path":"/tmp/$bundleId",
            "format":"v2",
            "is_legacy":false,
            "lease_id":"lease-$bundleId",
            "summary":{
                "bundle_id":"$bundleId",
                "path":"/tmp/$bundleId",
                "format":"v2",
                "is_legacy":false,
                "provenance":{"kind":"original"},
                "streams":{
                    "video":{"status":"present","frame_count":$frameCount,"tail_may_be_incomplete":false,"semantics":"video_encoder_input_estimate"}$audioStreamPart
                },
                "unanchored_summary":{"included_count":0,"truncated_count":0},
                "epochs":[{"boot_id":"boot-1","start_boottime_ns":$startNs,"end_boottime_ns":$endNs}],
                "epoch_frame_ranges":[],
                "frame_count":$frameCount,
                "keyframe_count":2,
                "log_main_count":0,
                "log_unanchored_count":0,
                "tap_main_count":0,
                "tap_unanchored_count":0,
                "discontinuity_count":0,
                "video_path":"/tmp/$bundleId/video.mp4"$audioPathPart
            }
        }""".trimIndent().replace("\n", "")
    }

    private data class SyntheticLog(val seq: Long, val tsNs: Long, val msg: String)

    /**
     * Mirrors arbd's `timeline.query_logs` SQL: inclusive `start_ns`/`end_ns` filter on the row
     * timestamp, `ORDER BY seq ASC`, then `LIMIT`.
     */
    private fun fakeArbdLogQuery(rows: List<SyntheticLog>, params: JsonObject, limit: Int): List<SyntheticLog> {
        val startNs = params.get("start_ns")?.takeUnless { it.isJsonNull }?.asLong
        val endNs = params.get("end_ns")?.takeUnless { it.isJsonNull }?.asLong
        return rows
            .asSequence()
            .filter { it.seq > (params.get("after_seq")?.asLong ?: -1L) }
            .filter { (startNs == null || it.tsNs >= startNs) && (endNs == null || it.tsNs <= endNs) }
            .sortedBy { it.seq }
            .take(limit)
            .toList()
    }

    // ---------------- H8: Real VFR frame_pts_ns wiring into PlaybackController ----------------

    @Test
    fun studioEditorWiresRealVfrFramePtsNsForPlayheadTimingAndPostLoopAudioAlignment(@TempDir tmp: Path) {
        val shmPath = writeValidShmFile(tmp.resolve("vfr.shm"))
        val wavPath = writeValidWavFile(tmp.resolve("vfr.wav"), durationSeconds = 8)

        // Synthetic VFR recording:
        // Frame 0 holds for 5.0 seconds static (10s -> 15s), then 10 frames burst at 30fps (33.33ms apart).
        val baseNs = 10_000_000_000L
        val step30FpsNs = 33_333_333L
        val framePtsNs = buildList {
            add(baseNs) // frame 0: 0.000s relative
            for (i in 0 until 10) {
                add(baseNs + 5_000_000_000L + i * step30FpsNs) // frames 1..10: 5.000s .. 5.300s
            }
        }
        val framePtsJson = framePtsNs.joinToString(",")
        val transport = StubTransport { method, params ->
            when (method) {
                "bundle.open" -> bundleOpenResultJson(
                    bundleId = "vfr-bundle",
                    frameCount = framePtsNs.size,
                    startNs = framePtsNs.first(),
                    endNs = framePtsNs.last(),
                    hasAudio = true,
                )
                "edit.get_session" -> """{"session":null}"""
                "media.open_viewport" -> """{"viewport_id":"vp-vfr","bundle_id":"vfr-bundle","shm_path":"${shmPath.toString().replace("\\", "\\\\")}","width":2,"height":2,"stride":8,"pixel_format":"rgba8","slot_capacity":64,"frame_count":${framePtsNs.size},"keyframe_count":2,"frame_pts_ns":[$framePtsJson]}"""
                "media.seek_frame" -> {
                    val idx = params.get("frame_index").asLong
                    """{"generation":1,"frame_index":$idx,"is_approximate":false,"width":2,"height":2,"cache_hits":1,"cache_misses":0,"elapsed_ms":1}"""
                }
                "media.decode_audio_pcm" -> """{"available":true,"wav_path":"${wavPath.toString().replace("\\", "\\\\")}","sample_rate_hz":48000,"channels":2,"duration_ns":8000000000,"av_offset_ms":50,"cached":true}"""
                "media.filmstrip" -> """{"origin":"cache","cache_path":"/tmp/f","count":0,"elapsed_ms":1,"entries":[]}"""
                "timeline.discontinuities" -> """{"discontinuities":[]}"""
                "timeline.crashes" -> """{"crashes":[]}"""
                "timeline.screens" -> """{"screens":[]}"""
                "timeline.query_logs" -> """{"logs":[]}"""
                "timeline.query_taps" -> """{"taps":[]}"""
                "media.close_viewport" -> """{"closed":true,"viewport_id":"vp-vfr"}"""
                "bundle.close" -> """{"released":true,"lease_id":"lease-vfr-bundle"}"""
                else -> "{}"
            }
        }
        val rpc = ArbdRpcClient(transport)
        val daemonService = object : ArbdDaemonService() {
            override fun <T> withClient(block: (ArbdRpcClient) -> T): T = block(rpc)
        }
        val clock = VirtualClock()
        val editor = ArbBundleFileEditor(
            project = null,
            backingFile = ArbBundleVirtualFile("vfr-bundle", "vfr-bundle"),
            bundleIdOrPath = "vfr-bundle",
            displayTitle = "vfr-bundle",
            daemonService = daemonService,
            sourceJumpResolver = noopJumpResolver,
            playbackClock = clock,
            autoRunPlaybackLoop = false,
            audioSinkFactory = { FakeAudioSink() },
            audioAutoRunPump = false,
            publishEdt = { it() },
        )

        try {
            awaitCondition(msg = "PlaybackController and AudioSyncPlayer should attach") {
                editor.playbackControllerForTest() != null && editor.audioPlayerForTest() != null && editor.studioController.viewerState != null
            }
            val ctrl = assertNotNull(editor.playbackControllerForTest())
            val audio = assertNotNull(editor.audioPlayerForTest())

            val entered = CountDownLatch(1)
            val otherWrite = CountDownLatch(1)
            val slowWriter = Thread {
                editor.updateViewerState {
                    entered.countDown()
                    otherWrite.await(200, TimeUnit.MILLISECONDS)
                    it.copy(lastActionMessage = "loaded")
                }
            }
            slowWriter.start()
            assertTrue(entered.await(5, TimeUnit.SECONDS))
            editor.updateViewerState { it.copy(showTapOverlay = false) }
            otherWrite.countDown()
            slowWriter.join(5_000)
            assertFalse(editor.studioController.viewerState!!.showTapOverlay)

            // Verify real VFR PTS map is active on PlaybackController (frame 1 = 5.000s, NOT average ~0.53s!).
            assertEquals(0L, ctrl.frameRelativePtsNs(0L))
            assertEquals(5_000_000_000L, ctrl.frameRelativePtsNs(1L))
            assertEquals(5_000_000_000L + 9L * step30FpsNs, ctrl.frameRelativePtsNs(10L))

            assertEquals(5_000L, editor.studioController.viewerState!!.frameToRelativeMs(1))

            // Start playback from frame 0 on virtual clock: must hold frame 0 through 4.9s, then move at 5.0s!
            ctrl.play()
            assertEquals(PlaybackMode.PLAYING, ctrl.state.mode)
            assertEquals(0L, ctrl.state.currentFrameIndex)

            clock.advanceMs(1_000L) // t = 1.0s (average FPS would already be at frame 1-2!)
            ctrl.tick()
            assertEquals(0L, ctrl.state.currentFrameIndex, "must remain on static frame 0 at 1.0s")

            clock.advanceMs(3_900L) // t = 4.9s
            ctrl.tick()
            assertEquals(0L, ctrl.state.currentFrameIndex, "must remain on static frame 0 at 4.9s")

            clock.advanceMs(100L) // t = 5.0s -> burst starts at frame 1!
            ctrl.tick()
            assertEquals(1L, ctrl.state.currentFrameIndex, "motion must begin at exact 5.0s real VFR PTS")

            clock.advanceMs(105L) // t = 5.105s -> 3 * 33.33ms into burst => frame 4
            ctrl.tick()
            assertEquals(4L, ctrl.state.currentFrameIndex, "burst frames must follow real 30fps PTS")

            // Loop selection [1..10]: wrapping to inFrame=1 must re-align audio to exact 5.0s - 50ms avOffset!
            ctrl.setInFrame(1L)
            ctrl.setOutFrame(10L)
            ctrl.setLoopEnabled(true)
            clock.advanceMs(500L) // past frame 10 end (5.333s) -> triggers loop wrap to frame 1
            ctrl.tick()
            assertEquals(1L, ctrl.state.currentFrameIndex, "loop wrap must land on inFrame=1")
            assertEquals(
                5_000_000_000L - 50_000_000L,
                audio.audioPositionNs,
                "post-loop audio position must match real VFR frame 1 PTS minus avOffsetMs",
            )
        } finally {
            editor.dispose()
            kotlin.test.assertNull(editor.audioPlayerForTest())
            awaitCondition(msg = "viewport should close on dispose") {
                transport.methodsCalled.contains("media.close_viewport")
            }
            rpc.close()
            daemonService.dispose()
        }
    }

    // ---------------- M1: Viewport Leak When Tab Closes Mid-open_viewport ----------------

    @Test
    fun closingEditorMidOpenViewportClosesReturnedViewportWithoutLeakingOrBreakingSharedIpc(@TempDir tmp: Path) {
        val shmPath = writeValidShmFile(tmp.resolve("race.shm"))
        val openViewportEntered = CountDownLatch(1)
        val releaseOpenViewport = CountDownLatch(1)
        val activeViewports = AtomicInteger(0)
        val activeLeases = AtomicInteger(0)
        val closedViewportIds = CopyOnWriteArrayList<String>()

        val transport = StubTransport { method, params ->
            when (method) {
                "bundle.open" -> {
                    activeLeases.incrementAndGet()
                    bundleOpenResultJson("race-bundle", frameCount = 5, startNs = 0L, endNs = 1_000_000_000L)
                }
                "edit.get_session" -> """{"session":null}"""
                "media.open_viewport" -> {
                    activeViewports.incrementAndGet()
                    openViewportEntered.countDown()
                    assertTrue(releaseOpenViewport.await(5, TimeUnit.SECONDS), "test must release open_viewport latch")
                    """{"viewport_id":"vp-race-1","bundle_id":"race-bundle","shm_path":"${shmPath.toString().replace("\\", "\\\\")}","width":2,"height":2,"stride":8,"pixel_format":"rgba8","slot_capacity":64,"frame_count":5,"keyframe_count":1,"frame_pts_ns":[0,100000000,200000000,300000000,400000000]}"""
                }
                "media.close_viewport" -> {
                    val vpId = params.get("viewport_id").asString
                    closedViewportIds += vpId
                    activeViewports.decrementAndGet()
                    """{"closed":true,"viewport_id":"$vpId"}"""
                }
                "bundle.close" -> {
                    activeLeases.decrementAndGet()
                    """{"released":true,"lease_id":"lease-race-bundle"}"""
                }
                "system.status" -> """{"pid":77,"uptime_ms":100,"capture":{"active":false,"connected":false,"storage_pressure_stopped":false,"video_frames":0,"audio_frames":0,"log_records":0,"tap_records":0,"discontinuities":0,"static_hold_events":0},"ring_chunk_count":0,"ring_total_bytes":0,"ring_pinned_chunk_count":0,"buffered_secs":0,"active_viewports":${activeViewports.get()},"active_save_jobs":0,"recovery":{}}"""
                else -> "{}"
            }
        }
        val rpc = ArbdRpcClient(transport)
        val daemonService = object : ArbdDaemonService() {
            override fun <T> withClient(block: (ArbdRpcClient) -> T): T = block(rpc)
        }

        val editor = ArbBundleFileEditor(
            project = null,
            backingFile = ArbBundleVirtualFile("race-bundle", "race-bundle"),
            bundleIdOrPath = "race-bundle",
            displayTitle = "race-bundle",
            daemonService = daemonService,
            sourceJumpResolver = noopJumpResolver,
            publishEdt = { it() },
        )

        try {
            assertTrue(openViewportEntered.await(5, TimeUnit.SECONDS), "editor must enter media.open_viewport")
            assertEquals(1, activeViewports.get(), "viewport allocated on daemon while in flight")

            // Close tab BEFORE media.open_viewport returns!
            editor.dispose()

            // Now let media.open_viewport return its viewport_id.
            releaseOpenViewport.countDown()

            awaitCondition(msg = "in-flight viewport and lease must both close to 0") {
                activeViewports.get() == 0 && activeLeases.get() == 0
            }
            assertEquals(listOf("vp-race-1"), closedViewportIds.toList(), "viewport must be closed exactly once")
            assertFalse(transport.closed.get(), "shared IPC transport must remain alive")

            // Confirm shared IPC connection still works for subsequent requests and reports active_viewports == 0.
            val status = daemonService.withClient { it.systemStatus() }
            assertEquals(0L, status.activeViewports)
        } finally {
            releaseOpenViewport.countDown()
            editor.dispose()
            rpc.close()
            daemonService.dispose()
        }
    }

    // ---------------- M2: Decoupled Video-First Open + Paged Logs Under 4 MiB Cap ----------------

    @Test
    fun largeLogBundleOpensVideoImmediatelyAndPaginatesAll5000LogsWithoutDuplicatesOrGaps(@TempDir tmp: Path) {
        val shmPath = writeValidShmFile(tmp.resolve("logs.shm"))
        val totalLogs = 5_000
        val payload1KiB = "L".repeat(1_000)

        // Build 5,000 synthetic 1 KiB logs. arbd filters by timestamp but orders/truncates by seq, so
        // include (a) a timestamp tie straddling the first page boundary and (b) late-arriving rows
        // whose seq is later but whose timestamp is earlier than their neighbours (logcat buffers
        // merge slightly out of order). A "last timestamp" cursor would silently skip (b).
        val allMainLogs = (1L..totalLogs.toLong()).map { seq ->
            val ts = when {
                seq in 498L..503L -> 10_000_500_000L // 6 rows sharing one exact nanosecond timestamp
                seq % 97L == 0L -> 10_000_000_000L + (seq - 40L) * 1_000L // arrived 40 rows late
                else -> 10_000_000_000L + seq * 1_000L
            }
            SyntheticLog(seq, ts, "line-$seq-$payload1KiB")
        }

        val releaseLogsLatch = CountDownLatch(1)
        val logQueryCalls = AtomicInteger(0)
        val requestedLimits = CopyOnWriteArrayList<Int>()
        val fourMiB = 4 * 1024 * 1024

        val transport = StubTransport(maxResponseBytes = fourMiB) { method, params ->
            when (method) {
                "bundle.open" -> bundleOpenResultJson("big-logs", frameCount = 10, startNs = 10_000_000_000L, endNs = 11_000_000_000L)
                "edit.get_session" -> """{"session":null}"""
                "media.open_viewport" -> """{"viewport_id":"vp-logs","bundle_id":"big-logs","shm_path":"${shmPath.toString().replace("\\", "\\\\")}","width":2,"height":2,"stride":8,"pixel_format":"rgba8","slot_capacity":64,"frame_count":10,"keyframe_count":1,"frame_pts_ns":[0,100000000,200000000,300000000,400000000,500000000,600000000,700000000,800000000,900000000]}"""
                "media.seek_frame" -> {
                    val idx = params.get("frame_index").asLong
                    """{"generation":1,"frame_index":$idx,"is_approximate":false,"width":2,"height":2,"cache_hits":1,"cache_misses":0,"elapsed_ms":1}"""
                }
                "media.filmstrip" -> """{"origin":"cache","cache_path":"/tmp/f","count":0,"elapsed_ms":1,"entries":[]}"""
                "timeline.discontinuities" -> """{"discontinuities":[]}"""
                "timeline.crashes" -> """{"crashes":[]}"""
                "timeline.screens" -> """{"screens":[]}"""
                "timeline.query_taps" -> if (params.get("bucket").asString == "main") "${RPC_ERROR_PREFIX}IPC frame limit" else """{"taps":[]}"""
                "timeline.query_logs" -> {
                    assertTrue(releaseLogsLatch.await(10, TimeUnit.SECONDS))
                    val bucket = params.get("bucket")?.asString
                    if (bucket == "unanchored") {
                        """{"logs":[]}"""
                    } else {
                        logQueryCalls.incrementAndGet()
                        val limit = params.get("limit")?.asInt ?: 10_000
                        requestedLimits += limit
                        val matched = fakeArbdLogQuery(allMainLogs, params, limit)
                        val arr = JsonArray()
                        for (row in matched) {
                            arr.add(
                                JsonObject().apply {
                                    addProperty("seq", row.seq)
                                    addProperty("pc_ingest_cont_ns", row.tsNs)
                                    add(
                                        "alignment",
                                        JsonObject().apply {
                                            addProperty("certainty", "anchored")
                                            addProperty("boot_id", "boot-1")
                                            addProperty("boottime_ns", row.tsNs)
                                            addProperty("half_width_ns", 1L)
                                            addProperty("semantics", "logcat_device_read_boottime")
                                        },
                                    )
                                    addProperty("bucket", "main")
                                    addProperty("level", "I")
                                    addProperty("tag", "BigTag")
                                    addProperty("message", row.msg)
                                },
                            )
                        }
                        JsonObject().apply { add("logs", arr) }.toString()
                    }
                }
                "media.close_viewport" -> """{"closed":true,"viewport_id":"vp-logs"}"""
                "bundle.close" -> """{"released":true,"lease_id":"lease-big-logs"}"""
                else -> "{}"
            }
        }
        val rpc = ArbdRpcClient(transport)
        val daemonService = object : ArbdDaemonService() {
            override fun <T> withClient(block: (ArbdRpcClient) -> T): T = block(rpc)
        }

        val editor = ArbBundleFileEditor(
            project = null,
            backingFile = ArbBundleVirtualFile("big-logs", "big-logs"),
            bundleIdOrPath = "big-logs",
            displayTitle = "big-logs",
            daemonService = daemonService,
            sourceJumpResolver = noopJumpResolver,
            publishEdt = { it() },
        )

        try {
            // Video MUST open and publish frame 0 IMMEDIATELY even while timeline.query_logs is still blocked!
            awaitCondition(msg = "video must open and render frame 0 while logs are still blocked") {
                editor.studioController.hasVideo && editor.studioController.frameImageWidth == 2
            }
            assertEquals(0, logQueryCalls.get(), "video opened before any log page completed")


            // Release log queries and verify all 5,000 1 KiB logs paginate cleanly under 4 MiB cap!
            releaseLogsLatch.countDown()
            awaitCondition(timeoutMs = 10_000L, msg = "all 5,000 logs must load via cursor pages") {
                editor.studioController.viewerState?.mainLogs?.size == totalLogs
            }

            val loaded = assertNotNull(editor.studioController.viewerState).mainLogs
            assertEquals(totalLogs, loaded.size, "no truncation of 5,000 1 KiB logs")
            assertEquals((1L..totalLogs.toLong()).toList(), loaded.map { it.seq }, "zero duplicates and zero gaps across page boundaries")
            assertTrue(logQueryCalls.get() >= 10, "must use bounded pages, got ${logQueryCalls.get()} calls")
            assertTrue(requestedLimits.all { it <= 501 }, "every page request must stay bounded: $requestedLimits")
            // The pre-fix single 5,000-row request would itself exceed the 4 MiB frame cap.
            val singleShotBytes = allMainLogs.sumOf { it.msg.length + 200 }
            assertTrue(singleShotBytes > fourMiB, "fixture must exceed the 4 MiB cap in one response")
            assertTrue(editor.studioController.timelineWarnings.any { it.contains("タップ") })
        } finally {
            releaseLogsLatch.countDown()
            editor.dispose()
            rpc.close()
            daemonService.dispose()
        }
    }

    @Test
    fun pagedLogQueryAdaptivelyHalvesPageSizeWhenSinglePageExceeds4MiB(@TempDir tmp: Path) {
        val shmPath = writeValidShmFile(tmp.resolve("huge.shm"))
        // 40 rows x 16 KiB each: initial page of 32 (~512 KiB) against a simulated 200 KiB cap forces halving!
        val bigMsg = "X".repeat(16_000)
        val legacy = AtomicBoolean(false)
        val requestedLimits = CopyOnWriteArrayList<Int>()
        val transport = StubTransport(maxResponseBytes = 200_000) { method, params ->
            when (method) {
                "bundle.open" -> bundleOpenResultJson("huge-lines", frameCount = 1, startNs = 0L, endNs = 100L)
                "edit.get_session" -> """{"session":null}"""
                "media.open_viewport" -> """{"viewport_id":"vp-huge","bundle_id":"huge-lines","shm_path":"${shmPath.toString().replace("\\", "\\\\")}","width":2,"height":2,"stride":8,"pixel_format":"rgba8","slot_capacity":64,"frame_count":1,"keyframe_count":1,"frame_pts_ns":[0]}"""
                "media.seek_frame" -> """{"generation":1,"frame_index":0,"is_approximate":false,"width":2,"height":2,"cache_hits":1,"cache_misses":0,"elapsed_ms":1}"""
                "media.filmstrip" -> """{"origin":"cache","cache_path":"/tmp/f","count":0,"elapsed_ms":1,"entries":[]}"""
                "timeline.discontinuities" -> """{"discontinuities":[]}"""
                "timeline.crashes" -> """{"crashes":[]}"""
                "timeline.screens" -> """{"screens":[]}"""
                "timeline.query_taps" -> """{"taps":[]}"""
                "timeline.query_logs" -> {
                    val bucket = params.get("bucket")?.asString
                    if (bucket == "unanchored") {
                        """{"logs":[]}"""
                    } else {
                        val startNs = params.get("start_ns")?.asLong
                        val endNs = params.get("end_ns")?.asLong
                        val limit = params.get("limit")?.asInt ?: 32
                        requestedLimits += limit
                        val arr = JsonArray()
                        for (seq in 1L..40L) {
                            val ts = 1_000L
                            if (!legacy.get() && seq <= (params.get("after_seq")?.asLong ?: 0L)) continue
                            if (startNs != null && ts < startNs) continue
                            if (endNs != null && ts > endNs) continue
                            if (arr.size() >= limit) break
                            arr.add(
                                JsonObject().apply {
                                    addProperty("seq", seq)
                                    addProperty("pc_ingest_cont_ns", ts)
                                    add(
                                        "alignment",
                                        JsonObject().apply {
                                            addProperty("certainty", "anchored")
                                            addProperty("boot_id", "b")
                                            addProperty("boottime_ns", ts)
                                            addProperty("half_width_ns", 1L)
                                            addProperty("semantics", "logcat_device_read_boottime")
                                        },
                                    )
                                    addProperty("bucket", "main")
                                    addProperty("message", bigMsg)
                                },
                            )
                        }
                        JsonObject().apply { add("logs", arr) }.toString()
                    }
                }
                "media.close_viewport" -> """{"closed":true,"viewport_id":"vp-huge"}"""
                "bundle.close" -> """{"released":true,"lease_id":"lease-huge-lines"}"""
                else -> "{}"
            }
        }
        val rpc = ArbdRpcClient(transport)
        val daemonService = object : ArbdDaemonService() {
            override fun <T> withClient(block: (ArbdRpcClient) -> T): T = block(rpc)
        }
        val editor = ArbBundleFileEditor(
            project = null,
            backingFile = ArbBundleVirtualFile("huge-lines", "huge-lines"),
            bundleIdOrPath = "huge-lines",
            displayTitle = "huge-lines",
            daemonService = daemonService,
            sourceJumpResolver = noopJumpResolver,
            initialLogPageSize = 32,
            publishEdt = { it() },
        )
        try {
            awaitCondition(msg = "adaptive halving must recover all 40 oversized log rows") {
                editor.studioController.viewerState?.mainLogs?.size == 40
            }
            val loaded = assertNotNull(editor.studioController.viewerState).mainLogs
            assertEquals((1L..40L).toList(), loaded.map { it.seq })
            assertTrue(requestedLimits.any { it == 32 }, "initial request tried pageSize 32")
            assertTrue(requestedLimits.any { it <= 17 }, "adaptively halved page size after frame-cap error: $requestedLimits")

            legacy.set(true)
            val partial = editor.queryLogsPaged("huge-lines", io.github.shinma06.arb.model.QueryBucket.MAIN, maxLogs = 40)
            assertTrue(partial.size in 1..39)
            assertEquals((1L..partial.size.toLong()).toList(), partial.map { it.seq })
            assertTrue(editor.studioController.timelineWarnings.any { it.contains("旧デーモン") })
        } finally {
            editor.dispose()
            rpc.close()
            daemonService.dispose()
        }
    }

    @Test
    fun pagedLogQueryIsGapFreeWhenSeqOrderOpposesTimestampOrder() {
        // Worst case for a timestamp cursor: seq ascending while timestamps descend, plus a tie
        // group larger than one page. Every row must still arrive exactly once.
        val rows = (1L..300L).map { seq ->
            val ts = if (seq in 100L..140L) 5_000L else (301L - seq) * 10L
            SyntheticLog(seq, ts, "m$seq")
        }
        val legacy = AtomicBoolean(false)
        val cursors = CopyOnWriteArrayList<Long>()
        val transport = StubTransport { method, params ->
            when (method) {
                "timeline.query_logs" -> {
                    params.get("after_seq")?.asLong?.let(cursors::add)
                    val effective = params.deepCopy().apply { if (legacy.get()) remove("after_seq") }
                    val matched = fakeArbdLogQuery(rows, effective, params.get("limit").asInt)
                    val arr = JsonArray()
                    for (row in matched) {
                        arr.add(
                            JsonObject().apply {
                                addProperty("seq", row.seq)
                                addProperty("pc_ingest_cont_ns", row.tsNs)
                                add(
                                    "alignment",
                                    JsonObject().apply {
                                        addProperty("certainty", "anchored")
                                        addProperty("boot_id", "b")
                                        addProperty("boottime_ns", row.tsNs)
                                        addProperty("half_width_ns", 1L)
                                        addProperty("semantics", "logcat_device_read_boottime")
                                    },
                                )
                                addProperty("bucket", "main")
                                addProperty("message", row.msg)
                            },
                        )
                    }
                    JsonObject().apply { add("logs", arr) }.toString()
                }
                // Fail bundle.open so the editor's own background load stays out of the way.
                else -> throw IllegalStateException("unexpected $method")
            }
        }
        val rpc = ArbdRpcClient(transport)
        val daemonService = object : ArbdDaemonService() {
            override fun <T> withClient(block: (ArbdRpcClient) -> T): T {
                return block(rpc)
            }
        }
        val editor = ArbBundleFileEditor(
            project = null,
            backingFile = ArbBundleVirtualFile("reversed", "reversed"),
            bundleIdOrPath = "reversed",
            displayTitle = "reversed",
            daemonService = daemonService,
            sourceJumpResolver = noopJumpResolver,
            initialLogPageSize = 16,
            publishEdt = { },
        )
        try {
            val loaded = editor.queryLogsPaged("reversed", io.github.shinma06.arb.model.QueryBucket.MAIN, maxLogs = 5_000)
            assertEquals((1L..300L).toList(), loaded.map { it.seq }, "no gaps or duplicates")
            assertEquals((16L until 300L step 16).toList(), cursors.toList())

            legacy.set(true)
            assertEquals((1L..300L).toList(), editor.queryLogsPaged("reversed", io.github.shinma06.arb.model.QueryBucket.MAIN).map { it.seq })
            legacy.set(false)

            // Cap: keep the same sequence prefix as a single daemon query.
            val capped = editor.queryLogsPaged("reversed", io.github.shinma06.arb.model.QueryBucket.MAIN, maxLogs = 50)
            assertEquals(50, capped.size)
            assertEquals(capped.map { it.seq }.sorted(), capped.map { it.seq })
            val expectedPrefix = (1L..50L).toList()
            assertEquals(expectedPrefix, capped.map { it.seq })
        } finally {
            editor.dispose()
            rpc.close()
            daemonService.dispose()
        }
    }

    @Test
    fun alwaysOnCaptureArmingWorksWithZeroDevicesAndDistinguishesAllThreeStates() {
        val captureEnabledOnDaemon = AtomicBoolean(false)
        val transport = StubTransport { method, params ->
            when (method) {
                "system.status" -> """{"pid":10,"uptime_ms":100,"capture":{"active":false,"connected":false,"storage_pressure_stopped":false,"video_frames":0,"audio_frames":0,"log_records":0,"tap_records":0,"discontinuities":0,"static_hold_events":0},"ring_chunk_count":0,"ring_total_bytes":0,"ring_pinned_chunk_count":0,"buffered_secs":0,"active_viewports":0,"active_save_jobs":0,"recovery":{},"capture_enabled":${captureEnabledOnDaemon.get()}}"""
                "config.get" -> """{"config":{"config_version":1,"save_window_secs":300,"ring_budget_bytes":1610612736,"min_free_disk_bytes":3221225472,"video_max_size":1920,"video_max_fps":30,"audio_enabled":true,"capture_mode":"video_and_logs","crash_autosave_enabled":true,"crash_autosave_cooldown_secs":15,"ide_default_save_action":"save_and_open","capture_enabled":${captureEnabledOnDaemon.get()}},"config_path":"/tmp/config.json"}"""
                "config.update" -> {
                    params.get("capture_enabled")?.let { captureEnabledOnDaemon.set(it.asBoolean) }
                    """{"config":{"config_version":1,"save_window_secs":300,"ring_budget_bytes":1610612736,"min_free_disk_bytes":3221225472,"video_max_size":1920,"video_max_fps":30,"audio_enabled":true,"capture_mode":"video_and_logs","crash_autosave_enabled":true,"crash_autosave_cooldown_secs":15,"ide_default_save_action":"save_and_open","capture_enabled":${captureEnabledOnDaemon.get()}},"config_path":"/tmp/config.json"}"""
                }
                "device.list" -> """{"devices":[]}"""
                "save.list_jobs" -> """{"jobs":[]}"""
                "bundle.list" -> """{"bundles":[]}"""
                else -> "{}"
            }
        }
        val rpc = ArbdRpcClient(transport)
        val service = object : ArbdDaemonService() {
            override fun <T> withClient(block: (ArbdRpcClient) -> T): T = block(rpc)
        }
        val controller = ArbToolWindowController(
            project = null,
            daemonService = service,
            onOpenBundle = { _, _, _ -> },
            pollIntervalMs = 0L,
            screenshotDirResolver = { null },
        )
        try {
            // 1. 0 devices + OFF -> startStopEnabled MUST still be true so user can arm capture!
            controller.refreshNow()
            awaitCondition(msg = "controller should connect with 0 devices") {
                SwingUtilities.invokeAndWait {}
                controller.captureState.daemonConnected
            }
            assertEquals(CaptureToggleState.OFF, controller.captureToggleState)
            assertEquals("OFF", controller.captureStatusBadgeText)
            assertNull(controller.armedWaitingNotice)
            assertTrue(controller.startStopEnabled, "must allow toggling Capture ON even with 0 devices")
            assertFalse(controller.screenshotEnabled, "screenshot still requires an online device")

            // 2. Toggle ON with 0 devices -> persists capture_enabled=true and enters ON・待機中!
            controller.toggleCapture()
            awaitCondition(msg = "controller should transition to ARMED_WAITING") {
                SwingUtilities.invokeAndWait {}
                controller.captureToggleState == CaptureToggleState.ARMED_WAITING
            }
            assertTrue(captureEnabledOnDaemon.get())
            assertEquals("ON・待機中", controller.captureStatusBadgeText)
            assertEquals("ON・待機中（端末接続で自動開始）", controller.armedWaitingNotice)
            assertTrue(controller.isCaptureOn)
            assertEquals(ArbFormatters.SECONDARY_STOP_CAPTURE_LABEL, controller.startStopLabel)

            // 3. Running Devices toolbar presentation for all 3 states + disconnect persistence!
            val offSnap = ArbToolbarDaemonSnapshot(
                probed = true,
                daemonConnected = true,
                captureActive = false,
                captureEnabled = false,
                hasOnlineDevice = false,
            )
            val armedSnap = offSnap.copy(captureEnabled = true)
            val recSnap = armedSnap.copy(captureActive = true, hasOnlineDevice = true, selectedDeviceSerial = "emu-1")
            val disconnectedStillArmed = recSnap.copy(captureActive = false, hasOnlineDevice = false, selectedDeviceSerial = null)

            val presOff = ArbToolbarPresentationState.toggleCapture(offSnap)
            val presArmed = ArbToolbarPresentationState.toggleCapture(armedSnap)
            val presRec = ArbToolbarPresentationState.toggleCapture(recSnap)
            val presReconnectWait = ArbToolbarPresentationState.toggleCapture(disconnectedStillArmed)

            assertTrue(presOff.enabled)
            assertFalse(presOff.selected)
            assertTrue(presOff.text.contains("OFF"))
            assertEquals(AllIcons.Actions.Restart, presOff.icon)

            assertTrue(presArmed.enabled)
            assertTrue(presArmed.selected, "DumbAwareToggleAction must stay selected while armed")
            assertTrue(presArmed.description.contains("ON・待機中（端末接続で自動開始）"))
            assertEquals(AllIcons.Actions.Suspend, presArmed.icon)

            assertTrue(presRec.enabled)
            assertTrue(presRec.selected)
            assertTrue(presRec.text.contains("ON・REC"))
            assertEquals(AllIcons.Actions.Suspend, presRec.icon)

            assertTrue(presReconnectWait.selected, "turning ON once must stay visibly ON across disconnects")
            assertTrue(presReconnectWait.description.contains("ON・待機中（端末接続で自動開始）"))

            // Also verify CaptureBarState.toToolbarSnapshot carries captureEnabled cleanly.
            val recordingState = CaptureBarReducer.fromStatus(
                CaptureBarState(),
                SystemStatusResponse(
                    pid = 1L,
                    uptimeMs = 1L,
                    capture = CaptureStatusView(
                        active = true,
                        connected = true,
                        serial = "emu-1",
                        storagePressureStopped = false,
                        captureMode = CaptureMode.VIDEO_AND_LOGS,
                        videoFrames = 10L,
                        audioFrames = 0L,
                        logRecords = 0L,
                        tapRecords = 0L,
                        discontinuities = 0L,
                        staticHoldEvents = 0L,
                    ),
                    ringChunkCount = 1L,
                    ringTotalBytes = 1024L,
                    ringPinnedChunkCount = 0L,
                    activeViewports = 0L,
                    activeSaveJobs = 0L,
                    recovery = RecoverySummaryView(),
                    captureEnabled = true,
                ),
                devices = listOf(AdbDeviceInfo("emu-1", AdbDeviceState.Device)),
                config = ArbConfig(captureEnabled = true),
            )
            controller.applyStateForTest(capture = recordingState, captureEnabled = true)
            assertEquals(CaptureToggleState.RECORDING, controller.captureToggleState)
            assertEquals("ON・REC", controller.captureStatusBadgeText)
            assertNull(controller.armedWaitingNotice)
            assertTrue(recordingState.toToolbarSnapshot(captureEnabled = true).captureEnabled)

            val disconnected = recordingState.copy(capture = recordingState.capture!!.copy(connected = false))
            controller.applyStateForTest(capture = disconnected)
            assertEquals(CaptureToggleState.ARMED_WAITING, controller.captureToggleState)
            val disconnectedAction = ArbToolbarPresentationState.toggleCapture(disconnected.toToolbarSnapshot())
            assertTrue(disconnectedAction.selected)
            assertTrue(disconnectedAction.description.contains("ON・待機中"))

            // 4. Toggle OFF -> sends config.update(capture_enabled = false) and returns to OFF!
            controller.applyStateForTest(
                capture = controller.captureState.copy(capture = null, config = ArbConfig(captureEnabled = true)),
                captureEnabled = true,
            )
            controller.toggleCapture()
            awaitCondition(msg = "toggleCapture from ARMED_WAITING must return to OFF") {
                SwingUtilities.invokeAndWait {}
                !captureEnabledOnDaemon.get() && controller.captureToggleState == CaptureToggleState.OFF
            }
            assertEquals("OFF", controller.captureStatusBadgeText)

            for (text in listOf(
                presOff.text,
                presOff.description,
                presArmed.text,
                presArmed.description,
                presRec.text,
                presRec.description,
                controller.captureStatusBadgeText,
                controller.startStopTooltip,
            )) {
                assertFalse(ArbFormatters.containsEmoji(text), "zero emoji allowed: $text")
            }
        } finally {
            controller.dispose()
            rpc.close()
            service.dispose()
        }
    }

    // ---------------- Item 8: OS Menu Bar / System Tray Settings Toggle & Lifecycle ----------------

    @Test
    fun trayEnabledDefaultsFalseRoundTripsInSettingsAndControlsManagedTrayProcessLifecycle(@TempDir tmp: Path) {
        // 1. ArbLocalSettingsState default is false and round-trips through State.
        val local = ArbLocalSettingsState()
        assertFalse(local.trayEnabled, "trayEnabled must default to false")
        local.trayEnabled = true
        val saved = local.getState()
        assertTrue(saved.trayEnabled)
        val restored = ArbLocalSettingsState()
        restored.loadState(saved)
        assertTrue(restored.trayEnabled)

        // 2. ArbSettingsConfigurable isModified / apply / reset round-trip.
        var storedTray = false
        val appliedTrayValues = mutableListOf<Boolean>()
        val configurable = ArbSettingsConfigurable(
            project = null,
            daemonConfigReader = { ArbConfig() },
            daemonConfigWriter = { base, _ -> base },
            localBinaryReader = { "" },
            localBinaryWriter = {},
            localTrayReader = { storedTray },
            localTrayWriter = { enabled ->
                storedTray = enabled
                appliedTrayValues += enabled
            },
        )
        val form = configurable.createFormModel()
        assertFalse(form.trayEnabled)
        assertFalse(configurable.isModified)

        form.trayEnabled = true
        assertTrue(configurable.isModified, "toggling trayEnabled must mark Settings modified")
        configurable.apply()
        assertFalse(configurable.isModified)
        assertTrue(storedTray)
        assertEquals(listOf(true), appliedTrayValues)

        form.trayEnabled = false
        assertTrue(configurable.isModified)
        configurable.apply()
        assertFalse(storedTray)
        assertEquals(listOf(true, false), appliedTrayValues)
        configurable.disposeUIResources()

        // 3. ArbdDaemonService managed arb-tray lifecycle: syncTrayState(true -> true -> false) + dispose().
        val fakeTrayBin = Files.writeString(tmp.resolve("arb-tray"), "#!/bin/sh\n")
        val spawnedProcs = mutableListOf<FakeManagedProcess>()
        val service = ArbdDaemonService(
            processManager = ArbdProcessManager(paths = ArbPaths.resolve(os = HostOs.detect())),
            trayBinaryResolver = { fakeTrayBin },
            trayProcessStarter = { _, _ -> FakeManagedProcess().also { spawnedProcs += it } },
        )
        try {
            assertFalse(service.isManagedTrayRunningForTest())
            service.syncTrayState(true)
            assertEquals(1, spawnedProcs.size)
            assertTrue(service.isManagedTrayRunningForTest())

            // Idempotent when already running.
            service.syncTrayState(true)
            assertEquals(1, spawnedProcs.size, "second syncTrayState(true) must not spawn duplicate tray")

            // Toggling OFF terminates ONLY managed arb-tray.
            service.syncTrayState(false)
            assertFalse(service.isManagedTrayRunningForTest())
            assertTrue(spawnedProcs[0].destroyed.get())
            assertFalse(spawnedProcs[0].isAlive)

            // Re-enabling launches a fresh managed tray, and dispose() cleans it up on IDE shutdown.
            service.syncTrayState(true)
            assertEquals(2, spawnedProcs.size)
            assertTrue(spawnedProcs[1].isAlive)
        } finally {
            service.dispose()
        }
        assertFalse(service.isManagedTrayRunningForTest())
        assertTrue(spawnedProcs[1].destroyed.get(), "IDE shutdown dispose() must stop managed arb-tray")
        assertFalse(spawnedProcs[1].isAlive)
    }

    @Test
    fun quickTrayOffThenOnWaitsForOldTrayToExitAndEndsRunning(@TempDir tmp: Path) {
        // The old arb-tray takes a while to exit after destroy(); while it is alive it holds the
        // single-instance lock, so a new arb-tray started meanwhile would exit immediately.
        val fakeTrayBin = Files.writeString(tmp.resolve("arb-tray"), "#!/bin/sh\n")
        val oldTrayAlive = AtomicBoolean(true)
        val startedWhileOldAlive = AtomicBoolean(false)
        val spawned = CopyOnWriteArrayList<Process>()
        val slowExitTray = object : Process() {
            override fun getOutputStream(): OutputStream = ByteArrayOutputStream()
            override fun getInputStream(): InputStream = ByteArrayInputStream(ByteArray(0))
            override fun getErrorStream(): InputStream = ByteArrayInputStream(ByteArray(0))
            override fun waitFor(): Int = 0
            override fun waitFor(timeout: Long, unit: TimeUnit): Boolean {
                Thread.sleep(200L) // exits 200 ms after being asked to
                oldTrayAlive.set(false)
                return true
            }
            override fun exitValue(): Int = 0
            override fun destroy() = Unit
            override fun isAlive(): Boolean = oldTrayAlive.get()
        }
        val service = ArbdDaemonService(
            processManager = ArbdProcessManager(paths = ArbPaths.resolve(os = HostOs.detect())),
            trayBinaryResolver = { fakeTrayBin },
            trayProcessStarter = { _, _ ->
                val proc: Process = if (spawned.isEmpty()) {
                    slowExitTray
                } else {
                    if (oldTrayAlive.get()) startedWhileOldAlive.set(true)
                    FakeManagedProcess()
                }
                spawned += proc
                proc
            },
        )
        try {
            service.syncTrayState(true)
            // Settings applied OFF and then ON again in quick succession.
            service.syncTrayStateAsync(false)
            val on = service.syncTrayStateAsync(true)
            assertNotNull(on).get(5, TimeUnit.SECONDS)
            assertEquals(2, spawned.size)
            assertFalse(startedWhileOldAlive.get(), "new arb-tray must not start before the old one exited")
            assertTrue(service.isManagedTrayRunningForTest(), "tray must be running after OFF -> ON")
        } finally {
            service.dispose()
        }
    }

    @Test
    fun turningTrayOffDuringProjectStartupKeepsItOff(@TempDir tmp: Path) {
        val service = ArbdDaemonService(
            processManager = ArbdProcessManager(paths = ArbPaths.resolve()),
            trayBinaryResolver = { tmp.resolve("arb-tray") },
            trayProcessStarter = { _, _ -> FakeManagedProcess() },
        )
        val preferenceRead = CountDownLatch(1)
        val resumeStartup = CountDownLatch(1)
        val startup = Thread {
            service.restoreTrayAtStartup {
                preferenceRead.countDown()
                resumeStartup.await(5, TimeUnit.SECONDS)
                true
            }
        }
        try {
            startup.start()
            assertTrue(preferenceRead.await(5, TimeUnit.SECONDS))
            val off = assertNotNull(service.syncTrayStateAsync(false))
            try {
                off.get(200, TimeUnit.MILLISECONDS)
            } catch (_: java.util.concurrent.TimeoutException) {
                // A serialized OFF remains queued until the startup read completes.
            }
            resumeStartup.countDown()
            startup.join(5_000)
            off.get(5, TimeUnit.SECONDS)
            assertFalse(service.isManagedTrayRunningForTest())
        } finally {
            resumeStartup.countDown()
            service.dispose()
        }
    }

    @Test
    fun captureConfigErrorsReachTheCaller() {
        val service = object : ArbdDaemonService() {
            override fun <T> withClient(block: (ArbdRpcClient) -> T): T =
                throw ArbdRpcException(-32603, "config write failed", "config.update")
        }
        try {
            assertFailsWith<ArbdRpcException> {
                persistCaptureEnabled(service, false)
            }
        } finally {
            service.dispose()
        }
    }

    // ---------------- Item 7 fallback: arbd without always-on capture ----------------

    @Test
    fun toggleFallsBackToDirectCaptureStartWhenDaemonRejectsCaptureEnabled() {
        val captureActive = AtomicBoolean(false)
        lateinit var service: ArbdDaemonService
        val rejectedStartCachedActive = LinkedBlockingQueue<Boolean>()
        val startedSerials = CopyOnWriteArrayList<String>()
        val transport = StubTransport { method, params ->
            when (method) {
                "system.status" -> {
                    if (startedSerials.size == 1) rejectedStartCachedActive.offer(service.cachedCaptureActive)
                    """{"pid":10,"uptime_ms":100,"capture":{"active":${captureActive.get()},"connected":${captureActive.get()},"serial":"emu-1","storage_pressure_stopped":false,"video_frames":0,"audio_frames":0,"log_records":0,"tap_records":0,"discontinuities":0,"static_hold_events":0},"ring_chunk_count":0,"ring_total_bytes":0,"ring_pinned_chunk_count":0,"buffered_secs":0,"active_viewports":0,"active_save_jobs":0,"recovery":{}}"""
                }
                "config.get" -> """{"config":{"config_version":1,"save_window_secs":300,"ring_budget_bytes":1610612736,"min_free_disk_bytes":3221225472,"video_max_size":1920,"video_max_fps":30,"audio_enabled":true,"capture_mode":"video_and_logs","crash_autosave_enabled":true,"crash_autosave_cooldown_secs":15,"ide_default_save_action":"save_and_open"},"config_path":"/tmp/config.json"}"""
                // Pre-T-7A arbd: ArbConfigPatch has deny_unknown_fields.
                "config.update" -> "${RPC_ERROR_PREFIX}unknown field `capture_enabled`"
                "device.list" -> """{"devices":[{"serial":"emu-1","state":"device"}]}"""
                "capture.start" -> {
                    startedSerials += params.get("serial").asString
                    captureActive.set(startedSerials.size > 1)
                    """{"started":${captureActive.get()},"serial":"emu-1"}"""
                }
                "save.list_jobs" -> """{"jobs":[]}"""
                "bundle.list" -> """{"bundles":[]}"""
                else -> "{}"
            }
        }
        val rpc = ArbdRpcClient(transport)
        service = object : ArbdDaemonService() {
            override fun <T> withClient(block: (ArbdRpcClient) -> T): T = block(rpc)
        }
        val controller = ArbToolWindowController(
            project = null,
            daemonService = service,
            onOpenBundle = { _, _, _ -> },
            pollIntervalMs = 0L,
            screenshotDirResolver = { null },
        )
        try {
            controller.refreshNow()
            awaitCondition(msg = "controller should see the single online device") {
                SwingUtilities.invokeAndWait {}
                controller.captureState.daemonConnected && controller.selectedDeviceSerial == "emu-1"
            }
            assertEquals(CaptureToggleState.OFF, controller.captureToggleState)

            controller.toggleCapture()
            assertEquals(false, rejectedStartCachedActive.poll(5, TimeUnit.SECONDS))
            controller.toggleCapture()
            awaitCondition(msg = "toggle must still start capture on an older arbd") {
                SwingUtilities.invokeAndWait {}
                controller.captureToggleState == CaptureToggleState.RECORDING
            }
            assertEquals(listOf("emu-1", "emu-1"), startedSerials.toList())
            assertFalse(service.cachedCaptureEnabled, "older arbd cannot be armed; only the live capture is ON")
        } finally {
            controller.dispose()
            rpc.close()
            service.dispose()
        }
    }

    private companion object {
        const val RPC_ERROR_PREFIX: String = "RPC_ERROR:"
    }
}
