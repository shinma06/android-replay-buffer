package io.github.shinma06.arb.state

import com.google.gson.JsonObject
import com.google.gson.JsonParser
import io.github.shinma06.arb.model.AudioCaptureMode
import io.github.shinma06.arb.model.AudioCaptureState
import io.github.shinma06.arb.model.AudioCause
import io.github.shinma06.arb.model.CaptureStatusView
import io.github.shinma06.arb.model.ProtocolJson
import org.junit.jupiter.api.Test
import java.io.File
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * PDD G8-7 (exhaustive presentation table) and G8-8 (Kotlin side of the golden table shared with
 * `arb-core`: `docs/specs/audio-policy-table.json`).
 */
class AudioPolicyTableTest {
    private val unsupportedWord = "非対応"

    private fun capture(state: AudioCaptureState?, cause: AudioCause?, api: Long? = 33, active: Boolean = true) =
        CaptureStatusView(
            active = active,
            connected = true,
            storagePressureStopped = false,
            videoFrames = 10L,
            audioFrames = 0L,
            logRecords = 1L,
            tapRecords = 0L,
            discontinuities = 0L,
            staticHoldEvents = 0L,
            audioState = state,
            audioCause = cause,
            audioReason = "reason from arbd",
            deviceApiLevel = api,
        )

    private fun goldenTable(): JsonObject {
        val root = System.getProperty("arb.repo.root")?.takeIf { it.isNotBlank() }?.let(::File)
            ?: generateSequence(File("").absoluteFile) { it.parentFile }
                .first { File(it, "docs/specs/audio-policy-table.json").isFile }
        val file = File(root, "docs/specs/audio-policy-table.json")
        assertTrue(file.isFile, "golden table exists at $file")
        return JsonParser.parseString(file.readText()).asJsonObject
    }

    /** G8-7: "非対応" is shown ⟺ the cause is `api_below_min`, for every cause, state and API. */
    @Test
    fun g8_7UnsupportedWordAppearsIfAndOnlyIfApiBelowMin() {
        val causes: List<AudioCause?> = AudioCause.entries + listOf(null)
        for (cause in causes) {
            for (state in AudioCaptureState.entries + listOf(null)) {
                for (api in listOf(null, 0L, 28L, 32L, 33L, 34L, 37L)) {
                    val status = capture(state, cause, api)
                    val banner = AudioPolicyTable.banner(status)
                    val label = AudioPolicyTable.statusLabel(audioEnabled = true, logsOnly = false, capture = status)
                    val shown = listOfNotNull(banner?.title, label)
                    val unsupported = (state == AudioCaptureState.UNAVAILABLE) && cause == AudioCause.API_BELOW_MIN
                    assertEquals(
                        unsupported,
                        shown.any { unsupportedWord in it },
                        "cause=$cause state=$state api=$api shown=$shown",
                    )
                    if (state == AudioCaptureState.UNAVAILABLE && cause != null) {
                        val expectBanner = AudioPolicyTable.classOf(cause) in
                            setOf(AudioLabelClass.UNSUPPORTED_API, AudioLabelClass.RUNTIME_FAILURE)
                        assertEquals(expectBanner, banner != null, "banner presence for $cause")
                        banner?.let { assertTrue(it.detail.orEmpty().startsWith("reason from arbd"), "detail keeps audio_reason") }
                    }
                    if (state != AudioCaptureState.UNAVAILABLE) {
                        assertNull(banner, "no audio banner unless unavailable ($cause/$state)")
                    }
                }
            }
        }
    }

    @Test
    fun g8_7EveryCauseHasAnExplicitClassAndRuntimeFailuresNameTheirCause() {
        val titles = mutableSetOf<String>()
        for (cause in AudioCause.entries) {
            val cls = AudioPolicyTable.classOf(cause)
            val title = AudioPolicyTable.bannerTitle(cause, 33)
            when (cls) {
                AudioLabelClass.NONE, AudioLabelClass.DISABLED -> assertNull(title, "$cause has no banner")
                AudioLabelClass.UNSUPPORTED_API, AudioLabelClass.RUNTIME_FAILURE -> {
                    assertNotNull(title, "$cause has a banner")
                    assertTrue(titles.add(title), "$cause has its own distinct title")
                }
            }
        }
        assertTrue(
            AudioPolicyTable.bannerTitle(AudioCause.API_BELOW_MIN, 28)!!.contains("API 28"),
            "the unsupported banner names the detected API level",
        )
    }

    @Test
    fun g8_7OlderDaemonWithoutCauseIsAnUnknownRuntimeFailureNotUnsupported() {
        val status = capture(AudioCaptureState.UNAVAILABLE, null, api = null)
        val banner = assertNotNull(AudioPolicyTable.banner(status))
        assertTrue(unsupportedWord !in banner.title, banner.title)
        assertEquals(AudioLabelClass.RUNTIME_FAILURE, AudioPolicyTable.labelClass(AudioCaptureState.UNAVAILABLE, null))
        assertEquals(AudioLabelClass.NONE, AudioPolicyTable.labelClass(AudioCaptureState.UNKNOWN, null))
    }

    @Test
    fun g8_7ToolWindowLabelShowsLiveStateNextToTheSetting() {
        assertEquals("音声 ON（設定 OFF）", AudioPolicyTable.statusLabel(false, false, capture(AudioCaptureState.AVAILABLE, AudioCause.NONE)))
        val output = capture(AudioCaptureState.AVAILABLE, AudioCause.NONE).copy(audioCaptureMode = AudioCaptureMode.OUTPUT_COMPAT)
        assertEquals("音声 ON（端末消音）", AudioPolicyTable.statusLabel(true, false, output))
        val disconnected = capture(AudioCaptureState.UNAVAILABLE, AudioCause.ENCODER_FATAL).copy(connected = false)
        assertNull(AudioPolicyTable.banner(disconnected))
        assertEquals("音声: 接続待ち", AudioPolicyTable.statusLabel(true, false, disconnected))
        assertNull(AudioPolicyTable.banner(disconnected.copy(active = false, connected = true)))
        assertEquals("映像・音声なし", AudioPolicyTable.statusLabel(true, true, null))
        assertEquals("音声 ON", AudioPolicyTable.statusLabel(true, false, null))
        assertEquals("音声 ON", AudioPolicyTable.statusLabel(true, false, capture(AudioCaptureState.AVAILABLE, AudioCause.NONE)))
        assertEquals("音声 ON（確認中）", AudioPolicyTable.statusLabel(true, false, capture(AudioCaptureState.UNKNOWN, null)))
        assertEquals(
            "音声: 失敗（映像のみ）",
            AudioPolicyTable.statusLabel(true, false, capture(AudioCaptureState.UNAVAILABLE, AudioCause.DEVICE_CAPTURE_FAILED)),
        )
        assertEquals(
            "音声: 非対応",
            AudioPolicyTable.statusLabel(true, false, capture(AudioCaptureState.UNAVAILABLE, AudioCause.API_BELOW_MIN, 28)),
        )
        assertEquals(
            "音声: 失敗（ログのみ）",
            AudioPolicyTable.statusLabel(true, false, capture(AudioCaptureState.UNAVAILABLE, AudioCause.LAUNCH_FAILED_BOTH)),
        )
        assertEquals(
            "音声 ON",
            AudioPolicyTable.statusLabel(true, false, capture(AudioCaptureState.UNAVAILABLE, AudioCause.ENCODER_FATAL, active = false)),
            "idle shows only the setting",
        )
    }

    /** G8-8: the shared golden table's cause list and label classes equal this client's. */
    @Test
    fun g8_8GoldenTableCausesMatchKotlinEnumAndClasses() {
        val table = goldenTable()
        val causes = table.getAsJsonArray("causes").map { it.asJsonObject }
        assertEquals(
            AudioCause.entries.map { it.wire },
            causes.map { it.get("cause").asString },
            "golden cause list == AudioCause.entries (same order as arb-core AudioCause::ALL)",
        )
        assertEquals(
            AudioLabelClass.entries.map { it.wire },
            table.getAsJsonArray("label_classes").map { it.asString },
            "label classes match",
        )
        for (entry in causes) {
            val cause = AudioCause.fromWire(entry.get("cause").asString)
            assertEquals(entry.get("label_class").asString, AudioPolicyTable.classOf(cause).wire, "class of $cause")
        }
    }

    /** G8-8: every golden row's (state, cause) yields the row's label class and banner wording rule. */
    @Test
    fun g8_8GoldenTableRowsMatchKotlinPresentation() {
        val rows = goldenTable().getAsJsonArray("rows").map { it.asJsonObject }
        assertTrue(rows.size >= 86, "golden table keeps its coverage")
        for (row in rows) {
            val id = row.get("id").asString
            val state = AudioCaptureState.fromWire(row.get("state").asString)
            val cause = AudioCause.fromWire(row.get("cause").asString)
            assertEquals(row.get("cause").asString, cause.wire, "$id: cause is known to the client")
            assertEquals(row.get("label_class").asString, AudioPolicyTable.labelClass(state, cause).wire, "$id: label class")
            val api = row.get("api").takeUnless { it.isJsonNull }?.asLong
            val mode = AudioCaptureMode.fromWire(row.get("audio_capture_mode").asString)
            val banner = AudioPolicyTable.banner(capture(state, cause, api).copy(audioCaptureMode = mode))
            val hint = mode == AudioCaptureMode.PLAYBACK_DUP && cause == AudioCause.DEVICE_CAPTURE_FAILED &&
                (api == null || api == 0L || api >= 30)
            assertEquals(hint, banner?.detail?.contains("端末消音") == true, id)
            if (cause == AudioCause.API_BELOW_MIN) {
                val floor = if (mode == AudioCaptureMode.PLAYBACK_DUP) "Android 13 / API 33" else "Android 11 / API 30"
                assertTrue(banner?.title?.contains(floor) == true, id)
            }
            val cls = AudioPolicyTable.classOf(cause)
            assertEquals(
                state == AudioCaptureState.UNAVAILABLE &&
                    (cls == AudioLabelClass.UNSUPPORTED_API || cls == AudioLabelClass.RUNTIME_FAILURE),
                banner != null,
                "$id: banner presence",
            )
            assertEquals(cls == AudioLabelClass.UNSUPPORTED_API, banner?.title?.contains(unsupportedWord) == true, "$id: wording")
        }
    }

    @Test
    fun missingAudioModeUsesPlaybackAndOnlyExplicitChangesSendTheField() {
        val gson = ProtocolJson.gson
        val config = gson.fromJson("""{"audio_enabled":true}""", io.github.shinma06.arb.model.ArbConfig::class.java)
        assertEquals(AudioCaptureMode.PLAYBACK_DUP, config.audioCaptureMode)
        val draft = ArbSettingsDraft.fromConfig(config)
        assertTrue(!gson.toJsonTree(draft.toConfigPatch(config)).asJsonObject.has("audio_capture_mode"))
        val patch = draft.copy(audioCaptureMode = AudioCaptureMode.OUTPUT_COMPAT).toConfigPatch(config)
        assertEquals("output_compat", gson.toJsonTree(patch).asJsonObject.get("audio_capture_mode").asString)
    }

    @Test
    fun audioCauseWireRoundTripsAndUnknownValuesDoNotBreakStatusParsing() {
        val json = """
            {"active": true, "connected": true, "storage_pressure_stopped": false,
             "video_frames": 1, "audio_frames": 0, "log_records": 0, "tap_records": 0,
             "discontinuities": 0, "static_hold_events": 0,
             "audio_state": "unavailable", "audio_cause": "a_future_cause", "device_api_level": 33}
        """.trimIndent()
        val parsed = ProtocolJson.gson.fromJson(json, CaptureStatusView::class.java)
        assertEquals(AudioCause.UNKNOWN, parsed.audioCause)
        assertEquals(33L, parsed.deviceApiLevel)
        for (cause in AudioCause.entries) {
            val reparsed = ProtocolJson.gson.fromJson(ProtocolJson.gson.toJson(cause), AudioCause::class.java)
            assertEquals(cause, reparsed, "round trip $cause")
        }
    }
}
