package io.github.shinma06.arb.state

import io.github.shinma06.arb.model.GestureKind
import io.github.shinma06.arb.model.RecordBucket
import io.github.shinma06.arb.model.TapRecord
import io.github.shinma06.arb.model.TimeAlignment
import io.github.shinma06.arb.model.TimestampSemantics
import org.junit.jupiter.api.Test
import kotlin.test.assertEquals

/**
 * [ArbFormatters.formatTapDisplay] pure-function tests (T-6H item 2): Tap vs Swipe display
 * strings, the four cardinal Japanese direction labels, and legacy bundle compatibility.
 */
class ArbFormattersTest {
    private fun tap(
        action: String,
        x: Double? = null,
        y: Double? = null,
        gestureKind: GestureKind = GestureKind.TAP,
        endX: Double? = null,
        endY: Double? = null,
    ): TapRecord = TapRecord(
        seq = 1L,
        pcIngestContNs = 1_000L,
        alignment = TimeAlignment.Anchored("boot-1", 1_000L, 5L, TimestampSemantics.INPUT_EVENT_TIME),
        bucket = RecordBucket.MAIN,
        action = action,
        x = x,
        y = y,
        gestureKind = gestureKind,
        endX = endX,
        endY = endY,
    )

    @Test
    fun plainTapShowsOnlyItsOwnCoordinates() {
        val display = ArbFormatters.formatTapDisplay(1, tap(action = "TAP", x = 10.4, y = 20.6))
        assertEquals("Tap #1 (10, 21)", display)
    }

    @Test
    fun tapWithoutCoordinatesOmitsTheParens() {
        val display = ArbFormatters.formatTapDisplay(3, tap(action = "TAP"))
        assertEquals("Tap #3", display)
    }

    @Test
    fun swipeShowsStartAndEndCoordinatesWithArrow() {
        val display = ArbFormatters.formatTapDisplay(
            2,
            tap(action = "SWIPE", x = 0.0, y = 0.0, gestureKind = GestureKind.SWIPE, endX = 300.0, endY = 0.0),
        )
        assertEquals("Swipe #2 (0, 0) -> (300, 0) (右スワイプ)", display)
    }

    @Test
    fun swipeDirectionCoversAllFourCardinalLabels() {
        fun directionOf(endX: Double, endY: Double): String = ArbFormatters.formatTapDisplay(
            1,
            tap(action = "SWIPE", x = 0.0, y = 0.0, gestureKind = GestureKind.SWIPE, endX = endX, endY = endY),
        )

        assertEquals("Swipe #1 (0, 0) -> (300, 0) (右スワイプ)", directionOf(300.0, 0.0))
        assertEquals("Swipe #1 (0, 0) -> (-300, 0) (左スワイプ)", directionOf(-300.0, 0.0))
        assertEquals("Swipe #1 (0, 0) -> (0, 300) (下スワイプ)", directionOf(0.0, 300.0))
        assertEquals("Swipe #1 (0, 0) -> (0, -300) (上スワイプ)", directionOf(0.0, -300.0))
    }

    @Test
    fun swipeWithIdenticalStartAndEndOmitsDirection() {
        val display = ArbFormatters.formatTapDisplay(
            1,
            tap(action = "SWIPE", x = 50.0, y = 50.0, gestureKind = GestureKind.SWIPE, endX = 50.0, endY = 50.0),
        )
        assertEquals("Swipe #1 (50, 50) -> (50, 50)", display, "zero net displacement must not guess a direction")
    }

    @Test
    fun swipeMissingEndCoordinatesFallsBackToStartOnly() {
        val display = ArbFormatters.formatTapDisplay(
            4,
            tap(action = "SWIPE", x = 10.0, y = 20.0, gestureKind = GestureKind.SWIPE),
        )
        assertEquals("Swipe #4 (10, 20)", display)
    }

    @Test
    fun legacyBundleMoveActionWithDefaultGestureKindStillLabelsAsSwipe() {
        // Bundles saved before T-6H item 2 never set gesture_kind (always defaults to Tap), but
        // the UI must keep rendering their raw per-sample MOVE rows exactly as before.
        val display = ArbFormatters.formatTapDisplay(5, tap(action = "MOVE", x = 1.0, y = 2.0))
        assertEquals("Swipe #5 (1, 2)", display)
    }

    @Test
    fun legacyBundleDownActionWithDefaultGestureKindLabelsAsTap() {
        val display = ArbFormatters.formatTapDisplay(6, tap(action = "DOWN", x = 1.0, y = 2.0))
        assertEquals("Tap #6 (1, 2)", display)
    }
}
