package io.github.shinma06.replaybuffer.ui

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
}
