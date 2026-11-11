package io.github.shinma06.replaybuffer.settings

import com.google.gson.JsonParser
import io.github.shinma06.replaybuffer.core.CaptureMode
import io.github.shinma06.replaybuffer.core.VideoQuality
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import org.junit.jupiter.api.Test

class ReplaySettingsJsonTest {
    @Test fun `settings round trip through the CLI field names`() {
        val settings = ReplaySettings(60, "/tmp/out", AppSelectionMode.MANUAL, "com.example.app", CaptureMode.LOG_ONLY, true, false,
            "/tmp/shots", VideoQuality.LIGHTWEIGHT)
        val json = JsonParser.parseString(settingsJson(settings, true)).asJsonObject
        assertEquals(true, json["enabled"].asBoolean)
        assertEquals(60, json["replay_seconds"].asInt)
        assertEquals("LIGHTWEIGHT", json["video_quality"].asString)
        json.remove("enabled")
        assertEquals(settings, patchSettings(ReplaySettings(), json.toString()))
    }

    @Test fun `device is a serial string shared with CLI and MCP`() {
        val chosen = patchSettings(ReplaySettings(), """{"device":"emulator-5556"}""")
        assertEquals("emulator-5556", chosen.deviceSerial)
        assertEquals("emulator-5556", JsonParser.parseString(settingsJson(chosen, false)).asJsonObject["device"].asString)
        assertEquals("", patchSettings(chosen, """{"device":""}""").deviceSerial)
        assertEquals(true, chosen.copy(deviceSerial = "bad serial").validationError(DestinationValidation("", null), DestinationValidation("", null)) != null)
        assertEquals(null, chosen.validationError(DestinationValidation("", null), DestinationValidation("", null)))
        assertFailsWith<IllegalArgumentException> { patchSettings(chosen, """{"device":5556}""") }
    }

    @Test fun `partial patch keeps omitted fields`() {
        val current = ReplaySettings(destination = "/tmp/out", autoSaveOnCrash = true)
        assertEquals(current.copy(retentionSeconds = 30, captureMode = CaptureMode.LOG_ONLY),
            patchSettings(current, """{"replay_seconds":30,"capture_mode":"LOG_ONLY"}"""))
        assertEquals(current.copy(showTouches = true), patchSettings(current, """{"show_touches":true}"""))
        assertEquals(true, JsonParser.parseString(settingsJson(current.copy(showTouches = true), false)).asJsonObject["show_touches"].asBoolean)
        assertEquals(current.copy(recordAudio = true), patchSettings(current, """{"record_audio":true}"""))
        assertEquals(false, JsonParser.parseString(settingsJson(current, false)).asJsonObject["record_audio"].asBoolean)
    }

    @Test fun `unknown duplicate mistyped and empty patches are rejected`() {
        for (text in listOf("{}", """{"enabled":true}""", """{"replay_seconds":"30"}""", """{"replay_seconds":1.5}""",
            """{"replay_seconds":1,"replay_seconds":2}""", """{"capture_mode":"VIDEO"}""", """{"auto_save_on_crash":"true"}""", """{"show_touches":1}""", """{"record_audio":"true"}""",
            """{"output_directory":null}""", "[]", """{"replay_seconds":1} {}""", "{")) {
            assertFailsWith<IllegalArgumentException>(text) { patchSettings(ReplaySettings(), text) }
        }
    }
}
