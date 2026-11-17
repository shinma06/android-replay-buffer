package io.github.shinma06.replaybuffer.ui

import io.github.shinma06.replaybuffer.core.SavedReplay
import io.github.shinma06.replaybuffer.core.ReplayAudioTrack

import com.google.gson.JsonParser
import net.sourceforge.jaad.aac.Decoder
import net.sourceforge.jaad.aac.SampleBuffer
import org.jcodec.codecs.mpeg4.mp4.EsdsBox
import org.jcodec.common.io.NIOUtils
import org.jcodec.common.model.Packet
import org.jcodec.containers.mp4.MP4TrackType
import org.jcodec.containers.mp4.boxes.AudioSampleEntry
import org.jcodec.containers.mp4.boxes.NodeBox
import org.jcodec.containers.mp4.demuxer.AbstractMP4DemuxerTrack
import org.jcodec.containers.mp4.demuxer.MP4Demuxer
import org.jcodec.containers.mp4.muxer.MP4Muxer
import org.jcodec.containers.mp4.muxer.MP4MuxerTrack
import org.junit.jupiter.api.io.TempDir
import java.io.ByteArrayOutputStream
import java.lang.reflect.Proxy
import java.nio.ByteBuffer
import java.nio.file.Files
import java.nio.file.Path
import java.security.MessageDigest
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong
import javax.sound.sampled.AudioFormat
import javax.sound.sampled.SourceDataLine
import kotlin.math.abs
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class ReplayPlaybackAudioTest {
    @TempDir lateinit var root: Path

    @Test
    fun monoPcmAndTheOpenedLineAgreeOnTheNumberOfFrames() {
        val folder = fixture()
        NIOUtils.readableChannel(folder.resolve("video.mp4").toFile()).use { input ->
            val track = MP4Demuxer.createRawMP4Demuxer(input).audioTracks.single() as AbstractMP4DemuxerTrack
            val entry = track.sampleEntries.single() as AudioSampleEntry
            assertEquals(2, entry.channelCount.toInt())
            val config = NIOUtils.toArray(NodeBox.findFirst(entry, EsdsBox::class.java, EsdsBox.fourcc()).streamInfo)
            val buffer = SampleBuffer().apply { isBigEndian = false }
            Decoder(config).decodeFrame(NIOUtils.toArray(track.nextFrame().data), buffer)
            assertEquals(1, buffer.channels)
            assertEquals(48_000, buffer.sampleRate)
            assertEquals(16, buffer.bitsPerSample)
            assertEquals(2048, buffer.data.size)
            val output = play(folder)
            assertEquals(1024, buffer.data.size / output.format.frameSize)
            assertEquals(1, output.format.channels)
            assertEquals(48_000f, output.format.sampleRate)
            assertEquals(AudioFormat.Encoding.PCM_SIGNED, output.format.encoding)
            assertEquals(false, output.format.isBigEndian)
        }
    }

    @Test
    fun beepSpacingIsOneSecondBothWithAndWithoutTheEncoderPrimingEdit() {
        for (mediaStart in listOf(0L, 1024L)) {
            val output = play(fixture(mediaStart = mediaStart))
            val onsets = onsets(output)
            assertEquals(8, onsets.size, "mediaStart=$mediaStart: $onsets")
            onsets.zipWithNext().forEach { (a, b) ->
                assertTrue(abs(b - a - 1.0) < .015, "mediaStart=$mediaStart: $onsets")
            }
            assertEquals(8 * 48_000, output.pcm.size / output.format.frameSize)
        }
    }

    @Test
    fun seekInsideAPacketAndAnAudioGapUsePcmFramesNotContainerChannels() {
        // Different decode histories produce different valid PNS noise; keep this byte comparison deterministic.
        val resource = "/aac-playback-sync/mono-no-pns.m4a"
        val output = play(fixture(resource = resource, gap = true), fromUs = 1_025_000)
        val reference = decodedPcm(resource)
        val seekSample = 1024 + 1_025_000L * 48_000 / 1_000_000
        val firstEnd = 1024 + 3 * 48_000
        val secondEnd = firstEnd + 4_500_000L * 48_000 / 1_000_000
        val expected = reference.copyOfRange(seekSample.toInt() * 2, firstEnd * 2) + ByteArray(24_000 * 2) +
            reference.copyOfRange(firstEnd * 2, secondEnd.toInt() * 2)
        assertContentEquals(expected, output.pcm)
        val onsets = onsets(output)
        assertEquals(7, onsets.size, onsets.toString())
        for ((actual, expected) in onsets.zip(listOf(0.0, .975, 2.475, 3.475, 4.475, 5.475, 6.475)))
            assertTrue(abs(actual - expected) < .015, onsets.toString())
        val pcm = output.pcm
        assertTrue(pcm.copyOfRange(94_800 * 2, 118_800 * 2).all { it == 0.toByte() })
        assertEquals(334_800, pcm.size / output.format.frameSize)
    }

    @Test
    fun stereoAt44100IsNotForcedToMonoOr48000() {
        val output = play(fixture(resource = "/aac-playback-sync/stereo-44100.m4a", rate = 44_100))
        assertEquals(2, output.format.channels)
        assertEquals(44_100f, output.format.sampleRate)
        assertEquals(8 * 44_100, output.pcm.size / output.format.frameSize)
        val onsets = onsets(output)
        assertEquals(8, onsets.size, onsets.toString())
        onsets.zipWithNext().forEach { (a, b) -> assertTrue(abs(b - a - 1.0) < .015, onsets.toString()) }
    }

    @Test
    fun initialVideoPreparationHoldsAudioAndClockAtTheSeekOrigin() {
        val folder = fixture(videoDurationUs = 2_050_000)
        val before = files(folder)
        val prepare = Gate()
        val frame = Gate()
        val write = Gate()
        val audioCheckpoint = CountDownLatch(1)
        val audioWaitCalls = AtomicInteger()
        val nanos = AtomicLong()
        val callbacks = ConcurrentLinkedQueue<String>()
        val from = 2_025_000L
        val output = Output().apply { beforeWrite = { audioCheckpoint.countDown(); write.block() } }
        lateinit var videoWorker: Thread
        val session = ReplayPlaybackSession(folder, from, 8_000_000,
            { assertTrue(from in it.startUs until it.endUs); frame.block() },
            { callbacks.add("finished: $it") }, callbacks::add,
            audioOutput = { output.line }, nanoTime = nanos::get,
            awaitAudio = { audioWaitCalls.incrementAndGet(); await(it); true },
            prepareVideo = { videoWorker = Thread.currentThread(); prepare.block(); it() },
            awaitVideo = { ready ->
                audioCheckpoint.countDown()
                ready.await()
            })
        try {
            session.start()
            await(prepare.entered); await(audioCheckpoint)
            nanos.addAndGet(5_000_000_000)
            output.position.set(300_000)
            assertEquals(from, session.nowUs(), "clock advanced before the first video frame was ready")
            assertEquals(0, output.starts.get())
            assertEquals(0, output.writes.get())
            assertEquals(0, audioWaitCalls.get())
            assertTrue(callbacks.isEmpty(), callbacks.toString())
            output.position.set(0)
            prepare.release.countDown()
            await(frame.entered); await(write.entered)
            assertEquals(from, session.nowUs())
            assertEquals(1, output.starts.get())
            assertEquals(1, audioWaitCalls.get())
            output.position.set(125_000)
            assertEquals(from + 125_000, session.nowUs())
            assertTrue(callbacks.isEmpty(), callbacks.toString())
        } finally {
            session.stop(); prepare.release.countDown(); frame.release.countDown(); write.release.countDown()
            await(output.closed); join(videoWorker)
        }
        assertFilesUnchanged(folder, before)
    }

    @Test
    fun stopWhilePreparingVideoClosesTheWaitingAudioWithoutStartingIt() {
        val folder = fixture()
        val before = files(folder)
        val prepare = Gate()
        val audioWaiting = CountDownLatch(1)
        val audioExited = CountDownLatch(1)
        val callbacks = ConcurrentLinkedQueue<String>()
        val output = Output()
        lateinit var videoWorker: Thread
        val session = ReplayPlaybackSession(folder, 2_025_000, 8_000_000,
            { callbacks.add("frame") }, { callbacks.add("finished") }, callbacks::add,
            audioOutput = { output.line }, prepareVideo = {
                videoWorker = Thread.currentThread()
                prepare.block()
                it()
            }, awaitVideo = { ready ->
                audioWaiting.countDown()
                try { ready.await() } finally { audioExited.countDown() }
            })
        try {
            session.start()
            await(prepare.entered); await(audioWaiting)
            session.stop(); session.stop()
            await(audioExited); await(output.closed)
            assertEquals(0, output.starts.get())
            assertEquals(0, output.writes.get())
            assertEquals(1, output.closes.get())
        } finally {
            session.stop(); prepare.release.countDown()
            join(videoWorker)
        }
        assertTrue(callbacks.isEmpty(), callbacks.toString())
        assertEquals(2_025_000L, session.nowUs())
        assertFilesUnchanged(folder, before)
    }

    @Test
    fun cancelledOrFailedVideoPreparationReleasesTheWaitingAudio() {
        for (cancel in listOf(false, true)) {
            val folder = fixture()
            val before = files(folder)
            val prepare = Gate()
            val audioWaiting = CountDownLatch(1)
            val callbacks = ConcurrentLinkedQueue<String>()
            val output = Output()
            lateinit var videoWorker: Thread
            val session = ReplayPlaybackSession(folder, 0, 8_000_000,
                { callbacks.add("frame") }, { callbacks.add("finished: $it") }, callbacks::add,
                audioOutput = { output.line }, prepareVideo = {
                    videoWorker = Thread.currentThread()
                    prepare.block()
                    if (cancel) throw InterruptedException() else error("synthetic video preparation failure")
                }, awaitVideo = { ready -> audioWaiting.countDown(); ready.await() })
            try {
                session.start()
                await(prepare.entered); await(audioWaiting)
                prepare.release.countDown()
                await(output.closed); join(videoWorker)
                assertEquals(0, output.starts.get())
                assertEquals(0, output.writes.get())
                assertEquals(1, output.closes.get())
                assertEquals(if (cancel) emptyList() else listOf("finished: synthetic video preparation failure"), callbacks.toList())
            } finally {
                session.stop(); prepare.release.countDown()
                join(videoWorker)
            }
            assertFilesUnchanged(folder, before)
        }
    }

    @Test
    fun preparedFramesOrVideoGapsFinishWithOrWithoutAudio() {
        for (audio in listOf(false, true)) for (gap in listOf(false, true)) {
            val folder = fixture(audio = audio, presented = !gap)
            val before = files(folder)
            val prepare = Gate()
            val display = Gate()
            val write = Gate()
            val finished = CountDownLatch(1)
            val callbacks = ConcurrentLinkedQueue<String>()
            val shown = ConcurrentLinkedQueue<String>()
            val nanos = AtomicLong()
            val output = Output().apply { beforeWrite = write::block }
            val from = 1_000_000L
            val until = from + 50_000
            lateinit var videoWorker: Thread
            val session = ReplayPlaybackSession(folder, from, until,
                { shown.add("frame"); display.block() },
                { failure -> callbacks.add("finished: $failure"); finished.countDown() }, callbacks::add,
                onGap = { us -> assertTrue(us in from until until); shown.add("gap"); display.block() },
                audioOutput = { output.line }, nanoTime = nanos::get,
                awaitAudio = { await(it); true },
                prepareVideo = { videoWorker = Thread.currentThread(); prepare.block(); it() })
            try {
                session.start()
                await(prepare.entered)
                nanos.addAndGet(5_000_000_000)
                assertEquals(from, session.nowUs())
                prepare.release.countDown()
                await(display.entered)
                if (audio) await(write.entered)
                assertEquals(listOf(if (gap) "gap" else "frame"), shown.toList())
                assertEquals(from, session.nowUs())
                assertEquals(if (audio) 1 else 0, output.starts.get())
                nanos.addAndGet(50_000_000)
                output.position.set(50_000)
                display.release.countDown(); write.release.countDown()
                await(finished); join(videoWorker)
                if (audio) await(output.closed)
                assertEquals(listOf("finished: null"), callbacks.toList())
            } finally {
                session.stop(); prepare.release.countDown(); display.release.countDown(); write.release.countDown()
                join(videoWorker)
            }
            assertFilesUnchanged(folder, before)
        }
    }

    @Test
    fun stoppedPreparationCannotStartAudioOrPublishIntoAReplacementSession() {
        val folder = fixture()
        val before = files(folder)
        val prepare = Gate()
        val audioWaiting = CountDownLatch(1)
        val oldCallbacks = ConcurrentLinkedQueue<String>()
        val oldOutput = Output()
        lateinit var oldWorker: Thread
        val old = ReplayPlaybackSession(folder, 1_000_000, 8_000_000,
            { oldCallbacks.add("frame") }, { oldCallbacks.add("finished") }, oldCallbacks::add,
            audioOutput = { oldOutput.line },
            prepareVideo = { oldWorker = Thread.currentThread(); prepare.block(); it() },
            awaitVideo = { ready -> audioWaiting.countDown(); ready.await() })
        val frame = Gate()
        val write = Gate()
        val newCallbacks = ConcurrentLinkedQueue<String>()
        val newOutput = Output().apply { beforeWrite = write::block }
        lateinit var newWorker: Thread
        val next = ReplayPlaybackSession(folder, 4_025_000, 8_000_000,
            { newWorker = Thread.currentThread(); frame.block() }, { newCallbacks.add("finished") }, newCallbacks::add,
            audioOutput = { newOutput.line }, awaitAudio = { await(it); true })
        try {
            old.start()
            await(prepare.entered); await(audioWaiting)
            old.stop(); await(oldOutput.closed)
            next.start()
            await(frame.entered); await(write.entered)
            prepare.release.countDown(); join(oldWorker)
            assertTrue(oldCallbacks.isEmpty(), oldCallbacks.toString())
            assertTrue(newCallbacks.isEmpty(), newCallbacks.toString())
            assertEquals(0, oldOutput.starts.get())
            assertEquals(0, oldOutput.writes.get())
            assertEquals(1, newOutput.starts.get())
            assertEquals(4_025_000L, next.nowUs())
        } finally {
            old.stop(); next.stop()
            prepare.release.countDown(); frame.release.countDown(); write.release.countDown()
            join(oldWorker)
        }
        join(newWorker); await(newOutput.closed)
        assertFilesUnchanged(folder, before)
    }

    @Test
    fun outputReturningAfterTheAudioDeadlineCannotRewindOrStart() {
        val nanos = AtomicLong()
        val factoryEntered = CountDownLatch(1)
        val releaseFactory = CountDownLatch(1)
        val videoRunning = CountDownLatch(1)
        val releaseVideo = CountDownLatch(1)
        val releaseWrite = CountDownLatch(1)
        val output = Output().apply { beforeWrite = { releaseWrite.await() } }
        val notices = ConcurrentLinkedQueue<String>()
        val from = 2_025_000L
        val session = ReplayPlaybackSession(fixture(), from, 8_000_000,
            { videoRunning.countDown(); releaseVideo.await() }, {}, notices::add,
            audioOutput = { factoryEntered.countDown(); releaseFactory.await(); output.line },
            nanoTime = nanos::get,
            awaitAudio = { await(factoryEntered); false })
        try {
            session.start()
            await(videoRunning)
            nanos.addAndGet(300_000_000)
            val before = session.nowUs()
            assertEquals(from + 300_000, before)
            releaseFactory.countDown()
            await(output.settled)
            assertTrue(session.nowUs() >= before, "late audio rewound the running video clock")
            assertEquals(0, output.starts.get())
            assertEquals(0, output.writes.get())
            assertEquals(1, notices.size)
        } finally {
            session.stop()
            releaseFactory.countDown(); releaseVideo.countDown(); releaseWrite.countDown()
            await(output.closed)
        }
    }

    @Test
    fun openOrStartAlreadyInFlightCannotBeAdoptedAfterTheDeadline() {
        for (stage in listOf("open", "start")) {
            val init = Gate()
            val video = Gate()
            val nanos = AtomicLong()
            val notices = ConcurrentLinkedQueue<String>()
            val output = Output().apply {
                if (stage == "open") beforeOpen = init::block else beforeStart = init::block
            }
            val session = ReplayPlaybackSession(fixture(), 2_025_000, 8_000_000,
                { video.block() }, {}, notices::add, audioOutput = { output.line },
                nanoTime = nanos::get, awaitAudio = { await(init.entered); false })
            try {
                session.start()
                await(video.entered)
                nanos.addAndGet(300_000_000)
                val before = session.nowUs()
                init.release.countDown()
                await(output.closed)
                assertEquals(before, session.nowUs(), stage)
                assertEquals(if (stage == "start") 1 else 0, output.starts.get(), stage)
                assertEquals(0, output.writes.get(), stage)
                assertEquals(1, output.closes.get(), stage)
                assertEquals(1, notices.size, stage)
            } finally {
                session.stop(); init.release.countDown(); video.release.countDown()
                await(output.closed)
            }
        }
    }

    @Test
    fun audioWinningAtTheDeadlineKeepsTheSeekOriginAndAudioClock() {
        for (from in listOf(0L, 2_025_000L)) {
            val write = Gate()
            val video = Gate()
            val nanos = AtomicLong()
            val notices = ConcurrentLinkedQueue<String>()
            val output = Output().apply { beforeWrite = write::block }
            val session = ReplayPlaybackSession(fixture(), from, 8_000_000,
                { video.block() }, {}, notices::add, audioOutput = { output.line },
                nanoTime = nanos::get, awaitAudio = { await(write.entered); false })
            try {
                session.start()
                await(video.entered)
                nanos.addAndGet(300_000_000)
                assertEquals(from, session.nowUs())
                output.position.set(250_000)
                assertEquals(from + 250_000, session.nowUs())
                assertEquals(1, output.starts.get())
                assertTrue(notices.isEmpty(), notices.toString())
            } finally {
                session.stop(); write.release.countDown(); video.release.countDown()
                await(output.closed)
            }
        }
    }

    @Test
    fun stopDuringFactoryOpenOrStartClosesLateOutputWithoutRevivingPlayback() {
        for (stage in listOf("factory", "open", "start")) {
            val init = Gate()
            val waitingVideo = CountDownLatch(1)
            val videoExited = CountDownLatch(1)
            val callbacks = ConcurrentLinkedQueue<String>()
            val output = Output().apply {
                if (stage == "open") beforeOpen = init::block
                if (stage == "start") beforeStart = init::block
            }
            val session = ReplayPlaybackSession(fixture(), 2_025_000, 8_000_000,
                { callbacks.add("frame") }, { callbacks.add("finished") }, { callbacks.add("audio") },
                audioOutput = { if (stage == "factory") init.block(); output.line },
                awaitAudio = {
                    waitingVideo.countDown()
                    try { CountDownLatch(1).await(); false } finally { videoExited.countDown() }
                })
            try {
                session.start()
                await(init.entered); await(waitingVideo)
                session.stop()
                await(videoExited)
                init.release.countDown()
                await(output.closed)
                assertEquals(if (stage == "start") 1 else 0, output.starts.get(), stage)
                assertEquals(0, output.writes.get(), stage)
                assertEquals(1, output.closes.get(), stage)
                assertTrue(callbacks.isEmpty(), "$stage: $callbacks")
            } finally {
                session.stop(); init.release.countDown()
                await(output.closed)
            }
        }
    }

    @Test
    fun audioFailureFallsBackContinuouslyAndNotifiesOnce() {
        for (stage in listOf("write", "drain")) {
            val failure = Gate()
            val video = Gate()
            val nanos = AtomicLong()
            val notices = ConcurrentLinkedQueue<String>()
            val output = Output().apply {
                position.set(350_000)
                val fail = { failure.block(); error("synthetic $stage failure") }
                if (stage == "write") beforeWrite = fail else beforeDrain = fail
                beforeClose = { position.set(0) }
            }
            val session = ReplayPlaybackSession(fixture(), 2_025_000, 8_000_000,
                { video.block() }, {}, notices::add, audioOutput = { output.line },
                nanoTime = nanos::get, awaitAudio = { await(failure.entered); true })
            try {
                session.start()
                await(video.entered)
                val before = session.nowUs()
                assertEquals(2_375_000L, before)
                failure.release.countDown()
                await(output.closed)
                assertEquals(before, session.nowUs(), stage)
                nanos.addAndGet(150_000_000)
                assertEquals(before + 150_000, session.nowUs(), stage)
                assertEquals(1, notices.size, stage)
                assertEquals(1, output.closes.get(), stage)
            } finally {
                session.stop(); failure.release.countDown(); video.release.countDown()
                await(output.closed)
            }
        }
    }

    @Test
    fun failedOpenOrStartClosesTheCandidateAndUsesMonotonicTimeOnce() {
        for (stage in listOf("open", "start")) {
            val video = Gate()
            val nanos = AtomicLong()
            val notices = ConcurrentLinkedQueue<String>()
            val output = Output().apply {
                val fail = { error("synthetic $stage failure") }
                if (stage == "open") beforeOpen = fail else beforeStart = fail
            }
            val session = ReplayPlaybackSession(fixture(), 2_025_000, 8_000_000,
                { video.block() }, {}, notices::add, audioOutput = { output.line },
                nanoTime = nanos::get, awaitAudio = { await(it); true })
            try {
                session.start()
                await(video.entered); await(output.closed)
                assertEquals(2_025_000L, session.nowUs(), stage)
                nanos.addAndGet(150_000_000)
                assertEquals(2_175_000L, session.nowUs(), stage)
                assertEquals(0, output.writes.get(), stage)
                assertEquals(1, output.closes.get(), stage)
                assertEquals(1, notices.size, stage)
            } finally {
                session.stop(); video.release.countDown()
                await(output.closed)
            }
        }
    }

    @Test
    fun stopUnblocksWriteOrDrainAndClosesWithoutAnAudioWarning() {
        for (stage in listOf("write", "drain")) {
            val io = Gate()
            val video = Gate()
            val notices = ConcurrentLinkedQueue<String>()
            val output = Output().apply {
                if (stage == "write") beforeWrite = io::block else beforeDrain = io::block
                beforeStop = { io.release.countDown() }
            }
            val session = ReplayPlaybackSession(fixture(), 0, 8_000_000,
                { video.block() }, {}, notices::add, audioOutput = { output.line },
                awaitAudio = { await(io.entered); true })
            try {
                session.start()
                await(video.entered)
                session.stop()
                await(output.closed)
                assertEquals(1, output.closes.get(), stage)
                assertEquals(if (stage == "drain") 1 else 0, output.drains.get(), stage)
                assertTrue(notices.isEmpty(), "$stage: $notices")
            } finally {
                session.stop(); io.release.countDown(); video.release.countDown()
                await(output.closed)
            }
        }
    }

    @Test
    fun outputFactoryFailureKeepsItsCategory() = assertAudioFailureCategory("factory")

    @Test
    fun outputOpenFailureKeepsItsCategory() = assertAudioFailureCategory("open")

    @Test
    fun outputStartFailureKeepsItsCategory() = assertAudioFailureCategory("start")

    @Test
    fun outputWriteFailureKeepsItsCategory() = assertAudioFailureCategory("write")

    @Test
    fun outputDrainFailureKeepsItsCategory() = assertAudioFailureCategory("drain")

    @Test
    fun corruptAacKeepsTheDecodeFailureCategory() = assertAudioFailureCategory("decode")

    @Test fun corruptOverlapPrimerRetainsSourceWhileKeptAudioStillPlays() {
        val sourceFailed = java.util.concurrent.atomic.AtomicBoolean()
        val folder = fixture(mediaStart = 0, corruptPacket = 6L)
        val output = Output()
        val video = Gate()
        val notices = ConcurrentLinkedQueue<String>()
        SavedReplay.open(folder).use { replay -> ReplayAudioTrack.open(replay)!!.use { track ->
            assertEquals(7, track.schedule(150_000).filterIsInstance<io.github.shinma06.replaybuffer.core.AudioChunk.Packets>().first().first)
        } }
        val session = ReplayPlaybackSession(folder, 150_000, 250_000, { video.block() }, {}, notices::add,
            audioOutput = { output.line }, awaitAudio = { await(it); true },
            onSourceFailure = { sourceFailed.set(true) })
        try {
            session.start()
            await(output.closed)
            await(video.entered)
            assertTrue(output.writes.get() > 0) // Only the unused overlap packet is corrupt; kept audio decodes.
            assertTrue(notices.isEmpty(), notices.toString())
            assertTrue(sourceFailed.get())
        } finally { session.stop(); video.release.countDown() }
        assertFalse(session.resourcesClosed.get(10, TimeUnit.SECONDS))
    }

    private fun assertAudioFailureCategory(stage: String) {
        val sourceFailed = java.util.concurrent.atomic.AtomicBoolean()
        val folder = fixture(mediaStart = 0, corruptAudio = stage == "decode")
        val before = files(folder)
        val output = Output().apply {
            val fail = { throw IllegalArgumentException("synthetic decode failure") }
            when (stage) {
                "open" -> beforeOpen = fail
                "start" -> beforeStart = fail
                "write" -> beforeWrite = fail
                "drain" -> beforeDrain = fail
            }
        }
        val video = Gate()
        val notified = CountDownLatch(1)
        val notices = ConcurrentLinkedQueue<String>()
        val session = ReplayPlaybackSession(folder, 0, 8_000_000, { video.block() }, {},
            { notices += it; notified.countDown() }, audioOutput = {
                if (stage == "factory") throw IllegalArgumentException("synthetic decode failure")
                output.line
            }, nanoTime = { 0L }, awaitAudio = { await(it); true }, onSourceFailure = { sourceFailed.set(true) })
        try {
            session.start()
            await(notified)
            if (stage != "factory") await(output.closed)
            await(video.entered)
            val reason = if (stage == "decode") "音声を再生できないため、音声なしで再生しています。"
                else "音声を出力できないため、音声なしで再生しています。"
            assertEquals(listOf(reason), notices.toList(), stage)
            assertEquals(stage == "decode", sourceFailed.get(), stage)
            assertEquals(if (stage == "factory") 0 else 1, output.closes.get(), stage)
            if (stage == "decode") {
                assertEquals(1, output.starts.get())
                assertEquals(0, output.writes.get())
            }
        } finally {
            session.stop(); video.release.countDown()
        }
        assertEquals(stage != "decode", session.resourcesClosed.get(10, TimeUnit.SECONDS), stage)
        assertFilesUnchanged(folder, before)
    }

    private class Gate {
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)

        fun block() {
            entered.countDown()
            var interrupted = false
            try {
                while (true) {
                    try {
                        check(release.await(10, TimeUnit.SECONDS)) { "gate was not released" }
                        return
                    } catch (_: InterruptedException) { interrupted = true }
                }
            } finally { if (interrupted) Thread.currentThread().interrupt() }
        }
    }

    @Test fun audioCallbackFailureCompletesAfterBothWorkerFinallyPaths() {
        val failed = CountDownLatch(1)
        val session = ReplayPlaybackSession(fixture(), 0, 8_000_000, {}, {}, {
            Thread.currentThread().uncaughtExceptionHandler = Thread.UncaughtExceptionHandler { _, _ -> failed.countDown() }
            error("Injected audio callback failure")
        }, audioOutput = { error("Injected output factory failure") })
        try {
            session.start()
            assertTrue(failed.await(10, TimeUnit.SECONDS))
            session.stop()
            assertFalse(session.resourcesClosed.get(10, TimeUnit.SECONDS))
        } finally { session.stop() }
    }

    @Test fun nativeAudioCloseFailureReportsUncertainCompletion() {
        val writing = CountDownLatch(1)
        val output = Output().apply {
            beforeWrite = { writing.countDown(); CountDownLatch(1).await() }
            beforeClose = { error("Injected native close failure") }
        }
        val session = ReplayPlaybackSession(fixture(), 0, 8_000_000, {}, {}, {}, audioOutput = { output.line })
        try {
            session.start()
            assertTrue(writing.await(10, TimeUnit.SECONDS))
            session.stop()
            assertFalse(session.resourcesClosed.get(10, TimeUnit.SECONDS))
        } finally { session.stop() }
    }

    private fun await(latch: CountDownLatch) {
        assertTrue(latch.await(10, TimeUnit.SECONDS), "playback latch did not complete")
    }

    private fun join(worker: Thread) {
        worker.join(10_000)
        assertTrue(!worker.isAlive, "playback worker did not finish")
    }

    private fun play(folder: Path, fromUs: Long = 0): Output {
        val output = Output()
        val finished = CountDownLatch(1)
        val failures = ConcurrentLinkedQueue<String>()
        val session = ReplayPlaybackSession(folder, fromUs, 8_000_000, {},
            { failure -> failure?.let(failures::add); finished.countDown() }, failures::add,
            audioOutput = { output.line })
        try {
            session.start()
            assertTrue(finished.await(10, TimeUnit.SECONDS), "playback did not finish: $failures")
            assertTrue(output.closed.await(10, TimeUnit.SECONDS), "audio line was not closed")
            assertTrue(failures.isEmpty(), failures.toString())
            return output
        } finally { session.stop(); assertTrue(session.resourcesClosed.get(10, TimeUnit.SECONDS)) }
    }

    private class Output {
        lateinit var format: AudioFormat
        private val bytes = ByteArrayOutputStream()
        val pcm: ByteArray get() = bytes.toByteArray()
        val closed = CountDownLatch(1)
        val settled = CountDownLatch(1)
        val starts = AtomicInteger()
        val writes = AtomicInteger()
        val closes = AtomicInteger()
        val drains = AtomicInteger()
        val position = AtomicLong(-1)
        var beforeOpen: () -> Unit = {}
        var beforeStart: () -> Unit = {}
        var beforeWrite: () -> Unit = {}
        var beforeDrain: () -> Unit = {}
        var beforeClose: () -> Unit = {}
        var beforeStop: () -> Unit = {}
        @Volatile private var drained = false
        val line = Proxy.newProxyInstance(SourceDataLine::class.java.classLoader, arrayOf(SourceDataLine::class.java)) { _, method, args ->
            when (method.name) {
                "open" -> { format = args[0] as AudioFormat; beforeOpen(); null }
                "start" -> { starts.incrementAndGet(); beforeStart(); null }
                "stop" -> { beforeStop(); null }
                "flush" -> null
                "write" -> {
                    writes.incrementAndGet()
                    settled.countDown()
                    beforeWrite()
                    val count = args[2] as Int
                    assertEquals(0, (args[1] as Int) % format.frameSize)
                    assertEquals(0, count % format.frameSize)
                    bytes.write(args[0] as ByteArray, args[1] as Int, count)
                    count
                }
                "getMicrosecondPosition" -> position.get().takeIf { it >= 0 } ?:
                    if (drained) (bytes.size() / format.frameSize * 1_000_000L / format.sampleRate.toInt()) else 0L
                "drain" -> { drains.incrementAndGet(); beforeDrain(); drained = true; null }
                "close" -> { closes.incrementAndGet(); beforeClose(); settled.countDown(); closed.countDown(); null }
                else -> error("Unexpected audio line call: ${method.name}")
            }
        } as SourceDataLine
    }

    private fun files(folder: Path): Map<String, ByteArray> = Files.list(folder).use { paths ->
        paths.toList().associate { it.fileName.toString() to Files.readAllBytes(it) }
    }

    private fun assertFilesUnchanged(folder: Path, before: Map<String, ByteArray>) {
        val after = files(folder)
        assertEquals(before.keys, after.keys)
        for ((name, bytes) in before) {
            assertContentEquals(bytes, after.getValue(name), name)
            assertContentEquals(MessageDigest.getInstance("SHA-256").digest(bytes),
                MessageDigest.getInstance("SHA-256").digest(after.getValue(name)), name)
        }
    }

    private fun onsets(output: Output): List<Double> {
        val pcm = output.pcm
        val frames = pcm.size / output.format.frameSize
        val rate = output.format.sampleRate.toInt()
        val block = rate / 100
        val starts = mutableListOf<Double>()
        var lastLoud = -rate
        for (start in 0 until frames - block step block) {
            var energy = 0.0
            for (frame in start until start + block) {
                val i = frame * output.format.frameSize
                val value = (pcm[i].toInt() and 255) or (pcm[i + 1].toInt() shl 8)
                energy += value.toDouble() * value
            }
            val loud = energy / block > 1_000_000
            if (loud) {
                if (start - lastLoud > rate / 5) starts += start.toDouble() / rate
                lastLoud = start
            }
        }
        return starts
    }

    private fun decodedPcm(resource: String): ByteArray =
        NIOUtils.readableChannel(Path.of(javaClass.getResource(resource)!!.toURI()).toFile()).use { input ->
            val track = MP4Demuxer.createRawMP4Demuxer(input).audioTracks.single() as AbstractMP4DemuxerTrack
            val config = NIOUtils.toArray(NodeBox.findFirst(track.sampleEntries.single(), EsdsBox::class.java, EsdsBox.fourcc()).streamInfo)
            val decoder = Decoder(config)
            val buffer = SampleBuffer().apply { isBigEndian = false }
            val bytes = ByteArrayOutputStream()
            while (true) {
                val packet = track.nextFrame() ?: break
                decoder.decodeFrame(NIOUtils.toArray(packet.data), buffer)
                bytes.write(buffer.data)
            }
            bytes.toByteArray()
        }

    private fun fixture(resource: String = "/aac-lc-escape/beeps.m4a", rate: Int = 48_000,
                        mediaStart: Long = 1024, gap: Boolean = false, audio: Boolean = true, presented: Boolean = true,
                        videoDurationUs: Long = 8_000_000, corruptAudio: Boolean = false, corruptPacket: Long? = null): Path {
        val folder = Files.createTempDirectory(root, "replay-")
        val videoRoot = "/replay-trim-parity/replay-20240101-000000-manual-feedface"
        val sourceVideo = Path.of(javaClass.getResource("$videoRoot/video.mp4")!!.toURI())
        val sourceAudio = Path.of(javaClass.getResource(resource)!!.toURI())
        NIOUtils.readableChannel(sourceVideo.toFile()).use { videoInput ->
            NIOUtils.readableChannel(sourceAudio.toFile()).use { audioInput ->
                NIOUtils.writableChannel(folder.resolve("video.mp4").toFile()).use { output ->
                    val mux = MP4Muxer.createMP4MuxerToChannel(output)
                    val videoSource = MP4Demuxer.createRawMP4Demuxer(videoInput).videoTrack as AbstractMP4DemuxerTrack
                    val video = mux.addTrack(MP4MuxerTrack(mux.nextTrackId, MP4TrackType.VIDEO))
                    video.addSampleEntry(videoSource.sampleEntries.first())
                    video.addFrameInternal(Packet.createPacket(videoSource.nextFrame().data, 0, 1_000_000,
                        videoDurationUs, 0, Packet.FrameType.KEY, null), 1)
                    val audioSource = MP4Demuxer.createRawMP4Demuxer(audioInput).audioTracks.single() as AbstractMP4DemuxerTrack
                    val audio = mux.addTrack(MP4MuxerTrack(mux.nextTrackId, MP4TrackType.SOUND))
                    audio.addSampleEntry(audioSource.sampleEntries.single())
                    var index = 0L
                    while (true) {
                        val packet = audioSource.nextFrame() ?: break
                        audio.addFrameInternal(Packet.createPacket(if (corruptAudio || index == corruptPacket) ByteBuffer.wrap(byteArrayOf(0)) else packet.data, index * 1024, rate,
                            1024, index, Packet.FrameType.KEY, null), 1)
                        index++
                    }
                    mux.finish()
                }
            }
        }
        val row = javaClass.getResource("$videoRoot/video-frames.jsonl")!!.readText().lineSequence().first()
            .let { JsonParser.parseString(it).asJsonObject }
        row.addProperty("movie_pts_us", "0")
        row.addProperty("movie_end_us", videoDurationUs.toString())
        row.addProperty("presented", presented)
        Files.writeString(folder.resolve("video-frames.jsonl"), "$row\n")
        fun segment(start: Long, duration: Long, media: Long): String {
            val packets = (media % 1024 + duration * rate / 1_000_000 + 1023) / 1024
            return """{"movie_start_us":"$start","duration_us":"$duration","media_start_samples":"$media","samples":$packets}"""
        }
        val segments = if (gap) listOf(segment(0, 3_000_000, mediaStart), segment(3_500_000, 4_500_000, mediaStart + 3L * rate))
            else listOf(segment(0, 8_000_000, mediaStart))
        fun sha(name: String) = MessageDigest.getInstance("SHA-256").digest(Files.readAllBytes(folder.resolve(name)))
            .joinToString("") { "%02x".format(it) }
        val audioTrack = if (audio) """, "audio_track":{"sample_rate":$rate,"segments":[${segments.joinToString(",")}]}""" else ""
        Files.writeString(folder.resolve("replay.json"), """{
            "schema":4,"complete":true,"files":{"video":"video.mp4","video_frames":"video-frames.jsonl"},
            "parts":[{"movie_end_us":"8000000"}],
            "files_sha256":{"video.mp4":"${sha("video.mp4")}","video-frames.jsonl":"${sha("video-frames.jsonl")}"}$audioTrack
        }""")
        return folder
    }
}
