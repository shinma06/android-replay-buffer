package io.github.shinma06.arb.playback

import io.github.shinma06.arb.ipc.ArbdRpcClient

/**
 * Operating mode of [PlaybackController].
 */
enum class PlaybackMode {
    PAUSED,
    PLAYING,
    SCRUBBING,
}

/**
 * Supported continuous playback speeds. Uses exact integer ratios to avoid floating-point drift.
 */
enum class PlaybackSpeed(
    val multiplier: Double,
    val label: String,
) {
    QUARTER(0.25, "0.25x"),
    HALF(0.5, "0.5x"),
    NORMAL(1.0, "1.0x"),
    DOUBLE(2.0, "2.0x");

    /**
     * Converts elapsed wall-clock nanoseconds into elapsed media nanoseconds at this speed.
     */
    fun wallToMediaNs(wallDeltaNs: Long): Long {
        if (wallDeltaNs <= 0L) return 0L
        return when (this) {
            QUARTER -> wallDeltaNs / 4L
            HALF -> wallDeltaNs / 2L
            NORMAL -> wallDeltaNs
            DOUBLE -> if (wallDeltaNs > Long.MAX_VALUE / 2L) Long.MAX_VALUE else wallDeltaNs * 2L
        }
    }

    /**
     * Converts remaining media nanoseconds into wall-clock nanoseconds to wait at this speed.
     */
    fun mediaToWallNs(mediaDeltaNs: Long): Long {
        if (mediaDeltaNs <= 0L) return 0L
        return when (this) {
            QUARTER -> if (mediaDeltaNs > Long.MAX_VALUE / 4L) Long.MAX_VALUE else mediaDeltaNs * 4L
            HALF -> if (mediaDeltaNs > Long.MAX_VALUE / 2L) Long.MAX_VALUE else mediaDeltaNs * 2L
            NORMAL -> mediaDeltaNs
            DOUBLE -> mediaDeltaNs / 2L + mediaDeltaNs % 2L
        }
    }
}

/**
 * Honest audio playback state exposed by [AudioSyncPlayer] and [PlaybackController].
 */
enum class AudioState {
    UNAVAILABLE,
    MUTED,
    PLAYING,
    PAUSED,
}

/**
 * Immutable snapshot of continuous playback & trim-range state.
 *
 * Frame identity and trim boundaries are strictly integer [Long] indices (never float seconds).
 * When `totalFrames > 0`, invariants guarantee:
 * `0 <= inFrameIndex <= outFrameIndex < totalFrames` and `0 <= currentFrameIndex < totalFrames`.
 */
data class PlaybackState(
    val mode: PlaybackMode = PlaybackMode.PAUSED,
    val currentFrameIndex: Long = 0L,
    val displayedFrameIndex: Long = currentFrameIndex,
    val totalFrames: Long = 0L,
    val inFrameIndex: Long = 0L,
    val outFrameIndex: Long = (totalFrames - 1L).coerceAtLeast(0L),
    val speed: PlaybackSpeed = PlaybackSpeed.NORMAL,
    val muted: Boolean = false,
    /**
     * Loop playback toggle (Owner trial feedback T-6H item 3). `false` (the default) stops
     * cleanly at `outFrameIndex`; `true` wraps smoothly back to `inFrameIndex` (or `0` for the
     * full range) and keeps playing. See [PlaybackController.loopEnabled] / `setLoopEnabled`.
     */
    val loopInSelection: Boolean = false,
    val isApproximate: Boolean = false,
    val avOffsetMs: Long = 0L,
    val audioState: AudioState = AudioState.UNAVAILABLE,
) {
    init {
        require(totalFrames >= 0L) { "totalFrames must be non-negative: $totalFrames" }
        if (totalFrames == 0L) {
            require(currentFrameIndex == 0L && inFrameIndex == 0L && outFrameIndex == 0L) {
                "Empty timeline must keep frame indices at 0"
            }
        } else {
            val maxFrame = totalFrames - 1L
            require(inFrameIndex in 0L..maxFrame) { "inFrameIndex $inFrameIndex out of 0..$maxFrame" }
            require(outFrameIndex in inFrameIndex..maxFrame) {
                "outFrameIndex $outFrameIndex out of $inFrameIndex..$maxFrame"
            }
            require(currentFrameIndex in 0L..maxFrame) {
                "currentFrameIndex $currentFrameIndex out of 0..$maxFrame"
            }
        }
    }
}

/**
 * Result of a viewport seek or scrub operation performed by [FrameSeeker].
 */
data class FrameSeekResult(
    val requestedFrameIndex: Long,
    val displayedFrameIndex: Long,
    val isApproximate: Boolean,
    val recovered: Boolean = false,
)

/**
 * Abstraction over `arbd`'s viewport seek/scrub RPCs so [PlaybackController] stays pure, testable,
 * and independent of transport/UI details.
 */
interface FrameSeeker {
    /**
     * Decodes and displays the exact integer [frameIndex] (`media.seek_frame`).
     * The resulting frame MUST have `isApproximate == false`.
     */
    fun seekExact(frameIndex: Long): FrameSeekResult

    fun seekExact(frameIndex: Long, onRecovery: () -> Unit): FrameSeekResult = seekExact(frameIndex)

    /**
     * Publishes a fast scrub preview for [frameIndex] (`media.scrub`), which may display the
     * nearest cached keyframe with `isApproximate == true`.
     */
    fun scrubApprox(frameIndex: Long): FrameSeekResult

    companion object {
        fun forViewport(client: ArbdRpcClient, viewportId: String): FrameSeeker =
            ArbdFrameSeeker(client, viewportId)
    }
}

/**
 * Production [FrameSeeker] delegating to `arbd`'s `media.seek_frame` and `media.scrub` JSON-RPCs.
 */
class ArbdFrameSeeker(
    private val client: ArbdRpcClient,
    private val viewportId: String,
) : FrameSeeker {
    override fun seekExact(frameIndex: Long): FrameSeekResult {
        val resp = client.mediaSeekFrame(viewportId, frameIndex)
        return FrameSeekResult(
            requestedFrameIndex = frameIndex,
            displayedFrameIndex = resp.frameIndex,
            isApproximate = resp.isApproximate,
        )
    }

    override fun scrubApprox(frameIndex: Long): FrameSeekResult {
        val resp = client.mediaScrub(viewportId, frameIndex)
        return FrameSeekResult(
            requestedFrameIndex = resp.requestedFrameIndex,
            displayedFrameIndex = resp.displayedFrameIndex,
            isApproximate = resp.isApproximate,
        )
    }
}

/**
 * Monotonic nanosecond clock injected into [PlaybackController] for deterministic pacing tests.
 */
fun interface PlaybackClock {
    fun nowNs(): Long
}

/**
 * Default monotonic wall clock backed by [System.nanoTime].
 */
object SystemPlaybackClock : PlaybackClock {
    override fun nowNs(): Long = System.nanoTime()
}
