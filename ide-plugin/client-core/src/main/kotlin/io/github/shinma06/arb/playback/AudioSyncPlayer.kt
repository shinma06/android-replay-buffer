package io.github.shinma06.arb.playback

import java.io.Closeable
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.TimeUnit
import java.util.concurrent.locks.ReentrantLock
import javax.sound.sampled.AudioFormat
import javax.sound.sampled.AudioSystem
import javax.sound.sampled.DataLine
import javax.sound.sampled.LineUnavailableException
import javax.sound.sampled.SourceDataLine
import kotlin.concurrent.withLock

/**
 * Output sink abstraction for signed 16-bit little-endian PCM playback.
 *
 * Production uses [JvmAudioSink] (`javax.sound.sampled.SourceDataLine`); unit tests inject a
 * deterministic fake sink without touching host audio hardware.
 */
interface AudioSink : Closeable {
    /** `true` when the sink is capable of opening/playing audio on this host. */
    val isAvailable: Boolean

    /**
     * Opens the output device for [format]. Returns `true` on success or `false` if no compatible
     * audio hardware/mixer exists (must never throw an uncaught exception on headless hosts).
     */
    fun open(format: PcmFormat): Boolean

    fun start()
    fun stop()
    fun flush()

    /**
     * Writes `[offset, offset + length)` of signed 16-bit LE PCM to the sink.
     * Returns the number of bytes accepted (or 0 if unavailable/stopped).
     */
    fun write(pcm: ByteArray, offset: Int = 0, length: Int = pcm.size): Int

    override fun close()
}

/**
 * Cross-OS JDK [AudioSink] backed by `javax.sound.sampled.SourceDataLine` (16-bit signed LE PCM,
 * 48 kHz stereo default).
 *
 * Gracefully degrades to unavailable (`isAvailable == false`) on headless CI / VM hosts with no
 * audio mixer without ever throwing an uncaught exception.
 */
class JvmAudioSink(
    private val targetBufferMs: Int = DEFAULT_BUFFER_MS,
) : AudioSink {
    private val lock = Any()

    @Volatile
    private var line: SourceDataLine? = null

    @Volatile
    private var available: Boolean = true

    @Volatile
    private var openedFormat: PcmFormat? = null

    override val isAvailable: Boolean
        get() = available

    override fun open(format: PcmFormat): Boolean = synchronized(lock) {
        if (openedFormat == format && line?.isOpen == true) {
            return true
        }
        closeLineLocked()
        val audioFormat = AudioFormat(
            format.sampleRateHz.toFloat(),
            format.bitsPerSample,
            format.channels,
            /* signed = */ true,
            /* bigEndian = */ false,
        )
        val info = DataLine.Info(SourceDataLine::class.java, audioFormat)
        return try {
            if (!AudioSystem.isLineSupported(info)) {
                available = false
                false
            } else {
                val acquired = AudioSystem.getLine(info) as? SourceDataLine
                if (acquired == null) {
                    available = false
                    false
                } else {
                    val rawBufferBytes = ((format.bytesPerSecond * targetBufferMs.coerceAtLeast(20).toLong()) / 1_000L)
                        .coerceAtMost(Int.MAX_VALUE.toLong())
                        .toInt()
                    val alignedBufferBytes =
                        ((rawBufferBytes / format.frameSizeBytes).coerceAtLeast(1)) * format.frameSizeBytes
                    acquired.open(audioFormat, alignedBufferBytes)
                    line = acquired
                    openedFormat = format
                    available = true
                    true
                }
            }
        } catch (_: LineUnavailableException) {
            closeLineLocked()
            available = false
            false
        } catch (_: IllegalArgumentException) {
            closeLineLocked()
            available = false
            false
        } catch (_: SecurityException) {
            closeLineLocked()
            available = false
            false
        } catch (_: UnsupportedOperationException) {
            closeLineLocked()
            available = false
            false
        }
    }

    override fun start() {
        val current = line ?: return
        runCatching { current.start() }
    }

    override fun stop() {
        val current = line ?: return
        runCatching { current.stop() }
    }

    override fun flush() {
        val current = line ?: return
        runCatching { current.flush() }
    }

    override fun write(pcm: ByteArray, offset: Int, length: Int): Int {
        if (length <= 0) return 0
        val current = line ?: return 0
        if (!current.isOpen) return 0
        return try {
            current.write(pcm, offset, length)
        } catch (_: RuntimeException) {
            0
        }
    }

    override fun close() = synchronized(lock) {
        closeLineLocked()
    }

    private fun closeLineLocked() {
        val existing = line
        line = null
        openedFormat = null
        if (existing != null) {
            runCatching { existing.stop() }
            runCatching { existing.flush() }
            runCatching { existing.close() }
        }
    }

    companion object {
        const val DEFAULT_BUFFER_MS: Int = 80
    }
}

/**
 * Cross-OS JDK synchronized PCM audio player driven alongside [PlaybackController].
 *
 * Design guarantees:
 * - Honors `avOffsetMs` honestly using the ARB v2 sign convention
 *   (`audio_pts_ns = video_pts_ns - avOffsetMs * 1_000_000L`, matching `arb-core` / `arb-media`).
 * - Immediately flushes the output [AudioSink] on [seek] so no stale buffered audio plays.
 * - Cleanly mutes and silences output during `SCRUBBING` or non-`1.0x` playback speeds
 *   (`HALF` / `DOUBLE`), avoiding chipmunk pitch distortion.
 * - Degrades cleanly to [AudioState.UNAVAILABLE] when [source] is `null` or host audio hardware is
 *   absent, without ever throwing an uncaught exception.
 */
class AudioSyncPlayer(
    private val source: PcmAudioSource?,
    private val sink: AudioSink = JvmAudioSink(),
    avOffsetMs: Long = 0L,
    muted: Boolean = false,
    private val sliceDurationNs: Long = DEFAULT_SLICE_DURATION_NS,
    private val autoRunPump: Boolean = true,
    private val closeSourceOnClose: Boolean = true,
) : Closeable {
    init {
        require(sliceDurationNs > 0L) { "sliceDurationNs must be positive: $sliceDurationNs" }
    }

    private val lock = ReentrantLock()
    private val stateChanged = lock.newCondition()
    private val listeners = CopyOnWriteArrayList<(AudioState) -> Unit>()

    private var closed: Boolean = false
    private var sinkReady: Boolean = false
    /** Set when the pump hit an unrecoverable read/write error; cleared by the next [seek]. */
    private var pumpFailed: Boolean = false
    private var playingRequested: Boolean = false
    private var scrubbing: Boolean = false
    private var speed: PlaybackSpeed = PlaybackSpeed.NORMAL
    private var userMuted: Boolean = muted
    private var avOffsetMsValue: Long = avOffsetMs
    private var currentAudioPtsNs: Long = 0L
    private var seekGeneration: Long = 0L

    @Volatile
    private var publishedState: AudioState = AudioState.UNAVAILABLE

    private var pumpThread: Thread? = null

    init {
        sinkReady = initializeSink()
        publishedState = computeAudioStateLocked()
        if (autoRunPump && sinkReady) {
            pumpThread = Thread(::runPumpLoop, "arb-audio-sync-pump").apply {
                isDaemon = true
                start()
            }
        }
    }

    val audioState: AudioState
        get() = publishedState

    val isAvailable: Boolean
        get() = sinkReady

    val isMuted: Boolean
        get() = lock.withLock { userMuted }

    val avOffsetMs: Long
        get() = lock.withLock { avOffsetMsValue }

    val audioPositionNs: Long
        get() = lock.withLock { currentAudioPtsNs }

    /**
     * Converts a relative video presentation timestamp (`videoPtsNs >= 0`) to the corresponding
     * audio track timestamp using the honest `avOffsetMs` metadata:
     * `audioPtsNs = videoPtsNs - avOffsetMs * 1_000_000`.
     *
     * Note: a positive `avOffsetMs` means audio began later than video, so `audioPtsNs` can be
     * negative during the lead-in window (during which silence is emitted).
     */
    fun videoPtsToAudioPtsNs(videoPtsNs: Long): Long {
        val offsetNs = saturatingMsToNs(lock.withLock { avOffsetMsValue })
        return saturatingSub(videoPtsNs, offsetNs)
    }

    fun addStateListener(listener: (AudioState) -> Unit): AutoCloseable {
        listeners.add(listener)
        return AutoCloseable { listeners.remove(listener) }
    }

    fun play() = updateStateAndNotify {
        if (closed) return@updateStateAndNotify
        playingRequested = true
        applySinkTransportLocked(flush = false)
    }

    fun resume() = play()

    fun pause() = updateStateAndNotify {
        if (closed) return@updateStateAndNotify
        playingRequested = false
        applySinkTransportLocked(flush = false)
    }

    fun mute() = setMuted(true)

    fun unmute() = setMuted(false)

    fun setMuted(muted: Boolean) = updateStateAndNotify {
        if (closed) return@updateStateAndNotify
        if (userMuted == muted) return@updateStateAndNotify
        userMuted = muted
        applySinkTransportLocked(flush = muted)
    }

    fun setScrubbing(isScrubbing: Boolean) = updateStateAndNotify {
        if (closed) return@updateStateAndNotify
        if (scrubbing == isScrubbing) return@updateStateAndNotify
        scrubbing = isScrubbing
        if (isScrubbing) {
            playingRequested = false
        }
        applySinkTransportLocked(flush = isScrubbing)
    }

    fun setPlaybackSpeed(newSpeed: PlaybackSpeed) = updateStateAndNotify {
        if (closed) return@updateStateAndNotify
        if (speed == newSpeed) return@updateStateAndNotify
        speed = newSpeed
        applySinkTransportLocked(flush = newSpeed != PlaybackSpeed.NORMAL)
    }

    fun setAvOffsetMs(newAvOffsetMs: Long, currentVideoPtsNs: Long? = null) = updateStateAndNotify {
        if (closed) return@updateStateAndNotify
        avOffsetMsValue = newAvOffsetMs
        if (currentVideoPtsNs != null) {
            val targetAudioNs = saturatingSub(currentVideoPtsNs, saturatingMsToNs(newAvOffsetMs))
            seekGeneration++
            currentAudioPtsNs = targetAudioNs
            if (sinkReady) {
                sink.flush()
            }
        }
        applySinkTransportLocked(flush = false)
    }

    /**
     * Seeks audio to the position corresponding to [videoPtsNs] (honoring `avOffsetMs`) and
     * immediately flushes the underlying [AudioSink] so no stale buffered audio plays.
     */
    fun seek(videoPtsNs: Long) = updateStateAndNotify {
        if (closed) return@updateStateAndNotify
        val targetAudioNs = saturatingSub(videoPtsNs, saturatingMsToNs(avOffsetMsValue))
        seekGeneration++
        pumpFailed = false
        currentAudioPtsNs = targetAudioNs
        if (sinkReady) {
            sink.flush()
        }
        applySinkTransportLocked(flush = false)
    }

    /**
     * Deterministically pumps one slice of PCM (default [sliceDurationNs]) to [sink] if currently
     * in [AudioState.PLAYING]. Returns the number of PCM bytes written, or `0` if paused/muted/
     * unavailable/at end of audio.
     */
    fun pumpNextSlice(durationNs: Long = sliceDurationNs): Int {
        require(durationNs > 0L) { "durationNs must be positive: $durationNs" }
        val src = source ?: return 0

        val snapshotGen: Long
        val sliceStartNs: Long
        val sliceEndNs: Long
        lock.withLock {
            if (closed || !sinkReady || computeAudioStateLocked() != AudioState.PLAYING) {
                return 0
            }
            if (currentAudioPtsNs >= src.durationNs) {
                return 0
            }
            snapshotGen = seekGeneration
            sliceStartNs = currentAudioPtsNs
            sliceEndNs = saturatingAdd(sliceStartNs, durationNs)
        }

        val pcm = buildAlignedSlice(src, sliceStartNs, sliceEndNs)
        if (pcm.isEmpty()) {
            lock.withLock {
                if (!closed && seekGeneration == snapshotGen) {
                    currentAudioPtsNs = maxOf(currentAudioPtsNs, src.durationNs)
                }
            }
            return 0
        }

        // Re-check generation before writing so a concurrent seek/mute/pause never lets stale PCM hit the sink.
        lock.withLock {
            if (closed || !sinkReady || seekGeneration != snapshotGen || computeAudioStateLocked() != AudioState.PLAYING) {
                return 0
            }
        }

        val written = sink.write(pcm, 0, pcm.size)

        lock.withLock {
            if (!closed && seekGeneration != snapshotGen && written > 0 && sinkReady) {
                // A seek/offset change flushed the sink while this slice was being written, so the
                // stale PCM landed after that flush. Drop it so it neither plays nor delays later slices.
                runCatching { sink.flush() }
            }
            if (!closed && seekGeneration == snapshotGen && written > 0) {
                val format = src.format
                val writtenFrames = (written / format.frameSizeBytes).toLong()
                val advancedNs = format.sampleFrameToNs(writtenFrames)
                val targetEnd = minOf(sliceEndNs, src.durationNs)
                // Keep sample-accurate advancement without stalling on rounding.
                currentAudioPtsNs = if (written == pcm.size) {
                    if (sliceEndNs <= 0L) sliceEndNs else minOf(sliceEndNs, src.durationNs)
                } else {
                    minOf(saturatingAdd(sliceStartNs, advancedNs), targetEnd)
                }
            }
        }
        return written
    }

    override fun close() {
        val threadToJoin: Thread?
        val notifyState: AudioState?
        lock.withLock {
            if (closed) return
            closed = true
            playingRequested = false
            seekGeneration++
            threadToJoin = pumpThread
            pumpThread = null
            if (sinkReady) {
                runCatching { sink.stop() }
                runCatching { sink.flush() }
            }
            val next = computeAudioStateLocked()
            notifyState = if (next != publishedState) {
                publishedState = next
                next
            } else {
                null
            }
            stateChanged.signalAll()
        }

        if (threadToJoin != null && threadToJoin !== Thread.currentThread()) {
            threadToJoin.join(CLOSE_JOIN_TIMEOUT_MS)
            if (threadToJoin.isAlive) {
                threadToJoin.interrupt()
                threadToJoin.join(CLOSE_JOIN_TIMEOUT_MS)
            }
        }

        runCatching { sink.close() }
        if (closeSourceOnClose) {
            runCatching { source?.close() }
        }
        if (notifyState != null) {
            notifyListeners(notifyState)
        }
    }

    private fun initializeSink(): Boolean {
        val src = source ?: return false
        if (!sink.isAvailable) return false
        return try {
            sink.open(src.format)
        } catch (_: Throwable) {
            false
        }
    }

    private fun computeAudioStateLocked(): AudioState {
        if (!sinkReady || source == null || pumpFailed) return AudioState.UNAVAILABLE
        if (scrubbing || userMuted || speed != PlaybackSpeed.NORMAL) return AudioState.MUTED
        return if (playingRequested && !closed) AudioState.PLAYING else AudioState.PAUSED
    }

    private fun applySinkTransportLocked(flush: Boolean) {
        if (!sinkReady) return
        val next = computeAudioStateLocked()
        when (next) {
            AudioState.PLAYING -> {
                if (flush) sink.flush()
                sink.start()
            }
            AudioState.MUTED, AudioState.PAUSED -> {
                sink.stop()
                if (flush) sink.flush()
            }
            AudioState.UNAVAILABLE -> Unit
        }
    }

    private inline fun updateStateAndNotify(block: () -> Unit) {
        val changedTo: AudioState? = lock.withLock {
            block()
            val next = computeAudioStateLocked()
            stateChanged.signalAll()
            if (next != publishedState) {
                publishedState = next
                next
            } else {
                null
            }
        }
        if (changedTo != null) {
            notifyListeners(changedTo)
        }
    }

    private fun notifyListeners(state: AudioState) {
        for (listener in listeners) {
            listener(state)
        }
    }

    private fun runPumpLoop() {
        val src = source ?: return
        while (true) {
            val genBefore: Long
            lock.withLock {
                while (!closed && (computeAudioStateLocked() != AudioState.PLAYING || currentAudioPtsNs >= src.durationNs)) {
                    try {
                        stateChanged.await(100L, TimeUnit.MILLISECONDS)
                    } catch (_: InterruptedException) {
                        if (closed) return
                    }
                }
                if (closed) return
                genBefore = seekGeneration
            }
            val written = try {
                pumpNextSlice(sliceDurationNs)
            } catch (t: Throwable) {
                if (isClosedOrInterrupted()) return
                markPumpFailed()
                continue
            }
            if (written == 0) {
                // Nothing was written (sink refused the data, line closed, stale generation, ...).
                // Back off instead of re-reading the same slice in a tight loop, unless a seek
                // already moved on, in which case retry immediately.
                lock.withLock {
                    if (!closed && seekGeneration == genBefore) {
                        try {
                            stateChanged.await(20L, TimeUnit.MILLISECONDS)
                        } catch (_: InterruptedException) {
                            if (closed) return
                        }
                    }
                }
            }
        }
    }

    private fun isClosedOrInterrupted(): Boolean = lock.withLock { closed }

    private fun markPumpFailed() {
        val changedTo: AudioState? = lock.withLock {
            if (closed) return
            pumpFailed = true
            runCatching { sink.stop() }
            val next = computeAudioStateLocked()
            stateChanged.signalAll()
            if (next != publishedState) {
                publishedState = next
                next
            } else {
                null
            }
        }
        if (changedTo != null) {
            notifyListeners(changedTo)
        }
    }

    /**
     * Builds signed 16-bit LE PCM covering `[startNs, endNs)`, padding any negative lead-in range
     * (`avOffsetMs > 0` before audio starts at `0 ns`) with exact zero-filled silence.
     */
    private fun buildAlignedSlice(src: PcmAudioSource, startNs: Long, endNs: Long): ByteArray {
        if (endNs <= startNs || startNs >= src.durationNs) return ByteArray(0)
        val format = src.format
        if (endNs <= 0L) {
            val silenceFrames = format.nsToSampleFrame(endNs - startNs).coerceAtLeast(1L)
            val silenceBytes = (silenceFrames * format.frameSizeBytes.toLong())
                .coerceAtMost(MAX_SILENCE_CHUNK_BYTES.toLong())
                .toInt()
            return ByteArray(silenceBytes)
        }
        if (startNs < 0L) {
            val silenceFrames = format.nsToSampleFrame(-startNs)
            val silenceBytes = (silenceFrames * format.frameSizeBytes.toLong())
                .coerceAtMost(MAX_SILENCE_CHUNK_BYTES.toLong())
                .toInt()
            val audioPart = src.readPcmSlice(0L, endNs)
            val combined = ByteArray(silenceBytes + audioPart.size)
            System.arraycopy(audioPart, 0, combined, silenceBytes, audioPart.size)
            return combined
        }
        return src.readPcmSlice(startNs, endNs)
    }

    companion object {
        const val DEFAULT_SLICE_DURATION_NS: Long = 20_000_000L // 20 ms
        private const val MAX_SILENCE_CHUNK_BYTES: Int = 1024 * 1024
        private const val CLOSE_JOIN_TIMEOUT_MS: Long = 2_000L

        private fun saturatingMsToNs(ms: Long): Long = when {
            ms > Long.MAX_VALUE / 1_000_000L -> Long.MAX_VALUE
            ms < Long.MIN_VALUE / 1_000_000L -> Long.MIN_VALUE
            else -> ms * 1_000_000L
        }

        private fun saturatingSub(a: Long, b: Long): Long {
            val diff = a - b
            return if (((a xor b) and (a xor diff)) < 0L) {
                if (a >= 0L) Long.MAX_VALUE else Long.MIN_VALUE
            } else {
                diff
            }
        }

        private fun saturatingAdd(a: Long, b: Long): Long {
            val sum = a + b
            return if (((a xor sum) and (b xor sum)) < 0L) {
                if (a >= 0L) Long.MAX_VALUE else Long.MIN_VALUE
            } else {
                sum
            }
        }
    }
}
