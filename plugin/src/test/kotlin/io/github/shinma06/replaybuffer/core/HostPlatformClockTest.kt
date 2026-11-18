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
        previous = clock.read()
        val timings = LongArray(100_000)
        var minH = Long.MAX_VALUE
        var minAwake = Long.MAX_VALUE
        for (i in timings.indices) {
            val start = System.nanoTime()
            val current = clock.read()
            timings[i] = System.nanoTime() - start
            assertNondecreasing(previous, current)
            val hStep = current.h - previous.h
            val awakeStep = current.awake - previous.awake
            if (hStep > 0) minH = minOf(minH, hStep)
            if (awakeStep > 0) minAwake = minOf(minAwake, awakeStep)
            previous = current
        }
        val method = clock.method
        val dll = if (method == "windows_interrupt_precise") WindowsHostClock.registeredLibraryName else "-"
        timings.sort()
        println("HOST_CLOCK method=$method arch=${System.getProperty("os.arch")} dll=$dll " +
            "sleepH_ns=$sleptH gapChange_ns=$gapChange minH_ns=$minH minAwake_ns=$minAwake " +
            "mean_ns=${timings.sum() / timings.size} p99_ns=${timings[98_999]}")

        assertTrue(method in nativeMethods, "Native clock fell back to $method")
        assertTrue(sleptH in 90_000_000L..2_000_000_000L, "Unexpected H delta after sleep: $sleptH ns")
        assertTrue(gapChange < 1_000_000, "Paired H/A gap changed by $gapChange ns")
        assertTrue(minH in 1L..<1_000_000L, "No positive H step smaller than 1 ms (min $minH ns)")
        assertTrue(minAwake in 1L..<1_000_000L, "No positive A step smaller than 1 ms (min $minAwake ns)")
    }

    private fun assertNondecreasing(previous: HostReading, current: HostReading) {
        check(current.includesSuspend) { "Native clock switched to fallback" }
        check(current.h >= previous.h && current.awake >= previous.awake) { "Native clock moved backwards" }
    }
}
