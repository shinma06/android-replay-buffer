package io.github.shinma06.replaybuffer.core

import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * v0.3.0 acceptance follow-up: clock-gap injection for crash autosave.
 * - A clock-read outage longer than 5 s (last good read -> recovery) still saves when the *hold*
 *   (first uncertain refresh -> recovery) stays within CRASH_RECEIPT_NS and the recovery read keeps
 *   the same clock region.
 * - A recovery read on a different boot (new region + segment) rejects held reports with
 *   NOT_SAVED_CLOCK_UNKNOWN and the gap stays unresolved.
 */
class ClockGapFollowupTest {
    @TempDir lateinit var root: Path

    @Test
    fun outageLongerThanFiveSecondsStillSavesWhenObservedHoldStaysWithinFiveSeconds() {
        CrashFixture(Files.createDirectory(root.resolve("long-gap"))).use { f ->
            f.clockMiss(11_000_000_000) // Invalid read: gap opens after the last good read (~10.001 s).
            f.refresh(14_900_000_000) // First observed staleness: hold starts here, not at the last good read.
            f.reporter(14_800_000_000, 14_900_000_000)
            // Received while the clock is stale: the Java report waits for the recovered mapping.
            f.log(javaCrash(wall = CRASH_WALL + 15_005_000_000), 15_200_000_000)
            f.log(platformCrash(wall = CRASH_WALL + 15_006_000_000), 15_300_000_000)
            assertEquals(AutoSaveState.WAITING, f.store.autosave.snapshot().state)
            // Recovery ~8 s after the last good read, but only ~3.1 s after the hold began: same region.
            f.anchor(18_000_000_000)
            f.store.clockStatus(true, 1)
            f.refresh(18_001_100_000)
            assertEquals(AutoSaveState.ARMED, f.store.autosave.snapshot().state)
            val pair = f.store.autosave.drain(f.at(18_200_000_000)).single()
            val capture = assertNotNull(f.store.capture(f.settings, pair, f.at(18_200_000_000)))
            assertNotNull(capture.autoTrigger)
            val clockGaps = capture.gaps.filter { it.stream == "clock" }
            assertEquals(1, clockGaps.size)
            assertTrue(clockGaps.single().resolvedClock(capture.clockRecoveries))
            assertEquals(0, f.store.autosave.snapshot().notSavedCount)
        }
    }

    @Test
    fun recoveryOnAnotherBootRejectsHeldReportsAndLeavesTheGapUnresolved() {
        CrashFixture(Files.createDirectory(root.resolve("boot-gap"))).use { f ->
            f.clockMiss(11_000_000_000)
            f.refresh(14_900_000_000)
            f.reporter(14_800_000_000, 14_900_000_000)
            f.log(javaCrash(wall = CRASH_WALL + 15_005_000_000), 15_200_000_000)
            f.log(platformCrash(wall = CRASH_WALL + 15_006_000_000), 15_300_000_000)
            val opened = f.store.clock.snapshot().map { it.region }.distinct()
            // The device rebooted during the outage: a different boot id opens a new region and segment.
            assertTrue(f.store.clock.add("8",
                listOf(1_000_000_000L, 1_000_000_000L, CRASH_WALL + 18_000_000_000, 1_000_000_000),
                f.at(18_000_000_000), f.at(18_001_000_000)))
            f.store.clockStatus(true, 1)
            f.refresh(18_001_100_000)
            val snapshot = f.store.autosave.snapshot()
            assertEquals(AutoSaveReason.NOT_SAVED_CLOCK_UNKNOWN, snapshot.lastOutcome?.reason)
            assertEquals(2, snapshot.notSavedCount)
            assertTrue(f.store.autosave.drain(f.at(18_200_000_000)).isEmpty())
            val capture = assertNotNull(f.store.capture(f.settings, host = f.at(18_300_000_000)))
            val clockGap = capture.gaps.single { it.stream == "clock" }
            assertTrue(!clockGap.resolvedClock(capture.clockRecoveries))
            assertNull(capture.autoTrigger)
            assertTrue(f.store.clock.snapshot().map { it.region }.distinct().size > opened.size)
        }
    }
}
