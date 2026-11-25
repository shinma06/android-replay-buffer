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

    @Test
    fun `ExportTrimParams ExportTrimResponse and ExportShareMp4Params round-trip burn_taps fields`() {
        val trimReq = ExportTrimParams(
            bundleIdOrPath = "b-source",
            inFrameIndex = 10,
            outFrameIndex = 90,
            destDir = "/tmp/out",
            holderPid = 42,
            burnTaps = true,
            includeShareMp4 = true,
            trashOriginal = true,
        )
        val trimReqJson = gson.toJson(trimReq)
        assertTrue(trimReqJson.contains("\"burn_taps\":true"))
        assertTrue(trimReqJson.contains("\"include_share_mp4\":true"))
        assertTrue(trimReqJson.contains("\"trash_original\":true"))

        val trimRespJson = """
            {
              "bundle_id": "trim-01",
              "bundle_path": "/tmp/out/trim-01",
              "trim_report": {
                "in_frame_index": 10,
                "out_frame_index": 90,
                "output_frame_count": 81,
                "head_decoded_frames": 10,
                "reencoded_frames": 10,
                "stream_copied_frames": 71,
                "backend_name": "videotoolbox",
                "edts_box_count": 0,
                "elst_box_count": 0
              },
              "unanchored_summary": { "included_count": 0, "truncated_count": 0 },
              "main_log_count": 12,
              "unanchored_log_count": 0,
              "main_tap_count": 3,
              "unanchored_tap_count": 0,
              "share_mp4_path": "/tmp/out/trim-01/share-taps.mp4",
              "app_logs_count": 2,
              "original_trashed": true,
              "burn_taps_applied": true
            }
        """.trimIndent()
        val parsed = gson.fromJson(trimRespJson, ExportTrimResponse::class.java)
        assertEquals("/tmp/out/trim-01/share-taps.mp4", parsed.shareMp4Path)
        assertEquals(2, parsed.appLogsCount)
        assertEquals(true, parsed.originalTrashed)
        assertEquals(true, parsed.burnTapsApplied)

        val shareReqJson = gson.toJson(
            ExportShareMp4Params(bundleIdOrPath = "b-source", burnTaps = true, trashOriginal = false),
        )
        assertTrue(shareReqJson.contains("\"burn_taps\":true"))
    }

    // ---------------- T-7B: VFR frame PTS (H8) & always-on capture wire fields ----------------

    private val viewportBase =
        """"viewport_id":"vp-1","bundle_id":"b-1","shm_path":"/tmp/vp.shm","width":320,"height":240,""" +
            """"stride":1280,"pixel_format":"rgba8","slot_capacity":307200,"frame_count":3,"keyframe_count":1"""

    @Test
    fun mediaOpenViewportParsesFramePtsNsForVariableFrameRate() {
        val json = """{$viewportBase,"frame_pts_ns":[0,5000000000,5033333333]}"""
        val resp = gson.fromJson(json, MediaOpenViewportResponse::class.java)
        assertEquals(listOf(0L, 5_000_000_000L, 5_033_333_333L), resp.framePtsNs)
        assertEquals("vp-1", resp.viewportId)
        assertEquals(307_200, resp.slotCapacity)
        assertEquals(3L, resp.frameCount)
        assertEquals(1, resp.keyframeCount)
    }

    @Test
    fun mediaOpenViewportWithoutFramePtsNsDefaultsToEmptyListForOlderDaemons() {
        val resp = gson.fromJson("""{$viewportBase}""", MediaOpenViewportResponse::class.java)
        // A Gson Unsafe allocation would leave this null despite the non-null Kotlin type.
        val pts: List<Long>? = resp.framePtsNs
        assertEquals(emptyList(), pts)
        val nulled = gson.fromJson("""{$viewportBase,"frame_pts_ns":null}""", MediaOpenViewportResponse::class.java)
        assertEquals(emptyList(), nulled.framePtsNs)
        assertFailsWith<com.google.gson.JsonParseException> {
            gson.fromJson("""{$viewportBase,"frame_pts_ns":["x"]}""", MediaOpenViewportResponse::class.java)
        }
    }

    @Test
    fun mediaOpenViewportParsesTimelineEndAndToleratesOlderDaemons() {
        val json = """{$viewportBase,"frame_pts_ns":[0,10,1000],"last_frame_duration_ns":999,"timeline_duration_ns":1999}"""
        val resp = gson.fromJson(json, MediaOpenViewportResponse::class.java)
        assertEquals(1_999L, resp.timelineDurationNs)
        assertEquals(999L, resp.lastFrameDurationNs)
        assertEquals(null, gson.fromJson("""{$viewportBase}""", MediaOpenViewportResponse::class.java).timelineDurationNs)
        assertEquals(
            null,
            gson.fromJson("""{$viewportBase,"timeline_duration_ns":null}""", MediaOpenViewportResponse::class.java).timelineDurationNs,
        )
    }

    @Test
    fun rejectsMalformedFrameTimestamps() {
        for (value in listOf("[1.5]", "[9223372036854775808]", "42")) {
            assertFailsWith<com.google.gson.JsonParseException> {
                gson.fromJson("""{$viewportBase,"frame_pts_ns":$value}""", MediaOpenViewportResponse::class.java)
            }
        }
    }

    @Test
    fun captureEnabledRoundTripsInConfigPatchAndStatus() {
        val config = gson.fromJson("""{"capture_enabled":true}""", ArbConfig::class.java)
        assertTrue(config.captureEnabled)
        assertEquals(false, gson.fromJson("{}", ArbConfig::class.java).captureEnabled)
        assertTrue(gson.toJson(ArbConfig(captureEnabled = true)).contains("\"capture_enabled\":true"))

        val enable = ProtocolJson.toConfigPatchJson(ArbConfigPatch(captureEnabled = true))
        assertEquals("""{"capture_enabled":true}""", enable.toString())
        val disable = ProtocolJson.toConfigPatchJson(ArbConfigPatch(captureEnabled = false))
        assertEquals("""{"capture_enabled":false}""", disable.toString())
        val unchanged = ProtocolJson.toConfigPatchJson(ArbConfigPatch(saveWindowSecs = 60))
        assertEquals("""{"save_window_secs":60}""", unchanged.toString())
        assertEquals(
            """{"capture_enabled":true}""",
            gson.toJson(ArbConfigPatch(captureEnabled = true)),
        )

        val statusBase =
            """"pid":7,"uptime_ms":1,"capture":{"active":false},"ring_chunk_count":0,"ring_total_bytes":0,""" +
                """"ring_pinned_chunk_count":0,"active_viewports":0,"active_save_jobs":0,"recovery":{}"""
        val armed = gson.fromJson("""{$statusBase,"capture_enabled":true}""", SystemStatusResponse::class.java)
        assertTrue(armed.captureEnabled)
        assertEquals(0L, armed.recovery.failedPurgingBundles)
        assertEquals(emptyList<String>(), armed.recovery.failedPurgingBundleIds)
        val legacy = gson.fromJson("""{$statusBase}""", SystemStatusResponse::class.java)
        assertEquals(false, legacy.captureEnabled)
        val withFailedPurge = gson.fromJson(
            """{"pid":7,"uptime_ms":1,"capture":{"active":false},"ring_chunk_count":0,"ring_total_bytes":0,""" +
                """"ring_pinned_chunk_count":0,"active_viewports":0,"active_save_jobs":0,""" +
                """"recovery":{"completed_purging_bundles":1,"failed_purging_bundles":2,"failed_purging_bundle_ids":["b-a","b-b"]}}""",
            SystemStatusResponse::class.java,
        )
        assertEquals(1L, withFailedPurge.recovery.completedPurgingBundles)
        assertEquals(2L, withFailedPurge.recovery.failedPurgingBundles)
        assertEquals(listOf("b-a", "b-b"), withFailedPurge.recovery.failedPurgingBundleIds)
    }

    @Test
    fun bundleEntryViewParsesDiskStateAndDefaultsMissingFieldToPresent() {
        val base =
            """"bundle_id":"b-1","path":"/tmp/b-1","format_version":"v2","state":"active","created_at_utc":"2026-11-25T00:00:00Z","active_leases":0"""
        assertEquals(
            BundleDiskState.PRESENT,
            gson.fromJson("""{$base,"disk_state":"present"}""", BundleEntryView::class.java).diskState,
        )
        assertEquals(
            BundleDiskState.MISSING,
            gson.fromJson("""{$base,"disk_state":"missing"}""", BundleEntryView::class.java).diskState,
        )
        assertEquals(
            BundleDiskState.STORAGE_UNAVAILABLE,
            gson.fromJson("""{$base,"disk_state":"storage_unavailable"}""", BundleEntryView::class.java).diskState,
        )
        // Older arbd omitting `disk_state` must cleanly default to non-null PRESENT.
        val legacyEntry = gson.fromJson("""{$base}""", BundleEntryView::class.java)
        val diskState: BundleDiskState? = legacyEntry.diskState
        assertEquals(BundleDiskState.PRESENT, diskState)

        val forget = gson.fromJson(
            """{"forgotten_bundle_ids":["b-1"],"skipped_present_bundle_ids":[],"skipped_storage_unavailable_bundle_ids":["b-2"],"skipped_leased_bundle_ids":[],"skipped_active_job_bundle_ids":[]}""",
            BundleForgetResponse::class.java,
        )
        assertEquals(listOf("b-1"), forget.forgottenBundleIds)
        assertEquals(listOf("b-2"), forget.skippedStorageUnavailableBundleIds)
        assertEquals(emptyList(), forget.reapedLeaseIds)
    }
}
