package io.github.shinma06.arb.ide

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.diagnostic.Logger
import com.intellij.openapi.fileEditor.FileEditor
import com.intellij.openapi.fileEditor.FileEditorLocation
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
import io.github.shinma06.arb.shm.ShmFrameRingReader
import io.github.shinma06.arb.state.TimelineViewerReducer
import io.github.shinma06.arb.state.TimelineViewerState
import io.github.shinma06.arb.ui.TimelineViewerPanel
import java.beans.PropertyChangeListener
import java.nio.file.Path
import java.util.concurrent.Executors
import javax.swing.JComponent
import javax.swing.SwingUtilities

/**
 * Thin IntelliJ [FileEditor] for viewing a v2 or Legacy (RO) bundle.
 *
 * Lifecycle guarantees:
 * - Acquires a Viewer Lease via `bundle.open(holder_kind = ide_viewer)` and renews it periodically.
 * - Attaches to `arbd`'s SHM triple-buffered viewport via [ShmFrameRingReader].
 * - On [dispose] (tab close), stops heartbeat, unmaps SHM, closes viewport, and releases the lease!
 */
class ArbBundleFileEditor(
    private val project: Project,
    private val virtualFile: ArbBundleVirtualFile,
    private val daemonService: ArbdDaemonService,
) : UserDataHolderBase(), FileEditor {
    private val logger = Logger.getInstance(ArbBundleFileEditor::class.java)
    private val worker = Executors.newSingleThreadExecutor { r ->
        Thread(r, "arb-viewer-${virtualFile.displayTitle}").apply { isDaemon = true }
    }

    private val panel = TimelineViewerPanel(
        onScrub = { frameIndex -> performScrub(frameIndex) },
        onSeekExact = { frameIndex -> performSeekExact(frameIndex) },
        onTrimExport = { inFrame, outFrame -> performTrim(inFrame, outFrame) },
        onPngExport = { frameIndex, outputPath -> performPngExport(frameIndex, outputPath) },
        onRangeChanged = { inFrame, outFrame, playhead -> persistEditSession(inFrame, outFrame, playhead) },
    )

    @Volatile
    private var leaseController: BundleLeaseController? = null

    @Volatile
    private var shmReader: ShmFrameRingReader? = null

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
                    ),
                    mainLogs = mainLogs,
                    unanchoredLogs = unanchoredLogs,
                    mainTaps = mainTaps,
                    unanchoredTaps = unanchoredTaps,
                )
                state = initialState
                SwingUtilities.invokeLater { panel.renderState(initialState, image = null) }

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

    private fun performScrub(frameIndex: Long) {
        val current = state ?: return
        val vpId = current.viewportId ?: return
        worker.submit {
            try {
                daemonService.withClient { it.mediaScrub(vpId, frameIndex) }
                readShmAndRender()
            } catch (t: Throwable) {
                logger.debug("ARB scrub failed", t)
            }
        }
    }

    private fun performSeekExact(frameIndex: Long) {
        val current = state ?: return
        val vpId = current.viewportId ?: return
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
                val msg = "トリム完了: ${res.bundleId} (${res.trimReport.outputFrameCount} フレーム, " +
                    "elst=${res.trimReport.elstBoxCount}, unanchored=${res.unanchoredLogCount} 件引継ぎ)"
                val updated = (state ?: current).copy(lastActionMessage = msg)
                state = updated
                SwingUtilities.invokeLater { panel.renderState(updated, image = null) }
            } catch (t: Throwable) {
                logger.warn("ARB export.trim failed", t)
                SwingUtilities.invokeLater { panel.showError("トリムに失敗しました: ${t.message}") }
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
                val msg = "PNG 書き出し完了: ${res.outputPath} (フレーム ${res.report.frameIndex}, ${res.report.width}x${res.report.height})"
                val updated = (state ?: current).copy(lastActionMessage = msg)
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

    override fun getName(): String = "ARB v2 Viewer"

    override fun setState(state: FileEditorState) = Unit

    override fun isModified(): Boolean = false

    override fun isValid(): Boolean = true

    override fun addPropertyChangeListener(listener: PropertyChangeListener) = Unit

    override fun removePropertyChangeListener(listener: PropertyChangeListener) = Unit

    override fun getCurrentLocation(): FileEditorLocation? = null

    override fun getFile(): VirtualFile = virtualFile

    override fun dispose() {
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
