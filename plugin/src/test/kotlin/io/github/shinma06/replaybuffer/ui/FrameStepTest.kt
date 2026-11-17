package io.github.shinma06.replaybuffer.ui

import io.github.shinma06.replaybuffer.core.ReplayTimeline
import org.junit.jupiter.api.Test
import org.junit.jupiter.params.ParameterizedTest
import org.junit.jupiter.params.provider.CsvSource
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class FrameStepTest {
    private fun rules(vararg spans: Pair<Long, Long>, durationUs: Long = spans.lastOrNull()?.second ?: 0) =
        TrimRules(FrameGrid(spans.map { ReplayTimeline.Span(it.first, it.second) }), durationUs)

    private val gapped = rules(0L to 1_000_000L, 1_000_000L to 2_000_000L,
        3_000_000L to 4_000_000L, 4_000_000L to 5_000_000L)

    @Test fun rightFromAPausedGapSelectsTwoSecondsRatherThanSkippingToThree() {
        val current = rules(0L to 1_000_000L, 2_000_000L to 3_000_000L, 3_000_000L to 4_000_000L)
        val paused = current.played(current.initial(), 1_600_000)
        assertEquals(-1, current.grid.indexAt(paused.playheadUs))
        assertEquals(2_000_000L, current.step(paused, 1).playheadUs)
    }

    @ParameterizedTest
    @CsvSource(
        "2000000, -1, 1000000", "2000000, 1, 3000000",
        "2100000, -1, 1000000", "2100000, 1, 3000000",
        "2499999, -1, 1000000", "2499999, 1, 3000000",
        "2500000, -1, 1000000", "2500000, 1, 3000000",
        "2900000, -1, 1000000", "2900000, 1, 3000000",
        "2999999, -1, 1000000", "2999999, 1, 3000000",
    )
    fun eitherDirectionSelectsTheAdjacentFrameEverywhereInsideAGap(us: Long, delta: Int, expected: Long) {
        val paused = gapped.played(gapped.initial(), us)
        assertEquals(-1, gapped.grid.indexAt(us))
        val stepped = gapped.step(paused, delta)
        assertEquals(paused.copy(playheadUs = expected), stepped, "at $us, delta $delta")
        assertTrue(gapped.grid.indexAt(stepped.playheadUs) >= 0)
    }

    @ParameterizedTest
    @CsvSource(
        "500000, -1, 500000", "500000, 1, 900000",
        "900000, -1, 500000", "900000, 1, 2000000",
        "1200000, -1, 500000", "1200000, 1, 2000000",
        "2000000, -1, 900000", "2000000, 1, 2000000",
        "500000, 2, 2000000", "2000000, -2, 500000",
    )
    fun normalVfrFramesKeepTheExistingFrameCountAndClamping(us: Long, delta: Int, expected: Long) {
        val current = rules(500_000L to 800_000L, 900_000L to 1_300_000L, 2_000_000L to 2_000_001L)
        val paused = current.played(current.initial(), us)
        assertTrue(current.grid.indexAt(us) >= 0)
        assertEquals(expected, current.step(paused, delta).playheadUs)
    }

    @ParameterizedTest
    @CsvSource(
        "0, -1, 500000", "0, 1, 900000",
        "499999, -1, 500000", "499999, 1, 900000",
        "2000001, -1, 900000", "2000001, 1, 2000000",
        "3000000, -1, 900000", "3000000, 1, 2000000",
    )
    fun beforeFirstAndTerminalPositionsRetainTheirExistingSemantics(us: Long, delta: Int, expected: Long) {
        // This change is only for an interior gap. Do not redefine the existing end/before-first policies.
        val current = rules(500_000L to 800_000L, 900_000L to 1_300_000L, 2_000_000L to 2_000_001L,
            durationUs = 3_000_000)
        assertEquals(-1, current.grid.indexAt(us))
        assertEquals(expected, current.step(current.played(current.initial(), us), delta).playheadUs)
    }

    @Test fun steppingStillStaysInsideTheTrimRange() {
        var state = gapped.markIn(gapped.seek(gapped.initial(), 1_000_000))
        state = gapped.markOut(gapped.seek(state, 3_000_000))
        assertEquals(1_000_000L, state.inUs)
        assertEquals(4_000_000L, state.outUs)
        for (us in listOf(2_100_000L, 2_900_000L)) {
            val paused = gapped.played(state, us)
            val previous = gapped.step(paused, -1)
            val next = gapped.step(paused, 1)
            assertEquals(state.copy(playheadUs = 1_000_000), previous)
            assertEquals(state.copy(playheadUs = 3_000_000), next)
            assertEquals(previous, gapped.step(previous, -1))
            assertEquals(next, gapped.step(next, 1))
        }
    }

    @Test fun zeroDeltaDoesNotAcquireADirectionAndOtherSnappingIsUnchanged() {
        for ((us, expected) in listOf(2_100_000L to 1_000_000L, 2_900_000L to 3_000_000L)) {
            val paused = gapped.played(gapped.initial(), us)
            assertEquals(expected, gapped.step(paused, 0).playheadUs)
            assertEquals(expected, gapped.seek(gapped.initial(), us).playheadUs)
        }
    }

    @Test fun emptyGridSingleFrameAndActiveDragKeepTheirGuards() {
        val empty = rules()
        for (delta in listOf(-1, 1)) assertEquals(empty.initial(), empty.step(empty.initial(), delta))
        val single = rules(500_000L to 800_000L)
        for (us in listOf(0L, 500_000L, 799_999L, 800_000L)) for (delta in listOf(-1, 1)) {
            assertEquals(500_000L, single.step(single.played(single.initial(), us), delta).playheadUs)
        }
        val drag = gapped.beginDrag(gapped.initial(), Grip.IN, 0)
        for (delta in listOf(-1, 1)) assertEquals(drag, gapped.step(drag, delta))
    }
}
