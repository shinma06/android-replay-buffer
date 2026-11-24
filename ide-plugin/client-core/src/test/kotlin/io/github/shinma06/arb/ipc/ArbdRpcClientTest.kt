package io.github.shinma06.arb.ipc

import com.google.gson.JsonObject
import com.google.gson.JsonParser
import io.github.shinma06.arb.model.ArbConfigPatch
import io.github.shinma06.arb.model.ExportShareMp4Params
import io.github.shinma06.arb.model.IdeDefaultSaveAction
import io.github.shinma06.arb.model.OutputDirPatch
import io.github.shinma06.arb.model.SaveJobState
import io.github.shinma06.arb.model.SaveLogsParams
import io.github.shinma06.arb.model.SaveScreenshotParams
import java.io.IOException
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

/** In-memory [LocalIpcTransport] that replays scripted response lines. */
private class FakeTransport(
    responses: List<String?>,
    private val hangOnRead: Boolean = false,
) : LocalIpcTransport {
    override val endpoint: String = "fake://arb-test"
    private val queue = ArrayDeque(responses)
    val written = mutableListOf<String>()
    var closed = false
        private set
    val readStarted = CountDownLatch(1)

    override fun writeLine(jsonLine: String) {
        written.add(jsonLine)
    }

    override fun readLine(): String? {
        readStarted.countDown()
        if (hangOnRead) {
            // A hung daemon: never respond so the bounded read timeout must fire.
            Thread.sleep(60_000)
            return null
        }
        return queue.removeFirstOrNull()
    }

    override fun close() {
        closed = true
    }
}

/** R4 S5/C5: strict response-id matching and bounded read timeout on the Kotlin RPC client. */
class ArbdRpcClientTest {
    private fun okResponse(id: Long, result: String = "{}"): String =
        """{"jsonrpc":"2.0","id":$id,"result":$result}"""

    @Test
    fun `response with mismatched id is rejected`() {
        val transport = FakeTransport(listOf(okResponse(999)))
        val client = ArbdRpcClient(transport)
        val err = assertFailsWith<IOException> {
            client.callRaw("system.status", JsonObject())
        }
        assertTrue(
            err.message.orEmpty().contains("id mismatch"),
            "expected id-mismatch error, got: ${err.message}",
        )
    }

    @Test
    fun `response with missing id is rejected`() {
        val transport = FakeTransport(listOf("""{"jsonrpc":"2.0","result":{}}"""))
        val client = ArbdRpcClient(transport)
        assertFailsWith<IOException> {
            client.callRaw("system.status", JsonObject())
        }
    }

    @Test
    fun `matching id returns parsed result`() {
        val transport = FakeTransport(listOf(okResponse(1, """{"ok":true}""")))
        val client = ArbdRpcClient(transport)
        val result = client.callRaw("system.status", JsonObject())
        assertTrue(result.isJsonObject)
        assertEquals(true, result.asJsonObject.get("ok").asBoolean)
        assertEquals(1, transport.written.size)
    }

    @Test
    fun `read timeout closes transport and raises io error`() {
        val transport = FakeTransport(emptyList(), hangOnRead = true)
        val client = ArbdRpcClient(transport, readTimeoutMs = 100)
        val err = assertFailsWith<IOException> {
            client.callRaw("system.status", JsonObject())
        }
        assertTrue(
            err.message.orEmpty().contains("timed out"),
            "expected timeout error, got: ${err.message}",
        )
        assertTrue(transport.readStarted.await(2, TimeUnit.SECONDS))
        // The transport must be closed on timeout so a late response can never desync
        // the framing of the next call (R4 S5/C5).
        assertTrue(transport.closed, "transport must be closed after read timeout")
        client.close()
    }

    @Test
    fun `long running export and audio decode RPCs use generous per-call timeout`() {
        assertEquals(120_000L, LONG_RUNNING_READ_TIMEOUT_MS)
        // Slow transport takes 180ms: exceeds short readTimeoutMs (60ms) but succeeds under
        // longRunningReadTimeoutMs (1000ms) for export.share_mp4 / export.trim / media.decode_audio_pcm!
        val slowTransport = object : LocalIpcTransport {
            override val endpoint: String = "fake://slow"
            private val responses = ArrayDeque(
                listOf(
                    okResponse(
                        1,
                        """{"share_mp4_path":"/tmp/share.mp4","audio_included":true,"video_frame_count":10,"edts_box_count":0,"elst_box_count":0,"output_bytes":1234,"original_trashed":false}""",
                    ),
                    okResponse(
                        2,
                        """{"available":true,"wav_path":"/tmp/a.wav","sample_rate_hz":48000,"channels":2,"duration_ns":1000000000,"av_offset_ms":0,"audio_lead_in_offset_ns":0,"cached":false}""",
                    ),
                    okResponse(3, """{"ok":true}"""),
                ),
            )
            private val closed = CountDownLatch(1)
            override fun writeLine(jsonLine: String) = Unit
            override fun readLine(): String? {
                if (responses.size == 1) {
                    // The normal RPC's reply arrives only after the client gives up and closes,
                    // so a stalled runner cannot let it slip in before the 60ms deadline.
                    closed.await(10, TimeUnit.SECONDS)
                } else {
                    Thread.sleep(180)
                }
                return responses.removeFirstOrNull()
            }
            override fun close() = closed.countDown()
        }
        val client = ArbdRpcClient(
            transport = slowTransport,
            readTimeoutMs = 60L,
            longRunningReadTimeoutMs = 1_000L,
        )
        try {
            val share = client.exportShareMp4(ExportShareMp4Params("b-1"))
            assertEquals("/tmp/share.mp4", share.shareMp4Path)

            val pcm = client.mediaDecodeAudioPcm("b-1")
            assertTrue(pcm.available)

            // Normal RPC still enforces the short default readTimeoutMs (60ms) and times out cleanly!
            val err = assertFailsWith<IOException> {
                client.callRaw("system.status", JsonObject())
            }
            assertTrue(err.message.orEmpty().contains("timed out"))
        } finally {
            client.close()
        }
    }

    @Test
    fun `t1 rpc methods and t2b graceful fallbacks serialize and deserialize accurately`() {
        val responses = listOf(
            okResponse(
                1,
                """{"config":{"config_version":1,"save_window_secs":180,"ring_budget_bytes":1610612736,"min_free_disk_bytes":3221225472,"video_max_size":1920,"video_max_fps":30,"audio_enabled":true,"crash_autosave_enabled":true,"crash_autosave_cooldown_secs":15,"output_dir_override":null,"ide_default_save_action":"save_only"},"config_path":"/tmp/config.json"}""",
            ),
            okResponse(
                2,
                """{"path":"/tmp/shot.png","width":1080,"height":2400,"sha256":"abc123","captured_at_unix_ms":1730000000000,"serial":"pixel-9"}""",
            ),
            okResponse(
                3,
                """{"job":{"job_id":"job-log-1","state":"completed","trigger":"manual","destination_path":"/tmp/logs","pinned_chunk_ids":[],"created_at_utc":"2026-11-23T00:00:00Z","updated_at_utc":"2026-11-23T00:00:01Z"},"bundle_id":"logs-bundle-1","bundle_path":"/tmp/logs-bundle-1","tail_may_be_incomplete":false}""",
            ),
            okResponse(
                4,
                """{"crashes":[{"boottime_ns":10500000000,"frame_index_hint":15,"process_name":"com.example.app","package_name":"com.example.app","pid":42,"exception_class":"java.lang.NullPointerException","message":"boom","source_log_seq":77}]}""",
            ),
            okResponse(
                5,
                """{"screens":[{"start_ns":10000000000,"end_ns":12000000000,"start_frame_index":0,"end_frame_index":60,"package_name":"com.example.app","activity_name":"com.example.app.MainActivity","short_name":"MainActivity"}]}""",
            ),
            """{"jsonrpc":"2.0","id":6,"error":{"code":-32601,"message":"Method not found: export.share_mp4"}}""",
            """{"jsonrpc":"2.0","id":7,"error":{"code":-32601,"message":"Method not found: media.decode_audio_pcm"}}""",
        )

        val transport = FakeTransport(responses)
        val client = ArbdRpcClient(transport)
        try {
            val cfgResp = client.configUpdate(
                ArbConfigPatch(
                    saveWindowSecs = 180,
                    ideDefaultSaveAction = IdeDefaultSaveAction.SAVE_ONLY,
                    outputDirOverride = OutputDirPatch.Clear,
                ),
            )
            assertEquals(180, cfgResp.config.saveWindowSecs)
            assertEquals(IdeDefaultSaveAction.SAVE_ONLY, cfgResp.config.ideDefaultSaveAction)
            val sentPatch = JsonParser.parseString(transport.written[0]).asJsonObject.getAsJsonObject("params")
            assertTrue(sentPatch.has("output_dir_override"))
            assertTrue(sentPatch.get("output_dir_override").isJsonNull)
            assertFalse(sentPatch.has("video_max_fps"), "omitted patch fields must not be serialized")

            val shot = client.saveScreenshot(SaveScreenshotParams(serial = "pixel-9"))
            assertEquals("/tmp/shot.png", shot.path)
            assertEquals(1080, shot.width)
            assertEquals(2400, shot.height)

            val logs = client.saveLogs(SaveLogsParams(windowSecs = 300L))
            assertEquals(SaveJobState.COMPLETED, logs.job.state)
            assertEquals("logs-bundle-1", logs.bundleId)

            val crashes = client.timelineCrashes("b-1").crashes
            assertEquals(1, crashes.size)
            assertEquals(15L, crashes[0].frameIndexHint)

            val screens = client.timelineScreens("b-1").screens
            assertEquals(1, screens.size)
            assertEquals("MainActivity", screens[0].shortName)

            // T-2B forward-compatibility: -32601 yields clean null fallback
            assertNull(client.exportShareMp4OrNull(ExportShareMp4Params("b-1", 0L, 10L)))
            assertNull(client.mediaDecodeAudioPcmOrNull("b-1"))
            assertNotNull(transport.written.lastOrNull())
        } finally {
            client.close()
        }
    }
}
