package io.github.shinma06.arb.ide

import androidx.compose.ui.graphics.ImageBitmap
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.diagnostic.Logger
import com.intellij.openapi.fileEditor.FileEditor
import com.intellij.openapi.fileEditor.FileEditorLocation
import com.intellij.openapi.fileEditor.FileEditorManager
import com.intellij.openapi.fileEditor.FileEditorState
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.UserDataHolderBase
import com.intellij.openapi.vfs.VirtualFile
import io.github.shinma06.arb.ipc.BundleLeaseController
import io.github.shinma06.arb.model.ArbdRpcException
import io.github.shinma06.arb.model.EditUpdateSessionParams
import io.github.shinma06.arb.model.ExportPngParams
import io.github.shinma06.arb.model.ExportTrimParams
import io.github.shinma06.arb.model.LeaseHolderKind
import io.github.shinma06.arb.model.LogRecord
import io.github.shinma06.arb.model.QueryBucket
import io.github.shinma06.arb.model.StreamStatus
import io.github.shinma06.arb.playback.AudioSink
import io.github.shinma06.arb.playback.AudioSyncPlayer
import io.github.shinma06.arb.playback.FrameSeekResult
import io.github.shinma06.arb.playback.FrameSeeker
import io.github.shinma06.arb.playback.JvmAudioSink
import io.github.shinma06.arb.playback.PlaybackClock
import io.github.shinma06.arb.playback.PlaybackController
import io.github.shinma06.arb.playback.PlaybackSpeed
import io.github.shinma06.arb.playback.SystemPlaybackClock
import io.github.shinma06.arb.playback.WavFilePcmSource
import io.github.shinma06.arb.shm.ShmFrameMeta
import io.github.shinma06.arb.shm.ShmFrame
import io.github.shinma06.arb.state.AuxStream
import io.github.shinma06.arb.state.ExportCompletionInfo
import io.github.shinma06.arb.state.LOG_DISPLAY_LIMIT
import io.github.shinma06.arb.state.StreamLoadState
import io.github.shinma06.arb.state.streamFailureReason
import io.github.shinma06.arb.state.TimelineViewerReducer
import io.github.shinma06.arb.state.TimelineViewerState
import io.github.shinma06.arb.state.TrimCloseState
import io.github.shinma06.arb.ui.ArbStudioContent
import io.github.shinma06.arb.ui.ArbStudioController
import io.github.shinma06.arb.ui.LatestValueMailbox
import io.github.shinma06.arb.ui.StudioTrimRange
import io.github.shinma06.arb.ui.StudioExportFormat
import org.jetbrains.jewel.bridge.JewelComposePanel
import java.awt.BorderLayout
import java.beans.PropertyChangeListener
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.RejectedExecutionException
import java.util.concurrent.TimeUnit
import javax.swing.JComponent
import javax.swing.JPanel
import javax.swing.SwingUtilities

internal fun Throwable.isMissingBundle(): Boolean {
    if (this is ArbdRpcException) {
        // Unlabeled older daemons only expose the numeric code on bundle open.
        val label = details?.errorCode
        return code == -32001 && (label == null || label == "BUNDLE_NOT_FOUND")
    }
    val msg = message.orEmpty()
    return msg.contains("canonicalize_bundle", ignoreCase = true) ||
        msg.contains("not found", ignoreCase = true) ||
        msg.contains("No such file", ignoreCase = true) ||
        msg.contains("cannot find the path", ignoreCase = true)
}

/**
 * Thin IntelliJ [FileEditor] for the Integrated Viewer & Trim Studio (ARB and Legacy RO bundles),
 * rendered natively with JetBrains Jewel Compose (`JewelComposePanel`).
 *
 * Lifecycle & safety guarantees:
 * - Acquires a Viewer Lease via `bundle.open(holder_kind = ide_viewer)` and renews it periodically.
 * - Opens the SHM viewport first and decouples timeline/log pagination so video renders immediately
 *   even for large log bundles (M2), fetching logs in bounded cursor pages with adaptive halving on
 *   4 MiB RPC frame-size limits.
 * - Wires real per-frame VFR timestamps (`framePtsNs` from `media.open_viewport`) into
 *   [PlaybackController] so play, seek, and loop wrap stay aligned with audio and logs (H8).
 * - Owns a per-editor viewport connection. Disposal releases the viewport, mapping and lease
 *   even when a background open or reconnect is still in flight.
 */
class ArbBundleFileEditor internal constructor(
    private val project: Project?,
    private val backingFile: VirtualFile,
    private val bundleIdOrPath: String,
    private val displayTitle: String,
    private val daemonService: ArbdDaemonService,
    private val sourceJumpResolver: SourceJumpResolver,
    private val playbackClock: PlaybackClock = SystemPlaybackClock,
    private val autoRunPlaybackLoop: Boolean = true,
    private val audioSinkFactory: () -> AudioSink = { JvmAudioSink() },
    private val audioAutoRunPump: Boolean = true,
    private val initialLogPageSize: Int = INITIAL_LOG_PAGE_SIZE,
    private val publishEdt: (() -> Unit) -> Unit = { block -> SwingUtilities.invokeLater(block) },
) : UserDataHolderBase(), FileEditor {
    constructor(
        project: Project,
        backingFile: VirtualFile,
        bundleIdOrPath: String,
        displayTitle: String,
        daemonService: ArbdDaemonService,
        sourceJumpResolver: SourceJumpResolver = IntelliJSourceJumpResolver(project),
    ) : this(
        project = project,
        backingFile = backingFile,
        bundleIdOrPath = bundleIdOrPath,
        displayTitle = displayTitle,
        daemonService = daemonService,
        sourceJumpResolver = sourceJumpResolver,
        playbackClock = SystemPlaybackClock,
    )

    constructor(
        project: Project,
        virtualFile: ArbBundleVirtualFile,
        daemonService: ArbdDaemonService,
        sourceJumpResolver: SourceJumpResolver = IntelliJSourceJumpResolver(project),
    ) : this(
        project = project,
        backingFile = virtualFile,
        bundleIdOrPath = virtualFile.bundleIdOrPath,
        displayTitle = virtualFile.displayTitle,
        daemonService = daemonService,
        sourceJumpResolver = sourceJumpResolver,
    )

    private val logger = Logger.getInstance(ArbBundleFileEditor::class.java)
    private val lifecycleLock = Any()

    private val worker = Executors.newSingleThreadExecutor { r ->
        Thread(r, "arb-studio-$displayTitle").apply { isDaemon = true }
    }
    private val timelineWorker = Executors.newSingleThreadExecutor { r ->
        Thread(r, "arb-studio-timeline-$displayTitle").apply { isDaemon = true }
    }
    private val audioDecodeWorker = Executors.newSingleThreadExecutor { r ->
        Thread(r, "arb-audio-decode-$displayTitle").apply { isDaemon = true }
    }
    private val trimCloseState = TrimCloseState()

    private val mediaSession = StudioViewportSession(daemonService::openMediaClient, { disposed })

    // Its own viewport and SHM reader keep trim decoding off the playing decoder's lock.
    private val trimViewport = StudioViewportSession(daemonService::openMediaClient, { disposed })

    internal val studioController: ArbStudioController = ArbStudioController(
        displayPositionNs = { currentPlaybackController()?.displayPositionNs() },
        onTimeSeek = { time, settled -> performTimeSeek(time, settled) },
        onScrub = { frameIndex -> performScrub(frameIndex) },
        onSeekExact = { frameIndex -> performSeekExact(frameIndex) },
        onPngExport = ::performPngExport,
        onTrimPreviewFrame = ::requestTrimPreview,
        onTrimPreviewEnded = ::endTrimPreview,
        onTimeRangeChanged = { range, playhead ->
            trimCloseState.onRangeAdjusted(range.inTimeNs, range.outTimeNs)
            currentPlaybackController()?.setStudioRange(range.inTimeNs, range.outTimeNs)
            persistEditSession(range, playhead)
        },
        onPlayPauseToggle = {
            submitWork {
                runCatching { currentPlaybackController()?.togglePlayPause() }
                    .onFailure { logger.debug("ARB play/pause failed", it) }
            }
        },
        onStepFrame = { delta ->
            submitWork {
                runCatching { currentPlaybackController()?.stepFrame(delta) }
                    .onFailure { logger.debug("ARB stepFrame failed", it) }
            }
        },
        onSpeedChange = { speed: PlaybackSpeed ->
            submitWork {
                runCatching { currentPlaybackController()?.setSpeed(speed) }
            }
        },
        onMuteToggle = {
            submitWork {
                runCatching { currentPlaybackController()?.toggleMute() }
            }
        },
        onLoopToggle = {
            submitWork {
                runCatching { currentPlaybackController()?.toggleLoop() }
            }
        },
        onSelectTapRange = { applySelectTapRange() },
        onSelectAroundCrash = { applySelectAroundCrash() },
        onResetFullRange = { applyResetFullRange() },
        onTrashOriginalToggled = { enabled ->
            trashOriginalRequested = enabled
        },
        onRevealInFinder = { path ->
            ArbNotifications.openPathInFileManager(path)
        },
        onOpenExportedBundle = { exportedIdOrPath, title ->
            publishEdt {
                val p = project
                if (p != null && !p.isDisposed) {
                    val vf = ArbBundleVirtualFile(exportedIdOrPath, title, isLegacyHint = false)
                    FileEditorManager.getInstance(p).openFile(vf, true)
                }
            }
        },
        onSourceJump = { target ->
            // PSI / FilenameIndex lookups are slow operations: never resolve them on the EDT
            // (Compose click handlers run there). Navigation itself hops back to the EDT.
            runOffEdt {
                val jumped = runCatching { sourceJumpResolver.resolveAndNavigate(target) }.getOrDefault(false)
                if (!jumped && !disposed) {
                    publishEdt {
                        studioController.showError("プロジェクト内に該当ソースが見つかりませんでした: ${target.displayLabel}")
                    }
                }
            }
        },
        onTimeRangeExport = { range, format ->
            performExport(range, format)
        },
        onAppPackageFilterChanged = { packageName -> performAppPackageFilter(packageName) },
        onRetryAuxStream = { stream -> retryAuxStream(stream) },
        onRetryEditSave = { retryEditSave() },
        initialBurnTapsOnExport = ArbLocalSettingsState.getInstanceOrNull()?.burnTapsOnExport ?: true,
        onBurnTapsToggled = { enabled ->
            ArbLocalSettingsState.getInstanceOrNull()?.burnTapsOnExport = enabled
        },
        onCloseTab = {
            publishEdt {
                val p = project
                if (p != null && !p.isDisposed) {
                    FileEditorManager.getInstance(p).closeFile(backingFile)
                }
            }
        },
    )

    /** A decoded SHM frame awaiting EDT publication (bitmap already converted off the EDT). */
    private class PendingFrame(val meta: ShmFrameMeta, val bitmap: ImageBitmap?, val width: Int, val height: Int)

    private val pendingFrame = LatestValueMailbox<PendingFrame>()

    // Lazy so headless JUnit tests exercising Studio wiring never require a live Skiko/Compose frame.
    private val rootComponent: JPanel by lazy {
        JPanel(BorderLayout()).apply {
            add(JewelComposePanel { ArbStudioContent(studioController) }, BorderLayout.CENTER)
        }
    }

    // All teardown-owned handles below are guarded by [lifecycleLock] so dispose() and async
    // open/register never race or double-close (M1).
    private var ownedLeaseController: BundleLeaseController? = null
    private var ownedPlaybackController: PlaybackController? = null

    @Volatile
    private var attachedAudioPlayer: AudioSyncPlayer? = null

    @Volatile
    private var editSession: EditSessionWrites? = null

    @Volatile
    private var disposed = false

    @Volatile
    private var trashOriginalRequested = false

    init {
        loadBundleAsync()
    }

    fun hasUnexportedTrimRange(): Boolean = trimCloseState.hasUnexportedCustomTrim()

    fun currentTrimRange(): Pair<Long, Long> = viewerState?.let { it.inFrameIndex to it.outFrameIndex } ?: (0L to 0L)

    internal fun playbackControllerForTest(): PlaybackController? = synchronized(lifecycleLock) {
        ownedPlaybackController
    }

    internal fun awaitQueuedWorkForTest() {
        for (executor in listOf(worker, timelineWorker)) {
            if (executor.isShutdown) {
                check(executor.awaitTermination(10, TimeUnit.SECONDS))
            } else {
                executor.submit {}.get(10, TimeUnit.SECONDS)
            }
        }
    }

    internal fun audioPlayerForTest(): AudioSyncPlayer? = attachedAudioPlayer

    /**
     * The one viewer state is [studioController]'s; workers read it ([viewerState]) and change it
     * only through [updateViewerState], which applies [transform] on the EDT after anything queued
     * before it (e.g. the initial render), so concurrent loaders never overwrite each other.
     */
    @Synchronized
    internal fun updateViewerState(transform: (TimelineViewerState) -> TimelineViewerState) {
        publishEdt {
            if (!disposed) studioController.updateState(transform)
        }
    }

    private val viewerState: TimelineViewerState?
        get() = studioController.viewerState

    private fun currentPlaybackController(): PlaybackController? = synchronized(lifecycleLock) {
        ownedPlaybackController
    }

    private fun loadBundleAsync() {
        submitWork {
            try {
                val opened = daemonService.withClient { rpc ->
                    rpc.bundleOpen(
                        bundleIdOrPath = bundleIdOrPath,
                        holderKind = LeaseHolderKind.IDE_VIEWER,
                        holderPid = ProcessHandle.current().pid(),
                    )
                }

                val lease = BundleLeaseController(
                    leaseId = opened.leaseId,
                    renewAction = { id -> daemonService.withClient { it.bundleLeaseRenew(id) } },
                    closeLeaseAction = { id -> daemonService.withClient { it.bundleClose(id) } },
                    // Viewport teardown is owned atomically under lifecycleLock (see dispose()).
                    closeViewportAction = null,
                    onHeartbeatError = { err -> logger.warn("ARB lease heartbeat failed", err) },
                )
                val leaseRegistered = synchronized(lifecycleLock) {
                    if (disposed) {
                        false
                    } else {
                        ownedLeaseController = lease
                        true
                    }
                }
                if (!leaseRegistered) {
                    runCatching { lease.close() }
                    return@submitWork
                }

                val sessionId = "ide-${opened.bundleId}"
                editSession = writesFor(sessionId)

                val existingSession = runCatching {
                    daemonService.withClient { it.editGetSession(sessionId) }
                }.getOrNull()

                // Phase 1 (M2): Open video viewport FIRST before querying logs/events so video
                // displays immediately even when log bundles are huge or slow.
                var viewportId: String? = null
                var frameCount = opened.summary.frameCount.toLong()
                var timelineDurationNs: Long? = null
                var lastFrameDurationNs: Long? = null
                var framePtsNs: List<Long> = emptyList()

                if (opened.summary.videoPath != null && frameCount > 0L) {
                    val vp = mediaSession.open(opened.bundleId)
                    viewportId = vp.viewportId
                    frameCount = vp.frameCount
                    framePtsNs = vp.framePtsNs
                    timelineDurationNs = vp.timelineDurationNs
                    lastFrameDurationNs = vp.lastFrameDurationNs
                }

                val initialIn = existingSession?.inFrameIndex ?: 0L
                val initialOut = existingSession?.outFrameIndex ?: (frameCount - 1L).coerceAtLeast(0L)
                val initialPlayhead = existingSession?.playheadFrameIndex ?: 0L

                var initialState = TimelineViewerState(
                    bundleId = opened.bundleId,
                    path = opened.path,
                    format = opened.format,
                    isLegacy = opened.isLegacy,
                    leaseId = opened.leaseId,
                    viewportId = viewportId,
                    summary = opened.summary,
                    frameCount = frameCount,
                    currentFrameIndex = initialPlayhead,
                )

                val fallbackAvOffsetMs = opened.summary.streams.audio?.avOffsetMs ?: 0L
                val hasAudioTrack = opened.summary.audioPath != null ||
                    opened.summary.streams.audio?.status in setOf(StreamStatus.PRESENT, StreamStatus.PARTIAL)

                var activeCtrl: PlaybackController? = null
                if (viewportId != null && frameCount > 0L) {
                    val seeker = object : FrameSeeker {
                        override fun seekExact(frameIndex: Long): FrameSeekResult = seekExact(frameIndex) {}

                        override fun seekExact(frameIndex: Long, onRecovery: () -> Unit): FrameSeekResult {
                            return mediaSession.seekFrame(frameIndex, onRecovery) { (_, frame, replacementId) ->
                                if (replacementId != null) updateViewerState { it.copy(viewportId = replacementId) }
                                readShmAndRender(frame)
                            }
                        }

                        override fun scrubApprox(frameIndex: Long): FrameSeekResult {
                            val (resp, frame, replacementId) = mediaSession.request { rpc, id -> rpc.mediaScrub(id, frameIndex) }
                            if (replacementId != null) updateViewerState { it.copy(viewportId = replacementId) }
                            readShmAndRender(frame)
                            return FrameSeekResult(resp.requestedFrameIndex, resp.displayedFrameIndex, resp.isApproximate)
                        }
                    }
                    // H8: Pass real per-frame VFR presentation timestamps from `media.open_viewport`
                    // into PlaybackController so play, seek, and loop wrap share the exact PTS map.
                    val ctrl = PlaybackController(
                        totalFrames = frameCount,
                        frameSeeker = seeker,
                        framePtsNs = framePtsNs,
                        lastFrameDurationNs = lastFrameDurationNs,
                        timelineDurationNs = timelineDurationNs,
                        fallbackFps = 1_000_000_000.0 / initialState.axis.outTime(0).coerceAtLeast(1L),
                        audioPlayer = null,
                        clock = playbackClock,
                        initialFrameIndex = initialPlayhead,
                        initialInFrameIndex = initialIn,
                        initialOutFrameIndex = initialOut,
                        initialAvOffsetMs = fallbackAvOffsetMs,
                        autoRunLoop = autoRunPlaybackLoop,
                        onPlaybackError = { err ->
                            logger.warn("ARB playback error", err)
                            publishEdt { if (!disposed) studioController.showPlaybackError(err.message ?: "映像の再生に失敗しました") }
                        },
                    )
                    initialState = initialState.copy(axis = ctrl.axis, outTimeNs = ctrl.axis.endNs)
                    ctrl.addStateListener { _ ->
                        publishEdt {
                            if (!disposed) {
                                studioController.renderPlaybackState(ctrl.state)
                            }
                        }
                    }
                    val ctrlRegistered = synchronized(lifecycleLock) {
                        if (disposed) {
                            false
                        } else {
                            ownedPlaybackController = ctrl
                            true
                        }
                    }
                    if (!ctrlRegistered) {
                        runCatching { ctrl.close() }
                        return@submitWork
                    }
                    activeCtrl = ctrl

                    if (hasAudioTrack) {
                        attachDecodedAudioAsync(opened.bundleId, ctrl)
                    }
                }

                val savedStart = existingSession?.inTimeNs
                val savedEnd = existingSession?.outTimeNs
                initialState = if (savedStart != null && savedEnd != null && savedStart >= 0 && savedStart < savedEnd && savedEnd <= initialState.axis.endNs) {
                    initialState.copy(inTimeNs = savedStart, outTimeNs = savedEnd)
                } else {
                    TimelineViewerReducer.setRange(initialState, initialState.axis.start(initialIn), initialState.axis.outTime(initialOut))
                }
                activeCtrl?.setStudioRange(initialState.inTimeNs, initialState.outTimeNs)
                trimCloseState.initializeTimeRange(initialState.axis.endNs, initialState.inTimeNs, initialState.outTimeNs)
                publishEdt {
                    if (!disposed) {
                        studioController.renderState(initialState, image = null)
                        activeCtrl?.state?.let(studioController::renderPlaybackState)
                    }
                }

                // Render initial frame right away before decoupled timeline/log pagination starts!
                if (activeCtrl != null && !disposed) {
                    try {
                        activeCtrl.seekToFrame(initialPlayhead)
                    } catch (e: Exception) {
                        // PlaybackController.onPlaybackError already surfaced the reason on the Studio.
                        logger.warn("ARB initial seek_frame failed", e)
                    }
                }

                // Phase 2 (M2): Decoupled background loading of filmstrip, events, and paged logs.
                if (!disposed) {
                    loadTimelineDataAsync(opened.bundleId, viewportId)
                }
            } catch (t: Throwable) {
                if (disposed) return@submitWork
                logger.warn("Failed to open ARB bundle $bundleIdOrPath", t)
                val msg = t.message.orEmpty()
                val looksMissing = t.isMissingBundle()
                if (looksMissing) {
                    daemonService.notifyCatalogChanged()
                }
                val displayError = if (looksMissing) {
                    "バンドルフォルダが見つかりませんでした（手動削除または移動された可能性があります）。ARB パネルの一覧から削除できます。詳細: $msg"
                } else {
                    "バンドルを開けませんでした: $msg"
                }
                publishEdt {
                    if (!disposed) {
                        studioController.showError(displayError)
                    }
                }
            }
        }
    }

    /**
     * Decoupled Phase 2 loader (M2): loads filmstrip, discontinuities, crashes, screens, taps, and
     * cursor-paged logs without blocking or aborting video viewport startup.
     */
    private fun loadTimelineDataAsync(bundleId: String, viewportId: String?) {
        submitGuarded(timelineWorker) {
            if (disposed) return@submitGuarded

            // A failed stream is recorded as Failed (never as an empty list) so the UI can say
            // "could not load" instead of "0 events"; the other streams and the video go on.
            for (stream in AuxStream.entries) {
                if (disposed) return@submitGuarded
                if (stream == AuxStream.FILMSTRIP && viewportId == null) continue
                val load = loadAuxStream(stream, bundleId, viewportId)
                updateViewerState { load.applyTo(it) }
            }
            fun loadTaps(bucket: QueryBucket) = runCatching {
                daemonService.withClient { it.timelineQueryTaps(bundleId, bucket = bucket, limit = LOG_QUERY_LIMIT).taps }
            }.onFailure {
                publishEdt {
                    if (!disposed) studioController.showTimelineWarning("タップを取得できませんでした（${bucket.wire}）。映像再生は継続できます。")
                }
            }.getOrDefault(emptyList())
            val mainTaps = loadTaps(QueryBucket.MAIN)
            val unanchoredTaps = loadTaps(QueryBucket.UNANCHORED)

            if (disposed) return@submitGuarded
            updateViewerState {
                TimelineViewerReducer.withBucketedRecords(
                    state = it,
                    mainLogs = it.mainLogs,
                    unanchoredLogs = it.unanchoredLogs,
                    mainTaps = mainTaps,
                    unanchoredTaps = unanchoredTaps,
                )
            }

            try {
                val mainLogs = queryLogsPagedWithLimit(
                    bundleId = bundleId,
                    bucket = QueryBucket.MAIN,
                    maxLogs = LOG_QUERY_LIMIT,
                )
                if (disposed) return@submitGuarded
                val unanchoredLogs = queryLogsPagedWithLimit(
                    bundleId = bundleId,
                    bucket = QueryBucket.UNANCHORED,
                    maxLogs = LOG_QUERY_LIMIT,
                )
                if (disposed) return@submitGuarded

                updateViewerState {
                    TimelineViewerReducer.withBucketedRecords(
                        state = it,
                        mainLogs = mainLogs.rows,
                        unanchoredLogs = unanchoredLogs.rows,
                        mainTaps = it.mainTaps,
                        unanchoredTaps = it.unanchoredTaps,
                        mainLogsLimited = mainLogs.limited,
                        unanchoredLogsLimited = unanchoredLogs.limited,
                        logLimitUnknown = mainLogs.limitUnknown || unanchoredLogs.limitUnknown,
                    )
                }
            } catch (t: Throwable) {
                if (disposed) return@submitGuarded
                logger.warn("ARB paged log load failed for $bundleId (video remains active)", t)
                publishEdt {
                    if (!disposed) {
                        studioController.showTimelineWarning("ログの読み込みに失敗しました（映像再生は継続できます）: ${t.message}")
                    }
                }
            }
        }
    }

    /** Outcome of one auxiliary-stream load: [apply] folds it (data or `Failed`) into the viewer state. */
    private class AuxLoad(private val apply: (TimelineViewerState) -> TimelineViewerState) {
        fun applyTo(state: TimelineViewerState): TimelineViewerState = apply(state)
    }

    /**
     * Loads one auxiliary stream. A failure becomes [StreamLoadState.Failed] with its reason and
     * leaves the stream's data untouched; success marks it [StreamLoadState.Loaded] (so an empty
     * list now truly means "none").
     */
    private fun loadAuxStream(stream: AuxStream, bundleId: String, viewportId: String?): AuxLoad {
        val data: (TimelineViewerState) -> TimelineViewerState
        try {
            data = fetchAuxStreamData(stream, bundleId, viewportId)
        } catch (t: Throwable) {
            logger.warn("ARB ${stream.name.lowercase()} load failed for $bundleId (other streams and video continue)", t)
            val failed = StreamLoadState.Failed(streamFailureReason(t))
            return AuxLoad { TimelineViewerReducer.withStreamState(it, stream, failed) }
        }
        return AuxLoad { TimelineViewerReducer.withStreamState(data(it), stream, StreamLoadState.Loaded) }
    }

    private fun stateMutation(block: (TimelineViewerState) -> TimelineViewerState) = block

    private fun fetchAuxStreamData(
        stream: AuxStream,
        bundleId: String,
        viewportId: String?,
    ): (TimelineViewerState) -> TimelineViewerState = when (stream) {
        AuxStream.FILMSTRIP -> {
            // A reconnect during startup replaces the viewport; always target the live one.
            val live = viewerState?.viewportId ?: viewportId
            if (live == null) {
                stateMutation { state -> state }
            } else {
                val entries = daemonService.withClient { it.mediaFilmstrip(live) }.entries
                stateMutation { state -> state.copy(filmstrip = entries) }
            }
        }
        AuxStream.DISCONTINUITIES -> {
            val items = daemonService.withClient { it.timelineDiscontinuities(bundleId).discontinuities }
            stateMutation { state -> state.copy(discontinuities = items) }
        }
        AuxStream.CRASHES -> {
            val items = daemonService.withClient { it.timelineCrashes(bundleId).crashes }
            stateMutation { state -> state.copy(crashes = items) }
        }
        AuxStream.SCREENS -> {
            val items = daemonService.withClient { it.timelineScreens(bundleId).screens }
            stateMutation { state -> state.copy(screens = items) }
        }
    }

    /** Re-requests only [stream] (the inline "再読込" action); other streams and playback are untouched. */
    internal fun retryAuxStream(stream: AuxStream) {
        val current = viewerState ?: return
        submitGuarded(timelineWorker) {
            if (disposed) return@submitGuarded
            val load = loadAuxStream(stream, current.bundleId, current.viewportId)
            updateViewerState { load.applyTo(it) }
        }
    }

    /** Rows of one log bucket and whether more rows exist beyond the display [LOG_QUERY_LIMIT]. */
    internal class PagedLogs(val rows: List<LogRecord>, val limited: Boolean, val limitUnknown: Boolean = false)

    /** Follows the same sequence order and limit as a single timeline.query_logs request. */
    internal fun queryLogsPaged(
        bundleId: String,
        bucket: QueryBucket,
        maxLogs: Int = LOG_QUERY_LIMIT,
        packageName: String? = null,
    ): List<LogRecord> = queryLogsPagedWithLimit(bundleId, bucket, maxLogs, packageName).rows

    /**
     * [queryLogsPaged] plus an honest "rows were cut" flag: once [maxLogs] rows were read, one more
     * row is probed after the last `seq`. An inconclusive probe (error, or an old daemon that
     * ignores `after_seq`) counts as limited, because the bucket may well be longer.
     */
    internal fun queryLogsPagedWithLimit(
        bundleId: String,
        bucket: QueryBucket,
        maxLogs: Int = LOG_QUERY_LIMIT,
        packageName: String? = null,
    ): PagedLogs {
        val rows = queryLogsPagedRows(bundleId, bucket, maxLogs, packageName)
        if (disposed || rows.size < maxLogs) return PagedLogs(rows, limited = false)
        val lastSeq = rows.lastOrNull()?.seq ?: return PagedLogs(rows, limited = false)
        val more = runCatching {
            daemonService.withClient {
                it.timelineQueryLogs(bundleId, bucket, limit = 1, packageName = packageName, afterSeq = lastSeq).logs
            }
        }.getOrNull()
        val confirmedExtra = more?.any { it.seq > lastSeq } == true
        val unknown = more == null || (more.isNotEmpty() && !confirmedExtra)
        return PagedLogs(rows, limited = confirmedExtra, limitUnknown = unknown)
    }

    private fun queryLogsPagedRows(
        bundleId: String,
        bucket: QueryBucket,
        maxLogs: Int,
        packageName: String?,
    ): List<LogRecord> {
        val rows = mutableListOf<LogRecord>()
        var pageSize = initialLogPageSize.coerceAtLeast(1)
        var legacy = false
        while (rows.size < maxLogs && !disposed) {
            val cursor = if (legacy) null else rows.lastOrNull()?.seq
            val limit = if (legacy) pageSize else minOf(pageSize, maxLogs - rows.size)
            val page = try {
                daemonService.withClient {
                    it.timelineQueryLogs(bundleId, bucket, limit = limit, packageName = packageName, afterSeq = cursor).logs
                }
            } catch (e: ArbdRpcException) {
                if (!isResponseTooLargeError(e) || limit <= 1) throw e
                pageSize = (limit / 2).coerceAtLeast(1)
                continue
            }
            if (legacy) {
                if (page.size == limit && limit < maxLogs) {
                    publishEdt {
                        if (!disposed) studioController.showTimelineWarning("旧デーモンの応答上限によりログの先頭 ${page.size} 件を表示しています。")
                    }
                }
                return if (page.size >= rows.size) page else rows
            }
            if (cursor != null && page.firstOrNull()?.let { it.seq <= cursor } == true) {
                // Older daemons ignore after_seq. Request the original bounded result in one call.
                legacy = true
                pageSize = maxLogs
                continue
            }
            rows.addAll(page)
            if (page.size < limit) break
        }
        return rows
    }

    private fun attachDecodedAudioAsync(
        bundleId: String,
        controller: PlaybackController,
    ) {
        val task = Runnable {
            if (disposed || controller.isClosed) return@Runnable
            val decoded = runCatching {
                daemonService.withClient { it.mediaDecodeAudioPcmOrNull(bundleId) }
            }.onFailure { err ->
                logger.debug("ARB media.decode_audio_pcm failed, continuing video-only", err)
            }.getOrNull() ?: return@Runnable

            if (disposed || controller.isClosed || !decoded.available) return@Runnable
            val wavPathStr = decoded.wavPath ?: return@Runnable
            val wavPath = runCatching { Path.of(wavPathStr) }.getOrNull() ?: return@Runnable
            if (!Files.isRegularFile(wavPath)) return@Runnable

            val effectiveAvOffsetMs = decoded.avOffsetMs
            val player = runCatching {
                AudioSyncPlayer(
                    source = WavFilePcmSource.open(wavPath),
                    sink = audioSinkFactory(),
                    avOffsetMs = effectiveAvOffsetMs,
                    autoRunPump = audioAutoRunPump,
                )
            }.onFailure { err ->
                logger.debug("ARB audio player unavailable, continuing video-only", err)
            }.getOrNull() ?: return@Runnable

            if (disposed || controller.isClosed) {
                runCatching { player.close() }
                return@Runnable
            }
            controller.attachAudioPlayer(player, effectiveAvOffsetMs)
            synchronized(lifecycleLock) {
                if (!disposed && ownedPlaybackController === controller) attachedAudioPlayer = player
            }
        }
        try {
            audioDecodeWorker.submit(task)
        } catch (_: RejectedExecutionException) {
            logger.debug("ARB audio decode worker already shut down; skipping audio attach")
        }
    }

    // Each exact seek closes the preceding gesture's mailbox. Later drags cannot be
    // drained by an older worker task queued before that exact seek.
    private var previewTimes = LatestValueMailbox<Long>()

    private fun performTimeSeek(timeNs: Long, settled: Boolean) {
        val ctrl = currentPlaybackController() ?: return
        if (settled) {
            previewTimes.take()
            previewTimes = LatestValueMailbox()
            submitWork {
                runCatching { ctrl.seekToTime(timeNs) }.onFailure { logger.debug("ARB seek_frame failed", it) }
            }
        } else {
            val pending = previewTimes
            if (pending.offer(timeNs)) submitWork {
                val latest = pending.take() ?: return@submitWork
                runCatching { ctrl.scrubToTime(latest) }.onFailure { logger.debug("ARB scrub failed", it) }
            }
        }
    }

    private fun performScrub(frameIndex: Long) {
        val ctrl = currentPlaybackController() ?: return
        submitWork {
            runCatching { ctrl.scrubTo(frameIndex) }
                .onFailure { logger.debug("ARB scrub failed", it) }
        }
    }

    private fun performSeekExact(frameIndex: Long) {
        val ctrl = currentPlaybackController() ?: return
        submitWork {
            runCatching { ctrl.seekToFrame(frameIndex) }
                .onFailure { logger.debug("ARB seek_frame failed", it) }
        }
    }

    private data class PreviewRequest(val frame: Long, val publish: (ImageBitmap?) -> Unit)
    private var trimPreviews = LatestValueMailbox<PreviewRequest>()

    private fun requestTrimPreview(frameIndex: Long, publish: (ImageBitmap?) -> Unit) {
        val bundleId = viewerState?.bundleId ?: return
        val mailbox = trimPreviews
        if (!mailbox.offer(PreviewRequest(frameIndex, publish))) return
        submitWork {
            val request = mailbox.take() ?: return@submitWork
            val bitmap = runCatching {
                trimViewport.ensureOpen(bundleId)
                val result = trimViewport.request { rpc, id -> rpc.mediaSeekFrame(id, request.frame) }
                result.frame?.takeIf { it.meta.frameIndex == request.frame }?.toBufferedImage()?.let(ArbStudioController::toImageBitmapOrNull)
            }.onFailure { logger.debug("ARB trim preview failed", it) }.getOrNull()
            publishEdt { if (!disposed) request.publish(bitmap) }
        }
    }

    private fun endTrimPreview() {
        trimPreviews.take()
        trimPreviews = LatestValueMailbox()
        submitWork { trimViewport.close() }
    }

    private fun applySelectTapRange() {
        val current = viewerState ?: return
        val times = current.mainTaps.mapNotNull(current::tapTime)
        if (times.isEmpty()) return
        val last = times.max()
        studioController.selectRange(times.min() - 1_000_000_000L, last + minOf(current.axis.endNs - last, 2_000_000_000L))
    }

    private fun applySelectAroundCrash() {
        val current = viewerState ?: return
        val time = current.crashes.firstOrNull()?.let(current::crashTime) ?: return
        studioController.selectRange(time - 5_000_000_000L, time + minOf(current.axis.endNs - time, 2_000_000_000L))
    }

    private fun applyResetFullRange() {
        viewerState?.let { studioController.selectRange(0L, it.axis.endNs) }
    }

    private fun readShmAndRender(frame: ShmFrame?) {
        if (frame == null) return
        if (viewerState == null) return
        val image = frame.toBufferedImage()
        // Full-frame pixel copy happens here on the worker, never on the EDT, and only the newest
        // frame is ever queued (a slow EDT skips stale frames instead of queueing every bitmap).
        val bitmap = ArbStudioController.toImageBitmapOrNull(image)
        if (pendingFrame.offer(PendingFrame(frame.meta, bitmap, image.width, image.height))) {
            publishEdt {
                if (disposed) return@publishEdt
                val latest = pendingFrame.take() ?: return@publishEdt
                studioController.applyFramePublished(latest.meta, latest.bitmap, latest.width, latest.height)
            }
        }
    }

    private fun performAppPackageFilter(packageName: String?) {
        val current = viewerState ?: return
        submitGuarded(timelineWorker) {
            if (disposed) return@submitGuarded
            try {
                val mainLogs = queryLogsPagedWithLimit(
                    bundleId = current.bundleId,
                    bucket = QueryBucket.MAIN,
                    maxLogs = LOG_QUERY_LIMIT,
                    packageName = packageName,
                )
                if (disposed) return@submitGuarded
                val unanchoredLogs = queryLogsPagedWithLimit(
                    bundleId = current.bundleId,
                    bucket = QueryBucket.UNANCHORED,
                    maxLogs = LOG_QUERY_LIMIT,
                    packageName = packageName,
                )
                if (disposed) return@submitGuarded
                publishEdt {
                    if (!disposed) {
                        studioController.applyAppPackageLogs(
                            packageName,
                            mainLogs.rows,
                            unanchoredLogs.rows,
                            mainLimited = mainLogs.limited,
                            unanchoredLimited = unanchoredLogs.limited,
                            limitUnknown = mainLogs.limitUnknown || unanchoredLogs.limitUnknown,
                        )
                    }
                }
            } catch (t: Throwable) {
                if (disposed) return@submitGuarded
                logger.warn("ARB timeline.query_logs(package=$packageName) failed", t)
                publishEdt {
                    if (!disposed) {
                        studioController.appPackageFilterFailed(packageName, "アプリ別ログの取得に失敗しました: ${t.message}")
                    }
                }
            }
        }
    }

    private fun performExport(
        range: StudioTrimRange,
        format: StudioExportFormat,
    ) {
        val current = viewerState ?: return
        val shouldTrashOriginal = trashOriginalRequested && !current.isLegacy
        val includeShareMp4 = format == StudioExportFormat.SHARE_MP4
        val burnTapsEnabled = includeShareMp4 && studioController.burnTapsOnExport
        val progressText = if (burnTapsEnabled) "焼き込み書き出し中..." else "書き出し中..."
        if (disposed || studioController.isExporting) return
        studioController.setExporting(true, progressText)

        submitWork {
            try {
                val res = daemonService.withClient {
                    it.exportTrim(
                        ExportTrimParams(
                            bundleIdOrPath = current.bundleId,
                            inFrameIndex = range.inFrameIndex,
                            outFrameIndex = range.outFrameIndex,
                            inTimeNs = range.inTimeNs,
                            outTimeNs = range.outTimeNs,
                            burnTaps = burnTapsEnabled,
                            includeShareMp4 = includeShareMp4,
                            trashOriginal = shouldTrashOriginal,
                            holderPid = ProcessHandle.current().pid(),
                        ),
                    )
                }
                trimCloseState.markExported(range.inTimeNs, range.outTimeNs)
                daemonService.notifyCatalogChanged()

                val shareName = res.shareMp4Path
                    ?.let { p -> runCatching { Path.of(p).fileName?.toString() }.getOrNull() }
                val shareNote = when {
                    shareName != null && res.burnTapsApplied -> " · $shareName (タップ焼き込み)"
                    shareName != null -> " · $shareName"
                    else -> ""
                }
                val trashedBundleId = if (res.originalTrashed) current.bundleId else null
                val trashedNote = if (res.originalTrashed) {
                    " · 元バンドルをゴミ箱へ移動しました（復元可）"
                } else {
                    ""
                }
                val warningNote = res.warning?.takeIf { it.isNotBlank() }?.let { " · 警告: $it" }.orEmpty()
                val summaryText = "書き出し完了: ${res.bundleId} (${res.trimReport.outputFrameCount}f)$shareNote$trashedNote$warningNote"
                val completion = ExportCompletionInfo(
                    summaryText = summaryText,
                    outputPath = res.bundlePath,
                    bundleId = res.bundleId,
                    isTrimBundle = true,
                    shareMp4Path = res.shareMp4Path,
                    originalTrashedBundleId = trashedBundleId,
                    burnTapsApplied = res.burnTapsApplied,
                )
                publishEdt {
                    if (!disposed) studioController.setExporting(false)
                }
                updateViewerState { it.copy(exportCompletion = completion, lastActionMessage = completion.summaryText) }
                ArbNotifications.notifyExportCompleted(
                    project = project,
                    daemonService = daemonService,
                    summaryText = completion.summaryText,
                    outputPath = completion.outputPath,
                    trimmedBundleId = completion.bundleId,
                    originalTrashedBundleId = completion.originalTrashedBundleId,
                )
            } catch (t: Throwable) {
                logger.warn("ARB export failed (format=$format)", t)
                publishEdt {
                    if (!disposed) {
                        studioController.setExporting(false)
                        studioController.showError("書き出しに失敗しました: ${t.message}")
                    }
                }
                ArbNotifications.notifyActionFailed(project, "書き出し", t.message)
            }
        }
    }

    private fun performPngExport(frameIndex: Long) {
        val current = viewerState ?: return
        submitWork {
            try {
                val outputDir = runCatching { ArbScreenshotDestinationResolver.resolveOutputDir(project) }.getOrNull()
                val res = daemonService.withClient {
                    it.exportPng(
                        ExportPngParams(
                            bundleIdOrPath = current.bundleId,
                            frameIndex = frameIndex,
                            outputDir = outputDir,
                        ),
                    )
                }
                val summaryText = "PNG を書き出しました: #${res.report.frameIndex} (${res.report.width}x${res.report.height})"
                val completion = ExportCompletionInfo(
                    summaryText = summaryText,
                    outputPath = res.outputPath,
                    bundleId = null,
                    isTrimBundle = false,
                )
                updateViewerState { it.copy(exportCompletion = completion, lastActionMessage = summaryText) }
                ArbNotifications.notifyImageSaved(
                    project = project,
                    imagePath = res.outputPath,
                    width = res.report.width,
                    height = res.report.height,
                )
            } catch (t: Throwable) {
                logger.warn("ARB export.png failed", t)
                publishEdt {
                    if (!disposed) {
                        studioController.showError("PNG 書き出しに失敗しました: ${t.message}")
                    }
                }
                ArbNotifications.notifyActionFailed(project, "PNG 書き出し", t.message)
            }
        }
    }

    /** The newest IN/OUT/playhead the user chose; stays set until `edit.update_session` accepted it. */
    private class PendingEdit(val revision: Long, val bundleId: String, val range: StudioTrimRange, val playhead: Long)

    private val editPersistLock = Any()
    private var pendingEdit: PendingEdit? = null

    private class EditSessionWrites(val id: String) {
        val revision = java.util.concurrent.atomic.AtomicLong()
        var lastSubmitted = 0L
    }

    private fun persistEditSession(range: StudioTrimRange, playhead: Long) {
        val current = viewerState ?: return
        val session = editSession ?: return
        synchronized(editPersistLock) {
            pendingEdit = PendingEdit(session.revision.incrementAndGet(), current.bundleId, range, playhead)
        }
        flushEditSessionAsync()
    }

    internal fun flushBeforeClose(): Boolean {
        if (synchronized(editPersistLock) { pendingEdit == null }) return true
        return ProgressDaemonCallRunner(project).run { flushEditSession(closing = false) }
    }

    /** Re-sends the newest unsaved range (also the target of the "再保存" action). */
    private fun retryEditSave() {
        submitWork {
            try {
                flushEditSession(closing = false)
            } finally {
                publishEdt { if (!disposed) studioController.finishEditRetry() }
            }
        }
    }

    internal fun flushEditSessionAsync() {
        submitWork { flushEditSession(closing = false) }
    }

    /**
     * Sends the newest pending range. Returns `true` when nothing is left unsaved. A failure keeps
     * the range pending and surfaces the reason on the Studio (it is retried on the next range
     * change, on "再保存" and once more when the tab closes); success clears the failure marker.
     */
    internal fun flushEditSession(closing: Boolean): Boolean {
        val session = editSession ?: return true
        return synchronized(session) { flushEditSession(session, closing) }
    }

    private fun flushEditSession(session: EditSessionWrites, closing: Boolean): Boolean {
        val edit = synchronized(editPersistLock) { pendingEdit } ?: return true
        return try {
            val superseded = edit.revision < session.lastSubmitted
            if (!superseded) {
                session.lastSubmitted = edit.revision
                daemonService.withClient {
                    it.editUpdateSession(
                        EditUpdateSessionParams(
                            sessionId = session.id,
                            bundleId = edit.bundleId,
                            inFrameIndex = edit.range.inFrameIndex,
                            outFrameIndex = edit.range.outFrameIndex,
                            inTimeNs = edit.range.inTimeNs,
                            outTimeNs = edit.range.outTimeNs,
                            playheadFrameIndex = edit.playhead,
                        ),
                    )
                }
            }
            if (superseded && !closing) publishEdt {
                if (!disposed) studioController.showTimelineWarning("別のタブに新しい編集があるため、このタブの範囲は再保存しませんでした。")
            }
            val drained = synchronized(editPersistLock) {
                if (pendingEdit === edit) pendingEdit = null
                pendingEdit == null
            }
            if (drained && !closing) publishEdt { if (!disposed) studioController.markEditSaveFailure(null) }
            drained
        } catch (t: Throwable) {
            logger.warn("ARB edit.update_session failed (range stays unsaved)", t)
            if (!closing) {
                val reason = streamFailureReason(t)
                publishEdt { if (!disposed) studioController.markEditSaveFailure(reason) }
            }
            false
        }
    }

    override fun getComponent(): JComponent = rootComponent

    override fun getPreferredFocusedComponent(): JComponent =
        rootComponent.getComponent(0) as? JComponent ?: rootComponent

    override fun getName(): String = "ARB Studio"

    override fun setState(state: FileEditorState) = Unit

    override fun isModified(): Boolean = false

    override fun isValid(): Boolean = true

    override fun addPropertyChangeListener(listener: PropertyChangeListener) = Unit

    override fun removePropertyChangeListener(listener: PropertyChangeListener) = Unit

    override fun getCurrentLocation(): FileEditorLocation? = null

    override fun getFile(): VirtualFile = backingFile

    override fun deselectNotify() = studioController.cancelGesture()

    override fun dispose() {
        studioController.cancelGesture()
        val ctrl: PlaybackController?
        val lease: BundleLeaseController?
        synchronized(lifecycleLock) {
            disposed = true
            ctrl = ownedPlaybackController
            ownedPlaybackController = null
            attachedAudioPlayer = null
            lease = ownedLeaseController
            ownedLeaseController = null
        }
        // Graceful shutdown only: `shutdownNow()` would interrupt a worker blocked inside the
        // shared app-level IPC client, closing its interruptible SocketChannel for every ARB view.
        // Queued tasks bail out on `disposed` (see [submitWork]).
        worker.shutdown()
        timelineWorker.shutdown()
        audioDecodeWorker.shutdown()

        runOffEdt {
            runCatching { ctrl?.close() }
            // Last chance to persist an IN/OUT that is still unsaved; the editor UI is gone, so a
            // failure is reported with a notification instead of being dropped silently.
            val saved = runCatching { flushEditSession(closing = true) }.getOrDefault(false)
            if (!saved) {
                ArbNotifications.notifyWarning(
                    project,
                    "「$displayTitle」の編集範囲（IN/OUT）の保存結果を確認できません。次回開くと以前の範囲に戻る場合があります。",
                )
            }
            trimViewport.close()
            mediaSession.close()
            runCatching { lease?.close() }
        }
    }

    /** Submits [block] to the IPC worker unless disposed; never throws on a shut-down executor. */
    private fun submitWork(block: () -> Unit) {
        submitGuarded(worker) { if (!disposed) block() }
    }

    private fun runOffEdt(block: () -> Unit) {
        val app = ApplicationManager.getApplication()
        if (app != null) {
            app.executeOnPooledThread(block)
        } else {
            Thread(block, "arb-studio-off-edt").apply {
                isDaemon = true
                start()
            }
        }
    }

    private fun submitGuarded(executor: ExecutorService, task: () -> Unit) {
        try {
            executor.submit(task)
        } catch (_: RejectedExecutionException) {
            logger.debug("ARB studio worker already shut down; dropping task")
        }
    }

    internal companion object {
        private val sessions = java.util.WeakHashMap<String, java.lang.ref.WeakReference<EditSessionWrites>>()

        @Synchronized
        private fun writesFor(id: String): EditSessionWrites =
            sessions[id]?.get() ?: EditSessionWrites(id).also {
                sessions.remove(id)
                sessions[id] = java.lang.ref.WeakReference(it)
            }


        const val LOG_QUERY_LIMIT: Int = LOG_DISPLAY_LIMIT
        const val INITIAL_LOG_PAGE_SIZE: Int = 500
        // Matches `arbd`'s `RPC_ERR_RESPONSE_TOO_LARGE` structured error code (-32010).
        const val RPC_ERR_RESPONSE_TOO_LARGE: Int = -32010

        internal fun isResponseTooLargeError(e: ArbdRpcException): Boolean =
            e.code == RPC_ERR_RESPONSE_TOO_LARGE ||
                e.message.contains("IPC frame limit", ignoreCase = true) ||
                e.message.contains("exceeds the", ignoreCase = true)
    }
}
