package io.github.shinma06.arb.ide

import androidx.compose.ui.graphics.ImageBitmap
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.diagnostic.Logger
import com.intellij.openapi.fileChooser.FileChooserFactory
import com.intellij.openapi.fileChooser.FileSaverDescriptor
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
import io.github.shinma06.arb.model.TimeAlignment
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
import io.github.shinma06.arb.shm.ShmFrameRingReader
import io.github.shinma06.arb.state.ExportCompletionInfo
import io.github.shinma06.arb.state.TimelineViewerReducer
import io.github.shinma06.arb.state.TimelineViewerState
import io.github.shinma06.arb.state.TrimCloseState
import io.github.shinma06.arb.ui.ArbStudioContent
import io.github.shinma06.arb.ui.ArbStudioController
import io.github.shinma06.arb.ui.LatestValueMailbox
import io.github.shinma06.arb.ui.StudioExportFormat
import org.jetbrains.jewel.bridge.JewelComposePanel
import java.awt.BorderLayout
import java.beans.PropertyChangeListener
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.RejectedExecutionException
import javax.swing.JComponent
import javax.swing.JPanel
import javax.swing.SwingUtilities
import kotlin.math.roundToLong

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
 * - Guards `disposed` + `ownedViewportId` / `ownedLeaseController` / `ownedShmReader` /
 *   `ownedPlaybackController` atomically under [lifecycleLock] so closing the tab mid-`open_viewport`
 *   immediately closes the returned viewport off-EDT without leaking or killing shared IPC (M1).
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

    internal val studioController: ArbStudioController = ArbStudioController(
        onScrub = { frameIndex -> performScrub(frameIndex) },
        onSeekExact = { frameIndex -> performSeekExact(frameIndex) },
        onTrimExport = { inFrame, outFrame -> performExport(inFrame, outFrame, StudioExportFormat.SHARE_MP4) },
        onPngExport = { frameIndex, outputPath -> performPngExport(frameIndex, outputPath) },
        onRangeChanged = { inFrame, outFrame, playhead ->
            trimCloseState.onRangeAdjusted(inFrame, outFrame)
            state = state?.copy(inFrameIndex = inFrame, outFrameIndex = outFrame, currentFrameIndex = playhead)
            submitWork {
                runCatching {
                    val ctrl = currentPlaybackController()
                    ctrl?.setInFrame(inFrame)
                    ctrl?.setOutFrame(outFrame)
                }.onFailure { logger.debug("ARB range update failed", it) }
            }
            persistEditSession(inFrame, outFrame, playhead)
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
            state = state?.let { TimelineViewerReducer.setTrashOriginalAfterExport(it, enabled) }
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
        onExportRequested = { inFrame, outFrame, format ->
            performExport(inFrame, outFrame, format)
        },
        onChoosePngOutputPath = { bundleId, frameIndex, onChosen ->
            // Default the native IntelliJ save dialog to the same screenshot destination as the
            // Tool Window / Running Devices screenshot (Logcat inheritance or custom folder).
            runOffEdt {
                val baseDir = runCatching { ArbScreenshotDestinationResolver.resolveOutputDir(project) }
                    .getOrNull()
                    ?.let { runCatching { Path.of(it) }.getOrNull() }
                    ?.takeIf { Files.isDirectory(it) }
                publishEdt {
                    val p = project
                    if (disposed || p == null || p.isDisposed) return@publishEdt
                    val descriptor = FileSaverDescriptor("フレーム #$frameIndex の PNG 保存先を選択", "", "png")
                    val wrapper = FileChooserFactory.getInstance()
                        .createSaveFileDialog(descriptor, p)
                        .save(baseDir, "$bundleId-frame-$frameIndex.png")
                    wrapper?.let { onChosen(it.file.toPath()) }
                }
            }
        },
        onAppPackageFilterChanged = { packageName -> performAppPackageFilter(packageName) },
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
    private var ownedViewportId: String? = null
    private var ownedShmReader: ShmFrameRingReader? = null
    private var ownedPlaybackController: PlaybackController? = null

    @Volatile
    private var attachedAudioPlayer: AudioSyncPlayer? = null

    @Volatile
    private var state: TimelineViewerState? = null

    @Volatile
    private var editSessionId: String? = null

    @Volatile
    private var disposed = false

    @Volatile
    private var trashOriginalRequested = false

    init {
        loadBundleAsync()
    }

    fun hasUnexportedTrimRange(): Boolean = trimCloseState.hasUnexportedCustomTrim()

    fun currentTrimRange(): Pair<Long, Long> = trimCloseState.currentRange()

    internal fun playbackControllerForTest(): PlaybackController? = synchronized(lifecycleLock) {
        ownedPlaybackController
    }

    internal fun audioPlayerForTest(): AudioSyncPlayer? = attachedAudioPlayer

    private fun currentPlaybackController(): PlaybackController? = synchronized(lifecycleLock) {
        ownedPlaybackController
    }

    private fun currentShmReader(): ShmFrameRingReader? = synchronized(lifecycleLock) {
        ownedShmReader
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
                editSessionId = sessionId

                val existingSession = runCatching {
                    daemonService.withClient { it.editGetSession(sessionId) }
                }.getOrNull()

                // Phase 1 (M2): Open video viewport FIRST before querying logs/events so video
                // displays immediately even when log bundles are huge or slow.
                var viewportId: String? = null
                var frameCount = opened.summary.frameCount.toLong()
                var framePtsNs: List<Long> = emptyList()

                if (opened.summary.videoPath != null && frameCount > 0L) {
                    val vp = daemonService.withClient { it.mediaOpenViewport(opened.bundleId) }
                    // M1: atomically check disposed vs register ownedViewportId. If disposed
                    // raced while media.open_viewport was in flight, close it immediately off-EDT!
                    val viewportRegistered = synchronized(lifecycleLock) {
                        if (disposed) {
                            false
                        } else {
                            ownedViewportId = vp.viewportId
                            true
                        }
                    }
                    if (!viewportRegistered) {
                        runCatching { daemonService.withClient { it.mediaCloseViewport(vp.viewportId) } }
                        return@submitWork
                    }
                    viewportId = vp.viewportId
                    frameCount = vp.frameCount
                    framePtsNs = vp.framePtsNs

                    val reader = ShmFrameRingReader.open(Path.of(vp.shmPath))
                    val readerRegistered = synchronized(lifecycleLock) {
                        if (disposed) {
                            false
                        } else {
                            ownedShmReader = reader
                            true
                        }
                    }
                    if (!readerRegistered) {
                        runCatching { reader.close() }
                        return@submitWork
                    }
                }

                val initialIn = existingSession?.inFrameIndex ?: 0L
                val initialOut = existingSession?.outFrameIndex ?: (frameCount - 1L).coerceAtLeast(0L)
                val initialPlayhead = existingSession?.playheadFrameIndex ?: 0L
                trimCloseState.initialize(frameCount, initialIn, initialOut)

                val initialState = TimelineViewerState(
                    bundleId = opened.bundleId,
                    path = opened.path,
                    format = opened.format,
                    isLegacy = opened.isLegacy,
                    leaseId = opened.leaseId,
                    viewportId = viewportId,
                    summary = opened.summary,
                    frameCount = frameCount,
                    currentFrameIndex = initialPlayhead,
                    inFrameIndex = initialIn,
                    outFrameIndex = initialOut,
                )
                state = initialState

                val fallbackAvOffsetMs = opened.summary.streams.audio?.avOffsetMs ?: 0L
                val hasAudioTrack = opened.summary.audioPath != null ||
                    opened.summary.streams.audio?.status in setOf(StreamStatus.PRESENT, StreamStatus.PARTIAL)

                var activeCtrl: PlaybackController? = null
                if (viewportId != null && frameCount > 0L) {
                    val openedVpId = viewportId
                    val seeker = object : FrameSeeker {
                        override fun seekExact(frameIndex: Long): FrameSeekResult {
                            val resp = daemonService.withClient { it.mediaSeekFrame(openedVpId, frameIndex) }
                            readShmAndRender()
                            return FrameSeekResult(frameIndex, resp.frameIndex, resp.isApproximate)
                        }

                        override fun scrubApprox(frameIndex: Long): FrameSeekResult {
                            val resp = daemonService.withClient { it.mediaScrub(openedVpId, frameIndex) }
                            readShmAndRender()
                            return FrameSeekResult(resp.requestedFrameIndex, resp.displayedFrameIndex, resp.isApproximate)
                        }
                    }
                    // H8: Pass real per-frame VFR presentation timestamps from `media.open_viewport`
                    // into PlaybackController so play, seek, and loop wrap share the exact PTS map.
                    val ctrl = PlaybackController(
                        totalFrames = frameCount,
                        frameSeeker = seeker,
                        framePtsNs = framePtsNs,
                        fallbackFps = initialState.estimatedFps,
                        audioPlayer = null,
                        clock = playbackClock,
                        initialFrameIndex = initialPlayhead,
                        initialInFrameIndex = initialIn,
                        initialOutFrameIndex = initialOut,
                        initialAvOffsetMs = fallbackAvOffsetMs,
                        autoRunLoop = autoRunPlaybackLoop,
                        onPlaybackError = { err -> logger.debug("ARB playback error", err) },
                    )
                    ctrl.addStateListener { pb ->
                        publishEdt {
                            if (!disposed) {
                                studioController.renderPlaybackState(pb)
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

                publishEdt {
                    if (!disposed) {
                        studioController.renderState(initialState, image = null)
                        activeCtrl?.state?.let(studioController::renderPlaybackState)
                    }
                }

                // Render initial frame right away before decoupled timeline/log pagination starts!
                if (activeCtrl != null && !disposed) {
                    runCatching { activeCtrl.seekToFrame(initialPlayhead) }
                        .onFailure { t ->
                            logger.warn("ARB initial seek_frame failed", t)
                            if (!disposed) {
                                publishEdt {
                                    studioController.showError("フレームシークに失敗しました: ${t.message}")
                                }
                            }
                        }
                }

                // Phase 2 (M2): Decoupled background loading of filmstrip, events, and paged logs.
                if (!disposed) {
                    loadTimelineDataAsync(opened.bundleId, viewportId)
                }
            } catch (t: Throwable) {
                if (disposed) return@submitWork
                logger.warn("Failed to open ARB bundle $bundleIdOrPath", t)
                publishEdt {
                    if (!disposed) {
                        studioController.showError("バンドルを開けませんでした: ${t.message}")
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

            if (viewportId != null) {
                val filmstrip = runCatching {
                    daemonService.withClient { it.mediaFilmstrip(viewportId) }.entries
                }.getOrDefault(emptyList())
                if (!disposed && filmstrip.isNotEmpty()) {
                    state = state?.copy(filmstrip = filmstrip)
                    publishEdt {
                        if (!disposed) {
                            studioController.updateState { it.copy(filmstrip = filmstrip) }
                        }
                    }
                }
            }
            if (disposed) return@submitGuarded

            val discontinuities = runCatching {
                daemonService.withClient { it.timelineDiscontinuities(bundleId).discontinuities }
            }.getOrDefault(emptyList())
            val crashes = runCatching {
                daemonService.withClient { it.timelineCrashes(bundleId).crashes }
            }.getOrDefault(emptyList())
            val screens = runCatching {
                daemonService.withClient { it.timelineScreens(bundleId).screens }
            }.getOrDefault(emptyList())
            val mainTaps = runCatching {
                daemonService.withClient { it.timelineQueryTaps(bundleId, bucket = QueryBucket.MAIN, limit = LOG_QUERY_LIMIT).taps }
            }.getOrDefault(emptyList())
            val unanchoredTaps = runCatching {
                daemonService.withClient { it.timelineQueryTaps(bundleId, bucket = QueryBucket.UNANCHORED, limit = LOG_QUERY_LIMIT).taps }
            }.getOrDefault(emptyList())

            if (disposed) return@submitGuarded
            state = state?.let {
                TimelineViewerReducer.withBucketedRecords(
                    state = it.copy(
                        discontinuities = discontinuities,
                        crashes = crashes,
                        screens = screens,
                    ),
                    mainLogs = it.mainLogs,
                    unanchoredLogs = it.unanchoredLogs,
                    mainTaps = mainTaps,
                    unanchoredTaps = unanchoredTaps,
                )
            }
            publishEdt {
                if (!disposed) {
                    studioController.updateState {
                        TimelineViewerReducer.withBucketedRecords(
                            state = it.copy(
                                discontinuities = discontinuities,
                                crashes = crashes,
                                screens = screens,
                            ),
                            mainLogs = it.mainLogs,
                            unanchoredLogs = it.unanchoredLogs,
                            mainTaps = mainTaps,
                            unanchoredTaps = unanchoredTaps,
                        )
                    }
                }
            }

            try {
                val mainLogs = queryLogsPaged(
                    bundleId = bundleId,
                    bucket = QueryBucket.MAIN,
                    maxLogs = LOG_QUERY_LIMIT,
                )
                if (disposed) return@submitGuarded
                val unanchoredLogs = queryLogsPaged(
                    bundleId = bundleId,
                    bucket = QueryBucket.UNANCHORED,
                    maxLogs = LOG_QUERY_LIMIT,
                )
                if (disposed) return@submitGuarded

                state = state?.let {
                    TimelineViewerReducer.withBucketedRecords(
                        state = it,
                        mainLogs = mainLogs,
                        unanchoredLogs = unanchoredLogs,
                        mainTaps = it.mainTaps,
                        unanchoredTaps = it.unanchoredTaps,
                    )
                }
                publishEdt {
                    if (!disposed) {
                        studioController.updateState {
                            TimelineViewerReducer.withBucketedRecords(
                                state = it,
                                mainLogs = mainLogs,
                                unanchoredLogs = unanchoredLogs,
                                mainTaps = it.mainTaps,
                                unanchoredTaps = it.unanchoredTaps,
                            )
                        }
                    }
                }
            } catch (t: Throwable) {
                if (disposed) return@submitGuarded
                logger.warn("ARB paged log load failed for $bundleId (video remains active)", t)
                publishEdt {
                    if (!disposed) {
                        studioController.showError("ログの読み込みに失敗しました（映像再生は継続できます）: ${t.message}")
                    }
                }
            }
        }
    }

    /**
     * Fetches up to [maxLogs] log rows in bounded pages without gaps or duplicates (M2).
     *
     * `timeline.query_logs` filters by the row timestamp (`COALESCE(aligned_boottime_ns,
     * pc_ingest_cont_ns)`, inclusive `start_ns`/`end_ns`) but orders and truncates by `seq`, so a
     * "last timestamp" cursor would silently skip rows whose `seq` is later but whose timestamp is
     * earlier than the cursor. Instead each request covers a disjoint closed time window and asks for
     * `pageSize + 1` rows: a window that returns at most `pageSize` rows is complete; a truncated
     * window is split (guided by the returned timestamps) into strictly smaller disjoint sub-windows,
     * earliest first. A window rejected by `arbd`'s 4 MiB IPC frame limit is retried with a halved
     * page size. Results are merged by `seq` and returned in `seq` order.
     */
    internal fun queryLogsPaged(
        bundleId: String,
        bucket: QueryBucket,
        maxLogs: Int = LOG_QUERY_LIMIT,
        packageName: String? = null,
    ): List<LogRecord> {
        val cap = maxLogs.coerceAtLeast(1)
        val collected = LinkedHashMap<Long, LogRecord>()
        var pageSize = initialLogPageSize.coerceIn(1, cap)
        // Depth-first, earliest window first: the deque head is the next window to fetch.
        val pending = ArrayDeque<LogWindow>()
        pending.addFirst(LogWindow(Long.MIN_VALUE, Long.MAX_VALUE))
        while (pending.isNotEmpty() && collected.size < cap && !disposed) {
            val window = pending.first()
            val limit = if (pageSize == 1) 1 else pageSize + 1
            val page = try {
                daemonService.withClient {
                    it.timelineQueryLogs(
                        bundleIdOrPath = bundleId,
                        bucket = bucket,
                        startNs = window.startNs.takeIf { s -> s != Long.MIN_VALUE },
                        endNs = window.endNs.takeIf { e -> e != Long.MAX_VALUE },
                        limit = limit,
                        afterSeq = window.afterSeq,
                        packageName = packageName,
                    ).logs
                }
            } catch (e: ArbdRpcException) {
                if (!isResponseTooLargeError(e) || pageSize == 1) throw e
                pageSize = (pageSize / 2).coerceAtLeast(1)
                continue
            }
            pending.removeFirst()

            check(window.afterSeq == null || page.all { it.seq > window.afterSeq }) {
                "ログのページ取得を続けられません。デーモンを更新してください。"
            }
            if (page.size < limit) {
                for (record in page) collected.putIfAbsent(record.seq, record)
                continue
            }
            val parts = splitLogWindow(window, page.map(::logCursorTimestampNs))
            if (parts.isNotEmpty()) {
                for (part in parts.asReversed()) pending.addFirst(part)
                continue
            }
            // The window is a single timestamp. Continue in the daemon's stable sequence order.
            for (record in page.take(cap - collected.size)) collected.putIfAbsent(record.seq, record)
            pending.addFirst(window.copy(afterSeq = page.last().seq))
        }

        val rows = collected.values.toList()
        val kept = if (rows.size <= cap) {
            rows
        } else {
            // Windows complete earliest-first, so keep the earliest rows by timestamp.
            rows.sortedWith(compareBy<LogRecord>({ logCursorTimestampNs(it) }, { it.seq })).take(cap)
        }
        return kept.sortedBy { it.seq }
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
            attachedAudioPlayer = player
            controller.attachAudioPlayer(player, effectiveAvOffsetMs)
        }
        try {
            audioDecodeWorker.submit(task)
        } catch (_: RejectedExecutionException) {
            logger.debug("ARB audio decode worker already shut down; skipping audio attach")
        }
    }

    private fun performScrub(frameIndex: Long) {
        val ctrl = currentPlaybackController()
        if (ctrl != null) {
            submitWork {
                runCatching { ctrl.scrubTo(frameIndex) }
                    .onFailure { logger.debug("ARB scrub failed", it) }
            }
            return
        }
        val vpId = state?.viewportId ?: return
        submitWork {
            runCatching {
                daemonService.withClient { it.mediaScrub(vpId, frameIndex) }
                readShmAndRender()
            }.onFailure { logger.debug("ARB scrub failed", it) }
        }
    }

    private fun performSeekExact(frameIndex: Long) {
        val ctrl = currentPlaybackController()
        if (ctrl != null) {
            submitWork {
                runCatching { ctrl.seekToFrame(frameIndex) }
                    .onFailure { t ->
                        logger.warn("ARB seek_frame failed", t)
                        publishEdt {
                            if (!disposed) {
                                studioController.showError("フレームシークに失敗しました: ${t.message}")
                            }
                        }
                    }
            }
            return
        }
        val vpId = state?.viewportId ?: return
        submitWork {
            try {
                daemonService.withClient { it.mediaSeekFrame(vpId, frameIndex) }
                readShmAndRender()
            } catch (t: Throwable) {
                logger.warn("ARB seek_frame failed", t)
                publishEdt {
                    if (!disposed) {
                        studioController.showError("フレームシークに失敗しました: ${t.message}")
                    }
                }
            }
        }
    }

    private fun applySelectTapRange() {
        val current = state ?: return
        val tapFrames = current.mainTapFrameIndices()
        if (tapFrames.isEmpty()) return
        val fps = current.estimatedFps
        val padBefore = (fps * 1.0).roundToLong().coerceAtLeast(1L)
        val padAfter = (fps * 2.0).roundToLong().coerceAtLeast(1L)
        submitWork {
            val ctrl = currentPlaybackController()
            if (ctrl != null && ctrl.selectTapRange(tapFrames, padBefore, padAfter)) {
                val pb = ctrl.state
                trimCloseState.onRangeAdjusted(pb.inFrameIndex, pb.outFrameIndex)
                val updated = (state ?: current).copy(inFrameIndex = pb.inFrameIndex, outFrameIndex = pb.outFrameIndex)
                state = updated
                persistEditSession(pb.inFrameIndex, pb.outFrameIndex, pb.currentFrameIndex)
                publishEdt {
                    if (!disposed) {
                        studioController.updateState { it.copy(inFrameIndex = pb.inFrameIndex, outFrameIndex = pb.outFrameIndex) }
                    }
                }
            }
        }
    }

    private fun applySelectAroundCrash() {
        val current = state ?: return
        val crashFrame = current.primaryCrashFrameIndex() ?: return
        val fps = current.estimatedFps
        val before = (fps * 5.0).roundToLong().coerceAtLeast(1L)
        val after = (fps * 2.0).roundToLong().coerceAtLeast(1L)
        submitWork {
            val ctrl = currentPlaybackController()
            if (ctrl != null) {
                ctrl.selectAroundCrash(crashFrame, before, after)
                val pb = ctrl.state
                trimCloseState.onRangeAdjusted(pb.inFrameIndex, pb.outFrameIndex)
                val updated = (state ?: current).copy(
                    inFrameIndex = pb.inFrameIndex,
                    outFrameIndex = pb.outFrameIndex,
                    currentFrameIndex = pb.currentFrameIndex,
                )
                state = updated
                persistEditSession(pb.inFrameIndex, pb.outFrameIndex, pb.currentFrameIndex)
                publishEdt {
                    if (!disposed) {
                        studioController.updateState {
                            it.copy(
                                inFrameIndex = pb.inFrameIndex,
                                outFrameIndex = pb.outFrameIndex,
                                currentFrameIndex = pb.currentFrameIndex,
                            )
                        }
                    }
                }
            }
        }
    }

    private fun applyResetFullRange() {
        val current = state ?: return
        submitWork {
            currentPlaybackController()?.resetFullRange()
            val maxF = (current.frameCount - 1L).coerceAtLeast(0L)
            trimCloseState.onRangeAdjusted(0L, maxF)
            val updated = (state ?: current).copy(inFrameIndex = 0L, outFrameIndex = maxF)
            state = updated
            persistEditSession(0L, maxF, updated.currentFrameIndex)
            publishEdt {
                if (!disposed) {
                    studioController.updateState { it.copy(inFrameIndex = 0L, outFrameIndex = maxF) }
                }
            }
        }
    }

    private fun readShmAndRender() {
        val reader = currentShmReader() ?: return
        val frame = reader.readLatest() ?: return
        val image = frame.toBufferedImage()
        val prev = state ?: return
        state = TimelineViewerReducer.onFramePublished(prev, frame.meta)
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
        val current = state ?: return
        submitGuarded(timelineWorker) {
            if (disposed) return@submitGuarded
            try {
                val mainLogs = queryLogsPaged(
                    bundleId = current.bundleId,
                    bucket = QueryBucket.MAIN,
                    maxLogs = LOG_QUERY_LIMIT,
                    packageName = packageName,
                )
                if (disposed) return@submitGuarded
                val unanchoredLogs = queryLogsPaged(
                    bundleId = current.bundleId,
                    bucket = QueryBucket.UNANCHORED,
                    maxLogs = LOG_QUERY_LIMIT,
                    packageName = packageName,
                )
                if (disposed) return@submitGuarded
                state = state?.let {
                    TimelineViewerReducer.withBucketedRecords(it, mainLogs, unanchoredLogs, it.mainTaps, it.unanchoredTaps)
                }
                publishEdt {
                    if (!disposed) {
                        studioController.applyAppPackageLogs(packageName, mainLogs, unanchoredLogs)
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
        inFrameIndex: Long,
        outFrameIndex: Long,
        format: StudioExportFormat,
    ) {
        val current = state ?: return
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
                            inFrameIndex = inFrameIndex,
                            outFrameIndex = outFrameIndex,
                            burnTaps = burnTapsEnabled,
                            includeShareMp4 = includeShareMp4,
                            trashOriginal = shouldTrashOriginal,
                            holderPid = ProcessHandle.current().pid(),
                        ),
                    )
                }
                trimCloseState.markExported(inFrameIndex, outFrameIndex)
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
                val updated = (state ?: current).copy(
                    exportCompletion = completion,
                    lastActionMessage = summaryText,
                )
                state = updated
                publishEdt {
                    if (!disposed) {
                        studioController.setExporting(false)
                        studioController.updateState {
                            it.copy(exportCompletion = completion, lastActionMessage = summaryText)
                        }
                    }
                }
                ArbNotifications.notifyExportCompleted(
                    project = project,
                    daemonService = daemonService,
                    summaryText = summaryText,
                    outputPath = res.bundlePath,
                    trimmedBundleId = res.bundleId,
                    originalTrashedBundleId = trashedBundleId,
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

    private fun performPngExport(frameIndex: Long, outputPath: Path) {
        val current = state ?: return
        submitWork {
            try {
                val res = daemonService.withClient {
                    it.exportPng(
                        ExportPngParams(
                            bundleIdOrPath = current.bundleId,
                            frameIndex = frameIndex,
                            outputPath = outputPath.toAbsolutePath().toString(),
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
                val updated = (state ?: current).copy(
                    exportCompletion = completion,
                    lastActionMessage = summaryText,
                )
                state = updated
                publishEdt {
                    if (!disposed) {
                        studioController.updateState {
                            it.copy(exportCompletion = completion, lastActionMessage = summaryText)
                        }
                    }
                }
                ArbNotifications.notifyImageSaved(
                    project = project,
                    imagePath = res.outputPath,
                    width = res.report.width,
                    height = res.report.height,
                    kindLabel = "フレーム画像",
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

    private fun persistEditSession(inFrame: Long, outFrame: Long, playhead: Long) {
        val current = state ?: return
        val sessionId = editSessionId ?: return
        submitWork {
            runCatching {
                daemonService.withClient {
                    it.editUpdateSession(
                        EditUpdateSessionParams(
                            sessionId = sessionId,
                            bundleId = current.bundleId,
                            inFrameIndex = inFrame,
                            outFrameIndex = outFrame,
                            playheadFrameIndex = playhead,
                        ),
                    )
                }
            }
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

    override fun dispose() {
        val ctrl: PlaybackController?
        val reader: ShmFrameRingReader?
        val vpId: String?
        val lease: BundleLeaseController?
        synchronized(lifecycleLock) {
            disposed = true
            ctrl = ownedPlaybackController
            ownedPlaybackController = null
            reader = ownedShmReader
            ownedShmReader = null
            vpId = ownedViewportId
            ownedViewportId = null
            lease = ownedLeaseController
            ownedLeaseController = null
        }
        // Graceful shutdown only: `shutdownNow()` would interrupt a worker blocked inside the
        // shared app-level IPC client, closing its interruptible SocketChannel for every ARB view.
        // Queued tasks bail out on `disposed` (see [submitWork]).
        worker.shutdown()
        timelineWorker.shutdown()
        audioDecodeWorker.shutdown()

        if (ctrl != null || reader != null || vpId != null || lease != null) {
            runOffEdt {
                runCatching { ctrl?.close() }
                runCatching { reader?.close() }
                vpId?.let { id ->
                    runCatching { daemonService.withClient { it.mediaCloseViewport(id) } }
                }
                runCatching { lease?.close() }
            }
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
        const val LOG_QUERY_LIMIT: Int = 5_000
        const val INITIAL_LOG_PAGE_SIZE: Int = 500
        // Matches `arbd`'s `RPC_ERR_RESPONSE_TOO_LARGE` structured error code (-32010).
        const val RPC_ERR_RESPONSE_TOO_LARGE: Int = -32010

        /** Inclusive timestamp bounds and an exclusive sequence cursor for timestamp ties. */
        internal data class LogWindow(
            val startNs: Long,
            val endNs: Long,
            val afterSeq: Long? = null,
        )

        /**
         * Timestamp `timeline.query_logs` filters on: `COALESCE(aligned_boottime_ns, pc_ingest_cont_ns)`
         * where `aligned_boottime_ns` is the anchored boottime or the bounded minimum.
         */
        internal fun logCursorTimestampNs(record: LogRecord): Long = when (val a = record.alignment) {
            is TimeAlignment.Anchored -> a.boottimeNs
            is TimeAlignment.Bounded -> a.minBoottimeNs
            is TimeAlignment.Unanchored -> record.pcIngestContNs
        }

        /**
         * Splits a truncated [window] into strictly smaller, disjoint, ordered sub-windows covering it,
         * guided by the timestamps of the rows it returned. Returns an empty list when the window is a
         * single timestamp and cannot be narrowed.
         */
        internal fun splitLogWindow(window: LogWindow, returnedTs: List<Long>): List<LogWindow> {
            val lo = window.startNs
            val hi = window.endNs
            if (lo >= hi) return emptyList()
            val ts = returnedTs.filter { it in lo..hi }.sorted()
            if (ts.isEmpty()) {
                // Timestamps disagree with the daemon's filter key: fall back to a plain midpoint split.
                val mid = (lo shr 1) + (hi shr 1) + (lo and hi and 1L)
                return listOf(LogWindow(lo, mid), LogWindow(mid + 1, hi))
            }
            val min = ts.first()
            val max = ts.last()
            if (min == max) {
                return buildList {
                    if (min > lo) add(LogWindow(lo, min - 1))
                    add(LogWindow(min, min))
                    if (min < hi) add(LogWindow(min + 1, hi))
                }
            }
            // Split at the 3/4 quantile so the left part usually fits in one page even when `seq`
            // order and timestamp order disagree slightly; never at `max`, so both parts shrink.
            var cut = ts[(ts.size * 3) / 4]
            if (cut >= max) cut = ts.last { it < max }
            return listOf(LogWindow(lo, cut), LogWindow(cut + 1, hi))
        }

        internal fun isResponseTooLargeError(e: ArbdRpcException): Boolean =
            e.code == RPC_ERR_RESPONSE_TOO_LARGE ||
                e.message.contains("IPC frame limit", ignoreCase = true) ||
                e.message.contains("exceeds the", ignoreCase = true)
    }
}
