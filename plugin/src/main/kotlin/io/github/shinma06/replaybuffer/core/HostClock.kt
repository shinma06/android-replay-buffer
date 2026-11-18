package io.github.shinma06.replaybuffer.core

import java.util.logging.Logger

/** A pair of monotonic host readings in nanoseconds; fallback cannot observe suspend. */
internal data class HostReading(val h: Long, val awake: Long, val includesSuspend: Boolean)

internal interface HostClock {
    val method: String
    fun read(): HostReading
}

/** The half-open interval between reads that bracket a possible suspend. */
internal data class HostSuspend(val fromH: Long, val toH: Long, val asleepNs: Long)

internal class HostTime(private val clock: HostClock, private val thresholdNs: Long = 100_000_000) {
    /** Called under the read lock; the receiver must not read this clock again. */
    @Volatile var onSuspend: (HostSuspend) -> Unit = {}
    private var last: HostReading? = null
    val method: String get() = clock.method

    @Synchronized
    fun now(): Long {
        val previous = last
        val reading = clock.read()
        val h = maxOf(previous?.h ?: reading.h, reading.h)
        val current = if (h == reading.h) reading else
            reading.copy(h = h, awake = if (reading.includesSuspend) reading.awake else h)
        last = current
        if (previous != null && previous.includesSuspend && current.includesSuspend) {
            val grown = (h - previous.h) - (current.awake - previous.awake)
            if (grown > thresholdNs) onSuspend(HostSuspend(previous.h, h, grown))
        }
        return h
    }
}

internal object NanoTimeHostClock : HostClock {
    override val method = "nanotime_fallback"
    override fun read(): HostReading = System.nanoTime().let { HostReading(it, it, includesSuspend = false) }
}

/** Publishes one time axis per instance, even when the native source permanently fails. */
internal class GuardedHostClock(
    private val native: HostClock,
    private val nano: () -> Long = System::nanoTime,
    private val reportFailure: (Throwable) -> Unit = {
        Logger.getLogger(GuardedHostClock::class.java.name)
            .warning("Host clock switched to nanotime_fallback: ${it.javaClass.simpleName}: ${it.message}")
    },
) : HostClock {
    private var failed = false
    private var hasLast = false
    private var lastH = 0L
    private var lastAwake = 0L
    private var refH = 0L
    private var refNanoBefore = 0L
    private var refNanoAfter = 0L

    override val method: String
        @Synchronized get() = if (failed) NanoTimeHostClock.method else native.method

    @Synchronized
    override fun read(): HostReading {
        if (failed) return fallback(nano())
        val failure = try {
            val before = nano()
            val reading = native.read()
            val after = nano()
            check(after - before >= 0) { "Native read interval is not representable" }
            check(reading.includesSuspend && reading.h >= 0 && reading.awake >= 0 &&
                (!hasLast || (reading.h >= lastH && reading.awake >= lastAwake))) { "Invalid native host clock reading" }
            refH = reading.h
            refNanoBefore = before
            refNanoAfter = after
            lastH = reading.h
            lastAwake = reading.awake
            hasLast = true
            return reading
        } catch (e: LinkageError) {
            e
        } catch (e: Exception) {
            e
        }

        val atFailure = nano()
        if (!hasLast) {
            refH = atFailure
            refNanoBefore = atFailure
            refNanoAfter = atFailure
        }
        failed = true
        reportFailure(failure)
        return fallback(atFailure)
    }

    private fun fallback(now: Long): HostReading {
        // Use the successful read's trailing bracket, not the failed call's time.
        val elapsed = now - refNanoAfter
        check(elapsed >= 0) { "Fallback interval is not representable" }
        val raw = Math.addExact(refH, elapsed)
        val h = if (hasLast) maxOf(lastH, raw) else raw
        lastH = h
        hasLast = true
        return HostReading(h, h, includesSuspend = false)
    }
}
