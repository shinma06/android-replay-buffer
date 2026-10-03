package io.github.shinma06.replaybuffer.core

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull
import kotlin.test.assertTrue

class CaptureClockTest {
    private fun add(clock: CaptureClock, elapsed: Long, mono: Long = elapsed, wall: Long = 1_700_000_000_000_000_000 + elapsed,
                    host: Long = elapsed, boot: String = "1", hostWall: Long = host) {
        clock.add(boot, listOf(elapsed, mono, wall, elapsed + 100_000), host, host + 200_000, hostWall)
    }

    @Test
    fun recoveredAnchorNeverPaintsTheUnobservedIntervalAsNormallySynchronized() {
        val clock = CaptureClock()
        add(clock, 1_000_000_000)
        val uncertain = clock.video(7_000_000, 7_000_000_000)
        assertNull(uncertain.elapsed)
        add(clock, 11_000_000_000)
        assertEquals(2, clock.snapshot().map { it.epoch }.distinct().size)
        assertNull(clock.video(7_000_000, 7_000_000_000, uncertain.epoch).elapsed)
        assertNull(clock.log(1_700_000_007_000_000_000, 7_000_000_000).elapsed)
        assertTrue(clock.video(11_000_000, 11_000_200_000).elapsed != null)
    }

    @Test
    fun slowRoundTripAdvancesEstimatedWindowButDoesNotClaimNormalSourceSynchronization() {
        val clock = CaptureClock()
        assertEquals(false, clock.add("1", listOf(1_000_000_000L, 1_000_000_000L, 1_700_000_001_000_000_000L, 1_000_000_000L),
            1_000_000_000, 1_100_000_000, 1_100_000_000))
        assertEquals(false, clock.add("1", listOf(2_000_000_000L, 2_000_000_000L, 1_700_000_002_000_000_000L, 2_000_000_000L),
            2_000_000_000, 2_100_000_000, 2_100_000_000))
        assertTrue(clock.now(2_200_000_000)!! >= 1_000_000_000)
        assertNull(clock.video(2_000_000, 2_200_000_000).elapsed)
    }

    @Test
    fun videoAndLogUseTheSameElapsedWithInterpolationAndIgnoreTinyOffsetChanges() {
        val clock = CaptureClock()
        add(clock, 1_000_000_000)
        add(clock, 2_000_000_000, mono = 1_999_800_000)
        assertEquals(1, clock.snapshot().map { it.epoch }.distinct().size)
        val video = clock.video(1_500_000, 2_000_200_000)
        val log = clock.log(1_700_000_001_500_000_000, 2_000_200_000)
        assertTrue(kotlin.math.abs(video.elapsed!! - 1_500_150_000) <= 100)
        assertEquals(1_500_050_000, log.elapsed)
        assertTrue(video.uncertainty < 20_000_000)
        assertNull(clock.video(10_000_000, 10_000_000_000).elapsed)
    }

    @Test
    fun backwardWallJumpMakesOverlappingEpochRecordsAmbiguousAndBootKeepsSequence() {
        val clock = CaptureClock()
        add(clock, 1_000_000_000)
        add(clock, 2_000_000_000)
        add(clock, 3_000_000_000, wall = 1_700_000_001_000_000_000)
        add(clock, 4_000_000_000, wall = 1_700_000_002_000_000_000)
        assertNull(clock.log(1_700_000_001_500_000_000, 4_000_200_000).elapsed)
        add(clock, 500_000_000, host = 5_000_000_000, boot = "2")
        val afterBoot = clock.video(500_000, 5_000_200_000)
        assertEquals(500_050_000, afterBoot.elapsed)
        assertTrue(afterBoot.sequence!! > 3_000_000_000)
        add(clock, 200_000_000, host = 6_000_000_000, boot = "3", hostWall = 100_000_000_000)
        assertNull(clock.video(200_000, 6_000_200_000).sequence)
        assertEquals(6, clock.snapshot().size)
    }
}
