package io.github.shinma06.replaybuffer.core

import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit.SECONDS
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertTrue

class GuardedHostClockTest {
    private val second = 1_000_000_000L

    private class Native(private val next: () -> HostReading) : HostClock {
        override val method = "native"
        override fun read() = next()
    }

    private class Fixture(vararg readings: Long, next: () -> HostReading) {
        private val samples = readings.iterator()
        val failures = mutableListOf<Throwable>()
        val clock = GuardedHostClock(Native(next), { samples.nextLong() }) { failures.add(it) }
    }

    private fun native(h: Long, awake: Long = h) = HostReading(h, awake, includesSuspend = true)
    private fun fallback(h: Long) = HostReading(h, h, includesSuspend = false)

    @Test
    fun fallbackCarriesElapsedFromSuccessfulPair() {
        var calls = 0
        val f = Fixture(50 * second, 50 * second, 51 * second - 1, 51 * second, 51 * second + 1) {
            if (++calls != 1) throw UnsatisfiedLinkError("unavailable")
            native(100 * second, 95 * second)
        }
        assertEquals(native(100 * second, 95 * second), f.clock.read())
        assertEquals("native", f.clock.method)
        assertEquals(fallback(101 * second), f.clock.read())
        assertEquals("nanotime_fallback", f.clock.method)
        assertEquals(fallback(101 * second + 1), f.clock.read())
        assertEquals(2, calls)
        assertEquals(1, f.failures.size)
    }

    @Test
    fun fallbackUsesSuccessfulAfterNano() {
        var calls = 0
        val f = Fixture(50 * second, 50 * second + 2_000_000, 51 * second - 1, 51 * second) {
            if (++calls != 1) error("second native read failed")
            native(100 * second)
        }
        f.clock.read()
        assertEquals(fallback(100 * second + 998_000_000), f.clock.read())
    }

    @Test
    fun partialReadFailureKeepsPreviousRef() {
        var awake = 90L
        var calls = 0
        val f = Fixture(10, 12, 14, 16, 17) {
            awake++
            if (++calls != 1) error("awake succeeded but h failed")
            native(100, awake)
        }
        assertEquals(native(100, 91), f.clock.read())
        assertEquals(fallback(104), f.clock.read())
        assertEquals(fallback(105), f.clock.read())
    }

    @Test
    fun repeatedFallbackDoesNotRebase() {
        var calls = 0
        val f = Fixture(10, 11, 11, 11, 15, 13, 16) {
            if (++calls != 1) error("failed")
            native(100, 90)
        }
        f.clock.read()
        assertEquals(fallback(100), f.clock.read())
        assertEquals(fallback(104), f.clock.read())
        assertEquals(fallback(104), f.clock.read())
        assertEquals(fallback(105), f.clock.read())
        assertEquals(2, calls)
        assertEquals(1, f.failures.size)
    }

    @Test
    fun firstReadFailureUsesZeroOffset() {
        val f = Fixture(-11, -10, -10, -9) { error("failed") }
        assertEquals(fallback(-10), f.clock.read())
        assertEquals(fallback(-10), f.clock.read())
        assertEquals(fallback(-9), f.clock.read())
    }

    @Test
    fun concurrentReadsPublishOneMonotoneState() {
        var ticks = 0L
        var calls = 0
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val failures = mutableListOf<Throwable>()
        val clock = GuardedHostClock(Native {
            if (++calls == 1) {
                entered.countDown()
                check(release.await(10, SECONDS))
                native(100, 90)
            } else {
                error("failed")
            }
        }, { ++ticks }) { failures.add(it) }
        val executor = Executors.newFixedThreadPool(4)
        try {
            val first = executor.submit<HostReading> { clock.read() }
            assertTrue(entered.await(10, SECONDS))
            val later = List(3) { executor.submit<HostReading> { clock.read() } }
            release.countDown()
            assertEquals(native(100, 90), first.get(10, SECONDS))
            assertEquals(listOf(fallback(102), fallback(103), fallback(104)),
                later.map { it.get(10, SECONDS) }.sortedBy { it.h })
            assertEquals(2, calls)
            assertEquals(1, failures.size)
            assertEquals("nanotime_fallback", clock.method)
        } finally {
            release.countDown()
            executor.shutdownNow()
        }
    }

    @Test
    fun clampKeepsPublishedLastAndFallbackAwakeEqual() {
        var calls = 0
        val f = Fixture(10, 11, 12, 13, 14, 19, 16, 20) {
            native(if (++calls == 1) 100 else 99, 90)
        }
        assertEquals(native(100, 90), f.clock.read())
        assertEquals(fallback(103), f.clock.read())
        assertEquals(fallback(108), f.clock.read())
        assertEquals(fallback(108), f.clock.read())
        assertEquals(fallback(109), f.clock.read())
    }

    @Test
    fun nanoWrapUsesElapsedDifference() {
        var calls = 0
        val f = Fixture(Long.MAX_VALUE - 1, Long.MAX_VALUE, Long.MIN_VALUE, Long.MIN_VALUE + 1) {
            if (++calls != 1) error("failed")
            native(100)
        }
        assertEquals(native(100), f.clock.read())
        assertEquals(fallback(102), f.clock.read())
    }

    @Test
    fun rejectsUnrepresentableElapsedAndOverflow() {
        val elapsed = Fixture(0, 1, Long.MIN_VALUE + 1) { error("failed") }
        assertEquals(fallback(1), elapsed.clock.read())
        assertFailsWith<IllegalStateException> { elapsed.clock.read() }

        var calls = 0
        val sum = Fixture(10, 11, 12, 13) {
            if (++calls != 1) error("failed")
            native(Long.MAX_VALUE - 1)
        }
        sum.clock.read()
        assertFailsWith<ArithmeticException> { sum.clock.read() }
    }
}
