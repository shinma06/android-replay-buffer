package io.github.shinma06.replaybuffer.ui

import io.github.shinma06.replaybuffer.core.CaptureStore
import io.github.shinma06.replaybuffer.core.ReplayFrameCursor
import io.github.shinma06.replaybuffer.core.ReplayVideoDecoder
import io.github.shinma06.replaybuffer.core.ReplaySettings
import io.github.shinma06.replaybuffer.core.SaveWriter
import io.github.shinma06.replaybuffer.core.VideoPacket
import org.jcodec.codecs.h264.H264Encoder
import org.jcodec.codecs.h264.H264Utils
import org.jcodec.common.model.ColorSpace
import org.jcodec.common.model.Picture
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.ByteBuffer
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicReference
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class ReplayPlaybackVideoTest {
    @TempDir lateinit var root: Path

    @Test
    fun anExpiredClockDoesNotDecodeBeyondTheInitialPreparedFrame() {
        val reads = AtomicInteger()
        Playback(nanoTime = { if (reads.getAndIncrement() == 0) 0 else 800_000_000L }).use { play ->
            play.startAndFinish()
            assertEquals(listOf(0), play.decoded)
            assertEquals(emptyList(), play.shown)
            assertEquals(listOf<String?>(null), play.finished)
        }
    }

    @Test
    fun reachingTheEndAfterShowingAFrameDoesNotStartAnotherDecode() {
        Playback().use { play ->
            play.afterShown = { play.moveTo(play.untilUs) }
            play.startAndFinish()
            assertEquals(listOf(0), play.decoded)
            assertEquals(listOf(0), play.shown)
            assertEquals(listOf<String?>(null), play.finished)
        }
    }

    @Test
    fun aSlowDecodeReachingOrPassingTheEndDoesNotDecodeTheRemainingFrames() {
        for (overrun in listOf(0L, 100_000L)) {
            Playback().use { play ->
                play.afterNext = { play.moveTo(play.untilUs + overrun) }
                play.startAndFinish()
                assertEquals(listOf(0, 1), play.decoded, "overrun=$overrun")
                assertEquals(listOf(0), play.shown)
                assertEquals(listOf<String?>(null), play.finished)
            }
        }
    }

    @Test
    fun lateFramesBeforeTheEndStillDecodeAsReferencesForLaterFrames() {
        Playback().use { play ->
            play.afterShown = { frame -> play.moveTo(if (frame.index == 0) 250_000 else frame.endUs) }
            play.startAndFinish()
            assertEquals((0..7).toList(), play.decoded)
            assertEquals(listOf(0) + (2..7), play.shown)
            assertEquals(listOf<String?>(null), play.finished)
        }
    }

    @Test
    fun normalPlaybackFromASeekStopsAtTheRequestedEnd() {
        Playback(fromUs = 150_000, untilUs = 650_000).use { play ->
            play.startAndFinish()
            assertEquals((1..6).toList(), play.decoded)
            assertEquals((1..6).toList(), play.shown)
            assertEquals(listOf<String?>(null), play.finished)
            assertEquals(650_000L, play.session.nowUs())
        }
    }

    @Test
    fun aDecodeFailureIsStillAFailureEvenIfItsClockHasReachedTheEnd() {
        for (expires in listOf(false, true)) {
            Playback(expectedClosed = false).use { play ->
                play.afterNext = {
                    if (expires) play.moveTo(play.untilUs)
                    error("decode failure")
                }
                play.startAndFinish()
                assertEquals(listOf(0, 1), play.decoded)
                assertEquals(listOf(0), play.shown)
                assertEquals(listOf<String?>("decode failure"), play.finished)
            }
        }
    }

    @Test
    fun cancellingDuringASlowDecodeDoesNotFinishOrDecodeMoreFrames() {
        Playback().use { play ->
            val decoded = CountDownLatch(1)
            val release = CountDownLatch(1)
            play.afterNext = { decoded.countDown(); release.await() }
            try {
                play.session.start()
                assertTrue(decoded.await(10, TimeUnit.SECONDS))
                play.close()
                assertEquals(listOf(0, 1), play.decoded)
                assertEquals(listOf(0), play.shown)
                assertTrue(play.finished.isEmpty())
            } finally { release.countDown() }
        }
    }

    @Test fun sourceOpenFailureCompletesBothWorkersConservatively() {
        val session = ReplayPlaybackSession(root.resolve("missing-replay"), 0, 1, {}, {}, {})
        try {
            session.start()
            assertFalse(session.resourcesClosed.get(10, TimeUnit.SECONDS))
        } finally { session.stop() }
    }

    @Test fun callbackFailureStillFinishesBothWorkersButReportsUncertainCompletion() {
        val failed = CountDownLatch(1)
        val session = ReplayPlaybackSession(fixture(), 0, 0, {}, {
            Thread.currentThread().uncaughtExceptionHandler = Thread.UncaughtExceptionHandler { _, _ -> failed.countDown() }
            error("Injected finished callback failure")
        }, {}, awaitAudio = { it.await(10, TimeUnit.SECONDS) })
        try {
            session.start()
            assertFalse(session.resourcesClosed.get(10, TimeUnit.SECONDS))
            assertTrue(failed.await(10, TimeUnit.SECONDS))
        } finally { session.stop() }
    }

    @Test fun decoderCloseFailureIsNotHiddenByCancellationOrFinally() = interruptedChannel(true)
    @Test fun stoppedNioReadCancellationWithoutCloseFailureFinishesSuccessfully() = interruptedChannel(false)

    private fun interruptedChannel(closeFails: Boolean) {
        val entered = CountDownLatch(1)
        val session = ReplayPlaybackSession(fixture(), 0, 800_000, {}, {}, {}, decoderFactory = { saved ->
            val real = ReplayFrameCursor(saved)
            object : ReplayVideoDecoder by real {
                override fun seekForPlayback(atUs: Long, checkActive: () -> Unit): ReplayFrameCursor.Decoded {
                    entered.countDown()
                    try { CountDownLatch(1).await() }
                    catch (_: InterruptedException) { throw java.nio.channels.ClosedByInterruptException() }
                    error("Unexpected continuation")
                }
                override fun close() { real.close(); if (closeFails) error("Injected decoder close failure") }
            }
        })
        try {
            session.start()
            assertTrue(entered.await(10, TimeUnit.SECONDS))
            session.stop()
            assertEquals(!closeFails, session.resourcesClosed.get(10, TimeUnit.SECONDS))
        } finally { session.stop() }
    }

    private inner class Playback(
        private val fromUs: Long = 0,
        val untilUs: Long = 800_000,
        private val clock: AtomicLong = AtomicLong(),
        nanoTime: () -> Long = clock::get,
        private val expectedClosed: Boolean = true,
    ) : AutoCloseable {
        val decoded = mutableListOf<Int>()
        val shown = mutableListOf<Int>()
        val finished = mutableListOf<String?>()
        private val done = CountDownLatch(1)
        private val worker = AtomicReference<Thread>()
        var afterNext: (ReplayFrameCursor.Decoded) -> Unit = {}
        var afterShown: (ReplayFrameCursor.Decoded) -> Unit = { moveTo(minOf(it.endUs, untilUs)) }
        val session = ReplayPlaybackSession(fixture(), fromUs, untilUs,
            onFrame = { shown += it.index; afterShown(it) },
            onFinished = { finished += it; done.countDown() },
            onAudioUnavailable = { error(it) },
            nanoTime = nanoTime,
            awaitAudio = { assertTrue(it.await(10, TimeUnit.SECONDS)); true },
            prepareVideo = { next ->
                worker.set(Thread.currentThread())
                next()?.also { decoded += it.index }
            },
            nextVideo = { next -> next()?.also { decoded += it.index; afterNext(it) } })

        fun moveTo(us: Long) { clock.set((us - fromUs) * 1000) }

        fun startAndFinish() {
            session.start()
            assertTrue(done.await(10, TimeUnit.SECONDS))
            join()
        }

        private fun join() {
            worker.get()?.let { it.join(10_000); assertFalse(it.isAlive, "video worker still running") }
            assertEquals(expectedClosed, session.resourcesClosed.get(10, TimeUnit.SECONDS))
        }

        override fun close() { session.stop(); join() }
    }

    private fun fixture(): Path {
        val directory = Files.createTempDirectory(root, "video-")
        CaptureStore(directory.resolve("ring"), minFree = 0).use { store ->
            store.generation(1)
            store.session(VideoPacket.Session(32, 32), 1)
            val encoder = H264Encoder.createH264Encoder().apply { setKeyInterval(4) }
            repeat(10) { n ->
                val encoded = encoder.encodeFrame(Picture.create(32, 32, ColorSpace.YUV420J).apply { fill(20 + n) },
                    ByteBuffer.allocate(32 * 32 * 6)).data
                val bytes = ByteArray(encoded.remaining()).also(encoded::get)
                if (n == 0) {
                    val config = H264Utils.splitFrame(ByteBuffer.wrap(bytes)).filter { it.get(0).toInt() and 31 in setOf(7, 8) }
                        .fold(byteArrayOf()) { all, nal -> all + byteArrayOf(0, 0, 0, 1) + ByteArray(nal.remaining()).also(nal::get) }
                    store.frame(VideoPacket.Frame(0, true, false, config), 1)
                }
                assertTrue(store.frame(VideoPacket.Frame(n * 100_000L, false, n % 4 == 0, bytes), 1))
            }
            return SaveWriter().write(store.capture(ReplaySettings())!!, directory, { false },
                { Files.move(it.partial, it.complete) }).directory
        }
    }
}
