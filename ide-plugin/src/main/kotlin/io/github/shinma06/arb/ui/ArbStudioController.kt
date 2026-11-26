package io.github.shinma06.arb.ui

import androidx.compose.foundation.text.input.TextFieldState
import androidx.compose.foundation.text.input.setTextAndPlaceCursorAtEnd
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.toComposeImageBitmap
import io.github.shinma06.arb.model.LogRecord
import io.github.shinma06.arb.model.ScreenSpan
import io.github.shinma06.arb.playback.AudioState
import io.github.shinma06.arb.playback.PlaybackMode
import io.github.shinma06.arb.playback.PlaybackSpeed
import io.github.shinma06.arb.playback.PlaybackState
import io.github.shinma06.arb.shm.ShmFrameMeta
import io.github.shinma06.arb.state.APPROXIMATE_PREVIEW_BADGE_TEXT
import io.github.shinma06.arb.state.ActiveTapOverlay
import io.github.shinma06.arb.state.ArbFormatters
import io.github.shinma06.arb.state.AuxStream
import io.github.shinma06.arb.state.StreamLoadState
import io.github.shinma06.arb.state.AuxStreamFailure
import io.github.shinma06.arb.state.EventStreamFilter
import io.github.shinma06.arb.state.activeTimelineItemIndex
import io.github.shinma06.arb.state.SourceJumpParser
import io.github.shinma06.arb.state.SourceJumpTarget
import io.github.shinma06.arb.state.SyncedEventKind
import io.github.shinma06.arb.state.SyncedEventRow
import io.github.shinma06.arb.state.TimelineViewerReducer
import io.github.shinma06.arb.state.TimelineViewerState
import io.github.shinma06.arb.state.TrimPreviewState
import io.github.shinma06.arb.state.UNANCHORED_TAB_TITLE
import java.awt.Rectangle
import java.awt.Toolkit
import java.awt.datatransfer.StringSelection
import java.awt.image.BufferedImage
import java.nio.file.Path
import java.util.concurrent.atomic.AtomicReference

enum class StudioExportFormat(val label: String) {
    SHARE_MP4("共有用 MP4"),
    WORM_BUNDLE_ONLY("WORM のみ"),
    ;

    override fun toString(): String = label
}

/** One entry of the per-app log filter (`null` [packageName] = `全アプリ`). */
data class AppPackageFilterOption(
    val packageName: String?,
    val label: String,
)

/**
 * Lock-free "latest value wins" mailbox used to coalesce decoded frames between the IPC worker and
 * the EDT: only one drain is ever queued, and a slow EDT skips stale frames instead of piling up
 * one full-frame bitmap per `invokeLater` during playback.
 */
class LatestValueMailbox<T : Any> {
    private val slot = AtomicReference<T?>(null)

    /** Stores [value]; returns `true` when the caller must schedule a drain (the slot was empty). */
    fun offer(value: T): Boolean = slot.getAndSet(value) == null

    /** Takes the latest pending value, or `null` when it was already drained. */
    fun take(): T? = slot.getAndSet(null)
}

internal typealias TrimEdge = io.github.shinma06.arb.state.TrimEdge

/** Continuous half-open edit interval and its inclusive legacy RPC projection. */
data class StudioTrimRange(
    val inTimeNs: Long,
    val outTimeNs: Long,
    val inFrameIndex: Long,
    val outFrameIndex: Long,
)

private fun TimelineViewerState.trimRange() = StudioTrimRange(inTimeNs, outTimeNs, inFrameIndex, outFrameIndex)

enum class StudioRightTab {
    EVENTS_AND_LOGS,
    UNANCHORED,
    DETAILS,
}

/**
 * Pure Kotlin + Compose state holder for the Integrated Viewer & Trim Studio (`ArbBundleFileEditor`).
 *
 * All state transitions, tap coordinate scaling, trim range sync, event filtering/correlation,
 * and honesty badge logic live here so headless JUnit 5 tests can exercise 100% of studio behavior
 * without requiring Skiko or a live Android Studio frame.
 */
class ArbStudioController(
    private val onScrub: (frameIndex: Long) -> Unit,
    private val onSeekExact: (frameIndex: Long) -> Unit,
    private val onTrimExport: (inFrameIndex: Long, outFrameIndex: Long) -> Unit,
    private val onPngExport: (frameIndex: Long, outputPath: Path) -> Unit,
    private val onRangeChanged: (inFrameIndex: Long, outFrameIndex: Long, playhead: Long) -> Unit = { _, _, _ -> },
    private val onPlayPauseToggle: () -> Unit = {},
    private val onStepFrame: ((delta: Int) -> Unit)? = null,
    private val onSpeedChange: (PlaybackSpeed) -> Unit = {},
    private val onMuteToggle: () -> Unit = {},
    private val onLoopToggle: () -> Unit = {},
    private val onSelectTapRange: () -> Unit = {},
    private val onSelectAroundCrash: () -> Unit = {},
    private val onResetFullRange: () -> Unit = {},
    private val onTrashOriginalToggled: (enabled: Boolean) -> Unit = {},
    private val onRevealInFinder: (path: String) -> Unit = {},
    private val onOpenExportedBundle: (bundleIdOrPath: String, title: String) -> Unit = { _, _ -> },
    private val onSourceJump: (SourceJumpTarget) -> Unit = {},
    private val onExportRequested: ((inFrameIndex: Long, outFrameIndex: Long, format: StudioExportFormat) -> Unit)? = null,
    private val onChoosePngOutputPath: ((bundleId: String, frameIndex: Long, onChosen: (Path) -> Unit) -> Unit)? = null,
    private val clipboardWriter: (String) -> Unit = ::copyPathToClipboard,
    /** Re-queries `timeline.query_logs(package = ...)` off the EDT (`null` = all apps). */
    private val onAppPackageFilterChanged: (packageName: String?) -> Unit = {},
    initialBurnTapsOnExport: Boolean = true,
    private val onBurnTapsToggled: (enabled: Boolean) -> Unit = {},
    private val onCloseTab: () -> Unit = {},
    /** Re-requests one auxiliary stream that failed to load (the inline "再読込" action). */
    private val onRetryAuxStream: (AuxStream) -> Unit = {},
    /** Re-sends the IN/OUT range that `edit.update_session` could not persist ("再保存"). */
    private val onRetryEditSave: () -> Unit = {},
    private val onTimeSeek: ((Long, Boolean) -> Unit)? = null,
    private val onTrimPreviewFrame: (Long, (ImageBitmap?) -> Unit) -> Unit = { _, _ -> },
    private val onTrimPreviewEnded: () -> Unit = {},
    private val onTimeRangeChanged: ((StudioTrimRange, playheadFrame: Long) -> Unit)? = null,
    private val onTimeRangeExport: ((StudioTrimRange, StudioExportFormat) -> Unit)? = null,
    private val displayPositionNs: () -> Long? = { null },
) {
    var viewerState: TimelineViewerState? by mutableStateOf(null)
        private set

    var playbackState: PlaybackState by mutableStateOf(PlaybackState())
        private set

    private var playbackBitmap: ImageBitmap? by mutableStateOf(null)
    private var previewBitmap: ImageBitmap? by mutableStateOf(null)
    val frameBitmap: ImageBitmap? get() = previewBitmap ?: playbackBitmap

    var frameImageWidth: Int by mutableStateOf(0)
        private set

    var frameImageHeight: Int by mutableStateOf(0)
        private set

    var approximatePreviewVisible: Boolean by mutableStateOf(false)
        private set

    var activeTab: StudioRightTab by mutableStateOf(StudioRightTab.EVENTS_AND_LOGS)
        private set

    /**
     * Video <-> right-panel Event/Log timeline auto-follow toggle (Owner trial feedback T-6H
     * item 4). `true` (default) scrolls the Event/Log `LazyColumn` to track [activeTimelineItemIndex]
     * as the playhead advances; `false` pins the list wherever the user scrolled.
     */
    var followPlayhead: Boolean by mutableStateOf(true)
        private set

    var eventFilter: EventStreamFilter by mutableStateOf(EventStreamFilter.ALL)
        private set

    val searchFieldState: TextFieldState = TextFieldState("")

    var searchQuery: String by mutableStateOf("")
        private set

    var selectedExportFormat: StudioExportFormat by mutableStateOf(StudioExportFormat.SHARE_MP4)
        private set

    /** Whether Share MP4 export burns tap ripples & swipe trajectories into `share-taps.mp4`. */
    var burnTapsOnExport: Boolean by mutableStateOf(initialBurnTapsOnExport)
        private set

    var isExporting: Boolean by mutableStateOf(false)
        private set

    var exportProgressLabel: String by mutableStateOf("書き出し中...")
        private set

    var timelineWarnings by mutableStateOf(emptySet<String>())
        private set

    fun showTimelineWarning(message: String) {
        timelineWarnings = timelineWarnings + message
    }

    var errorMessage: String? by mutableStateOf(null)
        private set

    /** Reason `edit.update_session` last failed; `null` when the IN/OUT range is persisted. */
    var editSaveFailure: String? by mutableStateOf(null)
        private set

    var editRetryBusy: Boolean by mutableStateOf(false)
        private set

    fun finishEditRetry() {
        editRetryBusy = false
    }

    fun markEditSaveFailure(reason: String?) {
        editSaveFailure = reason
    }

    val hasUnsavedEditRange: Boolean
        get() = editSaveFailure != null

    fun retryEditSave() {
        if (editSaveFailure == null || editRetryBusy) return
        editRetryBusy = true
        onRetryEditSave()
    }

    /** Streams whose load failed; shown inline with a reload action (never as "0 items"). */
    val failedAuxStreams: List<AuxStreamFailure>
        get() = viewerState?.failedAuxStreams.orEmpty()

    fun retryAuxStream(stream: AuxStream) {
        if (viewerState?.streamState(stream) !is StreamLoadState.Failed) return
        updateState { TimelineViewerReducer.withStreamState(it, stream, StreamLoadState.Pending) }
        onRetryAuxStream(stream)
    }

    /** Notice that a log bucket was cut at the display limit, or `null`. */
    val logLimitNotice: String?
        get() = viewerState?.logLimitNotice

    var syncedRows: List<SyncedEventRow> by mutableStateOf(emptyList())
        private set

    /** Selected foreground/crashed app package for the log stream (`null` = `全アプリ`). */
    var selectedAppPackage: String? by mutableStateOf(null)
        private set

    var appPackageFilterLoading: Boolean by mutableStateOf(false)
        private set

    private var lastStructureKey: List<Any?>? = null
    private var lastFlagKey: List<Any?>? = null

    // ---- Derived read-only view properties ----

    val hasVideo: Boolean
        get() = (viewerState?.frameCount ?: 0L) > 0L

    val headerTitle: String
        get() = viewerState?.bundleId ?: "読み込み中..."

    val headerSubtitle: String
        get() {
            val s = viewerState ?: return ""
            // Never fall back to the wire format name (`v2`, `legacy_schema4`, ...).
            val dev = s.summary.device?.let { "${it.model} (API ${it.apiLevel})" } ?: UNKNOWN_DEVICE_LABEL
            val created = s.summary.createdAtUtc?.replace('T', ' ')?.removeSuffix("Z")?.take(19).orEmpty()
            return listOf(dev, created).filter { it.isNotBlank() }.joinToString(" · ")
        }

    val isPlaying: Boolean
        get() = playbackState.mode == PlaybackMode.PLAYING

    val playPauseLabel: String
        get() = if (isPlaying) "一時停止" else "再生"

    val muteLabel: String
        get() = when {
            playbackState.audioState == AudioState.UNAVAILABLE -> "音声なし"
            playbackState.muted || playbackState.audioState == AudioState.MUTED -> "ミュート"
            else -> "音声 ON"
        }

    val muteEnabled: Boolean
        get() = playbackState.audioState != AudioState.UNAVAILABLE

    /** Loop playback toggle state (Owner trial feedback T-6H item 3). */
    val isLoopEnabled: Boolean
        get() = playbackState.loopInSelection

    val loopTooltip: String
        get() = if (isLoopEnabled) "ループ再生 ON" else "ループ再生 OFF"

    var trimPreview: TrimPreviewState? by mutableStateOf(null)
        private set
    private var originalRange: StudioTrimRange? = null
    private var previewGeneration = 0L
    private var scrubFrame: Long? = null
    private var scrubOrigin: Long? = null

    val playheadTimeNs: Long get() = clockTimeNs
    private var clockTimeNs by mutableStateOf(0L)

    private var deferredStopNs: Long? = null

    /** The next presentation tick also delivers a pause/EOF received while a handle was held. */
    fun sampleClock() {
        if (trimPreview != null || scrubOrigin != null) return
        val time = if (isPlaying) displayPositionNs() else deferredStopNs
        time?.let { clockTimeNs = it }
        deferredStopNs = null
    }

    val timecodeText: String
        get() {
            val s = viewerState ?: return "00:00.00 / 00:00.00 · #0"
            val curMs = playheadTimeNs / 1_000_000L
            val durationMs = s.totalDurationMs
            return "${ArbFormatters.formatTimecode(curMs)} / ${ArbFormatters.formatTimecode(durationMs)} · #${s.currentFrameIndex}"
        }

    val exportButtonLabel: String
        get() = if (isExporting) {
            exportProgressLabel
        } else {
            viewerState?.exportCtaLabel ?: ArbFormatters.EXPORT_FULL_LABEL
        }

    val isExportButtonEnabled: Boolean
        get() = hasVideo && !isExporting

    val isBurnTapsVisible: Boolean
        get() = selectedExportFormat == StudioExportFormat.SHARE_MP4

    val activeTapOverlays: List<ActiveTapOverlay>
        get() = viewerState?.let { state ->
            state.activeTapsAtFrame(trimPreview?.frameIndex ?: state.currentFrameIndex, timeNs = trimPreview?.timeNs ?: playheadTimeNs)
        }.orEmpty()

    val isTapOverlayEnabled: Boolean
        get() = viewerState?.showTapOverlay ?: true

    val unanchoredTabLabel: String
        get() = "$UNANCHORED_TAB_TITLE (${viewerState?.unanchoredLogs?.size ?: 0})"

    val mainLogRowCount: Int
        get() = syncedRows.count { it.kind == SyncedEventKind.LOG }

    val unanchoredLogRowCount: Int
        get() = viewerState?.unanchoredLogs?.size ?: 0

    val isUnanchoredTruncatedBannerVisible: Boolean
        get() = viewerState?.unanchoredTruncatedNotice != null

    val visibleHonestyChipLabels: List<String>
        get() = viewerState?.tier2HonestyChips?.map { it.label }.orEmpty()

    val isExportCompletionBarVisible: Boolean
        get() = viewerState?.exportCompletion != null

    val isSelectTapRangeVisible: Boolean
        get() = viewerState?.canSelectTapRange == true

    val isSelectAroundCrashVisible: Boolean
        get() = viewerState?.canSelectAroundCrash == true

    val isTrashOriginalEnabled: Boolean
        get() = viewerState?.canTrashOriginalAfterExport == true

    val isTrashOriginalSelected: Boolean
        get() = viewerState?.effectiveTrashOriginalAfterExport == true

    val selectionSummaryText: String
        get() = viewerState?.selectionSummaryText.orEmpty()

    val displayWidth: Int?
        get() = viewerState?.summary?.streams?.video?.displayWidth?.takeIf { it > 0 }

    val displayHeight: Int?
        get() = viewerState?.summary?.streams?.video?.displayHeight?.takeIf { it > 0 }

    /** Shown only for ARB bundles whose manifest lists extracted `app-logs/<package>.jsonl`. */
    val isAppPackageFilterVisible: Boolean
        get() = viewerState?.let { !it.isLegacy && it.appLogs.isNotEmpty() } == true

    val appPackageFilterOptions: List<AppPackageFilterOption>
        get() {
            val s = viewerState ?: return emptyList()
            if (!isAppPackageFilterVisible) return emptyList()
            return listOf(AppPackageFilterOption(null, ALL_APPS_LABEL)) + s.appLogs.map { summary ->
                val crash = if (summary.crashCount > 0L) " · クラッシュ ${summary.crashCount}" else ""
                AppPackageFilterOption(
                    packageName = summary.packageName,
                    label = "${summary.packageName} · ${summary.lineCount} 行$crash",
                )
            }
        }

    val selectedAppPackageIndex: Int
        get() = appPackageFilterOptions.indexOfFirst { it.packageName == selectedAppPackage }.coerceAtLeast(0)

    // ---- State ingestion from ArbBundleFileEditor ----

    fun renderState(next: TimelineViewerState, image: BufferedImage?) {
        renderFrame(next, image?.let(::toImageBitmapOrNull), image?.width ?: 0, image?.height ?: 0)
    }

    /**
     * Applies [next] with an already-converted [bitmap]. Callers convert `BufferedImage` ->
     * `ImageBitmap` off the EDT ([toImageBitmapOrNull]) so playback never copies full frames on
     * the UI thread; a `null` [bitmap] keeps the previous frame.
     */
    fun renderFrame(next: TimelineViewerState, bitmap: ImageBitmap?, width: Int, height: Int) {
        if (viewerState?.axis !== next.axis) {
            clearGesture()
            deferredStopNs = null
        }
        if (trimPreview == null && scrubOrigin == null) syncPlayheadToFrame(viewerState, next)
        viewerState = next
        errorMessage = null
        if (width > 0 && height > 0) {
            frameImageWidth = width
            frameImageHeight = height
            if (bitmap != null) playbackBitmap = bitmap
        }
        approximatePreviewVisible = next.showApproximatePreviewBadge || playbackState.isApproximate
        if (trimPreview == null) rebuildSyncedRows(next)
    }

    /**
     * Keeps [playheadTimeNs] on the displayed frame when an editor-side change (a range preset,
     * or a degraded open without a playback controller) moves `currentFrameIndex`. A time inside
     * the frame's hold, including its right boundary, is kept. While playing the presentation
     * clock owns the time, so a late frame or state update never pulls the playhead back.
     */
    private fun syncPlayheadToFrame(previous: TimelineViewerState?, next: TimelineViewerState) {
        val axis = next.axis
        val hold = axis.start(next.currentFrameIndex)..axis.outTime(next.currentFrameIndex)
        val stale = previous == null || previous.axis !== axis || (!isPlaying && previous.currentFrameIndex != next.currentFrameIndex && playheadTimeNs !in hold)
        if (stale) clockTimeNs = displayPositionNs()?.takeIf { it in hold } ?: hold.first
    }

    /** Merges a newly published SHM frame into controller-owned state (preserving UI edits). */
    fun applyFramePublished(meta: ShmFrameMeta, image: BufferedImage?) {
        val current = viewerState ?: return
        renderState(TimelineViewerReducer.onFramePublished(current, meta), image)
    }

    /** Merges a published SHM frame whose bitmap was converted off the EDT. */
    fun applyFramePublished(meta: ShmFrameMeta, bitmap: ImageBitmap?, width: Int, height: Int) {
        val current = viewerState ?: return
        renderFrame(TimelineViewerReducer.onFramePublished(current, meta), bitmap, width, height)
    }

    /**
     * Applies `timeline.query_logs(package = ...)` results for [packageName]. Stale responses
     * (the user already picked another package) are ignored.
     */
    fun applyAppPackageLogs(
        packageName: String?,
        mainLogs: List<LogRecord>,
        unanchoredLogs: List<LogRecord>,
        mainLimited: Boolean = false,
        unanchoredLimited: Boolean = false,
        limitUnknown: Boolean = false,
    ) {
        if (packageName != selectedAppPackage) return
        appPackageFilterLoading = false
        val current = viewerState ?: return
        updateState {
            TimelineViewerReducer.withBucketedRecords(
                state = it,
                mainLogs = mainLogs,
                unanchoredLogs = unanchoredLogs,
                mainTaps = current.mainTaps,
                unanchoredTaps = current.unanchoredTaps,
                mainLogsLimited = mainLimited,
                unanchoredLogsLimited = unanchoredLimited,
                logLimitUnknown = limitUnknown,
            )
        }
    }

    fun appPackageFilterFailed(packageName: String?, message: String) {
        if (packageName != selectedAppPackage) return
        appPackageFilterLoading = false
        showError(message)
    }

    /** Applies an editor-originated change on top of controller-owned state. */
    fun updateState(transform: (TimelineViewerState) -> TimelineViewerState) {
        val current = viewerState ?: return
        renderState(transform(current), image = null)
    }

    var playbackError by mutableStateOf<String?>(null)
        private set

    fun showPlaybackError(message: String) {
        playbackError = message
    }

    fun renderPlaybackState(pb: PlaybackState) {
        if (pb.mode == PlaybackMode.PLAYING) playbackError = null
        playbackState = pb
        approximatePreviewVisible = pb.isApproximate || (viewerState?.showApproximatePreviewBadge == true)
        val current = viewerState
        if (current != null) {
            if (scrubOrigin != null) return
            val sampled = displayPositionNs() ?: if (current.currentFrameIndex == pb.currentFrameIndex) {
                clockTimeNs
            } else {
                current.axis.start(pb.currentFrameIndex)
            }
            if (trimPreview == null) clockTimeNs = sampled
            deferredStopNs = sampled.takeIf { trimPreview != null && pb.mode == PlaybackMode.PAUSED }
            if (current.currentFrameIndex != pb.currentFrameIndex) {
                viewerState = current.copy(currentFrameIndex = pb.currentFrameIndex)
            }
        }
    }

    fun showError(message: String) {
        errorMessage = message
    }

    // ---- User actions from Jewel Compose UI & headless tests ----

    fun selectTab(tab: StudioRightTab) {
        if (tab != activeTab) cancelGesture()
        activeTab = tab
    }

    fun selectDetailsTab() {
        selectTab(StudioRightTab.DETAILS)
    }

    fun selectEventFilter(filter: EventStreamFilter) {
        eventFilter = filter
        viewerState?.let(::rebuildSyncedRows)
    }

    fun updateSearchQuery(query: String) {
        searchQuery = query
        if (searchFieldState.text.toString() != query) {
            searchFieldState.setTextAndPlaceCursorAtEnd(query)
        }
        viewerState?.let(::rebuildSyncedRows)
    }

    /** Selects the per-app log filter (`null` = `全アプリ`) and re-queries logs off the EDT. */
    fun selectAppPackage(packageName: String?) {
        if (packageName == selectedAppPackage) return
        if (packageName != null && viewerState?.appLogs?.none { it.packageName == packageName } != false) return
        selectedAppPackage = packageName
        appPackageFilterLoading = true
        onAppPackageFilterChanged(packageName)
    }

    fun selectAppPackageByIndex(index: Int) {
        val option = appPackageFilterOptions.getOrNull(index) ?: return
        selectAppPackage(option.packageName)
    }

    fun selectExportFormat(format: StudioExportFormat) {
        selectedExportFormat = format
    }

    fun togglePlayPause() = onPlayPauseToggle()

    fun stepFrame(delta: Int) {
        deferredStopNs = null
        val s = viewerState ?: return
        val next = TimelineViewerReducer.stepFrame(s, delta.toLong())
        viewerState = next
        rebuildSyncedRows(next)
        clockTimeNs = next.axis.start(next.currentFrameIndex)
        onStepFrame?.invoke(delta) ?: onSeekExact(next.currentFrameIndex)
        notifyRange(next)
    }

    fun cycleSpeed() {
        val nextSpeed = when (playbackState.speed) {
            PlaybackSpeed.QUARTER -> PlaybackSpeed.HALF
            PlaybackSpeed.HALF -> PlaybackSpeed.NORMAL
            PlaybackSpeed.NORMAL -> PlaybackSpeed.DOUBLE
            PlaybackSpeed.DOUBLE -> PlaybackSpeed.QUARTER
        }
        playbackState = playbackState.copy(speed = nextSpeed)
        onSpeedChange(nextSpeed)
    }

    fun toggleMute() = onMuteToggle()

    /** Flips the loop playback toggle (Owner trial feedback T-6H item 3). */
    fun toggleLoop() = onLoopToggle()

    fun toggleTapOverlay() {
        val s = viewerState ?: return
        viewerState = s.copy(showTapOverlay = !s.showTapOverlay)
    }

    /** Index into [syncedRows] of the row most relevant to the current playhead, or `null`. */
    val activeTimelineItemIndex: Int?
        get() = activeTimelineItemIndex(syncedRows, playheadTimeNs)

    /** Explicitly sets [followPlayhead]. Turning it back ON snaps immediately (next recompose). */
    fun selectFollowPlayhead(enabled: Boolean) {
        followPlayhead = enabled
    }

    /** Flips [followPlayhead]. */
    fun toggleFollowPlayhead() {
        followPlayhead = !followPlayhead
    }

    fun seekTime(timeNs: Long, settled: Boolean = true) {
        val s = viewerState ?: return
        if (s.frameCount == 0L) return
        deferredStopNs = null
        val time = timeNs.coerceIn(0, s.axis.endNs)
        val frame = s.axis.frameAt(time)
        if (!settled && scrubOrigin == null) scrubOrigin = clockTimeNs
        clockTimeNs = time
        viewerState = s.copy(currentFrameIndex = frame)
        if (settled) {
            scrubOrigin = null
            scrubFrame = null
            onTimeSeek?.invoke(time, true) ?: onSeekExact(frame)
            notifyRange(checkNotNull(viewerState))
        } else if (scrubFrame != frame) {
            scrubFrame = frame
            onTimeSeek?.invoke(time, false) ?: onScrub(frame)
        }
    }

    internal val gestureActive get() = originalRange != null || scrubOrigin != null

    internal fun cancelGesture() {
        val original = originalRange
        val seek = scrubOrigin
        clearGesture()
        if (original != null) {
            viewerState = viewerState?.copy(inTimeNs = original.inTimeNs, outTimeNs = original.outTimeNs)
            viewerState?.let(::rebuildSyncedRows)
        }
        if (seek != null) {
            val state = viewerState ?: return
            val frame = state.axis.frameAt(seek)
            clockTimeNs = seek
            viewerState = state.copy(currentFrameIndex = frame)
            onTimeSeek?.invoke(seek, true) ?: onSeekExact(frame)
        }
    }

    private fun clearGesture() {
        originalRange = null
        scrubFrame = null
        scrubOrigin = null
        if (trimPreview != null) onTrimPreviewEnded()
        trimPreview = null
        previewBitmap = null
        previewGeneration++
    }

    internal fun dragTrim(edge: TrimEdge, pointerTime: Long, settled: Boolean) {
        val state = viewerState ?: return
        if (state.frameCount == 0L) return
        val next = TimelineViewerReducer.setEdge(state, edge, pointerTime)
        if (settled) {
            commitRange(next)
            return
        }
        if (originalRange == null) originalRange = state.trimRange()
        viewerState = next
        val time = if (edge == TrimEdge.IN) next.inTimeNs else next.outTimeNs
        val frame = if (edge == TrimEdge.IN) next.inFrameIndex else next.outFrameIndex
        previewBoundary(edge, time, frame)
    }

    private fun previewBoundary(edge: TrimEdge, time: Long, frame: Long) {
        val previous = trimPreview
        trimPreview = TrimPreviewState(edge, time, frame)
        if (previous?.frameIndex == frame) return
        previewBitmap = null
        val generation = ++previewGeneration
        onTrimPreviewFrame(frame) { bitmap ->
            if (generation == previewGeneration) {
                previewBitmap = bitmap
                if (bitmap == null) showTimelineWarning("トリム境界のプレビューを表示できませんでした。")
            }
        }
    }

    private fun notifyRange(state: TimelineViewerState) {
        val handler = onTimeRangeChanged
        if (handler != null) handler(state.trimRange(), state.currentFrameIndex)
        else onRangeChanged(state.inFrameIndex, state.outFrameIndex, state.currentFrameIndex)
    }

    private fun commitRange(next: TimelineViewerState) {
        clearGesture()
        viewerState = next
        rebuildSyncedRows(next)
        notifyRange(next)
    }

    /** A held nudge button previews its candidate until click commits or the press is cancelled. */
    internal fun previewNudge(edge: TrimEdge, direction: Int, byTime: Boolean): TrimPreviewState? {
        val state = viewerState ?: return null
        if (gestureActive || state.frameCount == 0L) return null
        val next = TimelineViewerReducer.nudge(state, edge, direction, byTime)
        if (edge == TrimEdge.IN) previewBoundary(edge, next.inTimeNs, next.inFrameIndex)
        else previewBoundary(edge, next.outTimeNs, next.outFrameIndex)
        return trimPreview
    }

    internal fun releaseNudge(preview: TrimPreviewState?) {
        if (preview != null && trimPreview === preview && !gestureActive) {
            clearGesture()
            viewerState?.let(::rebuildSyncedRows)
        }
    }

    internal fun nudge(edge: TrimEdge, direction: Int, byTime: Boolean) {
        val state = viewerState ?: return
        if (state.frameCount <= 0) return
        commitRange(TimelineViewerReducer.nudge(state, edge, direction, byTime))
    }

    fun scrubToFrame(frameIndex: Long) {
        deferredStopNs = null
        val s = viewerState ?: return
        if (s.frameCount <= 0L) return
        val clamped = frameIndex.coerceIn(0L, s.frameCount - 1L)
        clockTimeNs = s.axis.start(clamped)
        viewerState = s.copy(currentFrameIndex = clamped)
        rebuildSyncedRows(viewerState!!)
        onScrub(clamped)
    }

    fun seekExactFrame(frameIndex: Long) {
        deferredStopNs = null
        val s = viewerState ?: return
        if (s.frameCount <= 0L) return
        val clamped = frameIndex.coerceIn(0L, s.frameCount - 1L)
        clockTimeNs = s.axis.start(clamped)
        viewerState = s.copy(currentFrameIndex = clamped)
        rebuildSyncedRows(viewerState!!)
        onSeekExact(clamped)
        notifyRange(checkNotNull(viewerState))
    }

    fun applyInFrame(frameIndex: Long, settled: Boolean) {
        val s = viewerState ?: return
        val next = TimelineViewerReducer.setInFrame(s, frameIndex)
        viewerState = next
        rebuildSyncedRows(next)
        if (settled) {
            notifyRange(next)
        }
    }

    fun applyOutFrame(frameIndex: Long, settled: Boolean) {
        val s = viewerState ?: return
        val next = TimelineViewerReducer.setOutFrame(s, frameIndex)
        viewerState = next
        rebuildSyncedRows(next)
        if (settled) {
            notifyRange(next)
        }
    }

    fun markInAtPlayhead() {
        viewerState?.takeIf { it.frameCount > 0 }?.let { commitRange(TimelineViewerReducer.setEdge(it, TrimEdge.IN, playheadTimeNs)) }
    }

    fun markOutAtPlayhead() {
        viewerState?.takeIf { it.frameCount > 0 }?.let { commitRange(TimelineViewerReducer.setEdge(it, TrimEdge.OUT, playheadTimeNs)) }
    }

    internal fun selectRange(start: Long, end: Long) {
        viewerState?.let { commitRange(TimelineViewerReducer.setRange(it, start, end)) }
    }

    fun selectTapRange() {
        onSelectTapRange()
    }

    fun selectAroundCrash() {
        onSelectAroundCrash()
    }

    fun resetFullRange() {
        onResetFullRange()
    }

    fun toggleTrashOriginal(enabled: Boolean) {
        val s = viewerState ?: return
        val next = TimelineViewerReducer.setTrashOriginalAfterExport(s, enabled)
        viewerState = next
        onTrashOriginalToggled(next.effectiveTrashOriginalAfterExport)
    }

    fun toggleBurnTapsOnExport(enabled: Boolean) {
        burnTapsOnExport = enabled
        onBurnTapsToggled(enabled)
    }

    fun setExporting(
        exporting: Boolean,
        label: String = if (burnTapsOnExport) "焼き込み書き出し中..." else "書き出し中...",
    ) {
        isExporting = exporting
        if (exporting) {
            exportProgressLabel = label
        }
    }

    fun closeStudioTab() {
        onCloseTab()
    }

    fun triggerExport() {
        if (isExporting) return
        val s = viewerState ?: return
        onTimeRangeExport?.let { export ->
            export(s.trimRange(), selectedExportFormat)
            return
        }
        val handler = onExportRequested
        if (handler != null) {
            handler(s.inFrameIndex, s.outFrameIndex, selectedExportFormat)
        } else {
            onTrimExport(s.inFrameIndex, s.outFrameIndex)
        }
    }

    fun requestPngExport() {
        val s = viewerState ?: return
        val chooser = onChoosePngOutputPath
        if (chooser != null) {
            chooser(s.bundleId, s.currentFrameIndex) { chosen ->
                onPngExport(s.currentFrameIndex, chosen)
            }
        }
    }

    fun revealBundleInFinder() {
        viewerState?.path?.let(onRevealInFinder)
    }

    fun revealExportedInFinder() {
        viewerState?.exportCompletion?.outputPath?.let(onRevealInFinder)
    }

    fun copyExportedPath() {
        val path = viewerState?.exportCompletion?.outputPath ?: return
        clipboardWriter(path)
    }

    fun openExportedBundleInNewTab() {
        val info = viewerState?.exportCompletion ?: return
        val id = info.bundleId ?: info.outputPath
        val title = info.bundleId ?: runCatching { Path.of(info.outputPath).fileName.toString() }.getOrDefault(id)
        onOpenExportedBundle(id, title)
    }

    fun dismissExportCompletion() {
        viewerState = viewerState?.copy(exportCompletion = null)
    }

    fun jumpToSource(target: SourceJumpTarget) = onSourceJump(target)

    fun jumpToScreenSpan(span: ScreenSpan) = onSourceJump(SourceJumpParser.fromScreenSpan(span))

    fun triggerSnapInAtRow(rowIndex: Int) {
        syncedRows.getOrNull(rowIndex)?.frameIndex?.let { applyInFrame(it, settled = true) }
    }

    fun triggerSnapOutAtRow(rowIndex: Int) {
        syncedRows.getOrNull(rowIndex)?.frameIndex?.let { applyOutFrame(it, settled = true) }
    }

    fun triggerSourceJumpAtRow(rowIndex: Int) {
        syncedRows.getOrNull(rowIndex)?.sourceJumpTarget?.let(onSourceJump)
    }

    private fun rebuildSyncedRows(s: TimelineViewerState) {
        val structureKey = listOf(
            System.identityHashCode(s.mainLogs),
            System.identityHashCode(s.mainTaps),
            System.identityHashCode(s.crashes),
            System.identityHashCode(s.screens),
            System.identityHashCode(s.summary.epochs),
            System.identityHashCode(s.summary.epochFrameRanges),
            s.axis,
            s.frameCount,
            eventFilter,
            searchQuery,
        )
        if (structureKey != lastStructureKey) {
            lastStructureKey = structureKey
            lastFlagKey = listOf(s.inTimeNs, s.outTimeNs)
            syncedRows = s.buildSyncedEventRows(filter = eventFilter, keyword = searchQuery)
            return
        }
        val flagKey = listOf(s.inTimeNs, s.outTimeNs)
        if (flagKey == lastFlagKey) return
        lastFlagKey = flagKey
        syncedRows = syncedRows.map { row ->
            val inRange = s.isTimeInTrimRange(row.mediaTimeNs)
            if (row.isInTrimRange == inRange) {
                row
            } else {
                row.copy(isInTrimRange = inRange)
            }
        }
    }

    companion object {
        const val APPROXIMATE_BADGE_TEXT: String = APPROXIMATE_PREVIEW_BADGE_TEXT
        const val ALL_APPS_LABEL: String = "全アプリ"
        const val BURN_TAPS_TOGGLE_LABEL: String = "タップ・スワイプを動画に焼き込む"

        /**
         * Converts a decoded frame to a Compose [ImageBitmap] (a full pixel copy). Safe to call off
         * the EDT; returns `null` where Skiko natives are unavailable (headless JUnit).
         */
        fun toImageBitmapOrNull(image: BufferedImage): ImageBitmap? =
            runCatching { image.toComposeImageBitmap() }.getOrNull()

        /**
         * Stable, collision-free LazyColumn keys. Duplicate base keys (e.g. two crash markers from
         * the same source log line) would otherwise throw `IllegalArgumentException` inside
         * Compose and blank the whole Studio panel; repeats get a `#n` suffix.
         */
        fun <T> uniqueKeys(items: List<T>, baseKey: (T) -> String): List<String> {
            val seen = HashMap<String, Int>(items.size * 2)
            return items.map { item ->
                val base = baseKey(item)
                val n = seen.merge(base, 1, Int::plus) ?: 1
                if (n == 1) base else "$base#$n"
            }
        }

        fun syncedRowKey(row: SyncedEventRow): String = "${row.kind.name}-${row.seq}-${row.boottimeNs}"

        /** Studio header text when a bundle carries no device information. */
        const val UNKNOWN_DEVICE_LABEL: String = "端末不明"

        /**
         * Mirrors arb-core's map_touch_to_frame before scaling frame pixels to the canvas.
         */
        fun mapTapToCanvasPoint(
            tapX: Double,
            tapY: Double,
            drawRect: Rectangle,
            videoW: Int,
            videoH: Int,
            displayW: Int? = null,
            displayH: Int? = null,
        ): Pair<Int, Int>? {
            if (!tapX.isFinite() || !tapY.isFinite() || tapX < 0.0 || tapY < 0.0) return null
            if (videoW <= 0 || videoH <= 0) return null
            val (dw, dh) = when {
                displayW == null && displayH == null -> 1 to 1
                displayW != null && displayW > 0 && displayH != null && displayH > 0 -> displayW to displayH
                else -> return null
            }
            if (tapX > dw || tapY > dh) return null
            val frameX = (tapX / dw * videoW).coerceAtMost(videoW - 1.0)
            val frameY = (tapY / dh * videoH).coerceAtMost(videoH - 1.0)
            val cx = drawRect.x + (frameX * drawRect.width / videoW).toInt()
            val cy = drawRect.y + (frameY * drawRect.height / videoH).toInt()
            return cx to cy
        }
    }
}

private fun copyPathToClipboard(text: String) {
    runCatching {
        com.intellij.openapi.ide.CopyPasteManager.getInstance().setContents(StringSelection(text))
    }.onFailure {
        runCatching {
            Toolkit.getDefaultToolkit().systemClipboard.setContents(StringSelection(text), null)
        }
    }
}
