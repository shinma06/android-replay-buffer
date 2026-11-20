package io.github.shinma06.replaybuffer.ui

import androidx.compose.ui.graphics.Color
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class ScrollableColumnVisibilityTest {
    @Test
    fun whenScrollingThumbStaysVisibleUntilLingerExpires() {
        val policy = ScrollbarVisibilityPolicy.WhenScrolling
        var activity = ScrollbarActivity.Hidden

        assertFalse(isScrollbarVisible(policy, false, activity))

        activity = scrollbarActivityAfterScrollChange(activity, true)
        assertEquals(ScrollbarActivity.Scrolling, activity)
        assertTrue(isScrollbarVisible(policy, true, activity))

        activity = scrollbarActivityAfterScrollChange(activity, false)
        assertEquals(ScrollbarActivity.Lingering, activity)
        assertTrue(isScrollbarVisible(policy, false, activity))

        activity = scrollbarActivityAfterLinger(activity)
        assertEquals(ScrollbarActivity.Hidden, activity)
        assertFalse(isScrollbarVisible(policy, false, activity))
    }

    @Test
    fun scrollingAgainDuringLingerRestartsVisibleState() {
        val lingering = scrollbarActivityAfterScrollChange(ScrollbarActivity.Scrolling, false)

        val resumed = scrollbarActivityAfterScrollChange(lingering, true)

        assertEquals(ScrollbarActivity.Scrolling, resumed)
        assertTrue(isScrollbarVisible(ScrollbarVisibilityPolicy.WhenScrolling, true, resumed))
        assertEquals(ScrollbarActivity.Scrolling, scrollbarActivityAfterLinger(resumed))
    }

    @Test
    fun alwaysVisiblePolicyKeepsThumbVisibleInEveryActivityState() {
        ScrollbarActivity.entries.forEach { activity ->
            assertTrue(isScrollbarVisible(ScrollbarVisibilityPolicy.AlwaysVisible, false, activity))
        }
    }

    @Test
    fun alwaysVisibleUsesOpaqueIdleColorWhenNormalBackgroundIsTransparent() {
        val thumbBackground = Color.Transparent
        val thumbOpaqueBackground = Color(0xFF777777)
        val thumbBackgroundActive = Color(0xFFAAAAAA)

        val colors = scrollbarThumbColorPair(
            ScrollbarVisibilityPolicy.AlwaysVisible,
            false,
            thumbOpaqueBackground,
            thumbBackgroundActive,
        )

        assertEquals(thumbOpaqueBackground, colors.idle)
        assertEquals(thumbBackgroundActive, colors.hover)
        assertFalse(colors.idle == thumbBackground)
        assertFalse(colors.idle == colors.hover)
    }

    @Test
    fun whenScrollingKeepsActiveAndHiddenColorSelectionUnchanged() {
        val thumbOpaqueBackground = Color(0xFF777777)
        val thumbBackgroundActive = Color(0xFFAAAAAA)

        val visible = scrollbarThumbColorPair(
            ScrollbarVisibilityPolicy.WhenScrolling,
            true,
            thumbOpaqueBackground,
            thumbBackgroundActive,
        )
        val hidden = scrollbarThumbColorPair(
            ScrollbarVisibilityPolicy.WhenScrolling,
            false,
            thumbOpaqueBackground,
            thumbBackgroundActive,
        )

        assertEquals(thumbBackgroundActive, visible.idle)
        assertEquals(Color.Transparent, hidden.idle)
        assertEquals(thumbBackgroundActive, visible.hover)
        assertEquals(thumbBackgroundActive, hidden.hover)
    }
}
