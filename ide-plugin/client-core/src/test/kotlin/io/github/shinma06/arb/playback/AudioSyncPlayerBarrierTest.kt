package io.github.shinma06.arb.playback

import org.junit.jupiter.api.Test
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotEquals
import kotlin.test.assertTrue

/**
 * Barrier-driven race tests for [AudioSyncPlayer] (no sleeps for synchronisation): a `write` that
 * stays blocked while the player is closed, a player attached after its controller was closed, and
 * the guarantee that an unavailable audio path never stops the video clock.
 */
class AudioSyncPlayerBarrierTest {
    private fun pcm(durationMs: Int, sampleRateHz: Int = 10_000): ByteArray = ByteArray(sampleRateHz * durationMs / 1000 * 4)

    private fun source(durationMs: Int = 1_000) = InMemoryPcmSource(10_000, 2, pcm(durationMs))

    /** A sink whose first [blockWrites] `write` calls park on [gate] (interruptibly) after signalling [entered]. */
    private class BarrierSink(blockWrites: Int = 1) : AudioSink {
        override val isAvailable: Boolean = true
        val gate = CountDownLatch(1)
        val entered = CountDownLatch(1)
        private val remainingBlocks = AtomicInteger(blockWrites)
        val writeCalls = AtomicInteger()
        val closed = AtomicBoolean(false)
        val closeCalls = AtomicInteger()
        @Volatile var writer: Thread? = null

        override fun open(format: PcmFormat): Boolean = true
        override fun start() = Unit
        override fun stop() = Unit
        override fun flush() = Unit

        override fun write(pcm: ByteArray, offset: Int, length: Int): Int {
            writer = Thread.currentThread()
            writeCalls.incrementAndGet()
            if (remainingBlocks.getAndDecrement() > 0) {
                entered.countDown()
                gate.await(10, TimeUnit.SECONDS)
            }
            return length
        }

        override fun close() {
            closed.set(true)
            closeCalls.incrementAndGet()
        }
    }

    private class VirtualClock : PlaybackClock {
        private val now = AtomicLong(1_000_000_000L)
        override fun nowNs(): Long = now.get()
        fun advanceMs(ms: Long) {
            now.addAndGet(ms * 1_000_000L)
        }
    }

    private class NoopSeeker : FrameSeeker {
        override fun seekExact(frameIndex: Long) = FrameSeekResult(frameIndex, frameIndex, false)
        override fun scrubApprox(frameIndex: Long) = FrameSeekResult(frameIndex, frameIndex, false)
    }

    @Test
    fun closeWhileAManualPumpIsBlockedInWriteNeverWritesAgain() {
        val sink = BarrierSink()
        val player = AudioSyncPlayer(source = source(), sink = sink, autoRunPump = false)
        player.play()
        val pump = Thread { player.pumpNextSlice() }.apply { isDaemon = true; start() }
        assertTrue(sink.entered.await(5, TimeUnit.SECONDS), "pump must be inside write")

        player.close() // close while the device write is still stuck
        assertTrue(sink.closed.get())
        val writesAtClose = sink.writeCalls.get()
        assertEquals(1, writesAtClose)

        sink.gate.countDown() // the stale write finally returns
        pump.join(5_000)
        assertFalse(pump.isAlive)
        assertEquals(0, player.pumpNextSlice(), "a closed player never pumps")
        player.play()
        player.seek(0)
        assertEquals(0, player.pumpNextSlice())
        assertEquals(writesAtClose, sink.writeCalls.get(), "no write may start after close")
        assertNotEquals(AudioState.PLAYING, player.audioState)
    }

    @Test
    fun closeWithTheBackgroundPumpStuckInWriteIsBoundedAndStopsTheThread() {
        val sink = BarrierSink()
        val player = AudioSyncPlayer(source = source(), sink = sink, autoRunPump = true)
        player.play()
        assertTrue(sink.entered.await(5, TimeUnit.SECONDS), "background pump must reach the blocked write")

        val started = System.nanoTime()
        player.close() // interrupts the parked pump after the join timeout
        val elapsedMs = (System.nanoTime() - started) / 1_000_000
        assertTrue(elapsedMs < 6_000, "close must be bounded even if the device write hangs: ${elapsedMs}ms")
        assertTrue(sink.closed.get())
        sink.gate.countDown()
        val pump = requireNotNull(sink.writer)
        pump.join(5_000)
        assertFalse(pump.isAlive, "no leaked pump")
        assertEquals(1, sink.writeCalls.get(), "exactly the one in-flight write, nothing after close")
    }

    @Test
    fun seekDuringABlockedWriteDropsTheStaleSliceAndOnlyNewPositionIsWrittenNext() {
        val flushes = AtomicInteger()
        val sink = object : AudioSink {
            private val inner = BarrierSink()
            val barrier get() = inner
            override val isAvailable = true
            override fun open(format: PcmFormat) = true
            override fun start() = Unit
            override fun stop() = Unit
            override fun flush() {
                flushes.incrementAndGet()
            }
            override fun write(pcm: ByteArray, offset: Int, length: Int) = inner.write(pcm, offset, length)
            override fun close() = inner.close()
        }
        val player = AudioSyncPlayer(source = source(), sink = sink, autoRunPump = false)
        player.play()
        val pump = Thread { player.pumpNextSlice() }.apply { isDaemon = true; start() }
        assertTrue(sink.barrier.entered.await(5, TimeUnit.SECONDS))
        player.seek(500_000_000L) // user seeks while the old slice is still in the device
        val flushesAfterSeek = flushes.get()
        sink.barrier.gate.countDown()
        pump.join(5_000)
        assertTrue(flushes.get() > flushesAfterSeek, "the stale slice written after the seek must be flushed")
        assertEquals(500_000_000L, player.audioPositionNs, "the stale write must not advance the new position")
        player.close()
    }

    // ---------------- late attach ----------------

    @Test
    fun attachingAfterTheControllerClosedClosesThePlayerAndNeverWrites() {
        val sink = BarrierSink(blockWrites = 0)
        val player = AudioSyncPlayer(source = source(), sink = sink, autoRunPump = false)
        val controller = PlaybackController(10, NoopSeeker(), clock = VirtualClock(), autoRunLoop = false)
        controller.close()

        controller.attachAudioPlayer(player) // the decode finished after the tab closed
        assertTrue(sink.closed.get(), "a rejected late player must be closed, not leaked")
        controller.play()
        assertEquals(0, player.pumpNextSlice())
        assertEquals(0, sink.writeCalls.get(), "no PCM may reach the device after close")
    }

    @Test
    fun attachRacingCloseEndsWithExactlyOneClosedPlayerAndNoWriteAfterClose() {
        repeat(30) {
            val sink = BarrierSink(blockWrites = 0)
            val player = AudioSyncPlayer(source = source(), sink = sink, autoRunPump = false)
            val controller = PlaybackController(10, NoopSeeker(), clock = VirtualClock(), autoRunLoop = false)
            controller.play()
            val start = CountDownLatch(1)
            val attacher = Thread { start.await(); controller.attachAudioPlayer(player) }.apply { isDaemon = true; start() }
            val closer = Thread { start.await(); controller.close() }.apply { isDaemon = true; start() }
            start.countDown()
            attacher.join(5_000)
            closer.join(5_000)
            assertFalse(attacher.isAlive || closer.isAlive)
            assertTrue(controller.isClosed)
            assertEquals(1, sink.closeCalls.get(), "whichever order won, the attached player must be closed exactly once")
            val writes = sink.writeCalls.get()
            player.pumpNextSlice()
            assertEquals(writes, sink.writeCalls.get(), "no write after the controller closed")
        }
    }

    // ---------------- audio unavailable keeps the video clock ----------------

    private class DownSink : AudioSink {
        override val isAvailable: Boolean = false
        override fun open(format: PcmFormat) = false
        override fun start() = Unit
        override fun stop() = Unit
        override fun flush() = Unit
        override fun write(pcm: ByteArray, offset: Int, length: Int) = 0
        override fun close() = Unit
    }

    @Test
    fun unavailableAudioNeverStallsTheVideoClock() {
        val clock = VirtualClock()
        val player = AudioSyncPlayer(source = source(), sink = DownSink(), autoRunPump = false)
        assertEquals(AudioState.UNAVAILABLE, player.audioState)
        // 20 frames, 100 ms each.
        val pts = List(20) { 10_000_000_000L + it * 100_000_000L }
        val controller = PlaybackController(20, NoopSeeker(), framePtsNs = pts, clock = clock, autoRunLoop = false)
        controller.attachAudioPlayer(player)
        controller.play()
        var last = controller.state.currentFrameIndex
        repeat(8) {
            clock.advanceMs(250)
            controller.tick()
            val now = controller.state.currentFrameIndex
            assertTrue(now >= last, "video clock must keep advancing: $last -> $now")
            last = now
        }
        assertTrue(last >= 1L, "video advanced although audio is unavailable")
        assertEquals(AudioState.UNAVAILABLE, controller.state.audioState)
        controller.close()
    }

    @Test
    fun audioPumpFailureMidPlaybackIsReportedUnavailableWhileVideoContinues() {
        val clock = VirtualClock()
        val inner = source()
        val failRead = CountDownLatch(1)
        val flaky = object : PcmAudioSource by inner {
            override fun readPcmSlice(startNs: Long, endNs: Long): ByteArray {
                check(failRead.await(10, TimeUnit.SECONDS))
                throw java.io.IOException("decoder died")
            }
        }
        val sink = BarrierSink(blockWrites = 0)
        val player = AudioSyncPlayer(source = flaky, sink = sink, autoRunPump = true)
        val pts = List(20) { 10_000_000_000L + it * 100_000_000L }
        val controller = PlaybackController(20, NoopSeeker(), framePtsNs = pts, clock = clock, autoRunLoop = false)
        controller.attachAudioPlayer(player)
        controller.play()
        failRead.countDown()
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
        while (player.audioState != AudioState.UNAVAILABLE && System.nanoTime() < deadline) Thread.sleep(5)
        assertEquals(AudioState.UNAVAILABLE, player.audioState, "the failure is surfaced, not silent")

        val before = controller.state.currentFrameIndex
        clock.advanceMs(600)
        controller.tick()
        assertTrue(controller.state.currentFrameIndex > before, "video keeps playing without audio")
        controller.close()
    }
}
