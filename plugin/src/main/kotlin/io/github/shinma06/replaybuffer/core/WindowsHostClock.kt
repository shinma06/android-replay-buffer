package io.github.shinma06.replaybuffer.core

import com.sun.jna.Memory
import com.sun.jna.Native
import com.sun.jna.Pointer

internal object WindowsHostClock : HostClock {
    override val method = "windows_interrupt_precise"

    private val binding by lazy {
        check(Native.POINTER_SIZE == 8) { "Unsupported Windows clock ABI" }
        var failure: UnsatisfiedLinkError? = null
        for (dll in listOf("kernel32", "kernelbase", "api-ms-win-core-realtime-l1-1-1")) {
            try {
                Native.register(PreciseClock::class.java, dll)
                return@lazy Binding(dll, Memory(8), Memory(8))
            } catch (e: UnsatisfiedLinkError) {
                if (failure == null) failure = e else failure.addSuppressed(e)
            }
        }
        throw checkNotNull(failure)
    }

    internal val registeredLibraryName: String get() = binding.dll

    @Synchronized
    override fun read(): HostReading {
        val buffers = binding
        PreciseClock.QueryUnbiasedInterruptTimePrecise(buffers.awake)
        val awake = ticksToNanos(buffers.awake)
        PreciseClock.QueryInterruptTimePrecise(buffers.h)
        return HostReading(ticksToNanos(buffers.h), awake, includesSuspend = true)
    }

    private fun ticksToNanos(buffer: Memory): Long {
        val ticks = buffer.getLong(0)
        check(ticks >= 0) { "Windows clock cannot be represented in nanoseconds" }
        return Math.multiplyExact(ticks, 100)
    }

    private class Binding(val dll: String, val awake: Memory, val h: Memory)

    private object PreciseClock {
        @JvmStatic external fun QueryInterruptTimePrecise(result: Pointer)
        @JvmStatic external fun QueryUnbiasedInterruptTimePrecise(result: Pointer)
    }
}
