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
import io.github.shinma06.arb.model.ExportTrimParams
import io.github.shinma06.arb.model.LeaseHolderKind
import io.github.shinma06.arb.model.QueryBucket
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
import io.github.shinma06.arb.ui.TimelineViewerPanel
import java.beans.PropertyChangeListener
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.Executors
import javax.swing.JComponent
import javax.swing.SwingUtilities
import kotlin.math.roundToLong

/**
 * Thin IntelliJ [FileEditor] for the Integrated Viewer & Trim Studio (v2 and Legacy RO bundles).
 *
 * Lifecycle & safety guarantees:
 * - Acquires a Viewer Lease via `bundle.open(holder_kind = ide_viewer)` and renews it periodically.
 * - Attaches to `arbd`'s SHM triple-buffered viewport via [ShmFrameRingReader].
 * - Drives continuous playback & deterministic range presets via T-3's [PlaybackController] &
 *   optional [AudioSyncPlayer] (probing T-2B's `media.decode_audio_pcm` gracefully).
 * - Resolves Activity / crash / stack-trace source jumps purely on PC via [IntelliJSourceJumpResolver].
 * - On non-destructive trim export with Trash-Original enabled (strictly blocked for Legacy RO),
 *   trashes the original bundle ONLY after the trimmed bundle commits successfully.
 * - On [dispose] (tab close), closes [PlaybackController] (and [AudioSyncPlayer]), unmaps SHM,
 *   closes viewport, and releases the lease!
 */
class ArbBundleFileEditor(
    private val project: Project,
    private val virtualFile: ArbBundleVirtualFile,
    private val daemonService: ArbdDaemonService,
    private val sourceJumpResolver: SourceJumpResolver = IntelliJSourceJumpResolver(project),
) : UserDataHolderBase(), FileEditor {
    private val logger = Logger.getInstance(ArbBundleFileEditor::class.java)
    private val worker = Executors.newSingleThreadExecutor { r ->
        Thread(r, "arb-studio-${virtualFile.displayTitle}").apply { isDaemon = true }
    }

    private val panel: TimelineViewerPanel = TimelineViewerPanel(
        onScrub = { frameIndex -> performScrub(frameIndex) },
        onSeekExact = { frameIndex -> performSeekExact(frameIndex) },
        onTrimExport = { inFrame, outFrame -> performTrim(inFrame, outFrame) },
        onPngExport = { frameIndex, outputPath -> performPngExport(frameIndex, outputPath) },
        onRangeChanged = { inFrame, outFrame, playhead ->
            playbackController?.setInFrame(inFrame)
            playbackController?.setOutFrame(outFrame)
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
            state = state?.let { TimelineViewerReducer.setTrashOriginalAfterExport(it, enabled) }
        },
        onRevealInFinder = { path ->
            runCatching { RevealFileAction.openFile(Path.of(path)) }
                .onFailure { logger.warn("Failed to reveal $path", it) }
        },
        onOpenExportedBundle = { bundleIdOrPath, title ->
            SwingUtilities.invokeLater {
                if (!project.isDisposed) {
                    val vf = ArbBundleVirtualFile(bundleIdOrPath, title, isLegacyHint = false)
                    FileEditorManager.getInstance(project).openFile(vf, true)
                }
            }
        },
        onSourceJump = { target ->
            val jumped = sourceJumpResolver.resolveAndNavigate(target)
            if (!jumped) {
                SwingUtilities.invokeLater {
                    panel.showError("プロジェクト内に該当ソースが見つかりませんでした: ${target.displayLabel}")
                }
            }
        },
    )

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

    init {
        loadBundleAsync()
    }

    private fun loadBundleAsync() {
        worker.submit {
            try {
                val opened = daemonService.withClient { rpc ->
                    rpc.bundleOpen(
                        bundleIdOrPath = virtualFile.bundleIdOrPath,
                        holderKind = LeaseHolderKind.IDE_VIEWER,
                        holderPid = ProcessHandle.current().pid(),
                    )
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
                    frameCount = vp.frameCount
                    shmReader = ShmFrameRingReader.open(Path.of(vp.shmPath))
                    filmstrip = runCatching {
                        daemonService.withClient { it.mediaFilmstrip(vp.viewportId) }.entries
                    }.getOrDefault(emptyList())
                }

                leaseController = BundleLeaseController(
                    leaseId = opened.leaseId,
                    renewAction = { id -> daemonService.withClient { it.bundleLeaseRenew(id) } },
                    closeLeaseAction = { id -> daemonService.withClient { it.bundleClose(id) } },
                    closeViewportAction = viewportId?.let { vpId ->
                        { daemonService.withClient { it.mediaCloseViewport(vpId) } }
                    },
                    onHeartbeatError = { err -> logger.warn("ARB lease heartbeat failed", err) },
                )

                val initialIn = existingSession?.inFrameIndex ?: 0L
                val initialOut = existingSession?.outFrameIndex ?: (frameCount - 1L).coerceAtLeast(0L)
                val initialPlayhead = existingSession?.playheadFrameIndex ?: 0L

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

                // Wire T-3 PlaybackController + optional AudioSyncPlayer (graceful fallback if T-2B audio RPC absent)
                if (viewportId != null && frameCount > 0L) {
                    val audioPlayer = createOptionalAudioPlayer(opened.bundleId, opened.summary.streams.audio?.avOffsetMs ?: 0L)
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
                        audioPlayer = audioPlayer,
                        initialFrameIndex = initialPlayhead,
                        initialInFrameIndex = initialIn,
                        initialOutFrameIndex = initialOut,
                        onPlaybackError = { err -> logger.debug("ARB playback error", err) },
                    )
                    ctrl.addStateListener { pb ->
                        SwingUtilities.invokeLater { panel.renderPlaybackState(pb) }
                    }
                    playbackController = ctrl
                }

                SwingUtilities.invokeLater {
                    panel.renderState(initialState, image = null)
                    playbackController?.state?.let(panel::renderPlaybackState)
                }

                if (viewportId != null && frameCount > 0L) {
                    performSeekExact(initialPlayhead)
                }
            } catch (t: Throwable) {
                logger.warn("Failed to open ARB bundle ${virtualFile.bundleIdOrPath}", t)
                SwingUtilities.invokeLater {
                    panel.showError("バンドルを開けませんでした: ${t.message}")
                }
            }
        }
    }

    private fun createOptionalAudioPlayer(bundleId: String, avOffsetMs: Long): AudioSyncPlayer? {
        val decoded = runCatching {
            daemonService.withClient { it.mediaDecodeAudioPcmOrNull(bundleId) }
        }.getOrNull() ?: return null
        val wavPath = Path.of(decoded.wavPath)
        if (!Files.isRegularFile(wavPath)) return null
        return runCatching {
            AudioSyncPlayer(
                source = WavFilePcmSource.open(wavPath),
                avOffsetMs = avOffsetMs,
            )
        }.onFailure { err ->
            logger.debug("ARB audio player unavailable, continuing video-only", err)
        }.getOrNull()
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
                        SwingUtilities.invokeLater { panel.showError("フレームシークに失敗しました: ${t.message}") }
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
                SwingUtilities.invokeLater { panel.showError("フレームシークに失敗しました: ${t.message}") }
            }
        }
    }

    private fun applySelectTapRange() {
        val current = state ?: return
        val tapFrames = current.mainTapFrameIndices()
        if (tapFrames.isEmpty()) return
        val fps = current.estimatedFps
        val padBefore = (fps * 1.0).roundToLong().coerceAtLeast(1L) // -1s
        val padAfter = (fps * 2.0).roundToLong().coerceAtLeast(1L)  // +2s
        worker.submit {
            val ctrl = playbackController
            if (ctrl != null && ctrl.selectTapRange(tapFrames, padBefore, padAfter)) {
                val pb = ctrl.state
                val updated = (state ?: current).copy(inFrameIndex = pb.inFrameIndex, outFrameIndex = pb.outFrameIndex)
                state = updated
                persistEditSession(pb.inFrameIndex, pb.outFrameIndex, pb.currentFrameIndex)
                SwingUtilities.invokeLater { panel.renderState(updated, image = null) }
            }
        }
    }

    private fun applySelectAroundCrash() {
        val current = state ?: return
        val crashFrame = current.primaryCrashFrameIndex() ?: return
        val fps = current.estimatedFps
        val before = (fps * 5.0).roundToLong().coerceAtLeast(1L) // -5s
        val after = (fps * 2.0).roundToLong().coerceAtLeast(1L)  // +2s
        worker.submit {
            val ctrl = playbackController
            if (ctrl != null) {
                ctrl.selectAroundCrash(crashFrame, before, after)
                val pb = ctrl.state
                val updated = (state ?: current).copy(
                    inFrameIndex = pb.inFrameIndex,
                    outFrameIndex = pb.outFrameIndex,
                    currentFrameIndex = pb.currentFrameIndex,
                )
                state = updated
                persistEditSession(pb.inFrameIndex, pb.outFrameIndex, pb.currentFrameIndex)
                SwingUtilities.invokeLater { panel.renderState(updated, image = null) }
            }
        }
    }

    private fun applyResetFullRange() {
        val current = state ?: return
        worker.submit {
            playbackController?.resetFullRange()
            val maxF = (current.frameCount - 1L).coerceAtLeast(0L)
            val updated = (state ?: current).copy(inFrameIndex = 0L, outFrameIndex = maxF)
            state = updated
            persistEditSession(0L, maxF, updated.currentFrameIndex)
            SwingUtilities.invokeLater { panel.renderState(updated, image = null) }
        }
    }

    private fun readShmAndRender() {
        val reader = shmReader ?: return
        val frame = reader.readLatest() ?: return
        val image = frame.toBufferedImage()
        val prev = state ?: return
        val next = TimelineViewerReducer.onFramePublished(prev, frame.meta)
        state = next
        SwingUtilities.invokeLater { panel.renderState(next, image) }
    }

    private fun performTrim(inFrameIndex: Long, outFrameIndex: Long) {
        val current = state ?: return
        val shouldTrashOriginal = current.effectiveTrashOriginalAfterExport
        worker.submit {
            try {
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

                // Non-destructive Trash of the original bundle happens ONLY after export succeeds,
                // and is strictly forbidden for Legacy (RO) bundles.
                val trashedNote = if (shouldTrashOriginal && !current.isLegacy) {
                    val trashed = runCatching {
                        daemonService.withClient { it.bundleTrash(current.bundleId) }
                    }.isSuccess
                    if (trashed) " · 元のバンドルをゴミ箱へ移動しました（復元可）" else ""
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
                SwingUtilities.invokeLater { panel.renderState(updated, image = null) }
            } catch (t: Throwable) {
                logger.warn("ARB export.trim failed", t)
                SwingUtilities.invokeLater { panel.showError("書き出しに失敗しました: ${t.message}") }
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
                SwingUtilities.invokeLater { panel.renderState(updated, image = null) }
            } catch (t: Throwable) {
                logger.warn("ARB export.png failed", t)
                SwingUtilities.invokeLater { panel.showError("PNG 書き出しに失敗しました: ${t.message}") }
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

    override fun getComponent(): JComponent = panel

    override fun getPreferredFocusedComponent(): JComponent = panel

    override fun getName(): String = "ARB Studio"

    override fun setState(state: FileEditorState) = Unit

    override fun isModified(): Boolean = false

    override fun isValid(): Boolean = true

    override fun addPropertyChangeListener(listener: PropertyChangeListener) = Unit

    override fun removePropertyChangeListener(listener: PropertyChangeListener) = Unit

    override fun getCurrentLocation(): FileEditorLocation? = null

    override fun getFile(): VirtualFile = virtualFile

    override fun dispose() {
        val ctrl = playbackController
        playbackController = null
        runCatching { ctrl?.close() }

        worker.shutdownNow()
        runCatching { shmReader?.close() }
        shmReader = null
        val lease = leaseController
        leaseController = null
        if (lease != null) {
            ApplicationManager.getApplication().executeOnPooledThread {
                runCatching { lease.close() }
            }
        }
    }
}
