package io.github.shinma06.arb.model

import io.github.shinma06.arb.ipc.ArbdRpcClient
import io.github.shinma06.arb.ipc.LocalIpcTransport
import io.github.shinma06.arb.ipc.NdjsonFraming
import org.junit.jupiter.api.Test
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.IOException
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertIs
import kotlin.test.assertTrue

class ProtocolJsonTest {
    private val gson = ProtocolJson.gson

    @Test
    fun provenanceTaggedEnumMatchesRustSerde() {
        val originalJson = """{"kind":"original"}"""
        val parsedOrig = gson.fromJson(originalJson, Provenance::class.java)
        assertEquals(Provenance.Original, parsedOrig)
        assertEquals(originalJson, gson.toJson(parsedOrig))

        val trimmedJson = """{"kind":"trimmed","parent_bundle_id":"b-1","in_frame_index":17,"out_frame_index":49}"""
        val parsedTrim = gson.fromJson(trimmedJson, Provenance::class.java)
        assertEquals(Provenance.Trimmed("b-1", 17L, 49L), parsedTrim)
        assertEquals(trimmedJson, gson.toJson(parsedTrim))
    }

    @Test
    fun timeAlignmentTaggedEnumMatchesRustSerde() {
        val anchoredJson = """{"certainty":"anchored","boot_id":"boot-1","boottime_ns":1000,"half_width_ns":50,"semantics":"video_encoder_input_estimate"}"""
        val anchored = gson.fromJson(anchoredJson, TimeAlignment::class.java)
        assertIs<TimeAlignment.Anchored>(anchored)
        assertEquals(TimestampSemantics.VIDEO_ENCODER_INPUT_ESTIMATE, anchored.semantics)
        assertEquals(anchoredJson, gson.toJson(anchored))

        val boundedJson = """{"certainty":"bounded","boot_id":"boot-1","min_boottime_ns":100,"max_boottime_ns":200,"reason":"device_sleep_spanned"}"""
        val bounded = gson.fromJson(boundedJson, TimeAlignment::class.java)
        assertIs<TimeAlignment.Bounded>(bounded)
        assertEquals(UncertaintyReason.DEVICE_SLEEP_SPANNED, bounded.reason)

        val unanchoredJson = """{"certainty":"unanchored","pc_ingest_cont_ns":999,"reason":"legacy_sentinel_unknown"}"""
        val unanchored = gson.fromJson(unanchoredJson, TimeAlignment::class.java)
        assertIs<TimeAlignment.Unanchored>(unanchored)
        assertEquals(UncertaintyReason.LEGACY_SENTINEL_UNKNOWN, unanchored.reason)
    }

    @Test
    fun adbDeviceStateSupportsKnownAndExternallyTaggedUnknown() {
        assertEquals(AdbDeviceState.Device, gson.fromJson(""""device"""", AdbDeviceState::class.java))
        assertEquals(AdbDeviceState.Unauthorized, gson.fromJson(""""unauthorized"""", AdbDeviceState::class.java))
        assertEquals(
            AdbDeviceState.Unknown("sideload"),
            gson.fromJson("""{"unknown":"sideload"}""", AdbDeviceState::class.java),
        )
    }

    @Test
    fun rpcClientHandlesSuccessErrorAndProtocolVersionMismatch() {
        val responses = ArrayDeque(
            listOf(
                """{"jsonrpc":"2.0","id":1,"result":{"protocol_version":1,"schema_version":2,"pid":42,"data_dir":"/d","cache_dir":"/c","runtime_dir":"/r","daemon_version":"0.1.0"}}""",
                """{"jsonrpc":"2.0","id":2,"error":{"code":-32001,"message":"bundle not found"}}""",
                """{"jsonrpc":"2.0","id":3,"result":{"protocol_version":99,"schema_version":2,"pid":42,"data_dir":"/d","cache_dir":"/c","runtime_dir":"/r","daemon_version":"0.9.0"}}""",
            ),
        )
        val written = mutableListOf<String>()
        val transport = object : LocalIpcTransport {
            override val endpoint: String = "mock://ipc"
            override fun writeLine(jsonLine: String) {
                written.add(jsonLine)
            }
            override fun readLine(): String? = responses.removeFirstOrNull()
            override fun close() = Unit
        }

        ArbdRpcClient(transport).use { client ->
            val hello = client.systemHello()
            assertEquals(1, hello.protocolVersion)
            assertEquals(42L, hello.pid)
            assertTrue(written[0].contains(""""method":"system.hello""""))

            val err = assertFailsWith<ArbdRpcException> {
                client.timelineSummary("missing-bundle")
            }
            assertEquals(-32001, err.code)
            assertEquals("bundle not found", err.message)

            assertFailsWith<IOException> {
                client.systemHello(verifyProtocolVersion = true)
            }
        }
    }

    @Test
    fun ndjsonFramingRejectsEmbeddedNewlinesAndOversizedLines() {
        val out = ByteArrayOutputStream()
        assertFailsWith<IllegalArgumentException> {
            NdjsonFraming.writeFrameLine(out, """{"a":"\n"}""".replace("\\n", "\n"), 1024)
        }
        assertFailsWith<IOException> {
            NdjsonFraming.writeFrameLine(out, "x".repeat(32), 16)
        }

        val input = ByteArrayInputStream("0123456789abcdefX\n{\"ok\":true}\n".toByteArray())
        assertFailsWith<IOException> {
            NdjsonFraming.readFrameLine(input, 16)
        }
        // Subsequent frame remains cleanly aligned after draining oversized line.
        assertEquals("""{"ok":true}""", NdjsonFraming.readFrameLine(input, 64))
    }

    @Test
    fun captureStatusViewParsesAudioUnavailableAndOlderJsonWithoutAudioState() {
        val aos9Json = """
            {
              "active": true,
              "connected": true,
              "capture_mode": "video_and_logs",
              "serial": "aos9-work",
              "boot_id": "fallback:bc=3:btime=1700000000",
              "storage_pressure_stopped": false,
              "video_frames": 90,
              "audio_frames": 0,
              "log_records": 42,
              "tap_records": 3,
              "discontinuities": 0,
              "static_hold_events": 0,
              "ring_write_errors": 0,
              "audio_state": "unavailable",
              "audio_reason": "Audio capture requires Android 13+ (API 33+, non-muting playback capture); detected API 28. Continuing with video + logs + taps."
            }
        """.trimIndent()
        val parsed = gson.fromJson(aos9Json, CaptureStatusView::class.java)
        assertEquals(AudioCaptureState.UNAVAILABLE, parsed.audioState)
        assertEquals(AudioCaptureState.UNAVAILABLE, parsed.effectiveAudioState)
        assertTrue(parsed.audioReason!!.contains("API 28"))

        val legacyJson = """
            {
              "active": false,
              "connected": false,
              "storage_pressure_stopped": false,
              "video_frames": 0,
              "audio_frames": 0,
              "log_records": 0,
              "tap_records": 0,
              "discontinuities": 0,
              "static_hold_events": 0
            }
        """.trimIndent()
        val legacyParsed = gson.fromJson(legacyJson, CaptureStatusView::class.java)
        assertEquals(null, legacyParsed.audioState)
        assertEquals(AudioCaptureState.UNKNOWN, legacyParsed.effectiveAudioState)
        assertEquals(null, legacyParsed.audioReason)
    }
}
