package io.github.shinma06.arb.state

import io.github.shinma06.arb.model.AdbDeviceInfo
import io.github.shinma06.arb.model.AppLogSummary
import io.github.shinma06.arb.model.ArbConfig
import io.github.shinma06.arb.model.AudioCaptureState
import io.github.shinma06.arb.model.AudioStreamMeta
import io.github.shinma06.arb.model.AvOffsetStatus
import io.github.shinma06.arb.model.BundleEntryView
import io.github.shinma06.arb.model.BundleFormat
import io.github.shinma06.arb.model.BundleKind
import io.github.shinma06.arb.model.BundleLifecycleState
import io.github.shinma06.arb.model.CaptureMode
import io.github.shinma06.arb.model.CaptureStatusView
import io.github.shinma06.arb.model.CatalogFormatVersion
import io.github.shinma06.arb.model.CrashMarker
import io.github.shinma06.arb.model.DiscontinuityEvent
import io.github.shinma06.arb.model.DiscontinuityKind
import io.github.shinma06.arb.model.EpochFrameRange
import io.github.shinma06.arb.model.FilmstripEntryMeta
import io.github.shinma06.arb.model.GestureKind
import io.github.shinma06.arb.model.IdeDefaultSaveAction
import io.github.shinma06.arb.model.LogCompleteness
import io.github.shinma06.arb.model.LogRecord
import io.github.shinma06.arb.model.LogcatStreamMeta
import io.github.shinma06.arb.model.Provenance
import io.github.shinma06.arb.model.RecordBucket
import io.github.shinma06.arb.model.SaveJobState
import io.github.shinma06.arb.model.SaveJobView
import io.github.shinma06.arb.model.ScreenSpan
import io.github.shinma06.arb.model.StreamStatus
import io.github.shinma06.arb.model.SystemStatusResponse
import io.github.shinma06.arb.model.TapRecord
import io.github.shinma06.arb.model.TimeAlignment
import io.github.shinma06.arb.model.TimelineSummaryResponse
import io.github.shinma06.arb.model.TimestampSemantics
import io.github.shinma06.arb.model.VideoStreamMeta
import io.github.shinma06.arb.shm.ShmFrameMeta
import kotlin.math.abs
import kotlin.math.roundToLong

const val APPROXIMATE_PREVIEW_BADGE_TEXT: String = "近似プレビュー（位置確定前）"
const val LEGACY_READ_ONLY_BADGE_TEXT: String = "Legacy (RO)"
const val UNANCHORED_TAB_TITLE: String = "時刻不明"
const val TAP_LOG_CORRELATION_WINDOW_NS: Long = 300_000_000L // 0..300ms
const val TAP_OVERLAY_WINDOW_NS: Long = 250_000_000L // +/-250ms

// ---------------- Capture Bar & Live Status Banners ----------------

enum class CaptureBannerKind {
    STORAGE_PRESSURE_STOP,
    CONNECTION_LOST,
    STREAM_GAP,
    STATIC_HOLD,
    CONFIG_WARNING,
}

data class CaptureBanner(
    val kind: CaptureBannerKind,
    val title: String,
    val detail: String,
)

data class CaptureBarState(
    val daemonConnected: Boolean = false,
    val daemonPid: Long? = null,
    val devices: List<AdbDeviceInfo> = emptyList(),
    val selectedSerial: String? = null,
    val autoSelectedSerial: String? = null,
    val deviceSelection: DeviceSelectionOutcome = DeviceSelectionOutcome.NoOnlineDevices,
    val capture: CaptureStatusView? = null,
    val config: ArbConfig = ArbConfig(),
    val ringChunkCount: Long = 0,
    val ringTotalBytes: Long = 0,
    val ringPinnedChunkCount: Long = 0,
    val ringBufferedSecs: Long? = null,
    val saveDurationSec: Long = ArbConfig.DEFAULT_SAVE_WINDOW_SECS.toLong(),
    val banners: List<CaptureBanner> = emptyList(),
    val lastStatusMessage: String? = null,
) {
    val activeCaptureMode: CaptureMode
        get() = if (capture?.active == true) capture.captureMode else config.captureMode

    val isLogsOnlyMode: Boolean
        get() = activeCaptureMode == CaptureMode.LOGS_ONLY

    val audioUnavailableReason: String?
        get() = capture?.takeIf { it.effectiveAudioState == AudioCaptureState.UNAVAILABLE }?.audioReason
            ?.takeIf { it.isNotBlank() }

    val multipleDevicesNeedSelection: Boolean
        get() = deviceSelection is DeviceSelectionOutcome.MultipleDevicesNeedSelection

    val saveWindowSecs: Int
        get() = config.saveWindowSecs

    val defaultSaveAction: IdeDefaultSaveAction
        get() = config.ideDefaultSaveAction

    val bufferedWindowSecs: Long
        get() {
            ringBufferedSecs?.let { return it.coerceIn(0L, saveWindowSecs.toLong()) }
            val frames = capture?.videoFrames ?: 0L
            if (frames <= 0L) return 0L
            val fps = config.videoMaxFps.coerceAtLeast(1).toLong()
            return (frames / fps).coerceIn(0L, saveWindowSecs.toLong())
        }

    val ringFillRatio: Double
        get() = if (saveWindowSecs <= 0) 0.0 else (bufferedWindowSecs.toDouble() / saveWindowSecs.toDouble()).coerceIn(0.0, 1.0)

    val ringMeterText: String
        get() {
            val base = ArbFormatters.formatRingMeter(bufferedWindowSecs, saveWindowSecs, ringTotalBytes)
            return if (isRecordingHealthy && ringBufferedSecs != null && ringBufferedSecs < saveWindowSecs.toLong()) {
                "$base (蓄積中)"
            } else {
                base
            }
        }

    val primarySaveLabel: String
        get() = ArbFormatters.formatPrimarySaveLabel(saveWindowSecs, defaultSaveAction)

    val secondarySaveLabel: String
        get() = ArbFormatters.formatSecondarySaveLabel(defaultSaveAction)

    val hasOnlineDevice: Boolean
        get() = devices.any { it.state == io.github.shinma06.arb.model.AdbDeviceState.Device }

    val selectedDevice: AdbDeviceInfo?
        get() = selectedSerial?.let { target -> devices.firstOrNull { it.serial == target } }

    val isSelectedDeviceOnline: Boolean
        get() = selectedDevice?.state == io.github.shinma06.arb.model.AdbDeviceState.Device

    val statusBadgeText: String
        get() = when {
            !daemonConnected -> "未接続"
            capture == null -> deviceIdleStatusText()
            capture.storagePressureStopped -> "容量停止"
            capture.active && capture.connected -> if (capture.captureMode == CaptureMode.LOGS_ONLY) "REC (ログのみ)" else "REC"
            capture.active && !capture.connected -> "再接続中"
            else -> deviceIdleStatusText()
        }

    private fun deviceIdleStatusText(): String {
        if (deviceSelection is DeviceSelectionOutcome.MultipleDevicesNeedSelection) return "端末を選択"
        if (deviceSelection is DeviceSelectionOutcome.WaitingForPreferredDevice) return "再接続待機中"
        if (hasOnlineDevice || devices.isEmpty()) return "待機中"
        if (devices.any { it.state == io.github.shinma06.arb.model.AdbDeviceState.Unauthorized }) return "未承認"
        if (devices.any { it.state == io.github.shinma06.arb.model.AdbDeviceState.Offline }) return "オフライン"
        return "待機中"
    }

    val isRecordingHealthy: Boolean
        get() = daemonConnected && capture?.active == true && capture.connected && !capture.storagePressureStopped

    val hasVisibleBanners: Boolean
        get() = banners.isNotEmpty()
}

object CaptureBarReducer {
    fun fromStatus(
        previous: CaptureBarState,
        status: SystemStatusResponse,
        devices: List<AdbDeviceInfo> = previous.devices,
        liveDiscontinuities: List<DiscontinuityEvent> = emptyList(),
        config: ArbConfig = previous.config,
    ): CaptureBarState {
        val banners = buildList {
            // STORAGE_PRESSURE_STOP must remain strictly distinct from CONNECTION_LOST!
            if (status.capture.storagePressureStopped) {
                add(
                    CaptureBanner(
                        kind = CaptureBannerKind.STORAGE_PRESSURE_STOP,
                        title = "STORAGE_PRESSURE_STOP: 空き容量不足のため新規取得を停止しました",
                        detail = "保存済みバンドルおよびピン留め済みチャンクは保護されています。空き容量確保後に再開してください。",
                    ),
                )
            }
            if (status.capture.active && !status.capture.connected && !status.capture.storagePressureStopped) {
                add(
                    CaptureBanner(
                        kind = CaptureBannerKind.CONNECTION_LOST,
                        title = "USB/ADB 切断 — 自動再接続待機中（ギャップ記録中）",
                        detail = "切断前のリングバッファはそのまま保存できます。",
                    ),
                )
            }
            if (liveDiscontinuities.any { it.kind == DiscontinuityKind.STREAM_GAP }) {
                add(
                    CaptureBanner(
                        kind = CaptureBannerKind.STREAM_GAP,
                        title = "STREAM_GAP: 一部ストリームに欠損区間があります",
                        detail = "映像・音声・ログのいずれかで欠損が観測されました。保存結果にも欠損区間として記録されます。",
                    ),
                )
            }
            if (status.capture.staticHoldEvents > 0L) {
                add(
                    CaptureBanner(
                        kind = CaptureBannerKind.STATIC_HOLD,
                        title = "STATIC_HOLD: 静止画面を継続検出中（${status.capture.staticHoldEvents} 回）",
                        detail = "端末ビーコンは正常応答しており、切断ではなく画面変化なしの静止状態です。",
                    ),
                )
            }
            if (!status.configWarning.isNullOrBlank()) {
                add(
                    CaptureBanner(
                        kind = CaptureBannerKind.CONFIG_WARNING,
                        title = "設定ファイル警告（既定値で稼働中）",
                        detail = status.configWarning,
                    ),
                )
            }
            if (!status.capture.lastVideoError.isNullOrBlank()) {
                add(
                    CaptureBanner(
                        kind = CaptureBannerKind.STREAM_GAP,
                        title = "映像エンコーダの警告",
                        detail = status.capture.lastVideoError,
                    ),
                )
            }
            if (!status.capture.deviceApiWarning.isNullOrBlank()) {
                add(
                    CaptureBanner(
                        kind = CaptureBannerKind.CONFIG_WARNING,
                        title = "非サポート Android バージョン（ベストエフォート稼働中）",
                        detail = status.capture.deviceApiWarning,
                    ),
                )
            }
            if (status.capture.effectiveAudioState == AudioCaptureState.UNAVAILABLE &&
                !status.capture.audioReason.isNullOrBlank()
            ) {
                add(
                    CaptureBanner(
                        kind = CaptureBannerKind.CONFIG_WARNING,
                        title = "音声キャプチャ非対応（映像＋ログで継続中）",
                        detail = status.capture.audioReason,
                    ),
                )
            }
        }

        // Strict no-auto-switch policy:
        // - If a target serial is explicitly bound (manually chosen by the user, persisted in
        //   `config.deviceSerial`, or actively capturing), stay bound to it even if it goes
        //   offline while other devices are online.
        // - If the previous serial was only auto-selected for a sole idle device (never explicitly
        //   pinned by the user), re-evaluate against the current device list: 1 online device
        //   auto-selects cleanly; 2+ online devices enter MultipleDevicesNeedSelection (NEVER
        //   auto-select); 1 unauthorized/offline sole device stays visible in the dropdown with
        //   actions disabled until it comes online.
        val explicitSelectedSerial = previous.selectedSerial
            ?.takeIf { it.isNotBlank() && it != previous.autoSelectedSerial }
        val boundSerial = explicitSelectedSerial
            ?: config.deviceSerial?.takeIf { it.isNotBlank() }
            ?: status.capture.serial?.takeIf { status.capture.active && it.isNotBlank() }
        val selection = DeviceSelectionPolicy.evaluate(boundSerial, devices)
        val (resolvedSerial, nextAutoSelectedSerial) = when (selection) {
            is DeviceSelectionOutcome.Selected ->
                selection.serial to if (selection.autoSelected) selection.serial else null
            is DeviceSelectionOutcome.WaitingForPreferredDevice ->
                selection.preferredSerial to null
            is DeviceSelectionOutcome.MultipleDevicesNeedSelection ->
                null to null
            DeviceSelectionOutcome.NoOnlineDevices -> when {
                boundSerial != null -> boundSerial to null
                devices.size == 1 -> devices.first().serial to devices.first().serial
                else -> null to null
            }
        }
        val finalBanners = if (selection is DeviceSelectionOutcome.MultipleDevicesNeedSelection) {
            banners + CaptureBanner(
                kind = CaptureBannerKind.CONFIG_WARNING,
                title = "複数端末が接続されています（${selection.onlineSerials.size} 台）",
                detail = "自動切替は行いません。対象端末を選択してから録画を開始してください。",
            )
        } else {
            banners
        }

        return previous.copy(
            daemonConnected = true,
            daemonPid = status.pid,
            devices = devices,
            selectedSerial = resolvedSerial,
            autoSelectedSerial = nextAutoSelectedSerial,
            deviceSelection = selection,
            capture = status.capture,
            config = config,
            saveDurationSec = config.saveWindowSecs.toLong(),
            ringChunkCount = status.ringChunkCount,
            ringTotalBytes = status.ringTotalBytes,
            ringPinnedChunkCount = status.ringPinnedChunkCount,
            ringBufferedSecs = status.bufferedSecs,
            banners = finalBanners,
        )
    }

    fun withConfig(previous: CaptureBarState, config: ArbConfig): CaptureBarState = previous.copy(
        config = config,
        saveDurationSec = config.saveWindowSecs.toLong(),
    )

    fun disconnected(previous: CaptureBarState, reason: String): CaptureBarState = previous.copy(
        daemonConnected = false,
        daemonPid = null,
        lastStatusMessage = reason,
    )
}

// ---------------- Failed Save Recovery State ----------------

data class FailedSaveRecoveryState(
    val jobs: List<SaveJobView> = emptyList(),
    val busyJobId: String? = null,
    val lastMessage: String? = null,
) {
    val retryableJobs: List<SaveJobView>
        get() = jobs.filter { it.state == SaveJobState.FAILED_RETRYABLE }

    val hasRecoverableFailures: Boolean
        get() = retryableJobs.isNotEmpty()
}

object FailedSaveRecoveryReducer {
    fun withJobs(previous: FailedSaveRecoveryState, jobs: List<SaveJobView>): FailedSaveRecoveryState =
        previous.copy(jobs = jobs, busyJobId = null)

    fun markBusy(previous: FailedSaveRecoveryState, jobId: String): FailedSaveRecoveryState =
        previous.copy(busyJobId = jobId)

    fun applyJobUpdated(
        previous: FailedSaveRecoveryState,
        updated: SaveJobView,
        message: String? = null,
    ): FailedSaveRecoveryState {
        val replaced = previous.jobs.map { if (it.jobId == updated.jobId) updated else it }
        val nextJobs = if (replaced.any { it.jobId == updated.jobId }) replaced else replaced + updated
        return previous.copy(jobs = nextJobs, busyJobId = null, lastMessage = message)
    }
}

// ---------------- Bundle Catalog State & Filterable Library ----------------

enum class BundleFilter(val label: String) {
    ALL("すべて"),
    TRIMMED("トリム済み"),
    CRASH("クラッシュ"),
    TRASH("ゴミ箱"),
}

data class BundleCatalogItem(
    val bundleId: String,
    val path: String,
    val isLegacyReadOnly: Boolean,
    val state: BundleLifecycleState,
    val activeLeases: Int,
    val createdAtUtc: String,
    val parentBundleId: String?,
    val durationSecs: Long? = null,
    val hasCrash: Boolean = false,
    val bundleKind: BundleKind = BundleKind.FULL,
    val displayTitle: String = bundleId,
) {
    val formatBadge: String
        get() = if (isLegacyReadOnly) LEGACY_READ_ONLY_BADGE_TEXT else "ARB"

    val isTrimmed: Boolean
        get() = !parentBundleId.isNullOrBlank()

    val semanticTags: List<String>
        get() = buildList {
            if (isTrimmed) add("トリム済み")
            if (hasCrash) add("クラッシュ")
            if (bundleKind == BundleKind.LOGS_ONLY) add("ログのみ")
            if (isLegacyReadOnly) add(LEGACY_READ_ONLY_BADGE_TEXT)
            if (isProtectedByLease) add("使用中")
        }

    val durationText: String
        get() = durationSecs?.let { ArbFormatters.formatSeconds(it) } ?: "--"

    val canTrash: Boolean
        get() = state == BundleLifecycleState.ACTIVE

    val canRestore: Boolean
        get() = state == BundleLifecycleState.TRASHED

    val isProtectedByLease: Boolean
        get() = activeLeases > 0
}

data class BundleCatalogState(
    val items: List<BundleCatalogItem> = emptyList(),
    val showTrashed: Boolean = true,
    val activeFilter: BundleFilter = BundleFilter.ALL,
) {
    val visibleItems: List<BundleCatalogItem>
        get() = if (showTrashed) items else items.filter { it.state == BundleLifecycleState.ACTIVE }

    /** Items matching the active Library filter chip (`すべて` / `トリム済み` / `クラッシュ` / `ゴミ箱`). */
    val filteredItems: List<BundleCatalogItem>
        get() = when (activeFilter) {
            BundleFilter.ALL -> items.filter { it.state == BundleLifecycleState.ACTIVE }
            BundleFilter.TRIMMED -> items.filter { it.state == BundleLifecycleState.ACTIVE && it.isTrimmed }
            BundleFilter.CRASH -> items.filter { it.state == BundleLifecycleState.ACTIVE && it.hasCrash }
            BundleFilter.TRASH -> items.filter { it.state == BundleLifecycleState.TRASHED }
        }

    fun countFor(filter: BundleFilter): Int = when (filter) {
        BundleFilter.ALL -> items.count { it.state == BundleLifecycleState.ACTIVE }
        BundleFilter.TRIMMED -> items.count { it.state == BundleLifecycleState.ACTIVE && it.isTrimmed }
        BundleFilter.CRASH -> items.count { it.state == BundleLifecycleState.ACTIVE && it.hasCrash }
        BundleFilter.TRASH -> items.count { it.state == BundleLifecycleState.TRASHED }
    }

    companion object {
        fun fromEntries(
            entries: List<BundleEntryView>,
            showTrashed: Boolean = true,
            activeFilter: BundleFilter = BundleFilter.ALL,
            crashBundleIds: Set<String> = emptySet(),
            durationsByBundleId: Map<String, Long> = emptyMap(),
            kindsByBundleId: Map<String, BundleKind> = emptyMap(),
        ): BundleCatalogState = BundleCatalogState(
            items = entries.map { e ->
                BundleCatalogItem(
                    bundleId = e.bundleId,
                    path = e.path,
                    isLegacyReadOnly = e.formatVersion == CatalogFormatVersion.LEGACY_RO,
                    state = e.state,
                    activeLeases = e.activeLeases,
                    createdAtUtc = e.createdAtUtc,
                    parentBundleId = e.parentBundleId,
                    durationSecs = durationsByBundleId[e.bundleId],
                    hasCrash = e.bundleId in crashBundleIds,
                    bundleKind = kindsByBundleId[e.bundleId] ?: BundleKind.FULL,
                )
            },
            showTrashed = showTrashed,
            activeFilter = activeFilter,
        )
    }
}

// ---------------- Studio Tier-2 Honesty Chips, Tap Overlay & Event Correlation ----------------

enum class HonestyChipKind {
    AUDIO_OFFSET,
    LOG_COMPLETENESS,
    DISCONTINUITY_GAP,
    LEGACY_READ_ONLY,
}

enum class HonestySeverity {
    INFO,
    WARNING,
    ERROR,
}

data class Tier2HonestyChip(
    val kind: HonestyChipKind,
    val label: String,
    val detail: String,
    val severity: HonestySeverity,
)

enum class EventStreamFilter(val label: String) {
    ALL("すべて"),
    TAPS("タップ"),
    ERRORS_CRASHES("エラー・クラッシュ"),
}

enum class SyncedEventKind {
    TAP,
    CRASH,
    SCREEN_TRANSITION,
    LOG,
}

data class SyncedEventRow(
    val kind: SyncedEventKind,
    val seq: Long,
    val boottimeNs: Long?,
    val relativeMs: Long?,
    val frameIndex: Long?,
    val title: String,
    val detail: String,
    val level: String? = null,
    val tag: String? = null,
    val isInTrimRange: Boolean = true,
    val isAtPlayhead: Boolean = false,
    val correlatedTapIndex: Int? = null,
    val sourceJumpTarget: SourceJumpTarget? = null,
)

/**
 * Pure computation for the right-hand Event/Log panel's video <-> timeline auto-follow sync
 * (Owner trial feedback T-6H item 4): finds the index inside [rows] of the row most relevant to
 * [playheadFrameIndex] — the latest row at or before the playhead, falling back to the nearest
 * upcoming row within [toleranceFrames] when nothing has happened yet. Returns `null` for an
 * empty [rows] (nothing to highlight/scroll to) or when no row lies within tolerance.
 *
 * Independent of list order and of [SyncedEventRow.isInTrimRange]/filtering — callers pass
 * whichever (already-filtered) row list is currently visible.
 */
fun activeTimelineItemIndex(
    rows: List<SyncedEventRow>,
    playheadFrameIndex: Long,
    toleranceFrames: Long = 30L,
): Int? {
    if (rows.isEmpty()) return null

    var bestAtOrBeforeIdx: Int? = null
    var bestAtOrBeforeFrame = Long.MIN_VALUE
    var nearestUpcomingIdx: Int? = null
    var nearestUpcomingDist = Long.MAX_VALUE

    for ((idx, row) in rows.withIndex()) {
        val f = row.frameIndex ?: continue
        if (f <= playheadFrameIndex) {
            if (f > bestAtOrBeforeFrame) {
                bestAtOrBeforeFrame = f
                bestAtOrBeforeIdx = idx
            }
        } else {
            val dist = f - playheadFrameIndex
            if (dist < nearestUpcomingDist) {
                nearestUpcomingDist = dist
                nearestUpcomingIdx = idx
            }
        }
    }

    bestAtOrBeforeIdx?.let { return it }
    return nearestUpcomingIdx?.takeIf { nearestUpcomingDist <= toleranceFrames }
}

data class ActiveTapOverlay(
    val tapIndex1Based: Int,
    val x: Double,
    val y: Double,
    val deltaMs: Long,
    val label: String,
    /** `Tap` renders a ripple; `Swipe` renders a polyline + arrowhead (T-6H item 2). */
    val gestureKind: GestureKind = GestureKind.TAP,
    val endX: Double? = null,
    val endY: Double? = null,
)

data class ExportCompletionInfo(
    val summaryText: String,
    val outputPath: String,
    val bundleId: String? = null,
    val isTrimBundle: Boolean = false,
)

// ---------------- Timeline & Video Viewport State ----------------

data class TimelineViewerState(
    val bundleId: String,
    val path: String,
    val format: BundleFormat,
    val isLegacy: Boolean,
    val leaseId: String,
    val viewportId: String? = null,
    val summary: TimelineSummaryResponse,
    val frameCount: Long,
    val currentFrameIndex: Long = 0L,
    val inFrameIndex: Long = 0L,
    val outFrameIndex: Long = (summary.frameCount - 1).coerceAtLeast(0).toLong(),
    val currentFrameMeta: ShmFrameMeta? = null,
    val filmstrip: List<FilmstripEntryMeta> = emptyList(),
    val discontinuities: List<DiscontinuityEvent> = emptyList(),
    val crashes: List<CrashMarker> = emptyList(),
    val screens: List<ScreenSpan> = emptyList(),
    val mainLogs: List<LogRecord> = emptyList(),
    val unanchoredLogs: List<LogRecord> = emptyList(),
    val mainTaps: List<TapRecord> = emptyList(),
    val unanchoredTaps: List<TapRecord> = emptyList(),
    val trashOriginalAfterExport: Boolean = false,
    val showTapOverlay: Boolean = true,
    val exportCompletion: ExportCompletionInfo? = null,
    val lastActionMessage: String? = null,
) {
    /**
     * Mandatory ARB requirement (`SPEC-03`): show approximate-preview badge whenever `isApproximate == true`,
     * and clear ONLY when a settled frame with `isApproximate == false` arrives.
     */
    val showApproximatePreviewBadge: Boolean
        get() = currentFrameMeta?.isApproximate == true

    val approximateBadgeText: String?
        get() = if (showApproximatePreviewBadge) APPROXIMATE_PREVIEW_BADGE_TEXT else null

    /** Non-destructive Trash-Original option is strictly disabled for Legacy (RO) bundles! */
    val canTrashOriginalAfterExport: Boolean
        get() = !isLegacy

    val effectiveTrashOriginalAfterExport: Boolean
        get() = trashOriginalAfterExport && !isLegacy

    val canSelectTapRange: Boolean
        get() = mainTaps.isNotEmpty() && frameCount > 0L

    val canSelectAroundCrash: Boolean
        get() = crashes.isNotEmpty() && frameCount > 0L

    /** Per-app extracted foreground log summaries (`app-logs/<package>.jsonl`). */
    val appLogs: List<AppLogSummary>
        get() = summary.appLogs

    /** `true` when this bundle was captured/saved in `logs_only` mode without a video track. */
    val isLogsOnlyBundle: Boolean
        get() = summary.bundleKind == BundleKind.LOGS_ONLY

    val unanchoredTruncatedCount: Long
        get() = summary.unanchoredSummary.truncatedCount

    val unanchoredTruncatedNotice: String?
        get() = if (unanchoredTruncatedCount > 0L) {
            "時刻不明の記録は上限超過のため $unanchoredTruncatedCount 件が省略されました。"
        } else {
            null
        }

    /** First known device boottime anchor in ns, used for deterministic frame <-> timestamp mapping. */
    val timelineStartBoottimeNs: Long
        get() {
            summary.epochs.firstOrNull()?.let { return it.startBoottimeNs }
            filmstrip.firstOrNull()?.let { return it.ptsNs }
            return 0L
        }

    /** End boottime anchor in ns. */
    val timelineEndBoottimeNs: Long
        get() {
            summary.epochs.lastOrNull()?.let { return it.endBoottimeNs }
            val fallbackDurNs = (frameCount - 1L).coerceAtLeast(0L) * DEFAULT_FRAME_STEP_NS
            return timelineStartBoottimeNs + fallbackDurNs
        }

    val totalDurationMs: Long
        get() {
            if (frameCount <= 1L) return 0L
            if (summary.epochFrameRanges.size > 1) {
                val sumNs = summary.epochFrameRanges.sumOf { (it.endBoottimeNs - it.startBoottimeNs).coerceAtLeast(0L) }
                if (sumNs > 0L) return sumNs / 1_000_000L
            }
            if (summary.epochs.size > 1) {
                val sumNs = summary.epochs.sumOf { (it.endBoottimeNs - it.startBoottimeNs).coerceAtLeast(0L) }
                if (sumNs > 0L) return sumNs / 1_000_000L
            }
            val spanNs = timelineEndBoottimeNs - timelineStartBoottimeNs
            return if (spanNs > 0L) spanNs / 1_000_000L else ((frameCount - 1L) * DEFAULT_FRAME_STEP_NS) / 1_000_000L
        }

    val estimatedFps: Double
        get() {
            val durMs = totalDurationMs
            return if (frameCount > 1L && durMs > 0L) {
                ((frameCount - 1L).toDouble() * 1_000.0 / durMs.toDouble()).coerceIn(1.0, 120.0)
            } else {
                30.0
            }
        }

    fun frameToRelativeMs(frameIndex: Long): Long {
        if (frameCount <= 1L) return 0L
        val clamped = frameIndex.coerceIn(0L, frameCount - 1L)
        if (summary.epochFrameRanges.size > 1) {
            var accNs = 0L
            for (ep in summary.epochFrameRanges) {
                val epDurNs = (ep.endBoottimeNs - ep.startBoottimeNs).coerceAtLeast(0L)
                if (clamped in ep.startFrameIndex..ep.endFrameIndex) {
                    val epFrames = ep.endFrameIndex - ep.startFrameIndex
                    val withinNs = if (epFrames > 0L) {
                        ((clamped - ep.startFrameIndex).toDouble() / epFrames.toDouble() * epDurNs.toDouble()).roundToLong()
                    } else {
                        0L
                    }
                    return (accNs + withinNs) / 1_000_000L
                }
                accNs += epDurNs
            }
        }
        return ((clamped.toDouble() / (frameCount - 1L).toDouble()) * totalDurationMs.toDouble()).roundToLong()
    }

    fun frameToBoottimeNs(frameIndex: Long): Long {
        if (summary.epochFrameRanges.isNotEmpty() && frameCount > 0L) {
            val clamped = frameIndex.coerceIn(0L, frameCount - 1L)
            val ep = epochRangeForFrame(clamped)
            if (ep != null) {
                val epFrames = ep.endFrameIndex - ep.startFrameIndex
                val epSpanNs = (ep.endBoottimeNs - ep.startBoottimeNs).coerceAtLeast(0L)
                if (epFrames <= 0L || epSpanNs <= 0L) return ep.startBoottimeNs
                val ratio = ((clamped - ep.startFrameIndex).toDouble() / epFrames.toDouble()).coerceIn(0.0, 1.0)
                return ep.startBoottimeNs + (ratio * epSpanNs.toDouble()).roundToLong()
            }
        }
        return timelineStartBoottimeNs + frameToRelativeMs(frameIndex) * 1_000_000L
    }

    fun boottimeNsToFrameIndex(boottimeNs: Long, bootId: String? = null): Long? {
        if (frameCount <= 0L) return null
        if (summary.epochFrameRanges.isNotEmpty()) {
            if (bootId != null) {
                val matched = summary.epochFrameRanges.firstOrNull { it.bootId == bootId }
                    ?: return null // Strict per-epoch isolation: never bleed into another boot epoch
                return interpolateFrameInEpoch(matched, boottimeNs)
            }
            val containingAll = summary.epochFrameRanges.filter {
                boottimeNs in it.startBoottimeNs..it.endBoottimeNs
            }
            // Boottime restarts after a reboot, so ranges can overlap: without a boot id an
            // ambiguous timestamp must not be guessed into one epoch (cross-epoch bleed).
            if (containingAll.size > 1) return null
            val containing = containingAll.firstOrNull()
            if (containing != null) {
                return interpolateFrameInEpoch(containing, boottimeNs)
            }
            if (summary.epochFrameRanges.size == 1) {
                return interpolateFrameInEpoch(summary.epochFrameRanges.first(), boottimeNs)
            }
            val nearest = summary.epochFrameRanges.minByOrNull { ep ->
                when {
                    boottimeNs < ep.startBoottimeNs -> ep.startBoottimeNs - boottimeNs
                    boottimeNs > ep.endBoottimeNs -> boottimeNs - ep.endBoottimeNs
                    else -> 0L
                }
            }
            if (nearest != null) {
                return interpolateFrameInEpoch(nearest, boottimeNs)
            }
        }
        if (frameCount == 1L) return 0L
        // Fallback for older daemons/fixtures without epochFrameRanges: map through filmstrip PTS.
        if (summary.epochs.size > 1 && filmstrip.size >= 2) {
            return boottimeNsToFrameIndexViaFilmstrip(boottimeNs)
        }
        val start = timelineStartBoottimeNs
        val end = timelineEndBoottimeNs
        val span = end - start
        if (span <= 0L) return 0L
        val ratio = ((boottimeNs - start).toDouble() / span.toDouble()).coerceIn(0.0, 1.0)
        return (ratio * (frameCount - 1L).toDouble()).roundToLong().coerceIn(0L, frameCount - 1L)
    }

    private fun interpolateFrameInEpoch(epoch: EpochFrameRange, boottimeNs: Long): Long {
        val maxFrame = (frameCount - 1L).coerceAtLeast(0L)
        val loFrame = epoch.startFrameIndex.coerceIn(0L, maxFrame)
        val hiFrame = epoch.endFrameIndex.coerceIn(loFrame, maxFrame)
        if (loFrame == hiFrame) return loFrame
        val span = epoch.endBoottimeNs - epoch.startBoottimeNs
        if (span <= 0L) return loFrame
        val ratio = ((boottimeNs - epoch.startBoottimeNs).toDouble() / span.toDouble()).coerceIn(0.0, 1.0)
        return (loFrame + (ratio * (hiFrame - loFrame).toDouble()).roundToLong()).coerceIn(loFrame, hiFrame)
    }

    private fun epochRangeForFrame(frameIndex: Long): EpochFrameRange? =
        summary.epochFrameRanges.firstOrNull { frameIndex in it.startFrameIndex..it.endFrameIndex }

    private fun epochOrderIndex(bootId: String?, boottimeNs: Long? = null): Int {
        if (bootId == null) {
            // Without a boot id fall back to the unique epoch containing the row's boottime, or
            // epoch 0 for single-epoch bundles, so such rows keep their time order.
            if (summary.epochFrameRanges.size <= 1) return summary.epochFrameRanges.firstOrNull()?.epochIndex ?: 0
            if (boottimeNs == null) return Int.MAX_VALUE
            val containing = summary.epochFrameRanges.filter { boottimeNs in it.startBoottimeNs..it.endBoottimeNs }
            return containing.singleOrNull()?.epochIndex ?: Int.MAX_VALUE
        }
        summary.epochFrameRanges.firstOrNull { it.bootId == bootId }?.let { return it.epochIndex }
        val idx = summary.epochs.indexOfFirst { it.bootId == bootId }
        return if (idx >= 0) idx else Int.MAX_VALUE
    }

    private fun boottimeToRelativeMs(boottimeNs: Long, bootId: String?): Long {
        if (summary.epochFrameRanges.size > 1 && bootId != null) {
            var accNs = 0L
            for (ep in summary.epochFrameRanges) {
                if (ep.bootId == bootId) {
                    return (accNs + (boottimeNs - ep.startBoottimeNs).coerceAtLeast(0L)) / 1_000_000L
                }
                accNs += (ep.endBoottimeNs - ep.startBoottimeNs).coerceAtLeast(0L)
            }
        }
        return ((boottimeNs - timelineStartBoottimeNs) / 1_000_000L).coerceAtLeast(0L)
    }

    private fun boottimeNsToFrameIndexViaFilmstrip(boottimeNs: Long): Long {
        val entries = filmstrip.sortedBy { it.frameIndex }
        val maxFrame = frameCount - 1L
        val upperIdx = entries.indexOfFirst { it.ptsNs >= boottimeNs }
        val result = when {
            upperIdx < 0 -> entries.last().frameIndex
            upperIdx == 0 -> entries.first().frameIndex
            else -> {
                val lo = entries[upperIdx - 1]
                val hi = entries[upperIdx]
                val span = hi.ptsNs - lo.ptsNs
                // A PTS jump across an epoch boundary must not be interpolated: snap to the nearer frame.
                if (span <= 0L || hi.ptsNs - boottimeNs > boottimeNs - lo.ptsNs && span > 2_000_000_000L) {
                    if (boottimeNs - lo.ptsNs <= hi.ptsNs - boottimeNs) lo.frameIndex else hi.frameIndex
                } else {
                    val ratio = (boottimeNs - lo.ptsNs).toDouble() / span.toDouble()
                    lo.frameIndex + (ratio * (hi.frameIndex - lo.frameIndex).toDouble()).roundToLong()
                }
            }
        }
        return result.coerceIn(0L, maxFrame)
    }

    val selectedDurationMs: Long
        get() = (frameToRelativeMs(outFrameIndex) - frameToRelativeMs(inFrameIndex)).coerceAtLeast(0L)

    val exportCtaLabel: String
        get() = ArbFormatters.formatExportCtaLabel(inFrameIndex, outFrameIndex, frameCount, selectedDurationMs)

    val selectionSummaryText: String
        get() = ArbFormatters.formatSelectionSummary(
            inMs = frameToRelativeMs(inFrameIndex),
            outMs = frameToRelativeMs(outFrameIndex),
            inFrameIndex = inFrameIndex,
            outFrameIndex = outFrameIndex,
        )

    /**
     * Conditional Tier-2 honesty chips for the Top Action Bar.
     * Healthy v2 bundles with calibrated 0ms audio, verified logcat, and 0 gaps yield an empty list!
     */
    val tier2HonestyChips: List<Tier2HonestyChip>
        get() = buildList {
            summary.streams.audio?.let { audio ->
                if (audio.status != StreamStatus.ABSENT && audio.status != StreamStatus.EXCLUDED) {
                    when {
                        audio.avOffsetStatus == AvOffsetStatus.UNCALIBRATED -> add(
                            Tier2HonestyChip(
                                kind = HonestyChipKind.AUDIO_OFFSET,
                                label = if (audio.avOffsetMs != 0L) "音声 ±${abs(audio.avOffsetMs)}ms" else "音声 未校正",
                                detail = ProvenanceFormatter.formatAudio(audio),
                                severity = HonestySeverity.WARNING,
                            ),
                        )
                        audio.avOffsetMs != 0L -> add(
                            Tier2HonestyChip(
                                kind = HonestyChipKind.AUDIO_OFFSET,
                                label = "音声 ±${abs(audio.avOffsetMs)}ms",
                                detail = ProvenanceFormatter.formatAudio(audio),
                                severity = HonestySeverity.INFO,
                            ),
                        )
                    }
                }
            }
            summary.streams.logcat?.let { logcat ->
                when (logcat.completeness) {
                    LogCompleteness.UNVERIFIED -> add(
                        Tier2HonestyChip(
                            kind = HonestyChipKind.LOG_COMPLETENESS,
                            label = "ログ UNVERIFIED",
                            detail = ProvenanceFormatter.formatLogcat(logcat),
                            severity = HonestySeverity.WARNING,
                        ),
                    )
                    LogCompleteness.OVERFLOW_DETECTED -> add(
                        Tier2HonestyChip(
                            kind = HonestyChipKind.LOG_COMPLETENESS,
                            label = "ログ欠落 ${logcat.overflowEvents} 件",
                            detail = ProvenanceFormatter.formatLogcat(logcat),
                            severity = HonestySeverity.ERROR,
                        ),
                    )
                    LogCompleteness.VERIFIED_NO_OVERFLOW -> Unit
                }
            }
            val gapCount = maxOf(discontinuities.size, summary.discontinuityCount)
            if (gapCount > 0) {
                add(
                    Tier2HonestyChip(
                        kind = HonestyChipKind.DISCONTINUITY_GAP,
                        label = "ギャップ $gapCount 件",
                        detail = "タイムラインに $gapCount 件の不連続・欠損イベントがあります。",
                        severity = HonestySeverity.WARNING,
                    ),
                )
            }
            if (isLegacy) {
                add(
                    Tier2HonestyChip(
                        kind = HonestyChipKind.LEGACY_READ_ONLY,
                        label = LEGACY_READ_ONLY_BADGE_TEXT,
                        detail = "旧形式バンドル（読取専用）。元バンドル保護のためゴミ箱移動は無効です。",
                        severity = HonestySeverity.INFO,
                    ),
                )
            }
        }

    /** Returns recorded main-bucket taps within ±[windowNs] (default ±250ms) of [frameIndex]. */
    fun activeTapsAtFrame(
        frameIndex: Long = currentFrameIndex,
        windowNs: Long = TAP_OVERLAY_WINDOW_NS,
    ): List<ActiveTapOverlay> {
        if (!showTapOverlay || mainTaps.isEmpty()) return emptyList()
        val frameEpochBootId = epochRangeForFrame(frameIndex)?.bootId
        val frameBt = currentFrameMeta?.takeIf { it.frameIndex == frameIndex && it.boottimeNs > 0L }?.boottimeNs
            ?: frameToBoottimeNs(frameIndex)
        return mainTaps.mapIndexedNotNull { idx, tap ->
            val x = tap.x ?: return@mapIndexedNotNull null
            val y = tap.y ?: return@mapIndexedNotNull null
            val tapBootId = tap.bootId ?: alignmentBootId(tap.alignment)
            if (frameEpochBootId != null && tapBootId != null && tapBootId != frameEpochBootId) {
                return@mapIndexedNotNull null
            }
            val tapBt = alignmentBoottimeNs(tap.alignment) ?: return@mapIndexedNotNull null
            val strokeDurationNs = if (tap.gestureKind == GestureKind.SWIPE) {
                (tap.endTOffsetNs ?: 0L).coerceAtLeast(0L)
            } else {
                0L
            }
            val tapEndBt = tapBt + strokeDurationNs
            val deltaNs = when {
                frameBt < tapBt -> tapBt - frameBt
                frameBt > tapEndBt -> tapEndBt - frameBt
                else -> 0L
            }
            if (abs(deltaNs) <= windowNs) {
                val n = idx + 1
                ActiveTapOverlay(
                    tapIndex1Based = n,
                    x = x,
                    y = y,
                    deltaMs = deltaNs / 1_000_000L,
                    label = "#$n",
                    gestureKind = tap.gestureKind,
                    endX = tap.endX,
                    endY = tap.endY,
                )
            } else {
                null
            }
        }
    }

    /** Integer frame indices of all recorded main-bucket taps (for `PlaybackController.selectTapRange`). */
    fun mainTapFrameIndices(): List<Long> = mainTaps.mapNotNull { tap ->
        val bt = alignmentBoottimeNs(tap.alignment) ?: return@mapNotNull null
        val bootId = tap.bootId ?: alignmentBootId(tap.alignment)
        boottimeNsToFrameIndex(bt, bootId)
    }

    /** Primary crash frame index (for `PlaybackController.selectAroundCrash`). */
    fun primaryCrashFrameIndex(): Long? {
        val crash = crashes.firstOrNull() ?: return null
        if (crash.frameIndexHint != null) return crash.frameIndexHint
        val bt = crash.boottimeNs ?: return null
        val bootId = mainLogs.firstOrNull { it.seq == crash.sourceLogSeq }?.let { it.bootId ?: alignmentBootId(it.alignment) }
        return boottimeNsToFrameIndex(bt, bootId)
    }

    /**
     * Builds the unified chronological event & log stream for the right-hand `"操作＆ログ"` tab:
     * - Strictly `MAIN` bucket only (never mixes `unanchored` records!)
     * - Maps every event to `frameIndex` strictly within its matching boot epoch
     * - Tags each row with real-time trim range membership (`inFrameIndex..=outFrameIndex`)
     * - Correlates logs occurring within `0..300ms` after a preceding tap in the same epoch (`correlatedTapIndex`)
     * - Extracts PC-side PSI source-jump targets for Activities, stack traces, and crashes
     */
    fun buildSyncedEventRows(
        filter: EventStreamFilter = EventStreamFilter.ALL,
        keyword: String = "",
    ): List<SyncedEventRow> {
        val logBootBySeq = mainLogs.associate { log ->
            log.seq to (log.bootId ?: alignmentBootId(log.alignment))
        }
        data class TapEntry(val tapNum: Int, val bootId: String?, val boottimeNs: Long?, val tap: TapRecord)
        val tapEntries = mainTaps.mapIndexed { idx, tap ->
            TapEntry(
                tapNum = idx + 1,
                bootId = tap.bootId ?: alignmentBootId(tap.alignment),
                boottimeNs = alignmentBoottimeNs(tap.alignment),
                tap = tap,
            )
        }

        val allRows = mutableListOf<Pair<Int, SyncedEventRow>>()

        // 1. Tap rows (honest "Tap #N (x, y)" only)
        for (entry in tapEntries) {
            val bt = entry.boottimeNs
            val frame = bt?.let { boottimeNsToFrameIndex(it, entry.bootId) }
            val relMs = bt?.let { boottimeToRelativeMs(it, entry.bootId) }
            allRows.add(
                epochOrderIndex(entry.bootId, bt) to SyncedEventRow(
                    kind = SyncedEventKind.TAP,
                    seq = entry.tap.seq,
                    boottimeNs = bt,
                    relativeMs = relMs,
                    frameIndex = frame,
                    title = ArbFormatters.formatTapDisplay(entry.tapNum, entry.tap),
                    detail = "action=${entry.tap.action}",
                    level = "TAP",
                    tag = "Input",
                    isInTrimRange = isFrameInTrimRange(frame),
                    isAtPlayhead = frame != null && frame == currentFrameIndex,
                    correlatedTapIndex = entry.tapNum,
                ),
            )
        }

        // 2. Crash markers
        for (crash in crashes) {
            val bt = crash.boottimeNs
            val crashBootId = logBootBySeq[crash.sourceLogSeq]
                ?: crash.frameIndexHint?.let { epochRangeForFrame(it)?.bootId }
            val frame = crash.frameIndexHint ?: bt?.let { boottimeNsToFrameIndex(it, crashBootId) }
            val relMs = bt?.let { boottimeToRelativeMs(it, crashBootId) }
            allRows.add(
                epochOrderIndex(crashBootId, bt) to SyncedEventRow(
                    kind = SyncedEventKind.CRASH,
                    seq = crash.sourceLogSeq,
                    boottimeNs = bt,
                    relativeMs = relMs,
                    frameIndex = frame,
                    title = "${crash.exceptionClass.substringAfterLast('.')} (${crash.packageName})",
                    detail = crash.message.ifBlank { crash.processName },
                    level = "F",
                    tag = "am_crash",
                    isInTrimRange = isFrameInTrimRange(frame),
                    isAtPlayhead = frame != null && frame == currentFrameIndex,
                    sourceJumpTarget = SourceJumpParser.fromCrashMarker(crash),
                ),
            )
        }

        // 3. Screen transitions (from PC-derived ScreenSpan)
        for ((idx, span) in screens.withIndex()) {
            val spanBootId = span.startFrameIndex?.let { epochRangeForFrame(it)?.bootId }
            val frame = span.startFrameIndex ?: boottimeNsToFrameIndex(span.startNs, spanBootId)
            val relMs = boottimeToRelativeMs(span.startNs, spanBootId)
            allRows.add(
                epochOrderIndex(spanBootId, span.startNs) to SyncedEventRow(
                    kind = SyncedEventKind.SCREEN_TRANSITION,
                    seq = -1_000L - idx.toLong(),
                    boottimeNs = span.startNs,
                    relativeMs = relMs,
                    frameIndex = frame,
                    title = span.shortName,
                    detail = span.activityName,
                    level = "SCR",
                    tag = "Screen",
                    isInTrimRange = isFrameInTrimRange(frame),
                    isAtPlayhead = frame != null && frame == currentFrameIndex,
                    sourceJumpTarget = SourceJumpParser.fromScreenSpan(span),
                ),
            )
        }

        // 4. Main-bucket logs with 0..300ms Tap -> Following Logs correlation (same boot epoch only)
        for (log in mainLogs) {
            val bt = alignmentBoottimeNs(log.alignment)
            val logBootId = log.bootId ?: alignmentBootId(log.alignment)
            val frame = bt?.let { boottimeNsToFrameIndex(it, logBootId) }
            val relMs = bt?.let { boottimeToRelativeMs(it, logBootId) }
            val corrTap = if (bt != null) {
                var bestTap: Int? = null
                var bestDelta = Long.MAX_VALUE
                for (entry in tapEntries) {
                    val tapBt = entry.boottimeNs ?: continue
                    if (logBootId != null && entry.bootId != null && logBootId != entry.bootId) continue
                    val delta = bt - tapBt
                    if (delta in 0L..TAP_LOG_CORRELATION_WINDOW_NS && delta < bestDelta) {
                        bestDelta = delta
                        bestTap = entry.tapNum
                    }
                }
                bestTap
            } else {
                null
            }
            allRows.add(
                epochOrderIndex(logBootId, bt) to SyncedEventRow(
                    kind = SyncedEventKind.LOG,
                    seq = log.seq,
                    boottimeNs = bt,
                    relativeMs = relMs,
                    frameIndex = frame,
                    title = log.tag ?: "-",
                    detail = log.message,
                    level = log.level ?: "I",
                    tag = log.tag,
                    isInTrimRange = isFrameInTrimRange(frame),
                    isAtPlayhead = frame != null && frame == currentFrameIndex,
                    correlatedTapIndex = corrTap,
                    sourceJumpTarget = SourceJumpParser.parseLogText(log.message),
                ),
            )
        }

        val sorted = allRows.sortedWith(
            compareBy<Pair<Int, SyncedEventRow>>(
                { it.first },
                { it.second.boottimeNs ?: Long.MAX_VALUE },
                { kindSortOrder(it.second.kind) },
                { it.second.seq },
            ),
        ).map { it.second }

        val q = keyword.trim().lowercase()
        return sorted.filter { row ->
            val filterMatch = when (filter) {
                EventStreamFilter.ALL -> true
                EventStreamFilter.TAPS -> row.kind == SyncedEventKind.TAP || row.correlatedTapIndex != null
                EventStreamFilter.ERRORS_CRASHES -> row.kind == SyncedEventKind.CRASH ||
                    row.level.equals("E", ignoreCase = true) ||
                    row.level.equals("F", ignoreCase = true) ||
                    row.level.equals("W", ignoreCase = true) ||
                    row.detail.contains("Exception", ignoreCase = true)
            }
            if (!filterMatch) return@filter false
            if (q.isEmpty()) return@filter true
            row.title.lowercase().contains(q) ||
                row.detail.lowercase().contains(q) ||
                (row.tag?.lowercase()?.contains(q) == true)
        }
    }

    private fun isFrameInTrimRange(frameIndex: Long?): Boolean {
        if (frameIndex == null || frameCount <= 0L) return true
        return frameIndex in inFrameIndex..outFrameIndex
    }

    private fun kindSortOrder(kind: SyncedEventKind): Int = when (kind) {
        SyncedEventKind.SCREEN_TRANSITION -> 0
        SyncedEventKind.TAP -> 1
        SyncedEventKind.CRASH -> 2
        SyncedEventKind.LOG -> 3
    }

    companion object {
        private const val DEFAULT_FRAME_STEP_NS: Long = 33_333_333L

        fun alignmentBoottimeNs(alignment: TimeAlignment): Long? = when (alignment) {
            is TimeAlignment.Anchored -> alignment.boottimeNs
            is TimeAlignment.Bounded -> (alignment.minBoottimeNs + alignment.maxBoottimeNs) / 2L
            is TimeAlignment.Unanchored -> null
        }

        fun alignmentBootId(alignment: TimeAlignment): String? = when (alignment) {
            is TimeAlignment.Anchored -> alignment.bootId
            is TimeAlignment.Bounded -> alignment.bootId
            is TimeAlignment.Unanchored -> null
        }
    }
}

object TimelineViewerReducer {
    fun onFramePublished(state: TimelineViewerState, meta: ShmFrameMeta): TimelineViewerState =
        state.copy(
            currentFrameIndex = meta.frameIndex.coerceIn(0L, (state.frameCount - 1L).coerceAtLeast(0L)),
            currentFrameMeta = meta,
        )

    fun stepFrame(state: TimelineViewerState, delta: Long): TimelineViewerState {
        if (state.frameCount <= 0L) return state
        val next = (state.currentFrameIndex + delta).coerceIn(0L, state.frameCount - 1L)
        return state.copy(currentFrameIndex = next)
    }

    fun setInFrame(state: TimelineViewerState, frameIndex: Long): TimelineViewerState {
        if (state.frameCount <= 0L) return state
        val clamped = frameIndex.coerceIn(0L, state.frameCount - 1L)
        val adjustedOut = if (state.outFrameIndex < clamped) clamped else state.outFrameIndex
        return state.copy(inFrameIndex = clamped, outFrameIndex = adjustedOut)
    }

    fun setOutFrame(state: TimelineViewerState, frameIndex: Long): TimelineViewerState {
        if (state.frameCount <= 0L) return state
        val clamped = frameIndex.coerceIn(0L, state.frameCount - 1L)
        val adjustedIn = if (state.inFrameIndex > clamped) clamped else state.inFrameIndex
        return state.copy(inFrameIndex = adjustedIn, outFrameIndex = clamped)
    }

    fun setTrashOriginalAfterExport(state: TimelineViewerState, enabled: Boolean): TimelineViewerState =
        state.copy(trashOriginalAfterExport = if (state.isLegacy) false else enabled)

    fun withBucketedRecords(
        state: TimelineViewerState,
        mainLogs: List<LogRecord>,
        unanchoredLogs: List<LogRecord>,
        mainTaps: List<TapRecord>,
        unanchoredTaps: List<TapRecord>,
    ): TimelineViewerState = state.copy(
        // Enforce strict separation: never mix unanchored records into main!
        mainLogs = mainLogs.filter { it.bucket == RecordBucket.MAIN },
        unanchoredLogs = unanchoredLogs.filter { it.bucket == RecordBucket.UNANCHORED },
        mainTaps = mainTaps.filter { it.bucket == RecordBucket.MAIN },
        unanchoredTaps = unanchoredTaps.filter { it.bucket == RecordBucket.UNANCHORED },
    )
}

// ---------------- Honest Provenance & Discontinuity Formatters ----------------

object DiscontinuityFormatter {
    fun label(kind: DiscontinuityKind): String = when (kind) {
        DiscontinuityKind.DEVICE_REBOOT -> "端末再起動 (DEVICE_REBOOT)"
        DiscontinuityKind.DEVICE_SLEEP -> "端末スリープ (DEVICE_SLEEP)"
        DiscontinuityKind.DEVICE_WALL_JUMP -> "壁時計ジャンプ (DEVICE_WALL_JUMP)"
        DiscontinuityKind.PC_SLEEP -> "PC スリープ (PC_SLEEP)"
        DiscontinuityKind.CONNECTION_LOST -> "接続切断 (CONNECTION_LOST)"
        DiscontinuityKind.STREAM_GAP -> "ストリーム欠損 (STREAM_GAP)"
        DiscontinuityKind.STORAGE_PRESSURE_STOP -> "容量圧迫停止 (STORAGE_PRESSURE_STOP)"
        DiscontinuityKind.CLOCK_SYNC_STALE -> "時刻同期鮮度低下 (CLOCK_SYNC_STALE)"
        DiscontinuityKind.STATIC_HOLD -> "静止画面継続 (STATIC_HOLD)"
    }

    fun describe(event: DiscontinuityEvent): String {
        val parts = mutableListOf(label(event.kind))
        event.stream?.let { parts.add("stream=${it.wire}") }
        event.deltaNs?.let { ns ->
            val ms = ns / 1_000_000.0
            parts.add("Δ=${"%.1f".format(ms)} ms")
        }
        event.detail?.takeIf { it.isNotBlank() }?.let { parts.add(it) }
        return parts.joinToString(" | ")
    }
}

data class ProvenanceDisplay(
    val provenanceLine: String,
    val videoSemanticsLine: String,
    val audioOffsetLine: String,
    val logcatCompletenessLine: String,
    val unanchoredLine: String,
)

object ProvenanceFormatter {
    fun format(summary: TimelineSummaryResponse): ProvenanceDisplay {
        val provLine = when (val p = summary.provenance) {
            Provenance.Original -> if (summary.isLegacy) {
                "来歴: 旧形式読取専用 (${summary.format.wire}) — $LEGACY_READ_ONLY_BADGE_TEXT"
            } else {
                "来歴: オリジナル保存 (WORM)"
            }
            is Provenance.Trimmed ->
                "来歴: 非破壊トリム (親=${p.parentBundleId}, フレーム [${p.inFrameIndex}..${p.outFrameIndex}])"
        }

        return ProvenanceDisplay(
            provenanceLine = provLine,
            videoSemanticsLine = formatVideo(summary.streams.video),
            audioOffsetLine = formatAudio(summary.streams.audio),
            logcatCompletenessLine = formatLogcat(summary.streams.logcat),
            unanchoredLine = formatUnanchored(summary),
        )
    }

    fun formatVideo(video: VideoStreamMeta?): String {
        if (video == null) return "映像: なし"
        val semanticsText = when (video.semantics) {
            TimestampSemantics.VIDEO_ENCODER_INPUT_ESTIMATE -> "エンコーダ入力時刻推定"
            TimestampSemantics.AUDIO_CAPTURE_PTS_UNCALIBRATED -> "音声キャプチャ PTS（未校正）"
            TimestampSemantics.INPUT_EVENT_TIME -> "入力イベント時刻"
            TimestampSemantics.LOGCAT_HEADER_WALL -> "logcat ヘッダ壁時計"
            TimestampSemantics.LOGCAT_DEVICE_READ_BOOTTIME -> "端末読取 boottime"
            TimestampSemantics.PC_INGEST_ONLY -> "PC 受信時刻のみ"
        }
        val tailWarn = if (video.tailMayBeIncomplete) " [末尾未確認: tail_may_be_incomplete]" else ""
        return "映像: ${statusLabel(video.status)} / ${video.frameCount} フレーム / PTS 意味=$semanticsText$tailWarn"
    }

    fun formatAudio(audio: AudioStreamMeta?): String {
        if (audio == null) return "音声: なし"
        val offsetLabel = when (audio.avOffsetStatus) {
            AvOffsetStatus.UNCALIBRATED -> "未校正"
            AvOffsetStatus.CALIBRATED -> "校正済み (${audio.avOffsetMs} ms)"
            AvOffsetStatus.USER_OVERRIDDEN -> "手動上書き (${audio.avOffsetMs} ms)"
        }
        val tailWarn = if (audio.tailMayBeIncomplete) " [末尾未確認]" else ""
        return "音声: ${statusLabel(audio.status)} / A/V オフセット=$offsetLabel$tailWarn"
    }

    fun formatLogcat(logcat: LogcatStreamMeta?): String {
        if (logcat == null) return "Logcat: なし"
        val compLabel = when (logcat.completeness) {
            LogCompleteness.VERIFIED_NO_OVERFLOW -> "確認済み（オーバーフローなし）"
            LogCompleteness.OVERFLOW_DETECTED -> "オーバーフロー検出 (${logcat.overflowEvents} 件)"
            LogCompleteness.UNVERIFIED -> "未確認 (UNVERIFIED)"
        }
        val tailWarn = if (logcat.tailMayBeIncomplete) " [末尾未確認]" else ""
        return "Logcat: ${statusLabel(logcat.status)} / 完全性=$compLabel$tailWarn"
    }

    private fun formatUnanchored(summary: TimelineSummaryResponse): String {
        val inc = summary.unanchoredSummary.includedCount
        val trunc = summary.unanchoredSummary.truncatedCount
        return if (trunc > 0L) {
            "時刻不明 (Unanchored): 収録 $inc 件 / 上限超過省略 $trunc 件"
        } else {
            "時刻不明 (Unanchored): 収録 $inc 件（省略なし）"
        }
    }

    private fun statusLabel(status: StreamStatus): String = when (status) {
        StreamStatus.PRESENT -> "Present"
        StreamStatus.PARTIAL -> "Partial"
        StreamStatus.ABSENT -> "Absent"
        StreamStatus.EXCLUDED -> "Excluded"
    }
}
