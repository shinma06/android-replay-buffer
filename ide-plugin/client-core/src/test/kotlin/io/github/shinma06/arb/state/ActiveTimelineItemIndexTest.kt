package io.github.shinma06.arb.state

import org.junit.jupiter.api.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull

/**
 * Pure unit coverage for [activeTimelineItemIndex] (Owner trial feedback T-6H item 4: video <->
 * right-panel Event/Log timeline auto-follow sync).
 */
class ActiveTimelineItemIndexTest {

    private fun row(seq: Long, frameIndex: Long?): SyncedEventRow = SyncedEventRow(
        kind = SyncedEventKind.LOG,
        seq = seq,
        boottimeNs = frameIndex?.let { it * 1_000_000L },
        relativeMs = frameIndex,
        frameIndex = frameIndex,
        title = "row-$seq",
        detail = "",
    )

    @Test
    fun emptyListReturnsNull() {
        assertNull(activeTimelineItemIndex(emptyList(), playheadFrameIndex = 10L))
    }

    @Test
    fun playheadBeforeFirstItemSnapsToNearestWithinTolerance() {
        val rows = listOf(row(1L, 50L), row(2L, 100L))
        assertEquals(0, activeTimelineItemIndex(rows, playheadFrameIndex = 40L, toleranceFrames = 30L))
    }

    @Test
    fun playheadBeforeFirstItemBeyondToleranceReturnsNull() {
        val rows = listOf(row(1L, 50L), row(2L, 100L))
        assertNull(activeTimelineItemIndex(rows, playheadFrameIndex = 0L, toleranceFrames = 10L))
    }

    @Test
    fun exactFrameMatchReturnsThatRow() {
        val rows = listOf(row(1L, 10L), row(2L, 20L), row(3L, 30L))
        assertEquals(1, activeTimelineItemIndex(rows, playheadFrameIndex = 20L))
    }

    @Test
    fun betweenItemsReturnsTheLatestAtOrBeforePlayhead() {
        val rows = listOf(row(1L, 10L), row(2L, 20L), row(3L, 30L))
        assertEquals(1, activeTimelineItemIndex(rows, playheadFrameIndex = 25L))
    }

    @Test
    fun playheadPastLastItemReturnsLastRow() {
        val rows = listOf(row(1L, 10L), row(2L, 20L), row(3L, 30L))
        assertEquals(2, activeTimelineItemIndex(rows, playheadFrameIndex = 1_000L))
    }

    @Test
    fun rowsWithNullFrameIndexAreSkipped() {
        val rows = listOf(row(1L, null), row(2L, 20L), row(3L, null), row(4L, 40L))
        assertEquals(1, activeTimelineItemIndex(rows, playheadFrameIndex = 25L))
    }

    @Test
    fun worksOnAnAlreadyFilteredSubsetOfRows() {
        val filtered = listOf(row(2L, 20L), row(4L, 40L)) // e.g. only TAPS-filtered rows
        assertEquals(1, activeTimelineItemIndex(filtered, playheadFrameIndex = 45L))
    }

    @Test
    fun toggleOffThenOnRecomputesAndSnapsToCurrentPlayhead() {
        // Follow is pure/stateless: whenever the UI flips `followPlayhead` back ON it just calls
        // this again with the live playhead, which always yields the currently-correct index —
        // no stale state to reconcile regardless of how long the list was pinned while OFF.
        val rows = listOf(row(1L, 10L), row(2L, 20L), row(3L, 30L))
        val whilePinned = activeTimelineItemIndex(rows, playheadFrameIndex = 12L)
        assertEquals(0, whilePinned)
        val afterScrubbingAhead = activeTimelineItemIndex(rows, playheadFrameIndex = 30L)
        assertEquals(2, afterScrubbingAhead)
    }
}
