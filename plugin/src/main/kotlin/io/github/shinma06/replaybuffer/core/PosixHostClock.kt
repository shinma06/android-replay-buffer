package io.github.shinma06.replaybuffer.core

import com.sun.jna.Memory
import com.sun.jna.Native
import com.sun.jna.Pointer

internal class PosixHostClock(private val darwin: Boolean) : HostClock {
    override val method = if (darwin) "mach_continuous" else "linux_boottime"

    private val timebase by lazy {
        Native.register(MachClock::class.java, "c")
        Memory(8).use {
            check(MachClock.mach_timebase_info(it) == 0) { "mach_timebase_info failed" }
            val numer = Integer.toUnsignedLong(it.getInt(0))
            val denom = Integer.toUnsignedLong(it.getInt(4))
            check(numer > 0 && denom > 0) { "Invalid Mach timebase" }
            Timebase(numer, denom)
        }
    }

    private val timespec by lazy {
        check(Native.POINTER_SIZE == 8 && Native.LONG_SIZE == 8 &&
            System.getProperty("os.arch") in setOf("amd64", "x86_64", "aarch64", "arm64")) { "Unsupported Linux timespec ABI" }
        Native.register(LibcClock::class.java, "c")
        Timespec(Memory(16), Memory(16))
    }

    @Synchronized
    override fun read(): HostReading {
        if (darwin) {
            val scale = timebase
            val awake = scale.nanos(MachClock.mach_absolute_time())
            val h = scale.nanos(MachClock.mach_continuous_time())
            return HostReading(h, awake, includesSuspend = true)
        }
        val buffers = timespec
        val awake = clockNanos(CLOCK_MONOTONIC, buffers.awake)
        val h = clockNanos(CLOCK_BOOTTIME, buffers.h)
        return HostReading(h, awake, includesSuspend = true)
    }

    private fun clockNanos(clock: Int, buffer: Memory): Long {
        check(LibcClock.clock_gettime(clock, buffer) == 0) { "clock_gettime($clock) failed: errno=${Native.getLastError()}" }
        val seconds = buffer.getLong(0)
        val nanos = buffer.getLong(8)
        check(seconds >= 0 && nanos in 0L until 1_000_000_000) { "Invalid Linux timespec" }
        return Math.addExact(Math.multiplyExact(seconds, 1_000_000_000), nanos)
    }

    private class Timespec(val awake: Memory, val h: Memory)

    private class Timebase(private val numer: Long, private val denom: Long) {
        fun nanos(ticks: Long): Long {
            val whole = java.lang.Long.divideUnsigned(ticks, denom)
            check(whole >= 0) { "Mach clock cannot be represented in nanoseconds" }
            // The two uint32 factors fit in 64 unsigned bits, even when the sign bit is set.
            val fraction = java.lang.Long.remainderUnsigned(ticks, denom) * numer
            val nanos = java.lang.Long.divideUnsigned(fraction, denom)
            return Math.addExact(Math.multiplyExact(whole, numer), nanos)
        }
    }

    private object LibcClock {
        @JvmStatic external fun clock_gettime(id: Int, result: Pointer): Int
    }

    private object MachClock {
        @JvmStatic external fun mach_continuous_time(): Long
        @JvmStatic external fun mach_absolute_time(): Long
        @JvmStatic external fun mach_timebase_info(result: Pointer): Int
    }

    private companion object {
        const val CLOCK_MONOTONIC = 1
        const val CLOCK_BOOTTIME = 7
    }
}
