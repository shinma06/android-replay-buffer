package io.github.shinma06.arb.playback

import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class AudioSyncPlayerTest {
    private class RecordingAudioSink(
        override val isAvailable: Boolean = true,
        private val failOnOpen: Boolean = false,
    ) : AudioSink {
        var openedFormat: PcmFormat? = null
        var started: Boolean = false
        var stopCount: Int = 0
        var flushCount: Int = 0
        var closed: Boolean = false
        val writtenChunks = mutableListOf<ByteArray>()

        override fun open(format: PcmFormat): Boolean {
            if (!isAvailable || failOnOpen) return false
            openedFormat = format
            return true
        }

        override fun start() {
            started = true
        }

        override fun stop() {
            started = false
            stopCount++
        }

        override fun flush() {
            flushCount++
        }

        override fun write(pcm: ByteArray, offset: Int, length: Int): Int {
            if (!started || length <= 0) return 0
            writtenChunks.add(pcm.copyOfRange(offset, offset + length))
            return length
        }

        override fun close() {
            closed = true
            started = false
        }

        fun allWrittenBytes(): ByteArray {
            val out = ByteArrayOutputStream()
            for (c in writtenChunks) out.write(c)
            return out.toByteArray()
        }
    }

    /**
     * Builds synthetic s16le stereo PCM where each sample frame `i` encodes `(L = i + 1, R = -(i + 1))`
     * so tests can assert exact sample-level alignment after WAV round-trip and avOffsetMs seeks.
     */
    private fun syntheticTaggedPcm(sampleRateHz: Int, durationMs: Int): ByteArray {
        val totalFrames = (sampleRateHz * durationMs) / 1000
        val out = ByteArray(totalFrames * 4)
        val bb = ByteBuffer.wrap(out).order(ByteOrder.LITTLE_ENDIAN)
        for (i in 0 until totalFrames) {
            val v = ((i + 1) and 0x7FFF).toShort()
            bb.putShort(v)
            bb.putShort((-v).toShort())
        }
        return out
    }

    private fun readFirstSampleLeft(pcm: ByteArray): Short =
        ByteBuffer.wrap(pcm, 0, 2).order(ByteOrder.LITTLE_ENDIAN).short

    @Test
    fun wavFilePcmSourceRoundTripsFileAndBytesAndSkipsExtraChunks(@TempDir tmp: Path) {
        val sampleRate = 10_000 // 10 samples per ms for crisp exact assertions
        val rawPcm = syntheticTaggedPcm(sampleRateHz = sampleRate, durationMs = 200) // 2000 sample frames
        val standardWav = WavFilePcmSource.encodeWav(sampleRateHz = sampleRate, channels = 2, pcmBytes = rawPcm)

        // Inject an odd-sized JUNK chunk between fmt and data to test RIFF word-alignment parsing!
        val withJunk = ByteArrayOutputStream().apply {
            write(standardWav, 0, 36) // RIFF + WAVE + fmt chunk (36 bytes)
            write("JUNK".toByteArray())
            val junkSize = ByteBuffer.allocate(4).order(ByteOrder.LITTLE_ENDIAN).putInt(5).array()
            write(junkSize)
            write(byteArrayOf(1, 2, 3, 4, 5, 0)) // 5 bytes + 1 pad byte
            write(standardWav, 36, standardWav.size - 36) // data chunk
        }.toByteArray()
        // Update RIFF chunk size field
        ByteBuffer.wrap(withJunk, 4, 4).order(ByteOrder.LITTLE_ENDIAN).putInt(withJunk.size - 8)

        val wavPath = tmp.resolve("decoded.wav")
        Files.write(wavPath, withJunk)

        WavFilePcmSource.open(wavPath).use { fileSrc ->
            assertEquals(sampleRate, fileSrc.sampleRateHz)
            assertEquals(2, fileSrc.channels)
            assertEquals(200_000_000L, fileSrc.durationNs)

            // Read [50ms, 70ms) -> sample frames [500, 700) -> 200 frames * 4 bytes = 800 bytes
            val slice = fileSrc.readPcmSlice(50_000_000L, 70_000_000L)
            assertEquals(800, slice.size)
            assertEquals(501.toShort(), readFirstSampleLeft(slice))

            // Out-of-range and empty intervals return empty array
            assertEquals(0, fileSrc.readPcmSlice(250_000_000L, 300_000_000L).size)
            assertEquals(0, fileSrc.readPcmSlice(70_000_000L, 50_000_000L).size)

            // In-memory parser returns byte-identical slice
            WavFilePcmSource.fromBytes(withJunk).use { memSrc ->
                assertEquals(fileSrc.durationNs, memSrc.durationNs)
                assertContentEquals(slice, memSrc.readPcmSlice(50_000_000L, 70_000_000L))
            }
        }

        // Reject invalid RIFF header
        assertFailsWith<WavFormatException> {
            WavFilePcmSource.fromBytes("NOT_A_WAV_FILE_AT_ALL".toByteArray())
        }
    }

    @Test
    fun avOffsetMsAlignmentAndImmediateFlushOnSeekWorkForPositiveAndNegativeOffsets() {
        val sampleRate = 10_000 // 10 samples/ms
        val rawPcm = syntheticTaggedPcm(sampleRateHz = sampleRate, durationMs = 500)
        val wav = WavFilePcmSource.encodeWav(sampleRateHz = sampleRate, channels = 2, pcmBytes = rawPcm)
        val sink = RecordingAudioSink()

        // Positive avOffsetMs = +50ms (audio starts 50ms AFTER video frame 0):
        // videoPts = 0ms -> audioPts = -50ms (emits silence until videoPts reaches 50ms!)
        AudioSyncPlayer(
            source = WavFilePcmSource.fromBytes(wav),
            sink = sink,
            avOffsetMs = 50L,
            sliceDurationNs = 20_000_000L,
            autoRunPump = false,
        ).use { player ->
            assertEquals(AudioState.PAUSED, player.audioState)
            assertEquals(-50_000_000L, player.videoPtsToAudioPtsNs(0L))
            assertEquals(50_000_000L, player.videoPtsToAudioPtsNs(100_000_000L))

            player.seek(0L)
            assertEquals(1, sink.flushCount, "seek must flush sink immediately")
            player.play()
            assertEquals(AudioState.PLAYING, player.audioState)

            // First 40ms (2 x 20ms) at video [0..40ms) -> audio [-50..-10ms) is pure silence!
            assertEquals(800, player.pumpNextSlice(20_000_000L))
            assertEquals(800, player.pumpNextSlice(20_000_000L))
            assertTrue(sink.allWrittenBytes().all { it == 0.toByte() }, "Lead-in window must emit silence")

            // Next 20ms at video [40..60ms) -> audio [-10..+10ms): 10ms silence + 10ms audio starting at sample #1!
            sink.writtenChunks.clear()
            assertEquals(800, player.pumpNextSlice(20_000_000L))
            val bridge = sink.writtenChunks.single()
            assertTrue(bridge.copyOfRange(0, 400).all { it == 0.toByte() })
            assertEquals(1.toShort(), readFirstSampleLeft(bridge.copyOfRange(400, 800)))

            // Seek to video = 150ms -> audio = 100ms (sample index 1000 -> value 1001) and flush immediately!
            sink.writtenChunks.clear()
            val flushesBefore = sink.flushCount
            player.seek(150_000_000L)
            assertEquals(flushesBefore + 1, sink.flushCount, "seek must immediately flush stale audio")
            assertEquals(800, player.pumpNextSlice(20_000_000L))
            assertEquals(1001.toShort(), readFirstSampleLeft(sink.writtenChunks.single()))

            // Negative avOffsetMs = -30ms (audio started 30ms BEFORE video):
            // videoPts = 20ms -> audioPts = 50ms (sample index 500 -> value 501)!
            sink.writtenChunks.clear()
            player.setAvOffsetMs(-30L, currentVideoPtsNs = 20_000_000L)
            assertEquals(50_000_000L, player.audioPositionNs)
            assertEquals(800, player.pumpNextSlice(20_000_000L))
            assertEquals(501.toShort(), readFirstSampleLeft(sink.writtenChunks.single()))
        }
    }

    @Test
    fun mutesCleanlyDuringScrubbingAndNonNormalSpeedsWithoutPitchDistortion() {
        val pcm = syntheticTaggedPcm(sampleRateHz = 10_000, durationMs = 200)
        val sink = RecordingAudioSink()

        AudioSyncPlayer(
            source = InMemoryPcmSource(10_000, 2, pcm),
            sink = sink,
            autoRunPump = false,
        ).use { player ->
            player.play()
            assertEquals(AudioState.PLAYING, player.audioState)
            assertTrue(player.pumpNextSlice(20_000_000L) > 0)

            // 2.0x speed -> mutes cleanly and writes 0 bytes
            val flushesBeforeDouble = sink.flushCount
            player.setPlaybackSpeed(PlaybackSpeed.DOUBLE)
            assertEquals(AudioState.MUTED, player.audioState)
            assertFalse(sink.started)
            assertTrue(sink.flushCount > flushesBeforeDouble)
            assertEquals(0, player.pumpNextSlice(20_000_000L))

            // 0.5x speed -> remains muted
            player.setPlaybackSpeed(PlaybackSpeed.HALF)
            assertEquals(AudioState.MUTED, player.audioState)
            assertEquals(0, player.pumpNextSlice(20_000_000L))

            // Back to 1.0x -> resumes PLAYING
            player.setPlaybackSpeed(PlaybackSpeed.NORMAL)
            assertEquals(AudioState.PLAYING, player.audioState)
            assertTrue(sink.started)
            assertTrue(player.pumpNextSlice(20_000_000L) > 0)

            // Explicit mute / unmute
            player.mute()
            assertEquals(AudioState.MUTED, player.audioState)
            assertEquals(0, player.pumpNextSlice(20_000_000L))
            player.unmute()
            assertEquals(AudioState.PLAYING, player.audioState)

            // Scrubbing mutes and pauses cleanly
            player.setScrubbing(true)
            assertEquals(AudioState.MUTED, player.audioState)
            assertEquals(0, player.pumpNextSlice(20_000_000L))
            player.setScrubbing(false)
            assertEquals(AudioState.PAUSED, player.audioState)
        }
    }

    @Test
    fun gracefullyDegradesToUnavailableWhenHostHasNoAudioHardwareOrNoSource() {
        val pcm = syntheticTaggedPcm(sampleRateHz = 48_000, durationMs = 100)

        // 1. Hardware unavailable sink
        val unavailableSink = RecordingAudioSink(isAvailable = false)
        AudioSyncPlayer(
            source = InMemoryPcmSource(48_000, 2, pcm),
            sink = unavailableSink,
            autoRunPump = true,
        ).use { player ->
            assertFalse(player.isAvailable)
            assertEquals(AudioState.UNAVAILABLE, player.audioState)
            // None of these may ever throw!
            player.play()
            player.seek(50_000_000L)
            player.setPlaybackSpeed(PlaybackSpeed.DOUBLE)
            player.setScrubbing(true)
            player.setScrubbing(false)
            player.mute()
            player.unmute()
            player.pause()
            assertEquals(0, player.pumpNextSlice())
            assertEquals(AudioState.UNAVAILABLE, player.audioState)
        }

        // 2. Sink fails to open line (e.g. headless VM with no mixer)
        val failOpenSink = RecordingAudioSink(isAvailable = true, failOnOpen = true)
        AudioSyncPlayer(
            source = InMemoryPcmSource(48_000, 2, pcm),
            sink = failOpenSink,
            autoRunPump = false,
        ).use { player ->
            assertFalse(player.isAvailable)
            assertEquals(AudioState.UNAVAILABLE, player.audioState)
        }

        // 3. Null audio source (video-only bundle)
        val okSink = RecordingAudioSink(isAvailable = true)
        AudioSyncPlayer(
            source = null,
            sink = okSink,
            autoRunPump = false,
        ).use { player ->
            assertFalse(player.isAvailable)
            assertEquals(AudioState.UNAVAILABLE, player.audioState)
            player.play()
            assertEquals(AudioState.UNAVAILABLE, player.audioState)
        }

        // 4. Real JvmAudioSink never throws even on headless hosts (either opens cleanly or returns false).
        JvmAudioSink().use { jvmSink ->
            val opened = jvmSink.open(PcmFormat.DEFAULT)
            assertEquals(opened, jvmSink.isAvailable)
            jvmSink.stop()
            jvmSink.flush()
        }
    }

    @Test
    fun backgroundAudioPumpStreamsPcmAndClosesWithoutLeakingThreads() {
        val pcm = syntheticTaggedPcm(sampleRateHz = 10_000, durationMs = 60)
        val receivedAll = CountDownLatch(1)
        var totalWritten = 0
        val sink = object : AudioSink {
            override val isAvailable: Boolean = true
            var closed = false
            override fun open(format: PcmFormat): Boolean = true
            override fun start() {}
            override fun stop() {}
            override fun flush() {}
            override fun write(pcm: ByteArray, offset: Int, length: Int): Int {
                synchronized(this) {
                    totalWritten += length
                    if (totalWritten >= 2400) receivedAll.countDown()
                }
                return length
            }
            override fun close() {
                closed = true
            }
        }

        val player = AudioSyncPlayer(
            source = InMemoryPcmSource(10_000, 2, pcm),
            sink = sink,
            sliceDurationNs = 20_000_000L,
            autoRunPump = true,
        )
        player.play()
        assertTrue(receivedAll.await(5, TimeUnit.SECONDS), "Background audio pump must stream PCM")

        player.close()
        player.close()
        assertTrue(sink.closed)
        val leaked = Thread.getAllStackTraces().keys.filter { it.isAlive && it.name == "arb-audio-sync-pump" }
        assertTrue(leaked.isEmpty(), "Expected zero leaked audio pump threads, found: $leaked")
    }

    @Test
    fun pumpBacksOffInsteadOfSpinningWhenSinkKeepsRefusingData() {
        val pcm = syntheticTaggedPcm(sampleRateHz = 10_000, durationMs = 500)
        val inner = InMemoryPcmSource(10_000, 2, pcm)
        val reads = java.util.concurrent.atomic.AtomicInteger()
        val source = object : PcmAudioSource by inner {
            override fun readPcmSlice(startNs: Long, endNs: Long): ByteArray {
                reads.incrementAndGet()
                return inner.readPcmSlice(startNs, endNs)
            }
        }
        val sink = object : AudioSink {
            override val isAvailable: Boolean = true
            override fun open(format: PcmFormat): Boolean = true
            override fun start() {}
            override fun stop() {}
            override fun flush() {}
            override fun write(pcm: ByteArray, offset: Int, length: Int): Int = 0 // e.g. line closed / device unplugged
            override fun close() {}
        }
        AudioSyncPlayer(source = source, sink = sink, sliceDurationNs = 20_000_000L, autoRunPump = true).use { player ->
            player.play()
            Thread.sleep(400)
            // 20 ms back-off => roughly 20 reads in 400 ms; a tight loop would issue many thousands.
            assertTrue(reads.get() < 100, "Pump must back off when write returns 0 (reads=${reads.get()})")
        }
    }

    @Test
    fun pumpSurvivesSourceReadFailureReportsUnavailableAndRecoversOnSeek() {
        val pcm = syntheticTaggedPcm(sampleRateHz = 10_000, durationMs = 500)
        val inner = InMemoryPcmSource(10_000, 2, pcm)
        val failing = java.util.concurrent.atomic.AtomicBoolean(true)
        val source = object : PcmAudioSource by inner {
            override fun readPcmSlice(startNs: Long, endNs: Long): ByteArray {
                if (failing.get()) throw java.io.IOException("simulated read failure")
                return inner.readPcmSlice(startNs, endNs)
            }
        }
        val written = java.util.concurrent.atomic.AtomicInteger()
        val sink = object : AudioSink {
            override val isAvailable: Boolean = true
            override fun open(format: PcmFormat): Boolean = true
            override fun start() {}
            override fun stop() {}
            override fun flush() {}
            override fun write(pcm: ByteArray, offset: Int, length: Int): Int {
                written.addAndGet(length)
                return length
            }
            override fun close() {}
        }
        AudioSyncPlayer(source = source, sink = sink, sliceDurationNs = 20_000_000L, autoRunPump = true).use { player ->
            player.play()
            val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
            while (player.audioState != AudioState.UNAVAILABLE && System.nanoTime() < deadline) Thread.sleep(10)
            assertEquals(AudioState.UNAVAILABLE, player.audioState, "Read failure must surface as UNAVAILABLE, not a silent PLAYING")
            assertTrue(
                Thread.getAllStackTraces().keys.any { it.isAlive && it.name == "arb-audio-sync-pump" },
                "Pump thread must stay alive after a read failure",
            )

            failing.set(false)
            player.seek(0L)
            val recoverDeadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
            while (written.get() == 0 && System.nanoTime() < recoverDeadline) Thread.sleep(10)
            assertTrue(written.get() > 0, "Pump must resume streaming after a seek once the source recovers")
            assertEquals(AudioState.PLAYING, player.audioState)
        }
    }

    @Test
    fun staleSliceWrittenAfterConcurrentSeekIsFlushedAgain() {
        val pcm = syntheticTaggedPcm(sampleRateHz = 10_000, durationMs = 200)
        val events = java.util.Collections.synchronizedList(mutableListOf<String>())
        lateinit var player: AudioSyncPlayer
        var seekDuringWrite = true
        val sink = object : AudioSink {
            override val isAvailable: Boolean = true
            override fun open(format: PcmFormat): Boolean = true
            override fun start() {}
            override fun stop() {}
            override fun flush() {
                events.add("flush")
            }
            override fun write(pcm: ByteArray, offset: Int, length: Int): Int {
                events.add("write")
                if (seekDuringWrite) {
                    seekDuringWrite = false
                    player.seek(100_000_000L) // seek lands while this (now stale) slice is being written
                }
                return length
            }
            override fun close() {}
        }
        player = AudioSyncPlayer(
            source = InMemoryPcmSource(10_000, 2, pcm),
            sink = sink,
            sliceDurationNs = 20_000_000L,
            autoRunPump = false,
        )
        player.use {
            it.play()
            events.clear()
            it.pumpNextSlice()
            // write -> seek's flush (during write) -> second flush dropping the stale slice
            assertEquals(listOf("write", "flush", "flush"), events.toList(), "Stale PCM written after a seek's flush must be flushed again")
            assertEquals(100_000_000L, it.audioPositionNs, "Stale slice must not advance the new position")
        }
    }

    @Test
    fun wavDataChunkBeforeFmtParsesButUnboundedDataBeforeFmtGivesClearError() {
        val sampleRate = 10_000
        val raw = syntheticTaggedPcm(sampleRateHz = sampleRate, durationMs = 20)
        val std = WavFilePcmSource.encodeWav(sampleRateHz = sampleRate, channels = 2, pcmBytes = raw)
        val fmtChunk = std.copyOfRange(12, 36)
        val dataChunk = std.copyOfRange(36, std.size)
        val header = std.copyOfRange(0, 12)

        val reordered = header + dataChunk + fmtChunk
        WavFilePcmSource.fromBytes(reordered).use { src ->
            assertEquals(sampleRate, src.sampleRateHz)
            assertEquals(raw.size / 4, (src.durationNs * sampleRate / 1_000_000_000L).toInt())
        }

        // Streaming-style data chunk (size 0xFFFFFFFF) before 'fmt ' runs to EOF: fmt can never follow.
        val unbounded = header + dataChunk.copyOf() .also {
            ByteBuffer.wrap(it, 4, 4).order(ByteOrder.LITTLE_ENDIAN).putInt(-1)
        }
        val ex = assertFailsWith<WavFormatException> { WavFilePcmSource.fromBytes(unbounded) }
        assertTrue(ex.message!!.contains("precedes 'fmt '"), "Expected a clear data-before-fmt error, got: ${ex.message}")
    }
}
