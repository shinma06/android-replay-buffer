package io.github.shinma06.arb.playback

import java.io.Closeable
import java.io.IOException
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock
import kotlin.math.roundToLong

/**
 * Deterministic, thread-safe continuous playback & trim-range controller for the ARB IDE viewer.
 *
 * Guarantees:
 * - Frame positions and trim boundaries are strictly integer `frameIndex: Long` values.
 * - Real frame `ptsNs` deltas (or [fallbackFps] when unavailable) are scaled by [PlaybackSpeed]
 *   using exact integer ratios, dropping late frames rather than accumulating wall-clock drift.
 * - `isApproximate` becomes `true` ONLY from [scrubTo] (`media.scrub`) when an approximate preview
 *   frame is returned, and is guaranteed to clear to `false` on exact seek (`endScrub`, `stepFrame`,
 *   `seekToFrame`, `play`, `selectAroundCrash`).
 * - Coordinates an optional [AudioSyncPlayer] (muting during `SCRUBBING` or non-`1.0x` speeds and
 *   flushing immediately on seek/loop).
 * - [close] stops all background threads deterministically with bounded join and zero leaks.
 */
class PlaybackController(
    val totalFrames: Long,
    private val frameSeeker: FrameSeeker,
    framePtsNs: List<Long> = emptyList(),
    fallbackFps: Double = DEFAULT_FALLBACK_FPS,
    private var audioPlayer: AudioSyncPlayer? = null,
    private val clock: PlaybackClock = SystemPlaybackClock,
    initialFrameIndex: Long = 0L,
    initialInFrameIndex: Long = 0L,
    initialOutFrameIndex: Long = (totalFrames - 1L).coerceAtLeast(0L),
    initialSpeed: PlaybackSpeed = PlaybackSpeed.NORMAL,
    initialMuted: Boolean = false,
    initialLoopInSelection: Boolean = false,
    initialAvOffsetMs: Long = audioPlayer?.avOffsetMs ?: 0L,
    private val autoRunLoop: Boolean = true,
    private val closeAudioPlayerOnClose: Boolean = true,
    private val lastFrameDurationNs: Long? = null,
    timelineDurationNs: Long? = null,
    private val onPlaybackError: (Throwable) -> Unit = {},
) : Closeable {
    init {
        require(totalFrames >= 0L) { "totalFrames must be non-negative: $totalFrames" }
        require(fallbackFps.isFinite() && fallbackFps > 0.0) { "fallbackFps must be positive finite: $fallbackFps" }
    }

    private val fallbackFrameDurationNs: Long = (1_000_000_000.0 / fallbackFps).roundToLong().coerceAtLeast(1L)
    val axis = TimelineCoordinateAxis(totalFrames, framePtsNs, fallbackFrameDurationNs, lastFrameDurationNs, timelineDurationNs)

    private val stateLock = ReentrantLock()
    private val loopWakeup = stateLock.newCondition()
    private val seekLock = Any()
    private val seekGeneration = AtomicLong(0L)
    private val droppedFramesCounter = AtomicLong(0L)
    private val listeners = CopyOnWriteArrayList<(PlaybackState) -> Unit>()

    private var initialSeekGeneration: Long? = null
    private var recoveringGeneration: Long? = null
    private var wakeVersion = 0L
    private var closed: Boolean = false
    private var anchorWallNs: Long = 0L
    private var anchorMediaPtsNs: Long = 0L
    private var loopThread: Thread? = null

    private var audioSubscription: AutoCloseable? = null

    @Volatile
    private var currentState: PlaybackState

    init {
        val clampedIn: Long
        val clampedOut: Long
        val clampedCur: Long
        if (totalFrames == 0L) {
            clampedIn = 0L
            clampedOut = 0L
            clampedCur = 0L
        } else {
            val maxFrame = totalFrames - 1L
            clampedIn = initialInFrameIndex.coerceIn(0L, maxFrame)
            clampedOut = initialOutFrameIndex.coerceIn(clampedIn, maxFrame)
            clampedCur = initialFrameIndex.coerceIn(0L, maxFrame)
        }

        anchorMediaPtsNs = axis.start(clampedCur)
        audioPlayer?.apply {
            setAvOffsetMs(initialAvOffsetMs)
            setPlaybackSpeed(initialSpeed)
            setMuted(initialMuted)
            seek(frameRelativePtsNs(clampedCur))
        }

        currentState = PlaybackState(
            mode = PlaybackMode.PAUSED,
            currentFrameIndex = clampedCur,
            displayedFrameIndex = clampedCur,
            totalFrames = totalFrames,
            inFrameIndex = clampedIn,
            outFrameIndex = clampedOut,
            speed = initialSpeed,
            muted = initialMuted,
            loopInSelection = initialLoopInSelection,
            isApproximate = false,
            avOffsetMs = initialAvOffsetMs,
            audioState = audioPlayer?.audioState ?: AudioState.UNAVAILABLE,
        )
        audioPlayer?.let(::observeAudioFailure)
    }

    val state: PlaybackState
        get() = currentState

    val isClosed: Boolean
        get() = stateLock.withLock { closed }

    /** Total number of late frames skipped during playback to prevent wall-clock drift. */
    val droppedFramesCount: Long
        get() = droppedFramesCounter.get()

    fun frameRelativePtsNs(frameIndex: Long): Long = axis.start(frameIndex)

    fun addStateListener(listener: (PlaybackState) -> Unit): AutoCloseable {
        listeners.add(listener)
        return AutoCloseable { listeners.remove(listener) }
    }

    /**
     * Starts continuous playback inside `[inFrameIndex..=outFrameIndex]`.
     *
     * Resumes within a held sample; at or outside the exclusive selection end, restarts at IN.
     */
    fun play() {
        val needInitialSeek: Long?
        val gen: Long
        val startPtsNs: Long
        val playSnapshot: PlaybackState?

        stateLock.withLock {
            if (closed || totalFrames <= 0L) return
            if (currentState.mode == PlaybackMode.PLAYING) return

            val cur = currentState
            val resume = anchorMediaPtsNs >= axis.start(cur.inFrameIndex) && anchorMediaPtsNs < axis.outTime(cur.outFrameIndex)
            startPtsNs = if (resume) anchorMediaPtsNs else axis.start(cur.inFrameIndex)
            val startFrame = axis.frameAt(startPtsNs)
            needInitialSeek = if (startFrame != cur.displayedFrameIndex || startFrame != cur.currentFrameIndex || cur.isApproximate) startFrame else null
            gen = seekGeneration.incrementAndGet()
            initialSeekGeneration = gen.takeIf { needInitialSeek != null }

            anchorWallNs = clock.nowNs()
            anchorMediaPtsNs = startPtsNs

            audioPlayer?.setScrubbing(false)
            audioPlayer?.setPlaybackSpeed(cur.speed)
            audioPlayer?.setMuted(cur.muted)
            audioPlayer?.seek(startPtsNs)
            audioPlayer?.play()

            val updated = cur.copy(
                mode = PlaybackMode.PLAYING,
                currentFrameIndex = startFrame,
                audioState = currentAudioState(),
            )
            playSnapshot = publishStateLocked(updated)
            if (needInitialSeek == null) {
                ensureLoopThreadStartedLocked()
                signalLoopLocked()
            }
        }

        if (playSnapshot != null) {
            notifyListeners(playSnapshot)
        }

        if (needInitialSeek != null) {
            seekDuringPlayback(needInitialSeek, gen)
            // Re-anchor after initial seek completes so startup decode time doesn't drop frame 1.
            stateLock.withLock {
                if (initialSeekGeneration == gen) initialSeekGeneration = null
                if (!closed && currentState.mode == PlaybackMode.PLAYING) {
                    // A concurrent seek/scrub/trim may have bumped the generation; the loop must
                    // still run while the mode is PLAYING, but only re-anchor if we still own it.
                    if (seekGeneration.get() == gen) {
                        anchorWallNs = clock.nowNs()
                        anchorMediaPtsNs = startPtsNs
                        audioPlayer?.seek(anchorMediaPtsNs)
                    }
                    ensureLoopThreadStartedLocked()
                    signalLoopLocked()
                }
            }
        }
    }

    fun pause() {
        val snapshot: PlaybackState? = stateLock.withLock {
            if (closed) return
            if (currentState.mode == PlaybackMode.PAUSED) return
            anchorMediaPtsNs = displayPositionNs()
            seekGeneration.incrementAndGet()
            val wasScrubbing = currentState.mode == PlaybackMode.SCRUBBING
            if (wasScrubbing) {
                audioPlayer?.setScrubbing(false)
            }
            audioPlayer?.pause()
            val updated = currentState.copy(
                mode = PlaybackMode.PAUSED,
                audioState = currentAudioState(),
            )
            val changed = publishStateLocked(updated)
            signalLoopLocked()
            changed
        }
        if (snapshot != null) {
            notifyListeners(snapshot)
        }
    }

    fun togglePlayPause() {
        val playing = stateLock.withLock { currentState.mode == PlaybackMode.PLAYING }
        if (playing) pause() else play()
    }

    /**
     * Steps by [delta] frames (e.g. `-1` or `+1`), pausing if currently playing, and performs an
     * exact seek (`isApproximate == false`).
     */
    fun stepFrame(delta: Int) {
        val targetFrame: Long
        val gen: Long
        val pausedSnapshot: PlaybackState?

        stateLock.withLock {
            if (closed || totalFrames <= 0L) return
            val maxFrame = totalFrames - 1L
            targetFrame = (currentState.currentFrameIndex + delta.toLong()).coerceIn(0L, maxFrame)
            gen = seekGeneration.incrementAndGet()
            audioPlayer?.setScrubbing(false)
            audioPlayer?.pause()
            anchorMediaPtsNs = axis.start(targetFrame)
            audioPlayer?.seek(anchorMediaPtsNs)
            val updated = currentState.copy(
                mode = PlaybackMode.PAUSED,
                currentFrameIndex = targetFrame,
                audioState = currentAudioState(),
            )
            pausedSnapshot = publishStateLocked(updated)
            signalLoopLocked()
        }

        if (pausedSnapshot != null) {
            notifyListeners(pausedSnapshot)
        }
        performExactSeek(targetFrame, gen)
    }

    /**
     * Exact seek to [frameIndex] (e.g. clicking a timeline log/tap entry). Clears `isApproximate`
     * once the exact frame settles and flushes/re-syncs audio immediately.
     */
    fun seekToFrame(frameIndex: Long) = seekToTime(axis.start(frameIndex))

    fun seekToTime(timeNs: Long) {
        val targetFrame: Long
        val gen: Long
        val interimSnapshot: PlaybackState?

        stateLock.withLock {
            if (closed || totalFrames <= 0L) return
            val cur = currentState
            targetFrame = axis.frameAt(timeNs)
            gen = seekGeneration.incrementAndGet()
            val targetPtsNs = timeNs.coerceIn(0, axis.endNs)
            anchorMediaPtsNs = targetPtsNs
            if (cur.mode == PlaybackMode.PLAYING) {
                anchorWallNs = clock.nowNs()
            } else if (cur.mode == PlaybackMode.SCRUBBING) {
                audioPlayer?.setScrubbing(false)
            }
            audioPlayer?.seek(targetPtsNs)
            val nextMode = if (cur.mode == PlaybackMode.SCRUBBING) PlaybackMode.PAUSED else cur.mode
            val updated = cur.copy(
                mode = nextMode,
                currentFrameIndex = targetFrame,
                audioState = currentAudioState(),
            )
            interimSnapshot = publishStateLocked(updated)
            signalLoopLocked()
        }

        if (interimSnapshot != null) {
            notifyListeners(interimSnapshot)
        }
        performExactSeek(targetFrame, gen)
    }

    /**
     * Begins an interactive scrub gesture. Pauses continuous playback and cleanly mutes audio.
     */
    fun beginScrub() {
        val snapshot = stateLock.withLock {
            if (closed || totalFrames <= 0L) return
            anchorMediaPtsNs = displayPositionNs()
            audioPlayer?.pause()
            audioPlayer?.setScrubbing(true)
            val updated = currentState.copy(
                mode = PlaybackMode.SCRUBBING,
                audioState = currentAudioState(),
            )
            val changed = publishStateLocked(updated)
            signalLoopLocked()
            changed
        }
        if (snapshot != null) {
            notifyListeners(snapshot)
        }
    }

    /**
     * Updates the scrub position to [frameIndex] via `media.scrub` (approximate preview allowed).
     * Sets `isApproximate = true` when the viewport returns an approximate preview frame.
     */
    fun scrubTo(frameIndex: Long) = scrubToTime(axis.start(frameIndex))

    fun scrubToTime(timeNs: Long) {
        val targetFrame: Long
        val gen: Long
        val preSnapshot: PlaybackState?

        stateLock.withLock {
            if (closed || totalFrames <= 0L) return
            anchorMediaPtsNs = timeNs.coerceIn(0L, axis.endNs)
            targetFrame = axis.frameAt(anchorMediaPtsNs)
            gen = seekGeneration.incrementAndGet()
            audioPlayer?.pause()
            audioPlayer?.setScrubbing(true)
            val updated = currentState.copy(
                mode = PlaybackMode.SCRUBBING,
                currentFrameIndex = targetFrame,
                audioState = currentAudioState(),
            )
            preSnapshot = publishStateLocked(updated)
            signalLoopLocked()
        }

        if (preSnapshot != null) {
            notifyListeners(preSnapshot)
        }

        val result = synchronized(seekLock) {
            frameSeeker.scrubApprox(targetFrame)
        }

        val postSnapshot = stateLock.withLock {
            if (closed || seekGeneration.get() != gen) return
            val displayed = result.displayedFrameIndex.coerceIn(0L, totalFrames - 1L)
            val updated = currentState.copy(
                currentFrameIndex = targetFrame,
                displayedFrameIndex = displayed,
                isApproximate = result.isApproximate,
                audioState = currentAudioState(),
            )
            publishStateLocked(updated)
        }
        if (postSnapshot != null) {
            notifyListeners(postSnapshot)
        }
    }

    /**
     * Completes an interactive scrub gesture at [frameIndex], guaranteeing a final exact seek
     * (`media.seek_frame`) with `isApproximate == false` on release.
     */
    fun endScrub() = seekToTime(displayPositionNs())

    fun endScrub(frameIndex: Long) {
        val targetFrame: Long
        val gen: Long
        val preSnapshot: PlaybackState?

        stateLock.withLock {
            if (closed || totalFrames <= 0L) return
            targetFrame = frameIndex.coerceIn(0L, totalFrames - 1L)
            gen = seekGeneration.incrementAndGet()
            audioPlayer?.setScrubbing(false)
            audioPlayer?.pause()
            anchorMediaPtsNs = axis.start(targetFrame)
            audioPlayer?.seek(anchorMediaPtsNs)
            val updated = currentState.copy(
                mode = PlaybackMode.PAUSED,
                currentFrameIndex = targetFrame,
                audioState = currentAudioState(),
            )
            preSnapshot = publishStateLocked(updated)
            signalLoopLocked()
        }

        if (preSnapshot != null) {
            notifyListeners(preSnapshot)
        }
        performExactSeek(targetFrame, gen)
    }

    /**
     * Sets the inclusive trim start boundary (`inFrameIndex`), clamping to `[0 .. totalFrames - 1]`
     * and preserving `inFrameIndex <= outFrameIndex`.
     */
    fun setInFrame(frameIndex: Long) {
        val seekTarget: Pair<Long, Long>?
        val snapshot: PlaybackState?

        stateLock.withLock {
            if (closed || totalFrames <= 0L) return
            val maxFrame = totalFrames - 1L
            val newIn = frameIndex.coerceIn(0L, maxFrame)
            val newOut = maxOf(currentState.outFrameIndex, newIn)
            var nextCur = currentState.currentFrameIndex
            seekTarget = if (currentState.mode == PlaybackMode.PLAYING && nextCur < newIn) {
                nextCur = newIn
                val pts = frameRelativePtsNs(nextCur)
                anchorWallNs = clock.nowNs()
                anchorMediaPtsNs = pts
                audioPlayer?.seek(pts)
                nextCur to seekGeneration.incrementAndGet()
            } else {
                null
            }
            val updated = currentState.copy(
                inFrameIndex = newIn,
                outFrameIndex = newOut,
                currentFrameIndex = nextCur,
                audioState = currentAudioState(),
            )
            snapshot = publishStateLocked(updated)
            signalLoopLocked()
        }

        if (snapshot != null) {
            notifyListeners(snapshot)
        }
        if (seekTarget != null) {
            performExactSeek(seekTarget.first, seekTarget.second)
        }
    }

    /**
     * Sets the inclusive trim end boundary (`outFrameIndex`), clamping to `[0 .. totalFrames - 1]`
     * and preserving `inFrameIndex <= outFrameIndex`.
     */
    fun setOutFrame(frameIndex: Long) {
        val seekTarget: Pair<Long, Long>?
        val snapshot: PlaybackState?

        stateLock.withLock {
            if (closed || totalFrames <= 0L) return
            val maxFrame = totalFrames - 1L
            val newOut = frameIndex.coerceIn(0L, maxFrame)
            val newIn = minOf(currentState.inFrameIndex, newOut)
            var nextCur = currentState.currentFrameIndex
            seekTarget = if (currentState.mode == PlaybackMode.PLAYING && nextCur > newOut) {
                nextCur = newIn
                val pts = frameRelativePtsNs(nextCur)
                anchorWallNs = clock.nowNs()
                anchorMediaPtsNs = pts
                audioPlayer?.seek(pts)
                nextCur to seekGeneration.incrementAndGet()
            } else {
                null
            }
            val updated = currentState.copy(
                inFrameIndex = newIn,
                outFrameIndex = newOut,
                currentFrameIndex = nextCur,
                audioState = currentAudioState(),
            )
            snapshot = publishStateLocked(updated)
            signalLoopLocked()
        }

        if (snapshot != null) {
            notifyListeners(snapshot)
        }
        if (seekTarget != null) {
            performExactSeek(seekTarget.first, seekTarget.second)
        }
    }

    fun setSpeed(speed: PlaybackSpeed) {
        val snapshot = stateLock.withLock {
            if (closed) return
            if (currentState.speed == speed) return
            val nowNs = clock.nowNs()
            if (currentState.mode == PlaybackMode.PLAYING) {
                // Anchor on the real media position (not the frame start) so a speed change does
                // not drop the time already elapsed inside the current frame.
                anchorMediaPtsNs = displayPositionNs()
                anchorWallNs = nowNs
                if (speed == PlaybackSpeed.NORMAL) {
                    audioPlayer?.seek(anchorMediaPtsNs)
                }
            }
            audioPlayer?.setPlaybackSpeed(speed)
            val updated = currentState.copy(
                speed = speed,
                audioState = currentAudioState(),
            )
            val changed = publishStateLocked(updated)
            signalLoopLocked()
            changed
        }
        if (snapshot != null) {
            notifyListeners(snapshot)
        }
    }

    fun setMuted(muted: Boolean) {
        val snapshot = stateLock.withLock {
            if (closed) return
            if (currentState.muted == muted) return
            if (!muted && currentState.mode == PlaybackMode.PLAYING) {
                audioPlayer?.seek(displayPositionNs())
            }
            audioPlayer?.setMuted(muted)
            val updated = currentState.copy(
                muted = muted,
                audioState = currentAudioState(),
            )
            publishStateLocked(updated)
        }
        if (snapshot != null) {
            notifyListeners(snapshot)
        }
    }

    fun toggleMute() {
        setMuted(!currentState.muted)
    }

    fun setLoopInSelection(loop: Boolean) {
        val snapshot = stateLock.withLock {
            if (closed) return
            if (currentState.loopInSelection == loop) return
            val updated = currentState.copy(loopInSelection = loop)
            val changed = publishStateLocked(updated)
            signalLoopLocked()
            changed
        }
        if (snapshot != null) {
            notifyListeners(snapshot)
        }
    }

    /**
     * Loop playback toggle (Owner trial feedback T-6H item 3) — alias for [PlaybackState.loopInSelection].
     * `true` means reaching the end of `[inFrameIndex..outFrameIndex]` (or the full bundle when no
     * trim is set) wraps smoothly back to the start and keeps playing instead of stopping.
     */
    val loopEnabled: Boolean
        get() = currentState.loopInSelection

    /** Sets [loopEnabled] explicitly. See [setLoopInSelection]. */
    fun setLoopEnabled(enabled: Boolean) = setLoopInSelection(enabled)

    /** Flips [loopEnabled]. */
    fun toggleLoop() = setLoopInSelection(!currentState.loopInSelection)

    fun setAvOffsetMs(avOffsetMs: Long) {
        val snapshot = stateLock.withLock {
            if (closed) return
            if (currentState.avOffsetMs == avOffsetMs) return
            val videoPts = displayPositionNs()
            audioPlayer?.setAvOffsetMs(avOffsetMs, videoPts)
            val updated = currentState.copy(
                avOffsetMs = avOffsetMs,
                audioState = currentAudioState(),
            )
            publishStateLocked(updated)
        }
        if (snapshot != null) {
            notifyListeners(snapshot)
        }
    }

    private fun observeAudioFailure(player: AudioSyncPlayer) {
        audioSubscription = player.addStateListener { state ->
            if (state == AudioState.UNAVAILABLE) {
                val snapshot = stateLock.withLock {
                    if (closed || audioPlayer !== player) null
                    else publishStateLocked(currentState.copy(audioState = currentAudioState()))
                }
                snapshot?.let(::notifyListeners)
            }
        }
    }

    /**
     * Attaches (or replaces) an asynchronously decoded [AudioSyncPlayer] after construction,
     * syncing its A/V offset, playback speed, mute state, playhead PTS, and active transport mode.
     * If this controller is already closed, [player] is closed immediately to avoid leaking resources.
     */
    fun attachAudioPlayer(player: AudioSyncPlayer, avOffsetMs: Long = player.avOffsetMs) {
        var oldToClose: AudioSyncPlayer? = null
        var rejectNew = false
        val snapshot = stateLock.withLock {
            if (closed) {
                rejectNew = true
                null
            } else {
                oldToClose = audioPlayer
                audioSubscription?.close()
                audioPlayer = player
                observeAudioFailure(player)
                val cur = currentState
                val curPtsNs = displayPositionNs()
                player.setAvOffsetMs(avOffsetMs)
                player.setPlaybackSpeed(cur.speed)
                player.setMuted(cur.muted)
                player.setScrubbing(cur.mode == PlaybackMode.SCRUBBING)
                player.seek(curPtsNs)
                if (cur.mode == PlaybackMode.PLAYING) {
                    player.play()
                } else {
                    player.pause()
                }
                val updated = cur.copy(
                    avOffsetMs = avOffsetMs,
                    audioState = currentAudioState(),
                )
                publishStateLocked(updated)
            }
        }
        if (rejectNew) {
            runCatching { player.close() }
            return
        }
        if (oldToClose != null && oldToClose !== player && closeAudioPlayerOnClose) {
            runCatching { oldToClose.close() }
        }
        if (snapshot != null) {
            notifyListeners(snapshot)
        }
    }

    /**
     * Deterministic range preset from recorded tap frame indices:
     * selects `[min(taps) - padBeforeFrames ..= max(taps) + padAfterFrames]` clamped to
     * `[0 .. totalFrames - 1]`. Returns `false` when [tapFrameIndices] is empty or the timeline
     * has no frames.
     */
    fun selectTapRange(
        tapFrameIndices: List<Long>,
        padBeforeFrames: Long,
        padAfterFrames: Long,
        playhead: Long? = null,
    ): Boolean {
        require(padBeforeFrames >= 0L) { "padBeforeFrames must be non-negative: $padBeforeFrames" }
        require(padAfterFrames >= 0L) { "padAfterFrames must be non-negative: $padAfterFrames" }
        if (tapFrameIndices.isEmpty() || totalFrames <= 0L) return false

        val range = paddedRange(tapFrameIndices.min(), tapFrameIndices.max(), padBeforeFrames, padAfterFrames, totalFrames)
        return selectRange(range, playhead)
    }

    fun selectRange(range: LongRange, playhead: Long?): Boolean {
        require(!range.isEmpty() && range.first >= 0 && range.last < totalFrames)
        val newIn = range.first
        val newOut = range.last

        val seekTarget: Pair<Long, Long>?
        val snapshot: PlaybackState?
        stateLock.withLock {
            if (closed) return false
            var nextCur = currentState.currentFrameIndex
            val target = playhead?.coerceIn(newIn, newOut) ?:
                if (currentState.mode == PlaybackMode.PLAYING && nextCur !in range) newIn else nextCur
            seekTarget = if (nextCur != target || (playhead != null && currentState.displayedFrameIndex != target)) {
                nextCur = target
                val pts = frameRelativePtsNs(nextCur)
                anchorWallNs = clock.nowNs()
                anchorMediaPtsNs = pts
                audioPlayer?.seek(pts)
                nextCur to seekGeneration.incrementAndGet()
            } else {
                null
            }
            val updated = currentState.copy(
                inFrameIndex = newIn,
                outFrameIndex = newOut,
                currentFrameIndex = nextCur,
                audioState = currentAudioState(),
            )
            snapshot = publishStateLocked(updated)
            signalLoopLocked()
        }

        if (snapshot != null) {
            notifyListeners(snapshot)
        }
        if (seekTarget != null) {
            performExactSeek(seekTarget.first, seekTarget.second)
        }
        return true
    }

    /**
     * Deterministic range preset around a recorded crash frame:
     * selects `[crashFrameIndex - beforeFrames ..= crashFrameIndex + afterFrames]` clamped to
     * `[0 .. totalFrames - 1]` and performs an exact seek to `crashFrameIndex`.
     */
    fun selectAroundCrash(
        crashFrameIndex: Long,
        beforeFrames: Long,
        afterFrames: Long,
    ) {
        require(beforeFrames >= 0L) { "beforeFrames must be non-negative: $beforeFrames" }
        require(afterFrames >= 0L) { "afterFrames must be non-negative: $afterFrames" }
        if (totalFrames <= 0L) return

        val maxFrame = totalFrames - 1L
        val crash = crashFrameIndex.coerceIn(0L, maxFrame)
        val range = paddedRange(crash, crash, beforeFrames, afterFrames, totalFrames)
        selectRangeAtTime(range, axis.start(crash))
    }

    fun selectRangeAtTime(range: LongRange, timeNs: Long) {
        require(!range.isEmpty() && range.first >= 0 && range.last < totalFrames)
        val time = timeNs.coerceIn(axis.start(range.first), axis.outTime(range.last))
        val crash = axis.frameAt(time).coerceIn(range)
        val newIn = range.first
        val newOut = range.last

        val gen: Long
        val snapshot: PlaybackState?
        stateLock.withLock {
            if (closed) return
            gen = seekGeneration.incrementAndGet()
            anchorMediaPtsNs = time
            if (currentState.mode == PlaybackMode.PLAYING) {
                anchorWallNs = clock.nowNs()
            } else if (currentState.mode == PlaybackMode.SCRUBBING) {
                audioPlayer?.setScrubbing(false)
            }
            audioPlayer?.seek(time)
            val nextMode = if (currentState.mode == PlaybackMode.SCRUBBING) PlaybackMode.PAUSED else currentState.mode
            val updated = currentState.copy(
                mode = nextMode,
                inFrameIndex = newIn,
                outFrameIndex = newOut,
                currentFrameIndex = crash,
                audioState = currentAudioState(),
            )
            snapshot = publishStateLocked(updated)
            signalLoopLocked()
        }

        if (snapshot != null) {
            notifyListeners(snapshot)
        }
        performExactSeek(crash, gen)
    }

    /**
     * Resets the trim range to the full timeline `[0 .. totalFrames - 1]`.
     */
    fun resetFullRange() {
        val snapshot = stateLock.withLock {
            if (closed || totalFrames <= 0L) return
            val updated = currentState.copy(
                inFrameIndex = 0L,
                outFrameIndex = totalFrames - 1L,
            )
            val changed = publishStateLocked(updated)
            signalLoopLocked()
            changed
        }
        if (snapshot != null) {
            notifyListeners(snapshot)
        }
    }

    /**
     * Advances playback according to [clock] (`nowNs()`).
     *
     * Returns the next monotonic wall-clock timestamp (in ns) when the next frame boundary is due,
     * or `null` if playback is not currently in [PlaybackMode.PLAYING].
     *
     * Used internally by the background playback loop and directly by deterministic unit tests with
     * `autoRunLoop = false`.
     */
    fun tick(): Long? {
        val nowNs = clock.nowNs()
        var frameToSeek: Long? = null
        var genForSeek: Long = 0L
        var nextWakeupNs: Long? = null
        var notifySnapshot: PlaybackState? = null

        stateLock.withLock {
            if (closed || totalFrames <= 0L || clockFrozenLocked() || currentState.mode != PlaybackMode.PLAYING) {
                return null
            }

            val cur = currentState
            val elapsedWallNs = (nowNs - anchorWallNs).coerceAtLeast(0L)
            val elapsedMediaNs = cur.speed.wallToMediaNs(elapsedWallNs)
            val targetMediaPtsNs = saturatingAdd(anchorMediaPtsNs, elapsedMediaNs)

            val outPtsNs = frameRelativePtsNs(cur.outFrameIndex)
            val outFrameDurNs = frameDurationNs(cur.outFrameIndex)
            val selectionEndPtsNs = saturatingAdd(outPtsNs, outFrameDurNs)

            if (targetMediaPtsNs >= selectionEndPtsNs || cur.currentFrameIndex > cur.outFrameIndex ||
                (cur.loopInSelection && cur.currentFrameIndex < cur.inFrameIndex)
            ) {
                if (cur.loopInSelection) {
                    val inPtsNs = frameRelativePtsNs(cur.inFrameIndex)
                    anchorWallNs = nowNs
                    anchorMediaPtsNs = inPtsNs
                    if (cur.inFrameIndex == cur.outFrameIndex && cur.currentFrameIndex == cur.inFrameIndex) {
                        // Single-frame selection: stay on frame without redundant video seeks,
                        // but keep audio inside the selection. (A playhead that a user seek left
                        // outside the selection falls through and is brought back to `in`.)
                        audioPlayer?.seek(inPtsNs)
                        nextWakeupNs = saturatingAdd(nowNs, cur.speed.mediaToWallNs(outFrameDurNs))
                    } else {
                        frameToSeek = cur.inFrameIndex
                        genForSeek = seekGeneration.incrementAndGet()
                        audioPlayer?.seek(inPtsNs)
                        val updated = cur.copy(
                            currentFrameIndex = cur.inFrameIndex,
                            audioState = currentAudioState(),
                        )
                        notifySnapshot = publishStateLocked(updated)
                        val nextFrameDurNs = frameDurationNs(cur.inFrameIndex)
                        nextWakeupNs = saturatingAdd(nowNs, cur.speed.mediaToWallNs(nextFrameDurNs))
                    }
                } else {
                    anchorMediaPtsNs = selectionEndPtsNs
                    // Reached end of selection without looping: settle on outFrameIndex and pause.
                    if (cur.currentFrameIndex != cur.outFrameIndex) {
                        val skipped = (cur.outFrameIndex - cur.currentFrameIndex - 1L).coerceAtLeast(0L)
                        if (skipped > 0L) droppedFramesCounter.addAndGet(skipped)
                        frameToSeek = cur.outFrameIndex
                        genForSeek = seekGeneration.incrementAndGet()
                    }
                    audioPlayer?.pause()
                    val updated = cur.copy(
                        mode = PlaybackMode.PAUSED,
                        currentFrameIndex = cur.outFrameIndex,
                        audioState = currentAudioState(),
                    )
                    notifySnapshot = publishStateLocked(updated)
                    nextWakeupNs = null
                }
            } else {
                // Find the latest frame in [cur.currentFrameIndex .. cur.outFrameIndex] with pts <= targetMediaPtsNs.
                val resolvedFrame = axis.frameAt(targetMediaPtsNs).coerceIn(cur.currentFrameIndex, cur.outFrameIndex)
                if (resolvedFrame > cur.currentFrameIndex) {
                    val skipped = resolvedFrame - cur.currentFrameIndex - 1L
                    if (skipped > 0L) {
                        droppedFramesCounter.addAndGet(skipped)
                    }
                    frameToSeek = resolvedFrame
                    genForSeek = seekGeneration.incrementAndGet()
                    val updated = cur.copy(
                        currentFrameIndex = resolvedFrame,
                        audioState = currentAudioState(),
                    )
                    notifySnapshot = publishStateLocked(updated)
                }

                val nextBoundaryMediaPtsNs = if (resolvedFrame < cur.outFrameIndex) {
                    frameRelativePtsNs(resolvedFrame + 1L)
                } else {
                    selectionEndPtsNs
                }
                val remainingMediaNs = (nextBoundaryMediaPtsNs - targetMediaPtsNs).coerceAtLeast(1L)
                nextWakeupNs = saturatingAdd(nowNs, cur.speed.mediaToWallNs(remainingMediaNs))
            }
        }

        val snapshotToNotify = notifySnapshot
        if (snapshotToNotify != null) {
            notifyListeners(snapshotToNotify)
        }
        val target = frameToSeek
        if (target != null) {
            seekDuringPlayback(target, genForSeek)
        }
        return nextWakeupNs
    }

    override fun close() {
        val threadToJoin: Thread?
        val closedSnapshot: PlaybackState?
        stateLock.withLock {
            if (closed) return
            closed = true
            audioSubscription?.close()
            audioSubscription = null
            seekGeneration.incrementAndGet()
            threadToJoin = loopThread
            loopThread = null
            audioPlayer?.pause()
            val updated = currentState.copy(
                mode = PlaybackMode.PAUSED,
                audioState = if (closeAudioPlayerOnClose) AudioState.UNAVAILABLE else currentAudioState(),
            )
            closedSnapshot = publishStateLocked(updated)
            signalLoopLocked()
        }

        if (threadToJoin != null && threadToJoin !== Thread.currentThread()) {
            threadToJoin.join(CLOSE_JOIN_TIMEOUT_MS)
            if (threadToJoin.isAlive) {
                threadToJoin.interrupt()
                threadToJoin.join(CLOSE_JOIN_TIMEOUT_MS)
            }
        }

        if (closeAudioPlayerOnClose) {
            runCatching { audioPlayer?.close() }
        }
        if (closedSnapshot != null) {
            notifyListeners(closedSnapshot)
        }
    }

    private fun performExactSeek(frameIndex: Long, expectedGeneration: Long, retried: Boolean = false) {
        val result = synchronized(seekLock) {
            if (seekGeneration.get() != expectedGeneration) return
            try {
                frameSeeker.seekExact(frameIndex) { freezeForRecovery(expectedGeneration) }
            } catch (error: Throwable) {
                // A failed reopen must not leave PLAYING with a frozen clock; callers decide whether to retry or pause.
                thawAfterFailedRecovery(expectedGeneration)
                throw error
            }
        }
        val postSnapshot = stateLock.withLock {
            if (closed || seekGeneration.get() != expectedGeneration) return
            val frozen = recoveringGeneration == expectedGeneration
            if ((result.recovered || retried || frozen) && currentState.mode == PlaybackMode.PLAYING) {
                anchorMediaPtsNs = displayPositionNs()
                recoveringGeneration = null
                anchorWallNs = clock.nowNs()
                audioPlayer?.seek(anchorMediaPtsNs)
                signalLoopLocked()
            }
            val displayed = result.displayedFrameIndex.coerceIn(0L, (totalFrames - 1L).coerceAtLeast(0L))
            val updated = currentState.copy(
                currentFrameIndex = frameIndex,
                displayedFrameIndex = displayed,
                // Exact seek_frame always clears isApproximate once settled.
                isApproximate = false,
                audioState = currentAudioState(),
            )
            publishStateLocked(updated)
        }
        if (postSnapshot != null) {
            notifyListeners(postSnapshot)
        }
    }

    private fun ensureLoopThreadStartedLocked() {
        if (!autoRunLoop || loopThread != null) return
        loopThread = Thread(::runPlaybackLoop, "arb-playback-loop").apply {
            isDaemon = true
            start()
        }
    }

    private fun runPlaybackLoop() {
        while (true) {
            val version = stateLock.withLock { wakeVersion }
            val deadline = try {
                tick()
            } catch (_: InterruptedException) {
                if (isClosed) return
                continue
            } catch (t: Throwable) {
                // Keep the loop thread alive: settle in PAUSED and report instead of freezing in PLAYING.
                val errSnapshot = stateLock.withLock {
                    if (closed) return
                    anchorMediaPtsNs = displayPositionNs()
                    runCatching { audioPlayer?.pause() }
                    seekGeneration.incrementAndGet()
                    publishStateLocked(currentState.copy(mode = PlaybackMode.PAUSED, audioState = currentAudioState()))
                }
                if (errSnapshot != null) runCatching { notifyListeners(errSnapshot) }
                runCatching { onPlaybackError(t) }
                null
            }
            stateLock.withLock {
                if (closed) return
                try {
                    while (!closed && (currentState.mode != PlaybackMode.PLAYING || clockFrozenLocked())) {
                        loopWakeup.await()
                    }
                    if (closed) return
                    if (version == wakeVersion && deadline != null) {
                        val remaining = deadline - clock.nowNs()
                        if (remaining > 0L) loopWakeup.awaitNanos(remaining)
                    }
                } catch (_: InterruptedException) {
                    if (closed) return
                }
            }
        }
    }

    private fun signalLoopLocked() {
        wakeVersion++
        loopWakeup.signalAll()
    }

    private fun seekDuringPlayback(frame: Long, generation: Long) {
        for (attempt in 0..1) {
            try {
                if (seekGeneration.get() != generation) return
                performExactSeek(frame, generation, retried = attempt > 0)
                return
            } catch (error: Exception) {
                if (attempt == 0 && error is IOException) {
                    freezeForRecovery(generation)
                    stateLock.withLock {
                        if (closed || seekGeneration.get() != generation) return
                        try {
                            loopWakeup.awaitNanos(IO_RETRY_DELAY_NS)
                        } catch (_: InterruptedException) {
                            if (closed) return
                        }
                    }
                    continue
                }
                val snapshot = stateLock.withLock {
                    if (closed || seekGeneration.get() != generation) return
                    anchorMediaPtsNs = displayPositionNs()
                    audioPlayer?.pause()
                    val next = currentState.copy(mode = PlaybackMode.PAUSED, audioState = currentAudioState())
                    signalLoopLocked()
                    publishStateLocked(next)
                }
                snapshot?.let(::notifyListeners)
                val reason = error.message ?: error.javaClass.simpleName
                runCatching { onPlaybackError(IllegalStateException("フレーム #$frame の再生に失敗しました（$reason）", error)) }
                return
            }
        }
    }

    private fun freezeForRecovery(generation: Long) = stateLock.withLock {
        if (seekGeneration.get() == generation && currentState.mode == PlaybackMode.PLAYING) {
            anchorMediaPtsNs = displayPositionNs()
            recoveringGeneration = generation
        }
    }

    private fun thawAfterFailedRecovery(generation: Long) = stateLock.withLock {
        if (recoveringGeneration == generation) {
            recoveringGeneration = null
            // The frozen anchor already holds the media time; restart the wall clock from now.
            anchorWallNs = clock.nowNs()
            signalLoopLocked()
        }
    }

    /** Exclusive end on the same media clock used by tick(), also for older daemons. */
    val durationNs: Long
        get() = axis.endNs

    /** Shared media time for all timeline layers. Reading the clock never requests a frame. */
    fun displayPositionNs(): Long = stateLock.withLock {
        val state = currentState
        if (state.mode != PlaybackMode.PLAYING || clockFrozenLocked()) return anchorMediaPtsNs
        val elapsed = state.speed.wallToMediaNs((clock.nowNs() - anchorWallNs).coerceAtLeast(0L))
        saturatingAdd(anchorMediaPtsNs, elapsed).coerceAtMost(saturatingAdd(frameRelativePtsNs(state.outFrameIndex), frameDurationNs(state.outFrameIndex)))
    }

    private fun clockFrozenLocked(): Boolean {
        val generation = seekGeneration.get()
        return initialSeekGeneration == generation || recoveringGeneration == generation
    }

    private fun frameDurationNs(frame: Long): Long = axis.outTime(frame) - axis.start(frame)

    private fun currentAudioState(): AudioState = audioPlayer?.audioState ?: AudioState.UNAVAILABLE

    private fun publishStateLocked(next: PlaybackState): PlaybackState? {
        if (next == currentState) return null
        currentState = next
        return next
    }

    private fun notifyListeners(snapshot: PlaybackState) {
        for (listener in listeners) {
            listener(snapshot)
        }
    }

    companion object {
        fun paddedRange(first: Long, last: Long, before: Long, after: Long, frameCount: Long): LongRange {
            val maxFrame = (frameCount - 1L).coerceAtLeast(0L)
            val start = saturatingSub(first, before).coerceIn(0L, maxFrame)
            return start..saturatingAdd(last, after).coerceIn(start, maxFrame)
        }

        private const val IO_RETRY_DELAY_NS = 25_000_000L
        const val DEFAULT_FALLBACK_FPS: Double = 30.0
        private const val CLOSE_JOIN_TIMEOUT_MS: Long = 2_000L

        private fun saturatingAdd(a: Long, b: Long): Long {
            val sum = a + b
            return if (((a xor sum) and (b xor sum)) < 0L) {
                if (a >= 0L) Long.MAX_VALUE else Long.MIN_VALUE
            } else {
                sum
            }
        }

        private fun saturatingSub(a: Long, b: Long): Long {
            val diff = a - b
            return if (((a xor b) and (a xor diff)) < 0L) {
                if (a >= 0L) Long.MAX_VALUE else Long.MIN_VALUE
            } else {
                diff
            }
        }
    }
}
