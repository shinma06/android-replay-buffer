package io.github.shinma06.arb.state

import io.github.shinma06.arb.model.AdbDeviceInfo
import io.github.shinma06.arb.model.AudioStreamMeta
import io.github.shinma06.arb.model.AvOffsetStatus
import io.github.shinma06.arb.model.BundleEntryView
import io.github.shinma06.arb.model.BundleFormat
import io.github.shinma06.arb.model.BundleLifecycleState
import io.github.shinma06.arb.model.CaptureStatusView
import io.github.shinma06.arb.model.CatalogFormatVersion
import io.github.shinma06.arb.model.DiscontinuityEvent
import io.github.shinma06.arb.model.DiscontinuityKind
import io.github.shinma06.arb.model.FilmstripEntryMeta
import io.github.shinma06.arb.model.LogCompleteness
import io.github.shinma06.arb.model.LogRecord
import io.github.shinma06.arb.model.LogcatStreamMeta
import io.github.shinma06.arb.model.Provenance
import io.github.shinma06.arb.model.RecordBucket
import io.github.shinma06.arb.model.SaveJobState
import io.github.shinma06.arb.model.SaveJobView
import io.github.shinma06.arb.model.StreamStatus
import io.github.shinma06.arb.model.StreamsSummary
import io.github.shinma06.arb.model.SystemStatusResponse
import io.github.shinma06.arb.model.TapRecord
import io.github.shinma06.arb.model.TimelineSummaryResponse
import io.github.shinma06.arb.model.TimestampSemantics
import io.github.shinma06.arb.model.VideoStreamMeta
import io.github.shinma06.arb.shm.ShmFrameMeta

const val APPROXIMATE_PREVIEW_BADGE_TEXT: String = "近似プレビュー（位置確定前）"
const val LEGACY_READ_ONLY_BADGE_TEXT: String = "Legacy (RO)"
const val UNANCHORED_TAB_TITLE: String = "時刻不明の記録 (Unanchored)"

// ---------------- Capture Bar & Live Status Banners ----------------

enum class CaptureBannerKind {
    STORAGE_PRESSURE_STOP,
    CONNECTION_LOST,
    STREAM_GAP,
    STATIC_HOLD,
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
    val capture: CaptureStatusView? = null,
    val ringChunkCount: Long = 0,
    val ringTotalBytes: Long = 0,
    val ringPinnedChunkCount: Long = 0,
    val saveDurationSec: Long = 30L,
    val banners: List<CaptureBanner> = emptyList(),
    val lastStatusMessage: String? = null,
)

object CaptureBarReducer {
    fun fromStatus(
        previous: CaptureBarState,
        status: SystemStatusResponse,
        devices: List<AdbDeviceInfo> = previous.devices,
        liveDiscontinuities: List<DiscontinuityEvent> = emptyList(),
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
                        title = "CONNECTION_LOST: 端末との接続が切断されました",
                        detail = "自動再接続を待機しています。切断前のリングバッファはそのまま保存できます。",
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
        }

        val resolvedSerial = previous.selectedSerial
            ?: status.capture.serial
            ?: devices.firstOrNull()?.serial

        return previous.copy(
            daemonConnected = true,
            daemonPid = status.pid,
            devices = devices,
            selectedSerial = resolvedSerial,
            capture = status.capture,
            ringChunkCount = status.ringChunkCount,
            ringTotalBytes = status.ringTotalBytes,
            ringPinnedChunkCount = status.ringPinnedChunkCount,
            banners = banners,
        )
    }

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

// ---------------- Bundle Catalog State ----------------

data class BundleCatalogItem(
    val bundleId: String,
    val path: String,
    val isLegacyReadOnly: Boolean,
    val state: BundleLifecycleState,
    val activeLeases: Int,
    val createdAtUtc: String,
    val parentBundleId: String?,
) {
    val formatBadge: String
        get() = if (isLegacyReadOnly) LEGACY_READ_ONLY_BADGE_TEXT else "v2"

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
) {
    val visibleItems: List<BundleCatalogItem>
        get() = if (showTrashed) items else items.filter { it.state == BundleLifecycleState.ACTIVE }

    companion object {
        fun fromEntries(entries: List<BundleEntryView>, showTrashed: Boolean = true): BundleCatalogState =
            BundleCatalogState(
                items = entries.map { e ->
                    BundleCatalogItem(
                        bundleId = e.bundleId,
                        path = e.path,
                        isLegacyReadOnly = e.formatVersion == CatalogFormatVersion.LEGACY_RO,
                        state = e.state,
                        activeLeases = e.activeLeases,
                        createdAtUtc = e.createdAtUtc,
                        parentBundleId = e.parentBundleId,
                    )
                },
                showTrashed = showTrashed,
            )
    }
}

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
    val mainLogs: List<LogRecord> = emptyList(),
    val unanchoredLogs: List<LogRecord> = emptyList(),
    val mainTaps: List<TapRecord> = emptyList(),
    val unanchoredTaps: List<TapRecord> = emptyList(),
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

    val unanchoredTruncatedCount: Long
        get() = summary.unanchoredSummary.truncatedCount

    val unanchoredTruncatedNotice: String?
        get() = if (unanchoredTruncatedCount > 0L) {
            "時刻不明の記録は上限超過のため $unanchoredTruncatedCount 件が省略されました。"
        } else {
            null
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
                "来歴: オリジナル保存 (v2 WORM)"
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
