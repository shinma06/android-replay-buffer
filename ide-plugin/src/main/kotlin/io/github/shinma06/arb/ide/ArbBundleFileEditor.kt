package io.github.shinma06.arb.ide

import com.intellij.ide.actions.RevealFileAction
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
import io.github.shinma06.arb.model.EditUpdateSessionParams
import io.github.shinma06.arb.model.ExportPngParams
import io.github.shinma06.arb.model.ExportShareMp4Params
import io.github.shinma06.arb.model.ExportTrimParams
import io.github.shinma06.arb.model.LeaseHolderKind
import io.github.shinma06.arb.model.QueryBucket
import io.github.shinma06.arb.model.StreamStatus
import io.github.shinma06.arb.playback.AudioSyncPlayer
import io.github.shinma06.arb.playback.FrameSeekResult
import io.github.shinma06.arb.playback.FrameSeeker
import io.github.shinma06.arb.playback.PlaybackController
import io.github.shinma06.arb.playback.PlaybackSpeed
import io.github.shinma06.arb.playback.WavFilePcmSource
import io.github.shinma06.arb.shm.ShmFrameRingReader
import io.github.shinma06.arb.state.ArbFormatters
import io.github.shinma06.arb.state.ExportCompletionInfo
import io.github.shinma06.arb.state.TimelineViewerReducer
import io.github.shinma06.arb.state.TimelineViewerState
import io.github.shinma06.arb.state.TrimCloseState
import io.github.shinma06.arb.ui.ArbStudioContent
import io.github.shinma06.arb.ui.ArbStudioController
import io.github.shinma06.arb.ui.StudioExportFormat
import org.jetbrains.jewel.bridge.JewelComposePanel
import java.awt.BorderLayout
import java.beans.PropertyChangeListener
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.Executors
import javax.swing.JComponent
import javax.swing.JFileChooser
import javax.swing.JPanel
import javax.swing.SwingUtilities
import kotlin.math.roundToLong

/**
 * Thin IntelliJ [FileEditor] for the Integrated Viewer & Trim Studio (v2 and Legacy RO bundles),
 * rendered natively with JetBrains Jewel Compose (`JewelComposePanel`).
 *
 * Lifecycle & safety guarantees:
 * - Acquires a Viewer Lease via `bundle.open(holder_kind = ide_viewer)` and renews it periodically.
 * - Attaches to `arbd`'s SHM triple-buffered viewport via [ShmFrameRingReader].
 * - Drives continuous playback & deterministic range presets via T-3's [PlaybackController], and
 *   asynchronously decodes `audio.m4a` via `media.decode_audio_pcm` -> [WavFilePcmSource] ->
 *   [AudioSyncPlayer] with real `av_offset_ms` (gracefully degrading when audio/hardware is absent).
 * - Exports standalone faststart zero-`elst` H.264+AAC `share.mp4` (+ optional trimmed WORM bundle)
 *   via `export.share_mp4`, honoring `trash_original` ONLY on non-Legacy bundles.
 * - Tracks unexported custom trim bounds in [TrimCloseState] for [ArbBundlePreCloseCheck].
 * - On [dispose], closes [PlaybackController], unmaps SHM, closes viewport, and releases the lease off EDT.
 */
class ArbBundleFileEditor(
    private val project: Project,
    private val backingFile: VirtualFile,
    private val bundleIdOrPath: String,
    private val displayTitle: String,
    private val daemonService: ArbdDaemonService,
    private val sourceJumpResolver: SourceJumpResolver = IntelliJSourceJumpResolver(project),
) : UserDataHolderBase(), FileEditor {
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
    private val worker = Executors.newSingleThreadExecutor { r ->
        Thread(r, "arb-studio-$displayTitle").apply { isDaemon = true }
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
            worker.submit {
                runCatching {
                    playbackController?.setInFrame(inFrame)
                    playbackController?.setOutFrame(outFrame)
                }.onFailure { logger.debug("ARB range update failed", it) }
            }
            persistEditSession(inFrame, outFrame, playhead)
        },
        onPlayPauseToggle = {
            worker.submit {
                runCatching { playbackController?.togglePlayPause() }
                    .onFailure { logger.debug("ARB play/pause failed", it) }
            }
        },
        onStepFrame = { delta ->
            worker.submit {
                runCatching { playbackController?.stepFrame(delta) }
                    .onFailure { logger.debug("ARB stepFrame failed", it) }
            }
        },
        onSpeedChange = { speed: PlaybackSpeed ->
            worker.submit {
                runCatching { playbackController?.setSpeed(speed) }
            }
        },
        onMuteToggle = {
            worker.submit {
                runCatching { playbackController?.toggleMute() }
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
            runCatching { RevealFileAction.openFile(Path.of(path)) }
                .onFailure { logger.warn("Failed to reveal $path", it) }
        },
        onOpenExportedBundle = { exportedIdOrPath, title ->
            SwingUtilities.invokeLater {
                if (!project.isDisposed) {
                    val vf = ArbBundleVirtualFile(exportedIdOrPath, title, isLegacyHint = false)
                    FileEditorManager.getInstance(project).openFile(vf, true)
                }
            }
        },
        onSourceJump = { target ->
            val jumped = sourceJumpResolver.resolveAndNavigate(target)
            if (!jumped) {
                SwingUtilities.invokeLater {
                    studioController.showError("プロジェクト内に該当ソースが見つかりませんでした: ${target.displayLabel}")
                }
            }
        },
        onExportRequested = { inFrame, outFrame, format ->
            performExport(inFrame, outFrame, format)
        },
        onChoosePngOutputPath = { bundleId, frameIndex, onChosen ->
            SwingUtilities.invokeLater {
                val chooser = JFileChooser().apply {
                    dialogTitle = "フレーム #$frameIndex の PNG 保存先を選択"
                    selectedFile = java.io.File("$bundleId-frame-$frameIndex.png")
                }
                if (chooser.showSaveDialog(rootComponent) == JFileChooser.APPROVE_OPTION) {
                    onChosen(chooser.selectedFile.toPath())
                }
            }
        },
    )

    private val rootComponent: JPanel = JPanel(BorderLayout()).apply {
        add(JewelComposePanel { ArbStudioContent(studioController) }, BorderLayout.CENTER)
    }

    @Volatile
    private var leaseController: BundleLeaseController? = null

    @Volatile
    private var shmReader: ShmFrameRingReader? = null

    @Volatile
    private var playbackController: PlaybackController? = null

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

    private fun loadBundleAsync() {
        worker.submit {
            try {
                val opened = daemonService.withClient { rpc ->
                    rpc.bundleOpen(
                        bundleIdOrPath = bundleIdOrPath,
                        holderKind = LeaseHolderKind.IDE_VIEWER,
                        holderPid = ProcessHandle.current().pid(),
                    )
                }

                val openedViewportId = java.util.concurrent.atomic.AtomicReference<String?>(null)
                leaseController = BundleLeaseController(
                    leaseId = opened.leaseId,
                    renewAction = { id -> daemonService.withClient { it.bundleLeaseRenew(id) } },
                    closeLeaseAction = { id -> daemonService.withClient { it.bundleClose(id) } },
                    closeViewportAction = {
                        openedViewportId.get()?.let { vpId -> daemonService.withClient { it.mediaCloseViewport(vpId) } }
                    },
                    onHeartbeatError = { err -> logger.warn("ARB lease heartbeat failed", err) },
                )
                if (disposed) {
                    releaseResources()
                    return@submit
                }

                val sessionId = "ide-${opened.bundleId}"
                editSessionId = sessionId

                val existingSession = runCatching {
                    daemonService.withClient { it.editGetSession(sessionId) }
                }.getOrNull()

                val discontinuities = daemonService.withClient {
                    it.timelineDiscontinuities(opened.bundleId).discontinuities
                }
                val crashes = runCatching {
                    daemonService.withClient { it.timelineCrashes(opened.bundleId).crashes }
                }.getOrDefault(emptyList())
                val screens = runCatching {
                    daemonService.withClient { it.timelineScreens(opened.bundleId).screens }
                }.getOrDefault(emptyList())
                val mainLogs = daemonService.withClient {
                    it.timelineQueryLogs(opened.bundleId, bucket = QueryBucket.MAIN, limit = 5_000).logs
                }
                val unanchoredLogs = daemonService.withClient {
                    it.timelineQueryLogs(opened.bundleId, bucket = QueryBucket.UNANCHORED, limit = 5_000).logs
                }
                val mainTaps = daemonService.withClient {
                    it.timelineQueryTaps(opened.bundleId, bucket = QueryBucket.MAIN, limit = 5_000).taps
                }
                val unanchoredTaps = daemonService.withClient {
                    it.timelineQueryTaps(opened.bundleId, bucket = QueryBucket.UNANCHORED, limit = 5_000).taps
                }

                var viewportId: String? = null
                var frameCount = opened.summary.frameCount.toLong()
                var filmstrip = emptyList<io.github.shinma06.arb.model.FilmstripEntryMeta>()

                if (opened.summary.videoPath != null && frameCount > 0L) {
                    val vp = daemonService.withClient { it.mediaOpenViewport(opened.bundleId) }
                    viewportId = vp.viewportId
                    openedViewportId.set(vp.viewportId)
                    frameCount = vp.frameCount
                    shmReader = ShmFrameRingReader.open(Path.of(vp.shmPath))
                    if (disposed) {
                        releaseResources()
                        return@submit
                    }
                    filmstrip = runCatching {
                        daemonService.withClient { it.mediaFilmstrip(vp.viewportId) }.entries
                    }.getOrDefault(emptyList())
                }

                val initialIn = existingSession?.inFrameIndex ?: 0L
                val initialOut = existingSession?.outFrameIndex ?: (frameCount - 1L).coerceAtLeast(0L)
                val initialPlayhead = existingSession?.playheadFrameIndex ?: 0L
                trimCloseState.initialize(frameCount, initialIn, initialOut)

                val initialState = TimelineViewerReducer.withBucketedRecords(
                    state = TimelineViewerState(
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
                        filmstrip = filmstrip,
                        discontinuities = discontinuities,
                        crashes = crashes,
                        screens = screens,
                    ),
                    mainLogs = mainLogs,
                    unanchoredLogs = unanchoredLogs,
                    mainTaps = mainTaps,
                    unanchoredTaps = unanchoredTaps,
                )
                state = initialState

                val fallbackAvOffsetMs = opened.summary.streams.audio?.avOffsetMs ?: 0L
                val hasAudioTrack = opened.summary.audioPath != null ||
                    opened.summary.streams.audio?.status in setOf(StreamStatus.PRESENT, StreamStatus.PARTIAL)

                if (viewportId != null && frameCount > 0L) {
                    val seeker = object : FrameSeeker {
                        override fun seekExact(frameIndex: Long): FrameSeekResult {
                            val resp = daemonService.withClient { it.mediaSeekFrame(viewportId, frameIndex) }
                            readShmAndRender()
                            return FrameSeekResult(frameIndex, resp.frameIndex, resp.isApproximate)
                        }

                        override fun scrubApprox(frameIndex: Long): FrameSeekResult {
                            val resp = daemonService.withClient { it.mediaScrub(viewportId, frameIndex) }
                            readShmAndRender()
                            return FrameSeekResult(resp.requestedFrameIndex, resp.displayedFrameIndex, resp.isApproximate)
                        }
                    }
                    val ctrl = PlaybackController(
                        totalFrames = frameCount,
                        frameSeeker = seeker,
                        fallbackFps = initialState.estimatedFps,
                        audioPlayer = null,
                        initialFrameIndex = initialPlayhead,
                        initialInFrameIndex = initialIn,
                        initialOutFrameIndex = initialOut,
                        initialAvOffsetMs = fallbackAvOffsetMs,
                        onPlaybackError = { err -> logger.debug("ARB playback error", err) },
                    )
                    ctrl.addStateListener { pb ->
                        SwingUtilities.invokeLater { studioController.renderPlaybackState(pb) }
                    }
                    playbackController = ctrl
                    if (disposed) {
                        releaseResources()
                        return@submit
                    }

                    if (hasAudioTrack) {
                        attachDecodedAudioAsync(opened.bundleId, ctrl)
                    }
                }

                SwingUtilities.invokeLater {
                    studioController.renderState(initialState, image = null)
                    playbackController?.state?.let(studioController::renderPlaybackState)
                }

                if (viewportId != null && frameCount > 0L) {
                    performSeekExact(initialPlayhead)
                }
            } catch (t: Throwable) {
                logger.warn("Failed to open ARB bundle $bundleIdOrPath", t)
                SwingUtilities.invokeLater {
                    studioController.showError("バンドルを開けませんでした: ${t.message}")
                }
            }
        }
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
                    avOffsetMs = effectiveAvOffsetMs,
                )
            }.onFailure { err ->
                logger.debug("ARB audio player unavailable, continuing video-only", err)
            }.getOrNull() ?: return@Runnable

            if (disposed || controller.isClosed) {
                runCatching { player.close() }
                return@Runnable
            }
            controller.attachAudioPlayer(player, effectiveAvOffsetMs)
        }
        try {
            audioDecodeWorker.submit(task)
        } catch (_: java.util.concurrent.RejectedExecutionException) {
            logger.debug("ARB audio decode worker already shut down; skipping audio attach")
        }
    }

    private fun performScrub(frameIndex: Long) {
        val ctrl = playbackController
        if (ctrl != null) {
            worker.submit {
                runCatching { ctrl.scrubTo(frameIndex) }
                    .onFailure { logger.debug("ARB scrub failed", it) }
            }
            return
        }
        val vpId = state?.viewportId ?: return
        worker.submit {
            runCatching {
                daemonService.withClient { it.mediaScrub(vpId, frameIndex) }
                readShmAndRender()
            }.onFailure { logger.debug("ARB scrub failed", it) }
        }
    }

    private fun performSeekExact(frameIndex: Long) {
        val ctrl = playbackController
        if (ctrl != null) {
            worker.submit {
                runCatching { ctrl.seekToFrame(frameIndex) }
                    .onFailure { t ->
                        logger.warn("ARB seek_frame failed", t)
                        SwingUtilities.invokeLater {
                            studioController.showError("フレームシークに失敗しました: ${t.message}")
                        }
                    }
            }
            return
        }
        val vpId = state?.viewportId ?: return
        worker.submit {
            try {
                daemonService.withClient { it.mediaSeekFrame(vpId, frameIndex) }
                readShmAndRender()
            } catch (t: Throwable) {
                logger.warn("ARB seek_frame failed", t)
                SwingUtilities.invokeLater {
                    studioController.showError("フレームシークに失敗しました: ${t.message}")
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
        worker.submit {
            val ctrl = playbackController
            if (ctrl != null && ctrl.selectTapRange(tapFrames, padBefore, padAfter)) {
                val pb = ctrl.state
                trimCloseState.onRangeAdjusted(pb.inFrameIndex, pb.outFrameIndex)
                val updated = (state ?: current).copy(inFrameIndex = pb.inFrameIndex, outFrameIndex = pb.outFrameIndex)
                state = updated
                persistEditSession(pb.inFrameIndex, pb.outFrameIndex, pb.currentFrameIndex)
                SwingUtilities.invokeLater {
                    studioController.updateState { it.copy(inFrameIndex = pb.inFrameIndex, outFrameIndex = pb.outFrameIndex) }
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
        worker.submit {
            val ctrl = playbackController
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
                SwingUtilities.invokeLater {
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

    private fun applyResetFullRange() {
        val current = state ?: return
        worker.submit {
            playbackController?.resetFullRange()
            val maxF = (current.frameCount - 1L).coerceAtLeast(0L)
            trimCloseState.onRangeAdjusted(0L, maxF)
            val updated = (state ?: current).copy(inFrameIndex = 0L, outFrameIndex = maxF)
            state = updated
            persistEditSession(0L, maxF, updated.currentFrameIndex)
            SwingUtilities.invokeLater { studioController.updateState { it.copy(inFrameIndex = 0L, outFrameIndex = maxF) } }
        }
    }

    private fun readShmAndRender() {
        val reader = shmReader ?: return
        val frame = reader.readLatest() ?: return
        val image = frame.toBufferedImage()
        val prev = state ?: return
        state = TimelineViewerReducer.onFramePublished(prev, frame.meta)
        SwingUtilities.invokeLater { studioController.applyFramePublished(frame.meta, image) }
    }

    private fun performExport(
        inFrameIndex: Long,
        outFrameIndex: Long,
        format: StudioExportFormat,
    ) {
        val current = state ?: return
        val isFullRange = current.frameCount > 0L &&
            inFrameIndex <= 0L &&
            outFrameIndex >= current.frameCount - 1L
        val shouldTrashOriginal = trashOriginalRequested && !current.isLegacy &&
            !(format == StudioExportFormat.SHARE_MP4 && isFullRange)

        worker.submit {
            try {
                if (format == StudioExportFormat.SHARE_MP4) {
                    val shareResp = daemonService.withClient {
                        it.exportShareMp4OrNull(
                            ExportShareMp4Params(
                                bundleIdOrPath = current.bundleId,
                                inFrameIndex = if (isFullRange) null else inFrameIndex,
                                outFrameIndex = if (isFullRange) null else outFrameIndex,
                                trashOriginal = shouldTrashOriginal,
                                holderPid = ProcessHandle.current().pid(),
                            ),
                        )
                    }
                    if (shareResp != null) {
                        trimCloseState.markExported(inFrameIndex, outFrameIndex)
                        val mp4Name = runCatching { Path.of(shareResp.shareMp4Path).fileName.toString() }
                            .getOrDefault(shareResp.shareMp4Path)
                        val trackLabel = if (shareResp.audioIncluded) "映像+音声" else "映像のみ"
                        val trimmedNote = shareResp.trimmedBundleId?.let { " · バンドル: $it" }.orEmpty()
                        val trashedNote = if (shareResp.originalTrashed) {
                            " · 元のバンドルをゴミ箱へ移動しました（復元可）"
                        } else {
                            ""
                        }
                        val warningNote = shareResp.warning?.takeIf { it.isNotBlank() }?.let { " · 警告: $it" }.orEmpty()
                        val summaryText = "共有用 MP4 を書き出しました: $mp4Name (${shareResp.videoFrameCount}f · $trackLabel)$trimmedNote$trashedNote$warningNote"
                        val completion = ExportCompletionInfo(
                            summaryText = summaryText,
                            outputPath = shareResp.shareMp4Path,
                            bundleId = shareResp.trimmedBundleId,
                            isTrimBundle = !shareResp.trimmedBundleId.isNullOrBlank(),
                        )
                        val updated = (state ?: current).copy(
                            exportCompletion = completion,
                            lastActionMessage = summaryText,
                        )
                        state = updated
                        SwingUtilities.invokeLater {
                            studioController.updateState {
                                it.copy(exportCompletion = completion, lastActionMessage = summaryText)
                            }
                        }
                        return@submit
                    }
                }

                val res = daemonService.withClient {
                    it.exportTrim(
                        ExportTrimParams(
                            bundleIdOrPath = current.bundleId,
                            inFrameIndex = inFrameIndex,
                            outFrameIndex = outFrameIndex,
                            holderPid = ProcessHandle.current().pid(),
                        ),
                    )
                }
                trimCloseState.markExported(inFrameIndex, outFrameIndex)

                val trashedNote = if (shouldTrashOriginal && !current.isLegacy) {
                    val failure = runCatching {
                        daemonService.withClient { it.bundleTrash(current.bundleId) }
                    }.exceptionOrNull()
                    if (failure == null) {
                        " · 元のバンドルをゴミ箱へ移動しました（復元可）"
                    } else {
                        logger.warn("ARB bundle.trash after export failed", failure)
                        " · 元のバンドルをゴミ箱へ移動できませんでした: ${failure.message}"
                    }
                } else {
                    ""
                }

                val summaryText = "${ArbFormatters.EXPORT_TOAST_TITLE}: ${res.bundleId} (${res.trimReport.outputFrameCount}f)$trashedNote"
                val completion = ExportCompletionInfo(
                    summaryText = summaryText,
                    outputPath = res.bundlePath,
                    bundleId = res.bundleId,
                    isTrimBundle = true,
                )
                val updated = (state ?: current).copy(
                    exportCompletion = completion,
                    lastActionMessage = summaryText,
                )
                state = updated
                SwingUtilities.invokeLater {
                    studioController.updateState {
                        it.copy(exportCompletion = completion, lastActionMessage = summaryText)
                    }
                }
            } catch (t: Throwable) {
                logger.warn("ARB export failed (format=$format)", t)
                SwingUtilities.invokeLater { studioController.showError("書き出しに失敗しました: ${t.message}") }
            }
        }
    }

    private fun performPngExport(frameIndex: Long, outputPath: Path) {
        val current = state ?: return
        worker.submit {
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
                SwingUtilities.invokeLater {
                    studioController.updateState {
                        it.copy(exportCompletion = completion, lastActionMessage = summaryText)
                    }
                }
            } catch (t: Throwable) {
                logger.warn("ARB export.png failed", t)
                SwingUtilities.invokeLater { studioController.showError("PNG 書き出しに失敗しました: ${t.message}") }
            }
        }
    }

    private fun persistEditSession(inFrame: Long, outFrame: Long, playhead: Long) {
        val current = state ?: return
        val sessionId = editSessionId ?: return
        worker.submit {
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
        disposed = true
        worker.shutdownNow()
        audioDecodeWorker.shutdownNow()
        releaseResources()
    }

    @Synchronized
    private fun releaseResources() {
        val ctrl = playbackController
        playbackController = null
        val reader = shmReader
        shmReader = null
        val lease = leaseController
        leaseController = null
        ApplicationManager.getApplication().executeOnPooledThread {
            runCatching { ctrl?.close() }
            runCatching { reader?.close() }
            runCatching { lease?.close() }
        }
    }
}
