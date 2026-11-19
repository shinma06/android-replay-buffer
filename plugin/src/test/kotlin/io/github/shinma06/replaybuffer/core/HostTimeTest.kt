package io.github.shinma06.replaybuffer.core

import io.github.shinma06.replaybuffer.testing.FakeHostClock
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class HostTimeTest {
    private val clock = FakeHostClock()
    private val events = mutableListOf<HostSuspend>()
    private val time = HostTime(clock, thresholdNs = 100).apply { onSuspend = { events.add(it) } }

    @Test
    fun firstReadNeverReportsSuspend() {
        assertEquals(clock.h, time.now())
        assertTrue(events.isEmpty())
    }

    @Test
    fun noSuspendWhenBothAdvanceEqually() {
        time.now()
        clock.advance(101)
        assertEquals(clock.h, time.now())
        assertTrue(events.isEmpty())
    }

    @Test
    fun wallClockStepsDoNotAffectElapsedClocks() {
        val first = clock.read()
        assertEquals(first.h, time.now())
        clock.wallTimeMs += 3_600_000
        clock.advance(101)
        assertEquals(first.h + 101, time.now())
        assertEquals(first.awake + 101, clock.read().awake)
        clock.wallTimeMs -= 7_200_000
        clock.advance(101)
        assertEquals(first.h + 202, time.now())
        assertEquals(first.awake + 202, clock.read().awake)
        assertTrue(events.isEmpty())
    }

    @Test
    fun suspendDetectedWhenGapGrowsBeyondThreshold() {
        time.now()
        clock.suspendFor(101)
        time.now()
        clock.advance(100)
        time.now()
        assertEquals(101L, events.single().asleepNs)
    }

    @Test
    fun growthExactlyAtThresholdIsNotSuspend() {
        time.now()
        clock.suspendFor(100)
        time.now()
        assertTrue(events.isEmpty())
    }

    @Test
    fun negativeGrowthIsIgnored() {
        time.now()
        clock.h += 100
        clock.awake += 201
        time.now()
        assertTrue(events.isEmpty())
    }

    @Test
    fun fallbackClockNeverReportsSuspend() {
        time.now()
        clock.includesSuspend = false
        clock.suspendFor(101)
        time.now()
        clock.suspendFor(101)
        time.now()
        clock.includesSuspend = true
        clock.suspendFor(101)
        time.now()
        assertTrue(events.isEmpty())
    }

    @Test
    fun suspendSpanIsPreviousReadToCurrentRead() {
        time.now()
        clock.advance(40)
        val from = time.now()
        clock.advance(30)
        clock.suspendFor(101)
        val to = time.now()
        assertEquals(listOf(HostSuspend(from, to, 101)), events)
    }

    @Test
    fun delayBetweenNativeReadsAlsoProducesSuspendCandidate() {
        val defaultTime = HostTime(clock).apply { onSuspend = { events.add(it) } }
        val from = defaultTime.now()
        clock.advance(10_000_000)
        // No actual suspend: awake is sampled before a scheduler delay and H after it.
        clock.h += 150_000_000
        val to = defaultTime.now()
        assertEquals(listOf(HostSuspend(from, to, 150_000_000)), events)
    }

    @Test
    fun clampRetainsPublishedValueInLastReading() {
        val from = time.now()
        clock.h -= 50
        assertEquals(from, time.now())
        clock.h = from + 101
        val to = time.now()
        assertEquals(listOf(HostSuspend(from, to, 101)), events)
    }
}
