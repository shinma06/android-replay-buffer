package io.github.shinma06.replaybuffer.settings

import com.google.gson.JsonParser
import io.github.shinma06.replaybuffer.core.TapDisplay
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

    @Test fun `partial patch keeps omitted fields`() {
        val current = ReplaySettings(destination = "/tmp/out", autoSaveOnCrash = true)
        assertEquals(current.copy(retentionSeconds = 30, captureMode = CaptureMode.LOG_ONLY),
            patchSettings(current, """{"replay_seconds":30,"capture_mode":"LOG_ONLY"}"""))
        assertEquals(current.copy(tapDisplay = TapDisplay.DEVICE), patchSettings(current, """{"show_touches":true}"""))
        assertEquals(true, JsonParser.parseString(settingsJson(current.copy(tapDisplay = TapDisplay.DEVICE), false)).asJsonObject["show_touches"].asBoolean)
    }

    @Test fun `unknown duplicate mistyped and empty patches are rejected`() {
        for (text in listOf("{}", """{"enabled":true}""", """{"replay_seconds":"30"}""", """{"replay_seconds":1.5}""",
            """{"replay_seconds":1,"replay_seconds":2}""", """{"capture_mode":"VIDEO"}""", """{"auto_save_on_crash":"true"}""", """{"show_touches":1}""",
            """{"output_directory":null}""", "[]", """{"replay_seconds":1} {}""", "{")) {
            assertFailsWith<IllegalArgumentException>(text) { patchSettings(ReplaySettings(), text) }
        }
    }

    @Test fun `tap_display has three choices and show_touches stays a legacy alias`() {
        val current = ReplaySettings()
        assertEquals("OFF", JsonParser.parseString(settingsJson(current, false)).asJsonObject["tap_display"].asString)
        val video = patchSettings(current, """{"tap_display":"VIDEO"}""")
        assertEquals(TapDisplay.VIDEO, video.tapDisplay)
        assertEquals(false, JsonParser.parseString(settingsJson(video, false)).asJsonObject["show_touches"].asBoolean)
        assertEquals(TapDisplay.VIDEO, patchSettings(video, """{"show_touches":false}""").tapDisplay)
        assertEquals(TapDisplay.OFF, patchSettings(current.copy(tapDisplay = TapDisplay.DEVICE), """{"show_touches":false}""").tapDisplay)
        assertEquals(TapDisplay.DEVICE, patchSettings(video, """{"show_touches":true}""").tapDisplay)
        assertEquals(TapDisplay.VIDEO, patchSettings(current, """{"show_touches":false,"tap_display":"VIDEO"}""").tapDisplay)
        for (text in listOf("""{"tap_display":"ON"}""", """{"tap_display":true}""", """{"tap_display":"VIDEO","show_touches":true}""",
            """{"show_touches":false,"tap_display":"DEVICE"}""")) {
            assertFailsWith<IllegalArgumentException>(text) { patchSettings(current, text) }
        }
    }

    @Test fun `stored legacy showTouches migrates to tap display`() {
        assertEquals(TapDisplay.DEVICE, ReplaySettingsState(showTouches = true).effectiveTapDisplay)
        assertEquals(TapDisplay.OFF, ReplaySettingsState().effectiveTapDisplay)
        assertEquals(TapDisplay.VIDEO, ReplaySettingsState(showTouches = false, tapDisplay = TapDisplay.VIDEO).effectiveTapDisplay)
    }
}
