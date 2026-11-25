package io.github.shinma06.arb.playback

import org.junit.jupiter.api.Test
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicLong
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

class PlaybackControllerTest {
    private class ManualClock(initialNs: Long = 1_000_000_000L) : PlaybackClock {
        private val now = AtomicLong(initialNs)
        override fun nowNs(): Long = now.get()
        fun advanceMs(ms: Long) {
            now.addAndGet(ms * 1_000_000L)
        }
        fun advanceNs(ns: Long) {
            now.addAndGet(ns)
        }
    }

    private data class SeekCall(val kind: String, val frameIndex: Long)

    private class RecordingSeeker(
        private val keyframeInterval: Long = 10L,
        private val onExactSeek: ((Long) -> Unit)? = null,
    ) : FrameSeeker {
        val calls = CopyOnWriteArrayList<SeekCall>()

        override fun seekExact(frameIndex: Long): FrameSeekResult {
            calls.add(SeekCall("exact", frameIndex))
            onExactSeek?.invoke(frameIndex)
            return FrameSeekResult(
                requestedFrameIndex = frameIndex,
                displayedFrameIndex = frameIndex,
                isApproximate = false,
            )
        }

        override fun scrubApprox(frameIndex: Long): FrameSeekResult {
            calls.add(SeekCall("scrub", frameIndex))
            val nearestKf = (frameIndex / keyframeInterval) * keyframeInterval
            return FrameSeekResult(
                requestedFrameIndex = frameIndex,
                displayedFrameIndex = nearestKf,
                isApproximate = nearestKf != frameIndex,
            )
        }
    }

    private class FakeAudioSink(override val isAvailable: Boolean = true) : AudioSink {
        var opened: Boolean = false
        var started: Boolean = false
        var flushCount: Int = 0
        var closed: Boolean = false

        override fun open(format: PcmFormat): Boolean {
            opened = isAvailable
            return isAvailable
        }

        override fun start() {
            started = true
        }

        override fun stop() {
            started = false
        }

        override fun flush() {
            flushCount++
        }

        override fun write(pcm: ByteArray, offset: Int, length: Int): Int = if (started) length else 0

        override fun close() {
            closed = true
            started = false
        }
    }

    private fun uniformPtsNs(count: Int, stepMs: Long = 100L, baseNs: Long = 10_000_000_000L): List<Long> =
        List(count) { i -> baseNs + i * stepMs * 1_000_000L }

    @Test
    fun sharedVfrQueriesAndConstantSpeedWhileHolding() {
        val path = java.nio.file.Path.of(System.getProperty("arb.repo.root"), "core/crates/arb-media/tests/data/vfr-timeline.txt")
        val lines = java.nio.file.Files.readAllLines(path).filterNot { it.startsWith('#') }
        val starts = lines.first().split(" ").map(String::toLong)
        val lastDuration = lines[1].toLong()
        for (speed in PlaybackSpeed.entries) {
            val clock = ManualClock()
            val seeker = RecordingSeeker()
            PlaybackController(starts.size.toLong(), seeker, starts, clock = clock, lastFrameDurationNs = lastDuration,
                autoRunLoop = false, initialSpeed = speed).use { ctrl ->
                val axis = ctrl.axis
                assertEquals(11_000_000_000L, axis.endNs)
                ctrl.play()
                var previous = 0L
                for (line in lines.drop(2)) {
                    val (time, frame, out) = line.split(" ").map(String::toLong)
                    val before = seeker.calls.size
                    clock.advanceNs(speed.mediaToWallNs(time - previous))
                    ctrl.tick()
                    assertEquals(time, ctrl.displayPositionNs())
                    assertEquals(time.toDouble() / 11_000_000_000.0, axis.fraction(ctrl.displayPositionNs()))
                    assertEquals(frame, ctrl.state.currentFrameIndex)
                    assertEquals(out, axis.outFrame(time))
                    if (previous >= 1_000_000_000L && time < 10_000_000_000L) assertEquals(before, seeker.calls.size)
                    previous = time
                }
            }
        }
    }

    @Test
    fun eachPresentationTickMovesTheBarWithoutDecodingDuringAHold() {
        for (speed in PlaybackSpeed.entries) {
            val clock = ManualClock()
            val seeker = RecordingSeeker()
            PlaybackController(2, seeker, listOf(0, 9_000_000_000), clock = clock,
                initialSpeed = speed, lastFrameDurationNs = 1_000_000_000, autoRunLoop = false).use { ctrl ->
                ctrl.play()
                repeat(179) { i ->
                    clock.advanceNs(speed.mediaToWallNs(50_000_000))
                    ctrl.tick()
                    assertEquals((i + 1) * 5f, ctrl.axis.pixel(ctrl.displayPositionNs(), 0f, 1000f))
                    assertEquals(0L, ctrl.state.currentFrameIndex)
                }
                assertTrue(seeker.calls.isEmpty())
            }
        }
    }

    @Test
    fun generatedHoldClockSeekAndPauseMatchTimeOracle() {
        val random = kotlin.random.Random(104135)
        for (speed in PlaybackSpeed.entries) repeat(32) {
            val starts = mutableListOf(0L)
            repeat(random.nextInt(0, 64)) {
                starts += starts.last() + listOf(1L, 16_666_667L, 10_000_000_000L).random(random)
            }
            val tail = random.nextLong(1, 20_000_000_000L)
            val end = starts.last() + tail
            val clock = ManualClock()
            val seeker = RecordingSeeker()
            PlaybackController(starts.size.toLong(), seeker, starts, clock = clock, lastFrameDurationNs = tail,
                autoRunLoop = false, initialSpeed = speed).use { ctrl ->
                val target = random.nextLong(end)
                ctrl.scrubToTime(target)
                ctrl.seekToTime(target)
                ctrl.play()
                val wall = random.nextLong(10_000_000_000L)
                clock.advanceNs(wall)
                ctrl.tick()
                val expected = (target + (wall * speed.multiplier).toLong()).coerceAtMost(end)
                assertEquals(expected, ctrl.displayPositionNs())
                assertEquals(starts.indexOfLast { it <= expected }.toLong(), ctrl.state.currentFrameIndex)
                ctrl.pause()
                clock.advanceMs(3000)
                assertEquals(expected, ctrl.displayPositionNs())
                ctrl.play()
                assertEquals(if (expected == end) 0L else expected, ctrl.displayPositionNs())
            }
        }
    }

    @Test
    fun axisFallbackAndMalformedTablesKeepRepresentableBoundaries() {
        val normalized = TimelineCoordinateAxis(4, listOf(Long.MIN_VALUE, Long.MAX_VALUE, -1, -1))
        val starts = (0L..3L).map(normalized::start)
        assertTrue(starts.zipWithNext().all { (a, b) -> a < b })
        assertTrue(starts.last() < normalized.endNs)
        val capped = TimelineCoordinateAxis(100_001, lastDurationNs = 10, reportedEndNs = 1_000_043)
        assertEquals(1_000_043L, capped.endNs)
        for (i in listOf(0L, 33_333L, 99_999L, 100_000L)) {
            assertEquals(i * 1_000_033 / 100_000, capped.start(i))
            assertEquals(i, capped.frameAt(capped.start(i)))
            assertEquals(i, capped.outFrame(capped.outTime(i)))
        }
        assertEquals(0L, TimelineCoordinateAxis(0).endNs)
        for (count in listOf(1L, 4L, 100_001L)) {
            val axis = TimelineCoordinateAxis(count, emptyList(), 25, 9_000_000_000L)
            assertEquals((count - 1) * 25 + 9_000_000_000, axis.endNs)
            assertEquals(count - 1, axis.frameAt(axis.endNs))
        }
    }

    @Test
    fun transientFrameFailureRetriesOnceWithoutStoppingAndInitialPermanentFailureRollsBack() {
        val clock = ManualClock()
        var attempts = 0
        val errors = mutableListOf<Throwable>()
        val seeker = RecordingSeeker(onExactSeek = { if (++attempts == 1) throw java.io.IOException("temporary") })
        PlaybackController(10, seeker, clock = clock, autoRunLoop = false, onPlaybackError = errors::add).use {
            it.play()
            clock.advanceMs(40)
            it.tick()
            assertEquals(2, attempts)
            assertEquals(1, it.state.displayedFrameIndex)
            assertEquals(PlaybackMode.PLAYING, it.state.mode)
            assertTrue(errors.isEmpty())
        }
        val broken = RecordingSeeker(onExactSeek = { throw java.io.IOException("permanent") })
        PlaybackController(10, broken, initialFrameIndex = 9, initialOutFrameIndex = 8, onPlaybackError = errors::add).use {
            it.play()
            assertEquals(PlaybackMode.PAUSED, it.state.mode)
            assertEquals(2, broken.calls.size)
            assertTrue(errors.single().message.orEmpty().contains("#0"))
        }
    }

    @Test
    fun resumingFromErrorBeforeWaitDoesNotLoseWakeup() {
        lateinit var ctrl: PlaybackController
        val calls = AtomicLong()
        val advanced = CountDownLatch(1)
        val seeker = RecordingSeeker(onExactSeek = {
            if (calls.incrementAndGet() == 1L) error("bad frame")
            if (it >= 3L) advanced.countDown()
        })
        ctrl = PlaybackController(500, seeker, onPlaybackError = { ctrl.play() })
        ctrl.use {
            it.play()
            assertTrue(advanced.await(3, TimeUnit.SECONDS))
            assertEquals(PlaybackMode.PLAYING, ctrl.state.mode)
        }
    }

    @Test
    fun errorInPlaybackLoopPausesReportsAndKeepsLoopThreadUsable() {
        val calls = AtomicLong()
        val reported = CountDownLatch(1)
        val advanced = CountDownLatch(1)
        val seeker = RecordingSeeker(onExactSeek = {
            if (calls.incrementAndGet() == 1L) throw OutOfMemoryError("frame buffer")
            if (it >= 3L) advanced.countDown()
        })
        PlaybackController(500, seeker, onPlaybackError = { reported.countDown() }).use { ctrl ->
            ctrl.play()
            assertTrue(reported.await(3, TimeUnit.SECONDS))
            val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(3)
            while (ctrl.state.mode != PlaybackMode.PAUSED && System.nanoTime() < deadline) Thread.sleep(5)
            assertEquals(PlaybackMode.PAUSED, ctrl.state.mode)
            ctrl.play()
            assertTrue(advanced.await(3, TimeUnit.SECONDS))
        }
    }

    @Test
    fun staleFailedSeekCannotPauseNewPlaybackOrReportAfterClose() {
        for (close in listOf(false, true)) {
            val clock = ManualClock()
            val entered = CountDownLatch(1)
            val release = CountDownLatch(1)
            val errors = CopyOnWriteArrayList<Throwable>()
            val seeker = RecordingSeeker(onExactSeek = {
                entered.countDown()
                check(release.await(3, TimeUnit.SECONDS))
                error("stale")
            })
            PlaybackController(100, seeker, clock = clock, autoRunLoop = false, onPlaybackError = errors::add).use { ctrl ->
                ctrl.play()
                clock.advanceMs(100)
                val task = Thread { ctrl.tick() }.apply { start() }
                try {
                    assertTrue(entered.await(3, TimeUnit.SECONDS))
                    if (close) ctrl.close() else ctrl.pause()
                } finally {
                    release.countDown()
                    task.join(3000)
                }
                assertFalse(task.isAlive)
                assertTrue(errors.isEmpty())
            }
        }
    }

    @Test
    fun viewportRecoveryTimeIsExcludedFromPlaybackClock() {
        val clock = ManualClock()
        val seeker = object : FrameSeeker {
            override fun seekExact(frameIndex: Long) = FrameSeekResult(frameIndex, frameIndex, false)
            override fun seekExact(frameIndex: Long, onRecovery: () -> Unit): FrameSeekResult {
                onRecovery()
                clock.advanceMs(30_000)
                return FrameSeekResult(frameIndex, frameIndex, false, recovered = true)
            }
            override fun scrubApprox(frameIndex: Long) = seekExact(frameIndex)
        }
        PlaybackController(50, seeker, uniformPtsNs(50, 100), clock = clock, autoRunLoop = false).use {
            it.setOutFrame(40)
            it.setSpeed(PlaybackSpeed.DOUBLE)
            it.play()
            clock.advanceMs(50)
            it.tick()
            it.tick()
            assertEquals(1L, it.state.currentFrameIndex)
            assertEquals(40L, it.state.outFrameIndex)
            assertEquals(PlaybackMode.PLAYING, it.state.mode)
            assertEquals(PlaybackSpeed.DOUBLE, it.state.speed)
        }
    }

    @Test
    fun legacyVfrDurationIncludesInferredFinalHold() {
        val clock = ManualClock()
        PlaybackController(3, RecordingSeeker(), listOf(0, 30_000_000, 5_030_000_000), clock = clock, autoRunLoop = false).use {
            assertEquals(10_030_000_000L, it.durationNs)
            it.play()
            clock.advanceMs(6000)
            it.tick()
            assertEquals(6_000_000_000L, it.displayPositionNs())
            assertEquals(PlaybackMode.PLAYING, it.state.mode)
            clock.advanceMs(4030)
            it.tick()
            assertEquals(PlaybackMode.PAUSED, it.state.mode)
        }
    }

    @Test
    fun vfrClockAdvancesDuringHoldAndStopsAtExactLastSampleDuration() {
        for (pts in listOf(listOf(0L), listOf(0L, 30L, 5030L), listOf(0L, 5000L, 5030L))) {
            val clock = ManualClock()
            val seeker = RecordingSeeker()
            PlaybackController(pts.size.toLong(), seeker, pts.map { it * 1_000_000L }, clock = clock,
                lastFrameDurationNs = 33_000_000L, autoRunLoop = false).use { ctrl ->
                ctrl.play()
                clock.advanceMs(20)
                ctrl.tick()
                assertEquals(20_000_000L, ctrl.displayPositionNs())
                assertTrue(seeker.calls.isEmpty())
                clock.advanceMs(pts.last() + 12L)
                ctrl.tick()
                assertEquals(PlaybackMode.PLAYING, ctrl.state.mode)
                clock.advanceMs(1)
                ctrl.tick()
                assertEquals(PlaybackMode.PAUSED, ctrl.state.mode)
                assertEquals(pts.lastIndex.toLong(), ctrl.state.displayedFrameIndex)
            }
        }
    }

    @Test
    fun playPauseAndLoopInsideTrimSelectionAtHalfNormalAndDoubleSpeed() {
        val clock = ManualClock()
        val seeker = RecordingSeeker()
        val pts = uniformPtsNs(count = 20, stepMs = 100L) // 100 ms/frame

        PlaybackController(
            totalFrames = 20L,
            frameSeeker = seeker,
            framePtsNs = pts,
            clock = clock,
            initialInFrameIndex = 5L,
            initialOutFrameIndex = 8L,
            initialFrameIndex = 5L,
            initialLoopInSelection = true,
            autoRunLoop = false,
        ).use { ctrl ->
            assertEquals(PlaybackMode.PAUSED, ctrl.state.mode)
            assertEquals(5L, ctrl.state.currentFrameIndex)
            assertEquals(5L, ctrl.state.inFrameIndex)
            assertEquals(8L, ctrl.state.outFrameIndex)
            assertTrue(ctrl.state.loopInSelection)

            // Start playing at 1.0x
            ctrl.play()
            assertEquals(PlaybackMode.PLAYING, ctrl.state.mode)

            // 50ms -> still on frame 5
            clock.advanceMs(50)
            assertNotNull(ctrl.tick())
            assertEquals(5L, ctrl.state.currentFrameIndex)

            // +50ms (total 100ms) -> frame 6
            clock.advanceMs(50)
            ctrl.tick()
            assertEquals(6L, ctrl.state.currentFrameIndex)

            // Switch to 2.0x -> frame 7 needs only 50ms wall time!
            ctrl.setSpeed(PlaybackSpeed.DOUBLE)
            clock.advanceMs(49)
            ctrl.tick()
            assertEquals(6L, ctrl.state.currentFrameIndex)
            clock.advanceMs(1)
            ctrl.tick()
            assertEquals(7L, ctrl.state.currentFrameIndex)

            // Switch to 0.5x -> frame 8 needs 200ms wall time!
            ctrl.setSpeed(PlaybackSpeed.HALF)
            clock.advanceMs(150)
            ctrl.tick()
            assertEquals(7L, ctrl.state.currentFrameIndex)
            clock.advanceMs(50)
            ctrl.tick()
            assertEquals(8L, ctrl.state.currentFrameIndex)

            // Another 200ms at 0.5x finishes frame 8's duration and loops smoothly back to inFrame (5)!
            clock.advanceMs(200)
            ctrl.tick()
            assertEquals(5L, ctrl.state.currentFrameIndex)
            assertEquals(PlaybackMode.PLAYING, ctrl.state.mode)
            assertFalse(ctrl.state.isApproximate)

            // Pause stops advancement
            ctrl.togglePlayPause()
            assertEquals(PlaybackMode.PAUSED, ctrl.state.mode)
            clock.advanceMs(1_000)
            assertNull(ctrl.tick())
            assertEquals(5L, ctrl.state.currentFrameIndex)

            // Every seek during playback was exact
            assertTrue(seeker.calls.all { it.kind == "exact" })
            assertEquals(listOf(6L, 7L, 8L, 5L), seeker.calls.map { it.frameIndex })
        }
    }

    @Test
    fun dropsLateFramesWithoutAccumulatingDriftAndPausesAtEndWhenNotLooping() {
        val clock = ManualClock()
        val seeker = RecordingSeeker()
        val pts = uniformPtsNs(count = 10, stepMs = 100L)

        PlaybackController(
            totalFrames = 10L,
            frameSeeker = seeker,
            framePtsNs = pts,
            clock = clock,
            initialInFrameIndex = 2L,
            initialOutFrameIndex = 7L,
            initialFrameIndex = 2L,
            initialLoopInSelection = false,
            autoRunLoop = false,
        ).use { ctrl ->
            ctrl.play()

            // Simulate a 350ms decode stall -> frames 3 & 4 dropped, jumps straight to frame 5!
            clock.advanceMs(350)
            val nextWakeup = assertNotNull(ctrl.tick())
            assertEquals(5L, ctrl.state.currentFrameIndex)
            assertEquals(2L, ctrl.droppedFramesCount)
            assertEquals(listOf(SeekCall("exact", 5L)), seeker.calls.toList())
            // Next frame (6) is due in 50ms (400ms from anchor), NOT 100ms — zero drift accumulation!
            assertEquals(clock.nowNs() + 50_000_000L, nextWakeup)

            clock.advanceMs(50)
            ctrl.tick()
            assertEquals(6L, ctrl.state.currentFrameIndex)

            // Jump past selection end -> settles on outFrameIndex (7) and pauses cleanly.
            clock.advanceMs(500)
            assertNull(ctrl.tick())
            assertEquals(7L, ctrl.state.currentFrameIndex)
            assertEquals(PlaybackMode.PAUSED, ctrl.state.mode)

            // Pressing play again at outFrameIndex when !loopInSelection restarts from inFrameIndex (2).
            ctrl.play()
            assertEquals(PlaybackMode.PLAYING, ctrl.state.mode)
            assertEquals(2L, ctrl.state.currentFrameIndex)
        }
    }

    @Test
    fun fallbackFpsPacingWorksWhenFramePtsUnavailable() {
        val clock = ManualClock()
        val seeker = RecordingSeeker()

        PlaybackController(
            totalFrames = 10L,
            frameSeeker = seeker,
            framePtsNs = emptyList(),
            fallbackFps = 20.0, // 50 ms per frame
            clock = clock,
            autoRunLoop = false,
        ).use { ctrl ->
            assertEquals(0L, ctrl.frameRelativePtsNs(0L))
            assertEquals(50_000_000L, ctrl.frameRelativePtsNs(1L))
            assertEquals(250_000_000L, ctrl.frameRelativePtsNs(5L))

            ctrl.play()
            clock.advanceMs(125)
            ctrl.tick()
            assertEquals(2L, ctrl.state.currentFrameIndex)
        }
    }

    @Test
    fun stepFramePausesPlaybackClampsBoundsAndPerformsExactSeek() {
        val clock = ManualClock()
        val seeker = RecordingSeeker()

        PlaybackController(
            totalFrames = 5L,
            frameSeeker = seeker,
            clock = clock,
            initialFrameIndex = 2L,
            autoRunLoop = false,
        ).use { ctrl ->
            ctrl.play()
            assertEquals(PlaybackMode.PLAYING, ctrl.state.mode)

            ctrl.stepFrame(+1)
            assertEquals(PlaybackMode.PAUSED, ctrl.state.mode)
            assertEquals(3L, ctrl.state.currentFrameIndex)
            assertFalse(ctrl.state.isApproximate)

            ctrl.stepFrame(-1)
            assertEquals(2L, ctrl.state.currentFrameIndex)

            // Clamp at 0 and totalFrames - 1
            ctrl.stepFrame(-100)
            assertEquals(0L, ctrl.state.currentFrameIndex)
            ctrl.stepFrame(+100)
            assertEquals(4L, ctrl.state.currentFrameIndex)
            assertEquals(
                listOf(SeekCall("exact", 3L), SeekCall("exact", 2L), SeekCall("exact", 0L), SeekCall("exact", 4L)),
                seeker.calls.toList(),
            )
        }
    }

    @Test
    fun scrubSetsApproximatePreviewAndEndScrubGuaranteesExactSettleAndAudioMute() {
        val clock = ManualClock()
        val seeker = RecordingSeeker(keyframeInterval = 10L)
        val pcm = ByteArray(48_000 * 4 * 2) // 2s stereo s16le
        val sink = FakeAudioSink(isAvailable = true)
        val audio = AudioSyncPlayer(
            source = InMemoryPcmSource(48_000, 2, pcm),
            sink = sink,
            avOffsetMs = 0L,
            autoRunPump = false,
        )

        PlaybackController(
            totalFrames = 50L,
            frameSeeker = seeker,
            audioPlayer = audio,
            clock = clock,
            autoRunLoop = false,
        ).use { ctrl ->
            ctrl.play()
            assertEquals(AudioState.PLAYING, ctrl.state.audioState)

            ctrl.beginScrub()
            assertEquals(PlaybackMode.SCRUBBING, ctrl.state.mode)
            assertEquals(AudioState.MUTED, ctrl.state.audioState)

            // Mid-GOP scrub -> nearest keyframe (20) displayed with isApproximate = true
            ctrl.scrubTo(24L)
            assertEquals(PlaybackMode.SCRUBBING, ctrl.state.mode)
            assertEquals(24L, ctrl.state.currentFrameIndex)
            assertEquals(20L, ctrl.state.displayedFrameIndex)
            assertTrue(ctrl.state.isApproximate)
            assertEquals(AudioState.MUTED, ctrl.state.audioState)

            // Release scrub -> guaranteed exact seek with isApproximate = false!
            ctrl.endScrub(24L)
            assertEquals(PlaybackMode.PAUSED, ctrl.state.mode)
            assertEquals(24L, ctrl.state.currentFrameIndex)
            assertEquals(24L, ctrl.state.displayedFrameIndex)
            assertFalse(ctrl.state.isApproximate)
            assertEquals(AudioState.PAUSED, ctrl.state.audioState)

            assertEquals(
                listOf(SeekCall("scrub", 24L), SeekCall("exact", 24L)),
                seeker.calls.toList(),
            )
        }
    }

    @Test
    fun trimBoundsAndDeterministicPresetsPreserveIntegerInvariants() {
        val clock = ManualClock()
        val seeker = RecordingSeeker()

        PlaybackController(
            totalFrames = 100L,
            frameSeeker = seeker,
            clock = clock,
            autoRunLoop = false,
        ).use { ctrl ->
            // setInFrame / setOutFrame preserve 0 <= in <= out < totalFrames
            ctrl.setOutFrame(30L)
            ctrl.setInFrame(50L)
            assertEquals(50L, ctrl.state.inFrameIndex)
            assertEquals(50L, ctrl.state.outFrameIndex)

            ctrl.setOutFrame(10L)
            assertEquals(10L, ctrl.state.inFrameIndex)
            assertEquals(10L, ctrl.state.outFrameIndex)

            // Empty tap list returns false without mutating range
            assertFalse(ctrl.selectTapRange(emptyList(), padBeforeFrames = 15L, padAfterFrames = 15L))
            assertEquals(10L, ctrl.state.inFrameIndex)
            assertEquals(10L, ctrl.state.outFrameIndex)

            // Real taps -> [min(taps) - padBefore ..= max(taps) + padAfter] clamped
            assertTrue(ctrl.selectTapRange(listOf(42L, 20L, 65L), padBeforeFrames = 30L, padAfterFrames = 10L))
            assertEquals(0L, ctrl.state.inFrameIndex) // 20 - 30 clamped to 0
            assertEquals(75L, ctrl.state.outFrameIndex) // 65 + 10

            // selectAroundCrash -> [crash - before ..= crash + after] AND exact seek to crash frame
            ctrl.scrubTo(17L)
            assertTrue(ctrl.state.isApproximate)
            ctrl.selectAroundCrash(crashFrameIndex = 80L, beforeFrames = 150L, afterFrames = 60L)
            assertEquals(0L, ctrl.state.inFrameIndex)
            assertEquals(99L, ctrl.state.outFrameIndex)
            assertEquals(80L, ctrl.state.currentFrameIndex)
            assertEquals(80L, ctrl.state.displayedFrameIndex)
            assertFalse(ctrl.state.isApproximate)
            assertEquals(SeekCall("exact", 80L), seeker.calls.last())

            // selectAroundCrash within bounds
            ctrl.selectAroundCrash(crashFrameIndex = 60L, beforeFrames = 15L, afterFrames = 10L)
            assertEquals(45L, ctrl.state.inFrameIndex)
            assertEquals(70L, ctrl.state.outFrameIndex)
            assertEquals(60L, ctrl.state.currentFrameIndex)

            // resetFullRange restores [0 .. totalFrames - 1]
            ctrl.resetFullRange()
            assertEquals(0L, ctrl.state.inFrameIndex)
            assertEquals(99L, ctrl.state.outFrameIndex)
        }
    }

    @Test
    fun backgroundPlaybackThreadAdvancesAndClosesCleanlyWithoutLeaks() {
        val advancedLatch = CountDownLatch(3)
        val seeker = RecordingSeeker(onExactSeek = { advancedLatch.countDown() })
        val pcm = ByteArray(48_000 * 4)
        val sink = FakeAudioSink(isAvailable = true)
        val audio = AudioSyncPlayer(
            source = InMemoryPcmSource(48_000, 2, pcm),
            sink = sink,
            autoRunPump = true,
        )

        val ctrl = PlaybackController(
            totalFrames = 10L,
            frameSeeker = seeker,
            fallbackFps = 100.0, // 10 ms/frame so real loop advances rapidly in test
            audioPlayer = audio,
            // Loop explicitly ON so slow CI scheduler pauses (>100ms) still wrap and advance
            // instead of settling at frame 9 and pausing before 3 seeks complete.
            initialLoopInSelection = true,
            autoRunLoop = true,
        )

        ctrl.play()
        assertTrue(advancedLatch.await(5, TimeUnit.SECONDS), "Background playback loop should advance frames")

        // Close must be idempotent and join all background threads cleanly.
        ctrl.close()
        ctrl.close()

        assertTrue(ctrl.isClosed)
        assertEquals(PlaybackMode.PAUSED, ctrl.state.mode)
        assertTrue(sink.closed, "Closing PlaybackController must also close owned AudioSyncPlayer sink")

        // Verify no arb-playback-loop or arb-audio-sync-pump threads leaked from this controller.
        val liveArbThreads = Thread.getAllStackTraces().keys.filter { t ->
            t.isAlive && (t.name == "arb-playback-loop" || t.name == "arb-audio-sync-pump")
        }
        assertTrue(liveArbThreads.isEmpty(), "Expected zero leaked playback/audio threads, found: $liveArbThreads")
    }

    @Test
    fun emptyTimelineIsSafeNoOpForAllOperations() {
        val seeker = RecordingSeeker()
        PlaybackController(
            totalFrames = 0L,
            frameSeeker = seeker,
            autoRunLoop = false,
        ).use { ctrl ->
            ctrl.play()
            assertEquals(PlaybackMode.PAUSED, ctrl.state.mode)
            ctrl.stepFrame(+1)
            ctrl.beginScrub()
            ctrl.scrubTo(5L)
            ctrl.endScrub(5L)
            ctrl.setInFrame(2L)
            ctrl.setOutFrame(4L)
            assertFalse(ctrl.selectTapRange(listOf(0L), 5L, 5L))
            ctrl.selectAroundCrash(0L, 5L, 5L)
            ctrl.resetFullRange()
            assertNull(ctrl.tick())
            assertTrue(seeker.calls.isEmpty())
        }
    }

    @Test
    fun coordinatesAudioMuteOnNonNormalSpeedAndAvOffsetAndNotifiesListeners() {
        val clock = ManualClock()
        val seeker = RecordingSeeker()
        val pts = uniformPtsNs(count = 20, stepMs = 100L)
        val pcm = ByteArray(48_000 * 4 * 3)
        val sink = FakeAudioSink(isAvailable = true)
        val audio = AudioSyncPlayer(
            source = InMemoryPcmSource(48_000, 2, pcm),
            sink = sink,
            avOffsetMs = 40L,
            autoRunPump = false,
        )
        val observedStates = CopyOnWriteArrayList<PlaybackState>()

        PlaybackController(
            totalFrames = 20L,
            frameSeeker = seeker,
            framePtsNs = pts,
            audioPlayer = audio,
            clock = clock,
            autoRunLoop = false,
        ).use { ctrl ->
            val sub = ctrl.addStateListener { observedStates.add(it) }
            try {
                assertEquals(40L, ctrl.state.avOffsetMs)
                assertEquals(AudioState.PAUSED, ctrl.state.audioState)

                ctrl.play()
                assertEquals(AudioState.PLAYING, ctrl.state.audioState)

                // Speed != 1.0x cleanly mutes audio without toggling user muted flag
                ctrl.setSpeed(PlaybackSpeed.DOUBLE)
                assertEquals(AudioState.MUTED, ctrl.state.audioState)
                assertFalse(ctrl.state.muted)

                // Returning to 1.0x restores audio playback
                ctrl.setSpeed(PlaybackSpeed.NORMAL)
                assertEquals(AudioState.PLAYING, ctrl.state.audioState)

                // Explicit mute/unmute
                ctrl.toggleMute()
                assertTrue(ctrl.state.muted)
                assertEquals(AudioState.MUTED, ctrl.state.audioState)
                ctrl.toggleMute()
                assertFalse(ctrl.state.muted)
                assertEquals(AudioState.PLAYING, ctrl.state.audioState)

                // Direct exact seek updates playhead, clears isApproximate, and syncs audio with avOffsetMs
                ctrl.setAvOffsetMs(-25L)
                assertEquals(-25L, ctrl.state.avOffsetMs)
                ctrl.seekToFrame(5L) // relative video pts = 500ms -> audio pts = 525ms
                assertEquals(5L, ctrl.state.currentFrameIndex)
                assertFalse(ctrl.state.isApproximate)
                assertEquals(525_000_000L, audio.audioPositionNs)

                assertTrue(observedStates.isNotEmpty(), "State listeners must receive updates")
            } finally {
                sub.close()
            }
        }
    }

    @Test
    fun singleFrameTimelineHandlesLoopAndNonLoopCleanly() {
        val clock = ManualClock()
        val seeker = RecordingSeeker()

        PlaybackController(
            totalFrames = 1L,
            frameSeeker = seeker,
            fallbackFps = 10.0, // 100 ms/frame
            clock = clock,
            initialLoopInSelection = true,
            autoRunLoop = false,
        ).use { ctrl ->
            ctrl.play()
            clock.advanceMs(150)
            assertNotNull(ctrl.tick())
            assertEquals(0L, ctrl.state.currentFrameIndex)
            assertEquals(PlaybackMode.PLAYING, ctrl.state.mode)
            assertTrue(seeker.calls.isEmpty(), "Single-frame loop must not issue redundant seeks")

            ctrl.setLoopInSelection(false)
            clock.advanceMs(150)
            assertNull(ctrl.tick())
            assertEquals(PlaybackMode.PAUSED, ctrl.state.mode)
        }
    }

    @Test
    fun setSpeedWhilePlayingKeepsTimeAlreadyElapsedInsideCurrentFrame() {
        val clock = ManualClock()
        val seeker = RecordingSeeker()
        PlaybackController(
            totalFrames = 20L,
            frameSeeker = seeker,
            framePtsNs = uniformPtsNs(count = 20, stepMs = 100L),
            clock = clock,
            autoRunLoop = false,
        ).use { ctrl ->
            ctrl.play()
            clock.advanceMs(150) // media position 150 ms => mid frame 1
            ctrl.tick()
            assertEquals(1L, ctrl.state.currentFrameIndex)

            ctrl.setSpeed(PlaybackSpeed.DOUBLE)
            clock.advanceMs(25) // +50 ms media => 200 ms => frame 2 (a frame-start re-anchor would give 150 ms => frame 1)
            ctrl.tick()
            assertEquals(2L, ctrl.state.currentFrameIndex)
        }
    }

    @Test
    fun loopingSingleFrameSelectionReseeksAudioToSelectionStart() {
        val clock = ManualClock()
        val seeker = RecordingSeeker()
        val pcm = ByteArray(48_000 * 4 * 3)
        val audio = AudioSyncPlayer(
            source = InMemoryPcmSource(48_000, 2, pcm),
            sink = FakeAudioSink(isAvailable = true),
            autoRunPump = false,
        )
        PlaybackController(
            totalFrames = 20L,
            frameSeeker = seeker,
            framePtsNs = uniformPtsNs(count = 20, stepMs = 100L),
            audioPlayer = audio,
            clock = clock,
            initialInFrameIndex = 5L,
            initialOutFrameIndex = 5L,
            initialFrameIndex = 5L,
            initialLoopInSelection = true,
            autoRunLoop = false,
        ).use { ctrl ->
            ctrl.play()
            assertEquals(500_000_000L, audio.audioPositionNs)
            repeat(4) { audio.pumpNextSlice() } // audio drifts forward 80 ms
            assertEquals(580_000_000L, audio.audioPositionNs)

            clock.advanceMs(100) // selection (one 100 ms frame) ends => loop
            ctrl.tick()
            assertEquals(5L, ctrl.state.currentFrameIndex)
            assertEquals(500_000_000L, audio.audioPositionNs, "Audio must loop back with the single-frame selection")
        }
    }

    @Test
    fun playStartsLoopThreadEvenWhenConcurrentSeekBumpsGenerationDuringInitialSeek() {
        val clock = ManualClock()
        var ctrlRef: PlaybackController? = null
        var injected = false
        val seeker = RecordingSeeker(onExactSeek = {
            if (!injected) {
                injected = true
                ctrlRef!!.setInFrame(7L) // bumps seekGeneration while mode stays PLAYING
            }
        })
        PlaybackController(
            totalFrames = 20L,
            frameSeeker = seeker,
            framePtsNs = uniformPtsNs(count = 20, stepMs = 100L),
            clock = clock,
            initialInFrameIndex = 5L,
            initialOutFrameIndex = 10L,
            initialFrameIndex = 0L, // outside selection => play() performs an initial exact seek
            autoRunLoop = true,
        ).use { ctrl ->
            ctrlRef = ctrl
            ctrl.play()
            assertEquals(PlaybackMode.PLAYING, ctrl.state.mode)
            val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
            fun loopAlive() = Thread.getAllStackTraces().keys.any { it.isAlive && it.name == "arb-playback-loop" }
            while (!loopAlive() && System.nanoTime() < deadline) Thread.sleep(10)
            assertTrue(loopAlive(), "Playback loop thread must run while mode is PLAYING")
        }
    }

    @Test
    fun loopEnabledDefaultsFalseAndStopsCleanlyAtFullRangeEndWithoutAnyTrim() {
        val clock = ManualClock()
        val seeker = RecordingSeeker()
        val pts = uniformPtsNs(count = 5, stepMs = 100L)

        PlaybackController(
            totalFrames = 5L,
            frameSeeker = seeker,
            framePtsNs = pts,
            clock = clock,
            initialFrameIndex = 3L,
            autoRunLoop = false,
        ).use { ctrl ->
            // Default loop is OFF: no trim set (full range [0..4]) must stop cleanly at frame 4.
            assertFalse(ctrl.state.loopInSelection)
            assertFalse(ctrl.loopEnabled)

            ctrl.play()
            clock.advanceMs(500) // well past the end of the 5-frame, 100ms/frame full timeline
            assertNull(ctrl.tick())
            assertEquals(4L, ctrl.state.currentFrameIndex)
            assertEquals(PlaybackMode.PAUSED, ctrl.state.mode)
        }
    }

    @Test
    fun loopEnabledOnWrapsFullRangeBackToFrameZeroAndKeepsPlaying() {
        val clock = ManualClock()
        val seeker = RecordingSeeker()
        val pts = uniformPtsNs(count = 5, stepMs = 100L)

        PlaybackController(
            totalFrames = 5L,
            frameSeeker = seeker,
            framePtsNs = pts,
            clock = clock,
            autoRunLoop = false,
        ).use { ctrl ->
            ctrl.setLoopEnabled(true)
            assertTrue(ctrl.loopEnabled)

            ctrl.play()
            // 5 frames at 100ms each span exactly 500ms; 520ms overshoots past the selection end
            // (frame 4's own 100ms duration included), which must wrap back to frame 0.
            clock.advanceMs(520)
            assertNotNull(ctrl.tick())
            assertEquals(0L, ctrl.state.currentFrameIndex)
            assertEquals(PlaybackMode.PLAYING, ctrl.state.mode, "loop ON must keep playing past the full-range end")
        }
    }

    @Test
    fun toggleLoopFlipsLoopEnabledFromCurrentState() {
        val seeker = RecordingSeeker()
        PlaybackController(
            totalFrames = 10L,
            frameSeeker = seeker,
            autoRunLoop = false,
        ).use { ctrl ->
            assertFalse(ctrl.loopEnabled)
            ctrl.toggleLoop()
            assertTrue(ctrl.loopEnabled)
            assertTrue(ctrl.state.loopInSelection)
            ctrl.toggleLoop()
            assertFalse(ctrl.loopEnabled)
        }
    }

    @Test
    fun loopWrapResyncsAudioPlayerOnFullRangeAndTrimRange() {
        val clock = ManualClock()
        val seeker = RecordingSeeker()
        val pts = uniformPtsNs(count = 6, stepMs = 100L) // 0..500ms, end at 600ms
        val pcm = ByteArray(48_000 * 4 * 2) // 2s stereo s16le
        val sink = FakeAudioSink(isAvailable = true)
        val audio = AudioSyncPlayer(
            source = InMemoryPcmSource(48_000, 2, pcm),
            sink = sink,
            autoRunPump = false,
        )

        PlaybackController(
            totalFrames = 6L,
            frameSeeker = seeker,
            framePtsNs = pts,
            audioPlayer = audio,
            clock = clock,
            initialLoopInSelection = true,
            autoRunLoop = false,
        ).use { ctrl ->
            ctrl.play()
            repeat(5) { audio.pumpNextSlice() }
            assertEquals(100_000_000L, audio.audioPositionNs)

            // Full-range wrap: 620ms > 600ms end -> wraps to frame 0 and flushes/resets audio to 0ns.
            val flushesBeforeFullWrap = sink.flushCount
            clock.advanceMs(620)
            assertNotNull(ctrl.tick())
            assertEquals(0L, ctrl.state.currentFrameIndex)
            assertEquals(PlaybackMode.PLAYING, ctrl.state.mode)
            assertEquals(0L, audio.audioPositionNs)
            assertTrue(sink.flushCount > flushesBeforeFullWrap, "Full-range loop wrap must flush audio sink")

            // Trim-range wrap [2..4]: 200ms..400ms (end at 500ms).
            ctrl.setInFrame(2L)
            ctrl.setOutFrame(4L)
            ctrl.seekToFrame(2L)
            repeat(5) { audio.pumpNextSlice() }
            assertEquals(300_000_000L, audio.audioPositionNs)

            val flushesBeforeTrimWrap = sink.flushCount
            clock.advanceMs(350) // 200ms + 350ms = 550ms >= 500ms selectionEnd -> wrap to frame 2 (200ms)
            assertNotNull(ctrl.tick())
            assertEquals(2L, ctrl.state.currentFrameIndex)
            assertEquals(PlaybackMode.PLAYING, ctrl.state.mode)
            assertEquals(200_000_000L, audio.audioPositionNs, "Trim-range loop wrap must reset audio to inFrame PTS")
            assertTrue(sink.flushCount > flushesBeforeTrimWrap, "Trim-range loop wrap must flush audio sink")
        }
    }
}
