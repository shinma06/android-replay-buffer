package io.github.shinma06.arb.playback

import org.junit.jupiter.api.Test
import java.util.Random
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicLong
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/**
 * Deterministic state-machine tests for [PlaybackController] (virtual clock, `autoRunLoop = false`,
 * explicit `tick()`): no wall-clock sleeps, no flakiness.
 *
 * Properties checked on every transition of a seeded random operation stream over a VFR timeline
 * (long static holds):
 *  - P1 time inside the selection never moves backwards, except a loop wrap or an explicit user
 *    action (seek / step / scrub / trim / restart-at-end);
 *  - P2 `PLAYING -> PAUSED` happens only for an explicit pause/step/scrub, the terminal end of a
 *    non-looping selection, an explained error (`onPlaybackError`) or close;
 *  - P3 a seek completion that belongs to an older generation never overwrites a newer position.
 */
class PlaybackStateMachineTest {
    private class ManualClock(initialNs: Long = 1_000_000_000L) : PlaybackClock {
        private val now = AtomicLong(initialNs)
        override fun nowNs(): Long = now.get()
        fun advanceMs(ms: Long) {
            now.addAndGet(ms * 1_000_000L)
        }
    }

    /** Synchronous exact seeker; can be told to fail specific frames with a non-retryable error. */
    private class ScriptedSeeker : FrameSeeker {
        val exactCalls = CopyOnWriteArrayList<Long>()
        @Volatile var failFrames: Set<Long> = emptySet()

        override fun seekExact(frameIndex: Long): FrameSeekResult {
            exactCalls.add(frameIndex)
            if (frameIndex in failFrames) throw IllegalStateException("decoder rejected #$frameIndex")
            return FrameSeekResult(frameIndex, frameIndex, isApproximate = false)
        }

        override fun scrubApprox(frameIndex: Long): FrameSeekResult =
            FrameSeekResult(frameIndex, (frameIndex / 4) * 4, isApproximate = frameIndex % 4 != 0L)
    }

    /**
     * VFR timeline: frame 0 held 5 s, 100 ms steps, a 3 s hold before frame 11, then 100 ms steps.
     */
    private fun vfrPtsNs(): List<Long> {
        val ms = ArrayList<Long>()
        ms += 0L
        var t = 5_000L
        for (i in 1..10) {
            ms += t
            t += 100L
        }
        t += 2_900L
        for (i in 11..19) {
            ms += t
            t += 100L
        }
        return ms.map { 10_000_000_000L + it * 1_000_000L }
    }

    private sealed class Op(val label: String) {
        object Play : Op("play")
        object Pause : Op("pause")
        class Tick(val ms: Long) : Op("tick+${ms}ms")
        class Seek(val frame: Long) : Op("seek($frame)")
        class Step(val delta: Int) : Op("step($delta)")
        class Scrub(val frame: Long) : Op("scrub($frame)")
        class SetIn(val frame: Long) : Op("setIn($frame)")
        class SetOut(val frame: Long) : Op("setOut($frame)")
        object ToggleLoop : Op("toggleLoop")
        class Speed(val speed: PlaybackSpeed) : Op("speed(${speed.name})")
    }

    private fun randomOp(rnd: Random, total: Long): Op = when (rnd.nextInt(100)) {
        in 0..17 -> Op.Play
        in 18..27 -> Op.Pause
        in 28..61 -> Op.Tick(listOf(1L, 40L, 120L, 900L, 2_500L, 6_000L)[rnd.nextInt(6)])
        in 62..71 -> Op.Seek(rnd.nextInt(total.toInt()).toLong())
        in 72..76 -> Op.Step(if (rnd.nextBoolean()) 1 else -1)
        in 77..82 -> Op.Scrub(rnd.nextInt(total.toInt()).toLong())
        in 83..87 -> Op.SetIn(rnd.nextInt(total.toInt()).toLong())
        in 88..92 -> Op.SetOut(rnd.nextInt(total.toInt()).toLong())
        in 93..96 -> Op.ToggleLoop
        else -> Op.Speed(PlaybackSpeed.entries[rnd.nextInt(PlaybackSpeed.entries.size)])
    }

    private fun runStream(seed: Long, steps: Int, withFaults: Boolean) {
        val rnd = Random(seed)
        val clock = ManualClock()
        val seeker = ScriptedSeeker()
        val errors = CopyOnWriteArrayList<Throwable>()
        val pts = vfrPtsNs()
        val total = pts.size.toLong()
        if (withFaults) seeker.failFrames = setOf(rnd.nextInt(total.toInt()).toLong())
        val ctrl = PlaybackController(
            totalFrames = total,
            frameSeeker = seeker,
            framePtsNs = pts,
            clock = clock,
            autoRunLoop = false,
            onPlaybackError = errors::add,
        )
        val trace = StringBuilder()
        ctrl.use {
            repeat(steps) { step ->
                val op = randomOp(rnd, total)
                val before = ctrl.state
                val errorsBefore = errors.size
                var threw = false
                try {
                    when (op) {
                        Op.Play -> ctrl.play()
                        Op.Pause -> ctrl.pause()
                        is Op.Tick -> {
                            clock.advanceMs(op.ms)
                            ctrl.tick()
                        }
                        is Op.Seek -> ctrl.seekToFrame(op.frame)
                        is Op.Step -> ctrl.stepFrame(op.delta)
                        is Op.Scrub -> {
                            ctrl.beginScrub()
                            ctrl.scrubTo(op.frame)
                            ctrl.endScrub(op.frame)
                        }
                        is Op.SetIn -> ctrl.setInFrame(op.frame)
                        is Op.SetOut -> ctrl.setOutFrame(op.frame)
                        Op.ToggleLoop -> ctrl.toggleLoop()
                        is Op.Speed -> ctrl.setSpeed(op.speed)
                    }
                } catch (e: IllegalStateException) {
                    // A synchronous user-initiated seek surfaces the decoder error to its caller.
                    threw = true
                }
                val after = ctrl.state
                trace.append(step).append(':').append(op.label).append(" ")
                    .append(before.mode).append('@').append(before.currentFrameIndex)
                    .append("->").append(after.mode).append('@').append(after.currentFrameIndex).append('\n')
                val ctx = { "seed=$seed step=$step op=${op.label}\n$trace" }

                // Structural invariants.
                assertTrue(after.inFrameIndex <= after.outFrameIndex, "in<=out ${ctx()}")
                assertTrue(after.currentFrameIndex in 0 until total, "current in range ${ctx()}")
                assertTrue(after.outFrameIndex < total && after.inFrameIndex >= 0, "selection in range ${ctx()}")

                val explained = threw || errors.size > errorsBefore
                if (op is Op.Tick) {
                    if (before.mode == PlaybackMode.PLAYING) {
                        if (after.mode == PlaybackMode.PLAYING) {
                            val wrapped = before.loopInSelection && after.currentFrameIndex == before.inFrameIndex
                            assertTrue(
                                after.currentFrameIndex >= before.currentFrameIndex || wrapped,
                                "P1 time went backwards without a loop wrap ${ctx()}",
                            )
                            assertTrue(after.currentFrameIndex <= after.outFrameIndex, "P1 left the selection ${ctx()}")
                        } else {
                            val terminal = !before.loopInSelection && after.currentFrameIndex == after.outFrameIndex
                            assertTrue(terminal || explained, "P2 unexplained stop ${ctx()}")
                        }
                    } else {
                        assertEquals(before.currentFrameIndex, after.currentFrameIndex, "idle tick must not move ${ctx()}")
                        assertEquals(before.mode, after.mode)
                    }
                }
                if (before.mode == PlaybackMode.PLAYING && after.mode == PlaybackMode.PAUSED) {
                    val stopper = op is Op.Pause || op is Op.Step || op is Op.Scrub || op is Op.Tick
                    assertTrue(stopper || explained, "P2 stop by a non-stopping action ${ctx()}")
                }
                if (op is Op.Seek && !explained) {
                    assertEquals(op.frame, after.currentFrameIndex, "seek lands exactly ${ctx()}")
                    assertEquals(op.frame, after.displayedFrameIndex, "exact seek displays the frame ${ctx()}")
                    assertFalse(after.isApproximate, "exact seek clears approximate ${ctx()}")
                    assertEquals(before.mode, after.mode, "seek keeps the mode ${ctx()}")
                }
                if (op is Op.Pause && before.mode == PlaybackMode.PLAYING) {
                    assertEquals(before.currentFrameIndex, after.currentFrameIndex, "pause freezes the position ${ctx()}")
                }
                if (op is Op.Speed || op is Op.ToggleLoop) {
                    assertEquals(before.mode, after.mode, "setting changes never stop playback ${ctx()}")
                    assertEquals(before.currentFrameIndex, after.currentFrameIndex, "setting changes never move ${ctx()}")
                }
            }
        }
        assertTrue(ctrl.isClosed)
        assertEquals(PlaybackMode.PAUSED, ctrl.state.mode, "close always settles paused")
    }

    @Test
    fun randomOperationStreamsKeepTimeMonotonicAndStopsExplainable() {
        for (seed in 1L..150L) runStream(seed, steps = 80, withFaults = false)
    }

    @Test
    fun randomOperationStreamsWithADecoderFaultOnlyStopForAReportedReason() {
        for (seed in 1000L..1100L) runStream(seed, steps = 80, withFaults = true)
    }

    // ---------------- the documented composite sequence ----------------

    /** Blocks the first exact seek of [blockFrame] until released; everything else is immediate. */
    private class BlockingSeeker(private val blockFrame: Long) : FrameSeeker {
        val exactCalls = CopyOnWriteArrayList<Long>()
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)

        override fun seekExact(frameIndex: Long): FrameSeekResult {
            exactCalls.add(frameIndex)
            if (frameIndex == blockFrame && entered.count == 1L) {
                entered.countDown()
                check(release.await(10, TimeUnit.SECONDS)) { "test never released the seek" }
            }
            return FrameSeekResult(frameIndex, frameIndex, isApproximate = false)
        }

        override fun scrubApprox(frameIndex: Long): FrameSeekResult = FrameSeekResult(frameIndex, frameIndex, false)
    }

    @Test
    fun playStaticTickSeekPauseStaleSeekCompletionClose() {
        val clock = ManualClock()
        val seeker = BlockingSeeker(blockFrame = 3L)
        val pts = vfrPtsNs()
        val snapshots = CopyOnWriteArrayList<PlaybackState>()
        val ctrl = PlaybackController(
            totalFrames = pts.size.toLong(),
            frameSeeker = seeker,
            framePtsNs = pts,
            clock = clock,
            autoRunLoop = false,
        )
        ctrl.addStateListener { snapshots += it }

        // 1. Play, then sit through the 5 s static hold of frame 0: no seeks, time keeps flowing.
        ctrl.play()
        assertEquals(PlaybackMode.PLAYING, ctrl.state.mode)
        var lastDisplay = ctrl.displayPositionNs()
        for (stepMs in listOf(1_000L, 1_500L, 2_000L, 400L)) {
            clock.advanceMs(stepMs)
            val wake = ctrl.tick()
            assertEquals(0L, ctrl.state.currentFrameIndex, "static hold keeps showing frame 0")
            assertEquals(PlaybackMode.PLAYING, ctrl.state.mode, "a static hold is not a stop")
            assertTrue(wake != null && wake > clock.nowNs(), "the next frame boundary is scheduled")
            val shown = ctrl.displayPositionNs()
            assertTrue(shown >= lastDisplay, "the presentation clock never moves back during a hold")
            lastDisplay = shown
        }
        assertTrue(seeker.exactCalls.isEmpty(), "no decode while a frame is simply held: ${seeker.exactCalls}")

        // 2. A user seek to #3 gets stuck inside the decoder (slow seek).
        val slowSeek = Thread { ctrl.seekToFrame(3) }.apply { isDaemon = true; start() }
        assertTrue(seeker.entered.await(5, TimeUnit.SECONDS))

        // 3. Meanwhile the user pauses and seeks to #6 on another thread (queues behind the decoder).
        ctrl.pause()
        assertEquals(PlaybackMode.PAUSED, ctrl.state.mode)
        val newerSeek = Thread { ctrl.seekToFrame(6) }.apply { isDaemon = true; start() }
        // Wait until the newer request published its target (generation advanced), then free the old one.
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
        while (ctrl.state.currentFrameIndex != 6L && System.nanoTime() < deadline) Thread.sleep(1)
        assertEquals(6L, ctrl.state.currentFrameIndex)
        val marker = snapshots.size

        // 4. The stale #3 decode finishes late: it must not overwrite the newer position.
        seeker.release.countDown()
        slowSeek.join(5_000)
        newerSeek.join(5_000)
        assertFalse(slowSeek.isAlive || newerSeek.isAlive)
        val late = snapshots.drop(marker)
        assertTrue(late.none { it.displayedFrameIndex == 3L || it.currentFrameIndex == 3L }, "stale completion leaked: $late")
        assertEquals(6L, ctrl.state.currentFrameIndex)
        assertEquals(6L, ctrl.state.displayedFrameIndex)
        assertEquals(PlaybackMode.PAUSED, ctrl.state.mode)
        assertFalse(ctrl.state.isApproximate)

        // 5. Close: terminal, and nothing resurrects playing afterwards.
        ctrl.close()
        val afterClose = snapshots.size
        ctrl.play()
        ctrl.seekToFrame(9)
        assertEquals(afterClose, snapshots.size, "a closed controller publishes nothing")
        assertEquals(PlaybackMode.PAUSED, ctrl.state.mode)
        assertTrue(ctrl.isClosed)
        assertTrue(snapshots.none { it.mode == PlaybackMode.PLAYING && snapshots.indexOf(it) >= afterClose })
    }

    @Test
    fun closeWhileASeekIsInFlightPublishesNothingAfterwards() {
        val seeker = BlockingSeeker(blockFrame = 4L)
        val pts = vfrPtsNs()
        val snapshots = CopyOnWriteArrayList<PlaybackState>()
        val ctrl = PlaybackController(pts.size.toLong(), seeker, framePtsNs = pts, clock = ManualClock(), autoRunLoop = false)
        ctrl.addStateListener { snapshots += it }
        val seek = Thread { ctrl.seekToFrame(4) }.apply { isDaemon = true; start() }
        assertTrue(seeker.entered.await(5, TimeUnit.SECONDS))
        ctrl.close()
        val marker = snapshots.size
        seeker.release.countDown()
        seek.join(5_000)
        assertFalse(seek.isAlive)
        assertEquals(marker, snapshots.size, "a seek finishing after close must not publish")
        assertEquals(PlaybackMode.PAUSED, ctrl.state.mode)
    }
}
