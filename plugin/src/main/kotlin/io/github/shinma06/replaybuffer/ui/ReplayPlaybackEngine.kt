package io.github.shinma06.replaybuffer.ui

import io.github.shinma06.replaybuffer.core.AacPcmDecoder
import io.github.shinma06.replaybuffer.core.AudioChunk
import io.github.shinma06.replaybuffer.core.ReplayAudioTrack
import io.github.shinma06.replaybuffer.core.ReplayFrameCursor
import io.github.shinma06.replaybuffer.core.SavedReplay
import io.github.shinma06.replaybuffer.core.ReplayVideoDecoder
import java.nio.file.Path
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import javax.sound.sampled.AudioFormat
import javax.sound.sampled.AudioSystem
import javax.sound.sampled.SourceDataLine

/**
 * Real-time playback of a saved replay from [fromUs] until [untilUs] (3.1 再生). One thread decodes
 * video forward with [ReplayFrameCursor]; another decodes AAC with JAAD and plays it on a Java Sound
 * line, whose position is the master clock. Without audio (none saved, no output device, decode
 * failure) a monotonic clock drives video only and [onAudioUnavailable] is told once.
 * Audio starts after the initial video frame or gap has been prepared.
 * Callbacks run on the worker threads; the caller hops to EDT.
 */
internal class ReplayPlaybackSession(
    private val folder: Path,
    private val fromUs: Long,
    private val untilUs: Long,
    private val onFrame: (ReplayFrameCursor.Decoded) -> Unit,
    private val onFinished: (failure: String?) -> Unit,
    private val onAudioUnavailable: (reason: String) -> Unit,
    private val onGap: (movieUs: Long) -> Unit = {},
    private val audioOutput: (AudioFormat) -> SourceDataLine = AudioSystem::getSourceDataLine,
    private val nanoTime: () -> Long = System::nanoTime,
    private val awaitAudio: (CountDownLatch) -> Boolean = { it.await(1, TimeUnit.SECONDS) },
    private val prepareVideo: (() -> ReplayFrameCursor.Decoded?) -> ReplayFrameCursor.Decoded? = { it() },
    private val awaitVideo: (CountDownLatch) -> Unit = { it.await() },
    private val nextVideo: (() -> ReplayFrameCursor.Decoded?) -> ReplayFrameCursor.Decoded? = { it() },
    private val decoderFactory: (SavedReplay) -> ReplayVideoDecoder = { ReplayFrameCursor(it) },
) {
    private val stopped = AtomicBoolean()
    private val audioNotified = AtomicBoolean()
    private val audioReady = CountDownLatch(1)
    private val videoReady = CountDownLatch(1)
    private enum class Clock { PENDING, STARTING_AUDIO, AUDIO, MONOTONIC, STOPPED }
    private val clockLock = Any()
    private var clock = Clock.PENDING
    private var line: SourceDataLine? = null
    private var baseNanos = 0L
    private var baseUs = fromUs
    private var lastUs = fromUs
    private val threads = mutableListOf<Thread>()

    internal fun nowUs(): Long = synchronized(clockLock) {
        lastUs = maxOf(lastUs, when (clock) {
            Clock.AUDIO -> baseUs + line!!.microsecondPosition
            Clock.MONOTONIC -> baseUs + (nanoTime() - baseNanos) / 1000
            else -> lastUs
        })
        lastUs
    }

    private fun useMonotonicClock(): Boolean = synchronized(clockLock) {
        if (clock != Clock.PENDING && clock != Clock.STARTING_AUDIO) return@synchronized false
        baseUs = fromUs
        baseNanos = nanoTime()
        clock = Clock.MONOTONIC
        true
    }

    private fun beginAudio(): Boolean = synchronized(clockLock) {
        if (stopped.get() || clock != Clock.PENDING) return@synchronized false
        clock = Clock.STARTING_AUDIO
        true
    }

    private fun adoptAudio(output: SourceDataLine): Boolean = synchronized(clockLock) {
        if (stopped.get() || clock != Clock.STARTING_AUDIO) return@synchronized false
        line = output
        baseUs = fromUs
        clock = Clock.AUDIO
        true
    }

    private fun canOpenAudio(): Boolean = synchronized(clockLock) {
        !stopped.get() && clock == Clock.PENDING
    }

    /** Switches to the monotonic clock without a jump when the audio line goes away mid-play. */
    private fun dropLine(output: SourceDataLine?) = synchronized(clockLock) {
        if (clock == Clock.AUDIO && line === output) {
            baseUs = runCatching { nowUs() }.getOrDefault(lastUs)
            baseNanos = nanoTime()
            line = null
            clock = Clock.MONOTONIC
        }
    }

    private class AudioOutputFailure(cause: Throwable) : RuntimeException(cause)

    private inline fun <T> outputCall(call: () -> T): T = try {
        call()
    } catch (interrupted: InterruptedException) {
        throw interrupted
    } catch (failure: Throwable) {
        throw AudioOutputFailure(failure)
    }

    private fun audioUnavailable(failure: Throwable? = null) {
        if (!stopped.get() && audioNotified.compareAndSet(false, true))
            onAudioUnavailable(if (failure is AudioOutputFailure) "音声を出力できないため、音声なしで再生しています。"
                else "音声を再生できないため、音声なしで再生しています。")
    }

    private fun check() { if (stopped.get()) throw InterruptedException() }

    fun start() {
        threads += Thread(::runVideo, "arb-replay-play-video").apply { isDaemon = true }
        threads += Thread(::runAudio, "arb-replay-play-audio").apply { isDaemon = true; priority = Thread.MAX_PRIORITY - 1 }
        threads.forEach(Thread::start)
    }

    fun stop() {
        if (!stopped.compareAndSet(false, true)) return
        threads.forEach(Thread::interrupt)
        val output = synchronized(clockLock) {
            clock = Clock.STOPPED
            line.also { line = null }
        }
        output?.let { runCatching { it.stop() }; runCatching { it.flush() } }
    }

    private fun runVideo() {
        try {
            SavedReplay.open(folder).use { saved ->
                decoderFactory(saved).use { cursor ->
                    val span = saved.timeline().spans.firstOrNull { it.endUs > fromUs && it.startUs < untilUs }
                    var frame = prepareVideo { span?.let { cursor.seek(maxOf(fromUs, it.startUs), ::check) } }
                    check()
                    videoReady.countDown()
                    val ready = awaitAudio(audioReady)
                    if (useMonotonicClock() && !ready) audioUnavailable()
                    var coveredUntil = fromUs
                    while (frame != null && frame.startUs < untilUs) {
                        check()
                        if (nowUs() >= untilUs) break
                        if (frame.startUs > coveredUntil) waitGap(coveredUntil, minOf(frame.startUs, untilUs))
                        waitUntil(frame.startUs)
                        // Late frames still decode (they are references) but are not shown.
                        if (nowUs() < frame.endUs) onFrame(frame)
                        coveredUntil = frame.endUs
                        if (coveredUntil >= untilUs || nowUs() >= untilUs) break
                        frame = nextVideo { cursor.next(::check) }
                    }
                    if (coveredUntil < untilUs) waitGap(coveredUntil, untilUs) else waitUntil(untilUs)
                }
            }
            if (!stopped.get()) onFinished(null)
        } catch (_: InterruptedException) {
        } catch (failure: Throwable) {
            if (!stopped.get()) onFinished(failure.message ?: "再生を続けられません。")
        } finally {
            stop()
        }
    }

    private fun waitGap(from: Long, until: Long) {
        waitUntil(from)
        while (nowUs() < until) {
            check()
            onGap(nowUs().coerceIn(from, until - 1))
            Thread.sleep(20)
        }
    }

    private fun waitUntil(us: Long) {
        while (true) {
            check()
            val remaining = us - nowUs()
            if (remaining <= 0) return
            Thread.sleep(minOf(remaining / 1000 + 1, 20))
        }
    }

    private fun runAudio() {
        var opened: SourceDataLine? = null
        try {
            SavedReplay.open(folder).use { saved ->
                val track = ReplayAudioTrack.open(saved)
                if (track == null) { audioReady.countDown(); return }
                track.use {
                    val decoder = AacPcmDecoder(track.config)
                    val sampleRate = decoder.sampleRate
                    val channels = decoder.channels
                    require(sampleRate == track.sampleRate && sampleRate > 0 && channels > 0) { "音声の形式と時刻記録が一致しません" }
                    val format = AudioFormat(sampleRate.toFloat(), 16, channels, true, false)
                    val frameBytes = 2 * channels
                    val output = outputCall { audioOutput(format) }
                    opened = output
                    check()
                    if (!canOpenAudio()) { audioUnavailable(); return }
                    outputCall { output.open(format, sampleRate / 4 * frameBytes) }
                    check()
                    awaitVideo(videoReady)
                    check()
                    if (!beginAudio()) { audioUnavailable(); return }
                    outputCall { output.start() }
                    check()
                    // Native start may have been in flight when video committed its fallback.
                    if (!adoptAudio(output)) { audioUnavailable(); return }
                    audioReady.countDown()
                    val limit = ((untilUs - fromUs) * sampleRate / 1_000_000L).coerceAtLeast(0)
                    var written = 0L
                    fun write(pcm: ByteArray, offsetSamples: Long, samples: Long) {
                        val count = minOf(samples, limit - written)
                        if (count <= 0) return
                        check()
                        outputCall { output.write(pcm, (offsetSamples * frameBytes).toInt(), (count * frameBytes).toInt()) }
                        check()
                        written += count
                    }
                    val silence = ByteArray(1024 * frameBytes)
                    for (chunk in track.schedule(fromUs)) {
                        check()
                        if (written >= limit) break
                        when (chunk) {
                            is AudioChunk.Silence -> {
                                var left = chunk.samples
                                while (left > 0 && written < limit) { check(); val n = minOf(left, 1024L); write(silence, 0, n); left -= n }
                            }
                            is AudioChunk.Packets -> {
                                // One preceding packet primes the decoder's overlap so the first kept samples are clean.
                                if (chunk.first > 0) { track.goTo(chunk.first - 1); track.packet()?.let { runCatching { decoder.decode(it) } } }
                                else track.goTo(chunk.first)
                                var skip = chunk.skipSamples
                                var keep = chunk.keepSamples
                                repeat(chunk.count) {
                                    check()
                                    val pcm = decoder.decode(track.packet() ?: return@repeat)
                                    val samples = pcm.size / frameBytes.toLong()
                                    val from = minOf(skip, samples)
                                    val n = minOf(samples - from, keep)
                                    write(pcm, from, n)
                                    skip -= from; keep -= n
                                }
                            }
                        }
                    }
                    // Keep the clock running to the end when the audio stops before the video.
                    while (written < limit) { check(); write(silence, 0, minOf(1024L, limit - written)) }
                    check()
                    outputCall { output.drain() }
                }
            }
        } catch (_: InterruptedException) {
        } catch (failure: Throwable) {
            if (!stopped.get()) {
                dropLine(opened)
                audioUnavailable(failure)
            }
        } finally {
            dropLine(opened)
            opened?.let { runCatching { it.close() } }
            audioReady.countDown()
        }
    }

}
