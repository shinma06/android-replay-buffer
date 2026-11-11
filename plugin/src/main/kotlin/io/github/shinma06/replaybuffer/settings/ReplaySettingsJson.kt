package io.github.shinma06.replaybuffer.settings

import com.google.gson.Gson
import com.google.gson.Strictness
import com.google.gson.stream.JsonReader
import com.google.gson.stream.JsonToken
import io.github.shinma06.replaybuffer.core.CaptureMode
import io.github.shinma06.replaybuffer.core.TapDisplay
import io.github.shinma06.replaybuffer.core.VideoQuality
import java.io.StringReader

/** Settings as returned to CLI/MCP clients; the field names match `replay status` where they overlap. */
internal fun settingsJson(settings: ReplaySettings, enabled: Boolean): String = Gson().toJson(linkedMapOf(
    "enabled" to enabled,
    "replay_seconds" to settings.retentionSeconds,
    "output_directory" to settings.destination,
    "app_selection" to settings.appSelection.name,
    "manual_package" to settings.manualPackage,
    "capture_mode" to settings.captureMode.name,
    "video_quality" to settings.videoQuality.name,
    "auto_save_on_crash" to settings.autoSaveOnCrash,
    "screenshot_uses_logcat_destination" to settings.screenshotUsesLogcatDestination,
    "screenshot_directory" to settings.screenshotDirectory,
    "tap_display" to settings.tapDisplay.name,
    // Legacy alias of tap_display == DEVICE, kept for existing scripts.
    "show_touches" to (settings.tapDisplay == TapDisplay.DEVICE),
    "device" to settings.deviceSerial,
))

/**
 * Applies a flat JSON object of [settingsJson] fields (except `enabled`) to [current] without I/O or validation.
 *
 * Unknown, duplicate or mistyped fields are rejected; omitted fields keep their current value.
 */
internal fun patchSettings(current: ReplaySettings, text: String): ReplaySettings = try {
    JsonReader(StringReader(text)).use { reader ->
        reader.strictness = Strictness.STRICT
        var settings = current
        val seen = mutableSetOf<String>()
        var chosenTapDisplay: TapDisplay? = null
        var legacyShowTouches: Boolean? = null
        reader.beginObject()
        while (reader.hasNext()) {
            val key = reader.nextName()
            require(seen.add(key)) { "設定項目が重複しています: $key" }
            fun text(): String {
                require(reader.peek() == JsonToken.STRING) { "${key}は文字列で指定してください。" }
                return reader.nextString()
            }
            fun flag(): Boolean {
                require(reader.peek() == JsonToken.BOOLEAN) { "${key}はtrue/falseで指定してください。" }
                return reader.nextBoolean()
            }
            fun <T : Enum<T>> choice(values: Array<T>): T {
                val value = text()
                return requireNotNull(values.firstOrNull { it.name == value }) { "${key}は${values.joinToString("/") { it.name }}で指定してください。" }
            }
            settings = when (key) {
                "replay_seconds" -> {
                    require(reader.peek() == JsonToken.NUMBER) { "replay_secondsは整数で指定してください。" }
                    val seconds = reader.nextString().toIntOrNull()
                    require(seconds != null) { "replay_secondsは整数で指定してください。" }
                    settings.copy(retentionSeconds = seconds)
                }
                "output_directory" -> settings.copy(destination = text())
                "app_selection" -> settings.copy(appSelection = choice(AppSelectionMode.entries.toTypedArray()))
                "manual_package" -> settings.copy(manualPackage = text())
                "capture_mode" -> settings.copy(captureMode = choice(CaptureMode.entries.toTypedArray()))
                "video_quality" -> settings.copy(videoQuality = choice(VideoQuality.entries.toTypedArray()))
                "auto_save_on_crash" -> settings.copy(autoSaveOnCrash = flag())
                "screenshot_uses_logcat_destination" -> settings.copy(screenshotUsesLogcatDestination = flag())
                "screenshot_directory" -> settings.copy(screenshotDirectory = text())
                "tap_display" -> settings.also { chosenTapDisplay = choice(TapDisplay.entries.toTypedArray()) }
                "show_touches" -> settings.also { legacyShowTouches = flag() }
                "device" -> settings.copy(deviceSerial = text())
                else -> throw IllegalArgumentException("変更できない設定項目です: $key")
            }
        }
        reader.endObject()
        require(reader.peek() == JsonToken.END_DOCUMENT) { "設定の末尾に余分な内容があります。" }
        require(seen.isNotEmpty()) { "変更する設定を指定してください。" }
        // show_touches is the legacy alias of tap_display == DEVICE; both may be given (as get_settings prints them) only when they agree.
        val legacy = legacyShowTouches
        val chosen = chosenTapDisplay
        if (chosen != null && legacy != null) {
            require(legacy == (chosen == TapDisplay.DEVICE)) { "tap_displayとshow_touchesが矛盾しています。" }
        }
        settings = when {
            chosen != null -> settings.copy(tapDisplay = chosen)
            legacy == true -> settings.copy(tapDisplay = TapDisplay.DEVICE)
            legacy == false && settings.tapDisplay == TapDisplay.DEVICE -> settings.copy(tapDisplay = TapDisplay.OFF)
            else -> settings
        }
        settings
    }
} catch (failure: java.io.IOException) {
    throw IllegalArgumentException("設定はJSON objectで指定してください。", failure)
} catch (failure: IllegalStateException) {
    throw IllegalArgumentException("設定はJSON objectで指定してください。", failure)
}
