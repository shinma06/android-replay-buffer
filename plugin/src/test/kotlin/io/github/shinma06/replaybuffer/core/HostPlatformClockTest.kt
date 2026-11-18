package io.github.shinma06.replaybuffer.core

import kotlin.math.abs
import kotlin.test.Test
import kotlin.test.assertTrue

class HostPlatformClockTest {
    @Test
    fun readsActualNativeClocks() {
        val clock = HostPlatform.hostClock
        val nativeMethods = setOf("linux_boottime", "mach_continuous", "windows_interrupt_precise")
        val first = clock.read()
        assertTrue(first.includesSuspend && clock.method in nativeMethods, "Native clock unavailable: ${clock.method}")
        var previous = first
        repeat(1_000) {
            val current = clock.read()
            assertNondecreasing(previous, current)
            previous = current
        }

        val before = clock.read()
        Thread.sleep(100)
        val after = clock.read()
        assertNondecreasing(before, after)
        val sleptH = after.h - before.h
        val gapChange = abs(sleptH - (after.awake - before.awake))

        repeat(20_000) { clock.read() }
        var start = System.nanoTime()
        previous = clock.read()
        var previousDuration = System.nanoTime() - start
        val timings = LongArray(100_000)
        var minH = Long.MAX_VALUE
        var minAwake = Long.MAX_VALUE
        var maxGapChange = 0L
        var maxShortReadGapChange = 0L
        var shortReadPairs = 0
        for (i in timings.indices) {
            start = System.nanoTime()
            val current = clock.read()
            val duration = System.nanoTime() - start
            timings[i] = duration
            assertNondecreasing(previous, current)
            val hStep = current.h - previous.h
            val awakeStep = current.awake - previous.awake
            if (hStep > 0) minH = minOf(minH, hStep)
            if (awakeStep > 0) minAwake = minOf(minAwake, awakeStep)
            val change = abs(hStep - awakeStep)
            maxGapChange = maxOf(maxGapChange, change)
            // A preempted read can give the two clocks different sample times.
            if (duration < ONE_MS_NS && previousDuration < ONE_MS_NS) {
                maxShortReadGapChange = maxOf(maxShortReadGapChange, change)
                shortReadPairs++
            }
            previous = current
            previousDuration = duration
        }
        val method = clock.method
        val dll = if (method == "windows_interrupt_precise") WindowsHostClock.registeredLibraryName else "-"
        timings.sort()
        println("HOST_CLOCK method=$method arch=${System.getProperty("os.arch")} dll=$dll " +
            "sleepH_ns=$sleptH gapChange_ns=$gapChange minH_ns=$minH minAwake_ns=$minAwake " +
            "maxPairGapChange_ns=$maxGapChange maxShortReadPairGapChange_ns=$maxShortReadGapChange " +
            "shortReadPairs=$shortReadPairs mean_ns=${timings.sum() / timings.size} p99_ns=${timings[98_999]}")

        assertTrue(method in nativeMethods, "Native clock fell back to $method")
        assertTrue(sleptH in 90_000_000L..2_000_000_000L, "Unexpected H delta after sleep: $sleptH ns")
        assertTrue(gapChange < ONE_MS_NS, "Paired H/A gap changed by $gapChange ns")
        assertTrue(shortReadPairs > 0, "No consecutive reads completed within 1 ms each")
        assertTrue(maxShortReadGapChange < ONE_MS_NS, "Consecutive short reads' H/A gap changed by $maxShortReadGapChange ns")
        assertTrue(minH in 1L..<ONE_MS_NS, "No positive H step smaller than 1 ms (min $minH ns)")
        assertTrue(minAwake in 1L..<ONE_MS_NS, "No positive A step smaller than 1 ms (min $minAwake ns)")
    }

    private fun assertNondecreasing(previous: HostReading, current: HostReading) {
        check(current.includesSuspend) { "Native clock switched to fallback" }
        check(current.h >= previous.h && current.awake >= previous.awake) { "Native clock moved backwards" }
    }

    private companion object {
        const val ONE_MS_NS = 1_000_000L
    }
}
