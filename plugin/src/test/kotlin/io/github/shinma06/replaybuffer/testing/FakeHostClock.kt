package io.github.shinma06.replaybuffer.testing

import io.github.shinma06.replaybuffer.core.HostClock
import io.github.shinma06.replaybuffer.core.HostReading

internal class FakeHostClock(
    var h: Long = 1_000_000_000_000,
    var awake: Long = 500_000_000_000,
    var includesSuspend: Boolean = true,
) : HostClock {
    var wallTimeMs: Long = 1_700_000_000_000
    override val method = "fake"
    override fun read() = HostReading(h, awake, includesSuspend)

    fun advance(ns: Long) {
        h += ns
        awake += ns
    }

    fun suspendFor(ns: Long) {
        h += ns
    }
}
