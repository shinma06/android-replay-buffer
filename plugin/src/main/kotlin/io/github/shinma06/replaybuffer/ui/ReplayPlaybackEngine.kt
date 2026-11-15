package io.github.shinma06.replaybuffer.ui

import io.github.shinma06.replaybuffer.core.AacPcmDecoder
import io.github.shinma06.replaybuffer.core.AudioChunk
import io.github.shinma06.replaybuffer.core.ReplayAudioTrack
import io.github.shinma06.replaybuffer.core.ReplayFrameCursor
import io.github.shinma06.replaybuffer.core.SavedReplay
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
) {
    private val stopped = AtomicBoolean()
    private val audioReady = CountDownLatch(1)
    @Volatile private var line: SourceDataLine? = null
    @Volatile private var baseNanos = System.nanoTime()
    @Volatile private var baseUs = fromUs
    private val threads = mutableListOf<Thread>()

    private fun nowUs(): Long = baseUs + (line?.microsecondPosition ?: ((System.nanoTime() - baseNanos) / 1000))

    /** Switches to the monotonic clock without a jump when the audio line goes away mid-play. */
    private fun dropLine() {
        val current = nowUs()
        baseNanos = System.nanoTime()
        baseUs = current
        line = null
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
        line?.let { runCatching { it.stop(); it.flush() } }
    }

    private fun runVideo() {
        try {
            SavedReplay.open(folder).use { saved ->
                ReplayFrameCursor(saved).use { cursor ->
                    val span = saved.timeline().spans.firstOrNull { it.endUs > fromUs && it.startUs < untilUs }
                    var frame = span?.let { cursor.seek(maxOf(fromUs, it.startUs), ::check) }
                    audioReady.await(1, TimeUnit.SECONDS)
                    if (line == null) { baseNanos = System.nanoTime(); baseUs = fromUs }
                    var coveredUntil = fromUs
                    while (frame != null && frame.startUs < untilUs) {
                        check()
                        if (frame.startUs > coveredUntil) waitGap(coveredUntil, minOf(frame.startUs, untilUs))
                        waitUntil(frame.startUs)
                        // Late frames still decode (they are references) but are not shown.
                        if (nowUs() < frame.endUs) onFrame(frame)
                        coveredUntil = frame.endUs
                        if (coveredUntil >= untilUs) break
                        frame = cursor.next(::check)
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
                    val output = try {
                        audioOutput(format).apply { open(format, sampleRate / 4 * frameBytes) }
                    } catch (failure: Exception) {
                        onAudioUnavailable("音声を出力できないため、音声なしで再生しています。")
                        audioReady.countDown(); return
                    }
                    opened = output
                    output.start()
                    baseUs = fromUs
                    line = output
                    audioReady.countDown()
                    val limit = ((untilUs - fromUs) * sampleRate / 1_000_000L).coerceAtLeast(0)
                    var written = 0L
                    fun write(pcm: ByteArray, offsetSamples: Long, samples: Long) {
                        val count = minOf(samples, limit - written)
                        if (count <= 0) return
                        output.write(pcm, (offsetSamples * frameBytes).toInt(), (count * frameBytes).toInt())
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
                    output.drain()
                }
            }
        } catch (_: InterruptedException) {
        } catch (failure: Throwable) {
            if (!stopped.get()) {
                if (line != null) dropLine()
                onAudioUnavailable("音声を再生できないため、音声なしで再生しています。")
            }
        } finally {
            audioReady.countDown()
            if (stopped.get() || line == null) opened?.let { runCatching { it.close() } }
            else { val current = opened; dropLine(); current?.let { runCatching { it.close() } } }
        }
    }

}
