package io.github.shinma06.arb.state

import io.github.shinma06.arb.model.GestureKind
import io.github.shinma06.arb.model.IdeDefaultSaveAction
import io.github.shinma06.arb.model.TapRecord
import java.util.Locale
import kotlin.math.abs
import kotlin.math.roundToInt

/**
 * Pure UI string and numeric formatters enforcing ARB's strict UX contracts:
 * - Strict seconds notation everywhere (never convert/round seconds to minutes!)
 * - Zero emoji chrome anywhere
 * - Generic non-narrowed Japanese copy (never "エビデンス")
 * - Honest tap display ("Tap #N (x, y)" only, never fake UI element labels)
 */
object ArbFormatters {
    const val EXPORT_FULL_LABEL: String = "MP4 を書き出し"
    const val SECONDARY_SCREENSHOT_LABEL: String = "スクショ"
    const val SECONDARY_LOGS_ONLY_LABEL: String = "ログのみ"
    const val SECONDARY_START_CAPTURE_LABEL: String = "開始"
    const val SECONDARY_STOP_CAPTURE_LABEL: String = "停止"
    const val PRESET_TAP_RANGE_LABEL: String = "操作区間を選択"
    const val PRESET_CRASH_RANGE_LABEL: String = "クラッシュ前後を選択 (-5s/+2s)"
    const val PRESET_RESET_RANGE_LABEL: String = "全選択に戻す"
    const val TRASH_ORIGINAL_TOGGLE_LABEL: String = "書き出し後に元のバンドルをゴミ箱へ（復元可）"
    const val EXPORT_TOAST_TITLE: String = "トリムを書き出しました"

    /** Strict seconds formatting (e.g. `300s`, `45s`). Never converts to minutes. */
    fun formatSeconds(seconds: Long): String = "${seconds.coerceAtLeast(0L)}s"

    fun formatSeconds(seconds: Int): String = formatSeconds(seconds.toLong())

    /** Formats a duration in milliseconds strictly in seconds (e.g. `15.7s` or `0.0s`). */
    fun formatDurationSeconds(durationMs: Long): String {
        val clamped = durationMs.coerceAtLeast(0L)
        val tenths = (clamped / 100.0).roundToInt()
        return if (tenths % 10 == 0 && clamped >= 10_000L && clamped % 1_000L == 0L) {
            "${clamped / 1_000L}s"
        } else {
            String.format(Locale.ROOT, "%.1fs", clamped / 1_000.0)
        }
    }

    /** Formats the live capture ring fill readout in strict seconds + MB. */
    fun formatRingMeter(bufferedSecs: Long, windowSecs: Int, ringBytes: Long): String {
        val clampedBuffered = bufferedSecs.coerceIn(0L, windowSecs.toLong().coerceAtLeast(0L))
        val mb = ringBytes.coerceAtLeast(0L) / (1024.0 * 1024.0)
        val mbText = if (mb >= 100.0) {
            String.format(Locale.ROOT, "%.0f MB", mb)
        } else {
            String.format(Locale.ROOT, "%.1f MB", mb)
        }
        return "${clampedBuffered}s / ${windowSecs}s · $mbText"
    }

    /** Primary full-width CTA label reflecting `ArbConfig.save_window_secs` & `ide_default_save_action`. */
    fun formatPrimarySaveLabel(windowSecs: Int, defaultAction: IdeDefaultSaveAction): String = when (defaultAction) {
        IdeDefaultSaveAction.SAVE_AND_OPEN -> "直前 ${windowSecs}s を保存して開く"
        IdeDefaultSaveAction.SAVE_ONLY -> "直前 ${windowSecs}s を保存のみ"
    }

    /** Compact secondary save button label paired with [formatPrimarySaveLabel]. */
    fun formatSecondarySaveLabel(defaultAction: IdeDefaultSaveAction): String = when (defaultAction) {
        IdeDefaultSaveAction.SAVE_AND_OPEN -> "保存のみ"
        IdeDefaultSaveAction.SAVE_ONLY -> "保存して開く"
    }

    /** Top action bar primary export CTA label (generic Japanese copy, strict seconds). */
    fun formatExportCtaLabel(
        inFrameIndex: Long,
        outFrameIndex: Long,
        totalFrames: Long,
        selectedDurationMs: Long,
    ): String {
        if (totalFrames <= 0L) return EXPORT_FULL_LABEL
        val isFullRange = inFrameIndex <= 0L && outFrameIndex >= totalFrames - 1L
        return formatExportCtaLabel(isFullRange, selectedDurationMs)
    }

    fun formatExportCtaLabel(isFullRange: Boolean, selectedDurationMs: Long): String =
        if (isFullRange) {
            EXPORT_FULL_LABEL
        } else {
            "選択範囲を書き出し (${formatDurationSeconds(selectedDurationMs)})"
        }

    /** Tabular monospace timecode `MM:SS.cc` for transport bar and timeline markers. */
    fun formatTimecode(relativeMs: Long): String {
        val clamped = relativeMs.coerceAtLeast(0L)
        val totalSeconds = clamped / 1_000L
        val minutes = totalSeconds / 60L
        val seconds = totalSeconds % 60L
        val centis = (clamped % 1_000L) / 10L
        return String.format(Locale.ROOT, "%02d:%02d.%02d", minutes, seconds, centis)
    }

    /** Compact `MM:SS.d` timecode for selection summary pills. */
    fun formatShortTimecode(relativeMs: Long): String {
        val clamped = relativeMs.coerceAtLeast(0L)
        val totalSeconds = clamped / 1_000L
        val minutes = totalSeconds / 60L
        val seconds = totalSeconds % 60L
        val tenths = (clamped % 1_000L) / 100L
        return String.format(Locale.ROOT, "%02d:%02d.%d", minutes, seconds, tenths)
    }

    /** Floating selection summary pill above the trim range (`00:08.0 – 00:23.7 · 15.7s (471f)`). */
    fun formatSelectionSummary(
        inMs: Long,
        outMs: Long,
        inFrameIndex: Long,
        outFrameIndex: Long,
        durationMs: Long = (outMs - inMs).coerceAtLeast(0L),
    ): String {
        val frameCount = (outFrameIndex - inFrameIndex + 1L).coerceAtLeast(0L)
        return "${formatShortTimecode(inMs)} – ${formatShortTimecode(outMs)} · ${formatDurationSeconds(durationMs)} (${frameCount}f)"
    }

    /**
     * Honest tap/swipe row label strictly from recorded coordinates — zero guessed UI labels.
     *
     * T-6H item 2: a true per-stroke [GestureKind.SWIPE] renders as
     * `Swipe #N (x1, y1) -> (x2, y2) (右スワイプ)`. Bundles saved before per-stroke
     * classification existed (`gesture_kind` missing, defaulting to `Tap`) keep the legacy
     * per-sample `MOVE`/`SWIPE` action heuristic so old bundles render exactly as before.
     */
    fun formatTapDisplay(tapIndex1Based: Int, tap: TapRecord): String {
        val x = tap.x
        val y = tap.y
        if (tap.gestureKind == GestureKind.SWIPE) {
            val endX = tap.endX
            val endY = tap.endY
            val direction = swipeDirectionLabel(x, y, endX, endY)?.let { " ($it)" }.orEmpty()
            return when {
                x != null && y != null && endX != null && endY != null ->
                    "Swipe #$tapIndex1Based (${x.roundToInt()}, ${y.roundToInt()}) -> " +
                        "(${endX.roundToInt()}, ${endY.roundToInt()})$direction"
                x != null && y != null -> "Swipe #$tapIndex1Based (${x.roundToInt()}, ${y.roundToInt()})"
                else -> "Swipe #$tapIndex1Based"
            }
        }
        val legacyIsSwipe = tap.action.equals("MOVE", ignoreCase = true) ||
            tap.action.equals("SWIPE", ignoreCase = true)
        val prefix = if (legacyIsSwipe) "Swipe" else "Tap"
        return if (x != null && y != null) {
            "$prefix #$tapIndex1Based (${x.roundToInt()}, ${y.roundToInt()})"
        } else {
            "$prefix #$tapIndex1Based"
        }
    }

    /** Cardinal Japanese swipe direction label from the dominant axis of travel, or `null` if still. */
    private fun swipeDirectionLabel(x: Double?, y: Double?, endX: Double?, endY: Double?): String? {
        if (x == null || y == null || endX == null || endY == null) return null
        val dx = endX - x
        val dy = endY - y
        if (dx == 0.0 && dy == 0.0) return null
        return if (abs(dx) >= abs(dy)) {
            if (dx >= 0.0) "右スワイプ" else "左スワイプ"
        } else {
            if (dy >= 0.0) "下スワイプ" else "上スワイプ"
        }
    }

    /**
     * Returns `true` if [text] contains any Unicode emoji / pictograph / variation-selector-16 codepoint.
     * Used by automated tests to guarantee zero emoji chrome in the IDE UI.
     */
    fun containsEmoji(text: String): Boolean {
        var i = 0
        while (i < text.length) {
            val cp = text.codePointAt(i)
            i += Character.charCount(cp)
            if (isEmojiCodePoint(cp)) return true
        }
        return false
    }

    private fun isEmojiCodePoint(cp: Int): Boolean = when {
        cp in 0x1F300..0x1FAFF -> true // Misc Symbols & Pictographs, Emoticons, Transport, Supplemental
        cp in 0x2600..0x27BF -> true   // Misc Symbols & Dingbats (U+2600..U+27BF)
        cp == 0xFE0F -> true           // Emoji Variation Selector-16
        cp == 0x200D -> true           // Zero-Width Joiner used in emoji sequences
        cp in 0x23E9..0x23FA -> true   // Media transport emoji block (U+23E9..U+23FA)
        cp == 0x25B6 || cp == 0x25C0 -> true // Play/reverse emoji triangles (U+25B6 / U+25C0)
        else -> false
    }
}
