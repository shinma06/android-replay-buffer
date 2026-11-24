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
import io.github.shinma06.arb.state.EventStreamFilter
import io.github.shinma06.arb.state.activeTimelineItemIndex
import io.github.shinma06.arb.state.SourceJumpParser
import io.github.shinma06.arb.state.SourceJumpTarget
import io.github.shinma06.arb.state.SyncedEventKind
import io.github.shinma06.arb.state.SyncedEventRow
import io.github.shinma06.arb.state.TimelineViewerReducer
import io.github.shinma06.arb.state.TimelineViewerState
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
    private val onStepFrame: (delta: Int) -> Unit = {},
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
) {
    var viewerState: TimelineViewerState? by mutableStateOf(null)
        private set

    var playbackState: PlaybackState by mutableStateOf(PlaybackState())
        private set

    var frameBitmap: ImageBitmap? by mutableStateOf(null)
        private set

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

    var errorMessage: String? by mutableStateOf(null)
        private set

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
            val dev = s.summary.device?.let { "${it.model} (API ${it.apiLevel})" } ?: s.format.wire
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

    val timecodeText: String
        get() {
            val s = viewerState ?: return "00:00.00 / 00:00.00 · #0"
            val curMs = s.frameToRelativeMs(s.currentFrameIndex)
            return "${ArbFormatters.formatTimecode(curMs)} / ${ArbFormatters.formatTimecode(s.totalDurationMs)} · #${s.currentFrameIndex}"
        }

    val exportButtonLabel: String
        get() = viewerState?.exportCtaLabel ?: ArbFormatters.EXPORT_FULL_LABEL

    val activeTapOverlays: List<ActiveTapOverlay>
        get() = viewerState?.activeTapsAtFrame().orEmpty()

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
        viewerState = next
        errorMessage = null
        if (width > 0 && height > 0) {
            frameImageWidth = width
            frameImageHeight = height
            if (bitmap != null) frameBitmap = bitmap
        }
        approximatePreviewVisible = next.showApproximatePreviewBadge || playbackState.isApproximate
        rebuildSyncedRows(next)
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

    fun renderPlaybackState(pb: PlaybackState) {
        playbackState = pb
        approximatePreviewVisible = pb.isApproximate || (viewerState?.showApproximatePreviewBadge == true)
        val current = viewerState
        if (current != null) {
            val synced = current.copy(currentFrameIndex = pb.currentFrameIndex)
            viewerState = synced
            rebuildSyncedRows(synced)
        }
    }

    fun showError(message: String) {
        errorMessage = message
    }

    // ---- User actions from Jewel Compose UI & headless tests ----

    fun selectTab(tab: StudioRightTab) {
        activeTab = tab
    }

    fun selectDetailsTab() {
        activeTab = StudioRightTab.DETAILS
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
        val s = viewerState ?: return
        val next = TimelineViewerReducer.stepFrame(s, delta.toLong())
        viewerState = next
        rebuildSyncedRows(next)
        onStepFrame(delta)
        onSeekExact(next.currentFrameIndex)
        onRangeChanged(next.inFrameIndex, next.outFrameIndex, next.currentFrameIndex)
    }

    fun cycleSpeed() {
        val nextSpeed = when (playbackState.speed) {
            PlaybackSpeed.HALF -> PlaybackSpeed.NORMAL
            PlaybackSpeed.NORMAL -> PlaybackSpeed.DOUBLE
            PlaybackSpeed.DOUBLE -> PlaybackSpeed.HALF
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
        get() = activeTimelineItemIndex(syncedRows, viewerState?.currentFrameIndex ?: 0L)

    /** Explicitly sets [followPlayhead]. Turning it back ON snaps immediately (next recompose). */
    fun selectFollowPlayhead(enabled: Boolean) {
        followPlayhead = enabled
    }

    /** Flips [followPlayhead]. */
    fun toggleFollowPlayhead() {
        followPlayhead = !followPlayhead
    }

    fun scrubToFrame(frameIndex: Long) {
        val s = viewerState ?: return
        if (s.frameCount <= 0L) return
        val clamped = frameIndex.coerceIn(0L, s.frameCount - 1L)
        viewerState = s.copy(currentFrameIndex = clamped)
        rebuildSyncedRows(viewerState!!)
        onScrub(clamped)
    }

    fun seekExactFrame(frameIndex: Long) {
        val s = viewerState ?: return
        if (s.frameCount <= 0L) return
        val clamped = frameIndex.coerceIn(0L, s.frameCount - 1L)
        viewerState = s.copy(currentFrameIndex = clamped)
        rebuildSyncedRows(viewerState!!)
        onSeekExact(clamped)
        onRangeChanged(s.inFrameIndex, s.outFrameIndex, clamped)
    }

    fun applyInFrame(frameIndex: Long, settled: Boolean) {
        val s = viewerState ?: return
        val next = TimelineViewerReducer.setInFrame(s, frameIndex)
        viewerState = next
        rebuildSyncedRows(next)
        if (settled) {
            onRangeChanged(next.inFrameIndex, next.outFrameIndex, next.currentFrameIndex)
        }
    }

    fun applyOutFrame(frameIndex: Long, settled: Boolean) {
        val s = viewerState ?: return
        val next = TimelineViewerReducer.setOutFrame(s, frameIndex)
        viewerState = next
        rebuildSyncedRows(next)
        if (settled) {
            onRangeChanged(next.inFrameIndex, next.outFrameIndex, next.currentFrameIndex)
        }
    }

    fun markInAtPlayhead() {
        viewerState?.let { applyInFrame(it.currentFrameIndex, settled = true) }
    }

    fun markOutAtPlayhead() {
        viewerState?.let { applyOutFrame(it.currentFrameIndex, settled = true) }
    }

    fun selectTapRange() = onSelectTapRange()

    fun selectAroundCrash() = onSelectAroundCrash()

    fun resetFullRange() = onResetFullRange()

    fun toggleTrashOriginal(enabled: Boolean) {
        val s = viewerState ?: return
        val next = TimelineViewerReducer.setTrashOriginalAfterExport(s, enabled)
        viewerState = next
        onTrashOriginalToggled(next.effectiveTrashOriginalAfterExport)
    }

    fun triggerExport() {
        val s = viewerState ?: return
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
            System.identityHashCode(s.filmstrip),
            s.frameCount,
            eventFilter,
            searchQuery,
        )
        if (structureKey != lastStructureKey) {
            lastStructureKey = structureKey
            lastFlagKey = listOf(s.currentFrameIndex, s.inFrameIndex, s.outFrameIndex)
            syncedRows = s.buildSyncedEventRows(filter = eventFilter, keyword = searchQuery)
            return
        }
        val flagKey = listOf(s.currentFrameIndex, s.inFrameIndex, s.outFrameIndex)
        if (flagKey == lastFlagKey) return
        lastFlagKey = flagKey
        val hasRange = s.frameCount > 0L
        syncedRows = syncedRows.map { row ->
            val f = row.frameIndex
            val inRange = f == null || !hasRange || f in s.inFrameIndex..s.outFrameIndex
            val atPlayhead = f != null && f == s.currentFrameIndex
            if (row.isInTrimRange == inRange && row.isAtPlayhead == atPlayhead) {
                row
            } else {
                row.copy(isInTrimRange = inRange, isAtPlayhead = atPlayhead)
            }
        }
    }

    companion object {
        const val APPROXIMATE_BADGE_TEXT: String = APPROXIMATE_PREVIEW_BADGE_TEXT
        const val ALL_APPS_LABEL: String = "全アプリ"

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

        /**
         * Same rule as arbd/MCP: a coordinate beyond the physical display bounds (e.g. raw evdev
         * coordinates) or an orientation mismatch is skipped instead of being edge-clamped.
         */
        fun isTapUnmappable(
            tapX: Double,
            tapY: Double,
            videoW: Int,
            videoH: Int,
            displayW: Int?,
            displayH: Int?,
        ): Boolean {
            val dw = displayW?.takeIf { it > 0 } ?: return false
            val dh = displayH?.takeIf { it > 0 } ?: return false
            val isNormalized = tapX in 0.0..1.0 && tapY in 0.0..1.0
            if (isNormalized) return false
            if (videoW != videoH && dw != dh && (dw < dh) != (videoW < videoH)) return true
            return tapX > dw || tapY > dh
        }

        /**
         * Maps a recorded tap coordinate onto canvas pixels inside [drawRect].
         * Prefers physical display dimensions ([displayW] x [displayH]) when present so touch points
         * land accurately on downscaled video; falls back to video dimensions for legacy bundles.
         */
        fun mapTapToCanvasPoint(
            tapX: Double,
            tapY: Double,
            drawRect: Rectangle,
            videoW: Int,
            videoH: Int,
            displayW: Int? = null,
            displayH: Int? = null,
        ): Pair<Int, Int> {
            val isNormalized = tapX in 0.0..1.0 && tapY in 0.0..1.0
            val srcW = (displayW?.takeIf { it > 0 } ?: videoW).coerceAtLeast(1)
            val srcH = (displayH?.takeIf { it > 0 } ?: videoH).coerceAtLeast(1)
            val normX = if (isNormalized) tapX else (tapX / srcW.toDouble())
            val normY = if (isNormalized) tapY else (tapY / srcH.toDouble())
            val cx = drawRect.x + (normX.coerceIn(0.0, 1.0) * drawRect.width).toInt()
            val cy = drawRect.y + (normY.coerceIn(0.0, 1.0) * drawRect.height).toInt()
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
