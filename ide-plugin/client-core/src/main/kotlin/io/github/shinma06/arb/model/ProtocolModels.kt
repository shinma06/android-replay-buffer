package io.github.shinma06.arb.model

const val IPC_PROTOCOL_VERSION: Int = 1
const val JSONRPC_VERSION: String = "2.0"

// ---------------- JSON-RPC 2.0 Envelope ----------------

data class RpcErrorObject(
    val code: Int,
    val message: String,
    val data: Any? = null,
)

class ArbdRpcException(
    val code: Int,
    override val message: String,
    val method: String? = null,
) : RuntimeException(
    if (method != null) "arbd RPC error ($method) [$code]: $message" else "arbd RPC error [$code]: $message",
) {
    val isMethodNotFound: Boolean
        get() = code == RPC_ERR_METHOD_NOT_FOUND

    companion object {
        const val RPC_ERR_METHOD_NOT_FOUND: Int = -32601
    }
}

// ---------------- System ----------------

data class SystemHelloResponse(
    val protocolVersion: Int,
    val schemaVersion: Int,
    val pid: Long,
    val dataDir: String,
    val cacheDir: String,
    val runtimeDir: String,
    val daemonVersion: String,
)

data class CaptureStatusView(
    val active: Boolean,
    val connected: Boolean,
    val serial: String? = null,
    val bootId: String? = null,
    val storagePressureStopped: Boolean,
    val videoFrames: Long,
    val audioFrames: Long,
    val logRecords: Long,
    val tapRecords: Long,
    val discontinuities: Long,
    val staticHoldEvents: Long,
    val ringWriteErrors: Long = 0L,
)

data class RecoverySummaryView(
    val removedStagingDirs: Long = 0,
    val reapedLeases: Long = 0,
    val recoveredJobs: Long = 0,
    val completedPurgingBundles: Long = 0,
    val releasedPinJournals: Long = 0,
    val repinnedJobs: Long = 0,
)

data class CrashAutoSaveStatusView(
    val enabled: Boolean = false,
    val cooldownSecs: Int = 15,
    val detectedCrashes: Long = 0L,
    val triggeredSaves: Long = 0L,
    val suppressedCooldown: Long = 0L,
    val suppressedStoragePressure: Long = 0L,
    val inFlight: Boolean = false,
    val lastCrashProcess: String? = null,
    val lastCrashException: String? = null,
    val lastJobId: String? = null,
    val lastBundleId: String? = null,
    val lastError: String? = null,
)

data class SystemStatusResponse(
    val pid: Long,
    val uptimeMs: Long,
    val capture: CaptureStatusView,
    val ringChunkCount: Long,
    val ringTotalBytes: Long,
    val ringPinnedChunkCount: Long,
    val activeViewports: Long,
    val activeSaveJobs: Long,
    val recovery: RecoverySummaryView,
    val crashAutosave: CrashAutoSaveStatusView = CrashAutoSaveStatusView(),
    val configWarning: String? = null,
)

data class ShutdownResponse(
    val ok: Boolean,
)

// ---------------- Persisted Config (T-1) ----------------

enum class IdeDefaultSaveAction(val wire: String) {
    SAVE_AND_OPEN("save_and_open"),
    SAVE_ONLY("save_only");

    companion object {
        fun fromWire(value: String): IdeDefaultSaveAction =
            entries.firstOrNull { it.wire == value }
                ?: throw IllegalArgumentException("Unknown IdeDefaultSaveAction: $value")
    }
}

data class ArbConfig(
    val configVersion: Int = 1,
    val saveWindowSecs: Int = DEFAULT_SAVE_WINDOW_SECS,
    val ringBudgetBytes: Long = DEFAULT_RING_BUDGET_BYTES,
    val minFreeDiskBytes: Long = DEFAULT_MIN_FREE_DISK_BYTES,
    val videoMaxSize: Int = DEFAULT_VIDEO_MAX_SIZE,
    val videoMaxFps: Int = DEFAULT_VIDEO_MAX_FPS,
    val audioEnabled: Boolean = true,
    val crashAutosaveEnabled: Boolean = true,
    val crashAutosaveCooldownSecs: Int = DEFAULT_CRASH_AUTOSAVE_COOLDOWN_SECS,
    val outputDirOverride: String? = null,
    val ideDefaultSaveAction: IdeDefaultSaveAction = IdeDefaultSaveAction.SAVE_AND_OPEN,
) {
    companion object {
        const val CONFIG_SCHEMA_VERSION: Int = 1
        const val DEFAULT_SAVE_WINDOW_SECS: Int = 300
        const val MIN_SAVE_WINDOW_SECS: Int = 5
        const val MAX_SAVE_WINDOW_SECS: Int = 600
        const val DEFAULT_RING_BUDGET_BYTES: Long = 1_610_612_736L
        const val DEFAULT_MIN_FREE_DISK_BYTES: Long = 3_221_225_472L
        const val DEFAULT_VIDEO_MAX_SIZE: Int = 1_920
        const val DEFAULT_VIDEO_MAX_FPS: Int = 30
        const val DEFAULT_CRASH_AUTOSAVE_COOLDOWN_SECS: Int = 15
    }
}

data class ConfigResponse(
    val config: ArbConfig,
    val configPath: String,
)

sealed class OutputDirPatch {
    data object Unchanged : OutputDirPatch()
    data object Clear : OutputDirPatch()
    data class Set(val path: String) : OutputDirPatch()
}

data class ArbConfigPatch(
    val saveWindowSecs: Int? = null,
    val ringBudgetBytes: Long? = null,
    val minFreeDiskBytes: Long? = null,
    val videoMaxSize: Int? = null,
    val videoMaxFps: Int? = null,
    val audioEnabled: Boolean? = null,
    val crashAutosaveEnabled: Boolean? = null,
    val crashAutosaveCooldownSecs: Int? = null,
    val outputDirOverride: OutputDirPatch = OutputDirPatch.Unchanged,
    val ideDefaultSaveAction: IdeDefaultSaveAction? = null,
)

// ---------------- Device & Capture ----------------

sealed class AdbDeviceState {
    data object Device : AdbDeviceState()
    data object Offline : AdbDeviceState()
    data object Unauthorized : AdbDeviceState()
    data object Recovery : AdbDeviceState()
    data object Bootloader : AdbDeviceState()
    data class Unknown(val value: String) : AdbDeviceState()

    val wireName: String
        get() = when (this) {
            Device -> "device"
            Offline -> "offline"
            Unauthorized -> "unauthorized"
            Recovery -> "recovery"
            Bootloader -> "bootloader"
            is Unknown -> value
        }
}

data class AdbDeviceInfo(
    val serial: String,
    val state: AdbDeviceState,
    val product: String? = null,
    val model: String? = null,
    val device: String? = null,
    val transportId: String? = null,
)

data class DeviceListResponse(
    val devices: List<AdbDeviceInfo>,
)

data class CaptureStartParams(
    val serial: String? = null,
    val companionJar: String? = null,
    val scrcpyJar: String? = null,
    val maxSize: Int? = null,
    val maxFps: Int? = null,
    val videoBitRate: Int? = null,
    val audioEnabled: Boolean? = null,
    val logcatEnabled: Boolean? = null,
    val inputEnabled: Boolean? = null,
    val heartbeatMs: Int? = null,
)

data class CaptureStartResponse(
    val serial: String,
    val started: Boolean,
)

data class CaptureStopResponse(
    val stopped: Boolean,
    val capture: CaptureStatusView,
)

// ---------------- Save Jobs ----------------

enum class SaveJobState(val wire: String) {
    PINNED("pinned"),
    DRAINING("draining"),
    MATERIALIZING("materializing"),
    FAILED_RETRYABLE("failed_retryable"),
    COMPLETED("completed"),
    DISCARDED("discarded");

    val isTerminal: Boolean
        get() = this == COMPLETED || this == DISCARDED

    companion object {
        fun fromWire(value: String): SaveJobState =
            entries.firstOrNull { it.wire == value }
                ?: throw IllegalArgumentException("Unknown SaveJobState: $value")
    }
}

data class SelectionPolicy(
    val unanchoredIngestMarginNs: Long = 2_000_000_000L,
    val maxUnanchoredRecords: Int = 50_000,
    val maxUnanchoredBytes: Int = 16 * 1024 * 1024,
    val drainTimeoutMs: Long = 500L,
)

enum class SaveMode(val wire: String) {
    FULL("full"),
    LOGS_ONLY("logs_only");

    companion object {
        fun fromWire(value: String): SaveMode =
            entries.firstOrNull { it.wire == value }
                ?: throw IllegalArgumentException("Unknown SaveMode: $value")
    }
}

enum class SaveTriggerKind(val wire: String) {
    MANUAL("manual"),
    CRASH_AUTO_SAVE("crash_auto_save");

    companion object {
        fun fromWire(value: String): SaveTriggerKind =
            entries.firstOrNull { it.wire == value }
                ?: throw IllegalArgumentException("Unknown SaveTriggerKind: $value")
    }
}

enum class BundleKind(val wire: String) {
    FULL("full"),
    LOGS_ONLY("logs_only");

    companion object {
        fun fromWire(value: String): BundleKind =
            entries.firstOrNull { it.wire == value }
                ?: throw IllegalArgumentException("Unknown BundleKind: $value")
    }
}

data class SaveTriggerParams(
    val durationSec: Long? = null,
    val destDir: String? = null,
    val drainTimeoutMs: Long? = null,
    val selectionPolicy: SelectionPolicy? = null,
    val mode: SaveMode = SaveMode.FULL,
)

data class SaveLogsParams(
    val windowSecs: Long? = null,
    val destDir: String? = null,
    val drainTimeoutMs: Long? = null,
    val selectionPolicy: SelectionPolicy? = null,
)

data class SaveScreenshotParams(
    val serial: String? = null,
    val outputPath: String? = null,
    val overwrite: Boolean = false,
)

data class ScreenshotResult(
    val path: String,
    val width: Int,
    val height: Int,
    val sha256: String,
    val capturedAtUnixMs: Long,
    val serial: String,
)

typealias SaveScreenshotResponse = ScreenshotResult
typealias SaveLogsResult = SaveOutcomeResponse

data class SaveJobView(
    val jobId: String,
    val state: SaveJobState,
    val trigger: SaveTriggerKind = SaveTriggerKind.MANUAL,
    val destinationPath: String,
    val pinnedChunkIds: List<String>,
    val lastError: String? = null,
    val createdAtUtc: String,
    val updatedAtUtc: String,
)

data class UnanchoredSummary(
    val includedCount: Long,
    val truncatedCount: Long,
)

data class SaveOutcomeResponse(
    val job: SaveJobView,
    val bundleId: String? = null,
    val bundlePath: String? = null,
    val tailMayBeIncomplete: Boolean,
    val unanchoredSummary: UnanchoredSummary? = null,
    val warning: String? = null,
)

data class SaveListJobsResponse(
    val jobs: List<SaveJobView>,
)

// ---------------- Bundles & Leases ----------------

enum class CatalogFormatVersion(val wire: String) {
    V2("v2"),
    LEGACY_RO("legacy_ro");

    companion object {
        fun fromWire(value: String): CatalogFormatVersion =
            entries.firstOrNull { it.wire == value }
                ?: throw IllegalArgumentException("Unknown CatalogFormatVersion: $value")
    }
}

enum class BundleLifecycleState(val wire: String) {
    ACTIVE("active"),
    TRASHED("trashed"),
    PURGING("purging");

    companion object {
        fun fromWire(value: String): BundleLifecycleState =
            entries.firstOrNull { it.wire == value }
                ?: throw IllegalArgumentException("Unknown BundleLifecycleState: $value")
    }
}

enum class BundleFormat(val wire: String) {
    V2("v2"),
    LEGACY_SCHEMA4("legacy_schema4"),
    LEGACY_SCHEMA3("legacy_schema3");

    companion object {
        fun fromWire(value: String): BundleFormat =
            entries.firstOrNull { it.wire == value }
                ?: throw IllegalArgumentException("Unknown BundleFormat: $value")
    }
}

enum class LeaseHolderKind(val wire: String) {
    IDE_VIEWER("ide_viewer"),
    EXPORT_JOB("export_job"),
    CLI("cli");

    companion object {
        fun fromWire(value: String): LeaseHolderKind =
            entries.firstOrNull { it.wire == value }
                ?: throw IllegalArgumentException("Unknown LeaseHolderKind: $value")
    }
}

data class BundleEntryView(
    val bundleId: String,
    val path: String,
    val formatVersion: CatalogFormatVersion,
    val state: BundleLifecycleState,
    val trashedAtUtc: String? = null,
    val parentBundleId: String? = null,
    val createdAtUtc: String,
    val activeLeases: Int,
)

data class BundleListResponse(
    val bundles: List<BundleEntryView>,
)

data class BundleOpenResponse(
    val bundleId: String,
    val path: String,
    val format: BundleFormat,
    val isLegacy: Boolean,
    val leaseId: String,
    val summary: TimelineSummaryResponse,
)

data class LeaseRenewResponse(
    val renewed: Boolean,
    val leaseId: String,
)

data class LeaseCloseResponse(
    val released: Boolean,
    val leaseId: String,
)

data class BundleGcFailure(
    val bundleId: String,
    val error: String,
)

data class BundleGcResponse(
    val purgedBundleIds: List<String>,
    val skippedLeasedBundleIds: List<String>,
    val skippedActiveJobBundleIds: List<String>,
    val skippedUnmanagedOrLegacyBundleIds: List<String>,
    val skippedInGraceBundleIds: List<String>,
    val failed: List<BundleGcFailure> = emptyList(),
)

// ---------------- Timeline & Provenance ----------------

enum class QueryBucket(val wire: String) {
    MAIN("main"),
    UNANCHORED("unanchored"),
    ALL("all");

    companion object {
        fun fromWire(value: String): QueryBucket =
            entries.firstOrNull { it.wire == value }
                ?: throw IllegalArgumentException("Unknown QueryBucket: $value")
    }
}

enum class RecordBucket(val wire: String) {
    MAIN("main"),
    UNANCHORED("unanchored");

    companion object {
        fun fromWire(value: String): RecordBucket =
            entries.firstOrNull { it.wire == value }
                ?: throw IllegalArgumentException("Unknown RecordBucket: $value")
    }
}

enum class StreamKind(val wire: String) {
    VIDEO("video"),
    AUDIO("audio"),
    LOGCAT("logcat"),
    INPUT("input");

    companion object {
        fun fromWire(value: String): StreamKind =
            entries.firstOrNull { it.wire == value }
                ?: throw IllegalArgumentException("Unknown StreamKind: $value")
    }
}

enum class StreamStatus(val wire: String) {
    PRESENT("present"),
    PARTIAL("partial"),
    ABSENT("absent"),
    EXCLUDED("excluded");

    companion object {
        fun fromWire(value: String): StreamStatus =
            entries.firstOrNull { it.wire == value }
                ?: throw IllegalArgumentException("Unknown StreamStatus: $value")
    }
}

enum class TimestampSemantics(val wire: String) {
    VIDEO_ENCODER_INPUT_ESTIMATE("video_encoder_input_estimate"),
    AUDIO_CAPTURE_PTS_UNCALIBRATED("audio_capture_pts_uncalibrated"),
    INPUT_EVENT_TIME("input_event_time"),
    LOGCAT_HEADER_WALL("logcat_header_wall"),
    LOGCAT_DEVICE_READ_BOOTTIME("logcat_device_read_boottime"),
    PC_INGEST_ONLY("pc_ingest_only");

    companion object {
        fun fromWire(value: String): TimestampSemantics =
            entries.firstOrNull { it.wire == value }
                ?: throw IllegalArgumentException("Unknown TimestampSemantics: $value")
    }
}

enum class AvOffsetStatus(val wire: String) {
    UNCALIBRATED("uncalibrated"),
    CALIBRATED("calibrated"),
    USER_OVERRIDDEN("user_overridden");

    companion object {
        fun fromWire(value: String): AvOffsetStatus =
            entries.firstOrNull { it.wire == value }
                ?: throw IllegalArgumentException("Unknown AvOffsetStatus: $value")
    }
}

enum class LogCompleteness(val wire: String) {
    VERIFIED_NO_OVERFLOW("verified_no_overflow"),
    OVERFLOW_DETECTED("overflow_detected"),
    UNVERIFIED("unverified");

    companion object {
        fun fromWire(value: String): LogCompleteness =
            entries.firstOrNull { it.wire == value }
                ?: throw IllegalArgumentException("Unknown LogCompleteness: $value")
    }
}

enum class DiscontinuityKind(val wire: String) {
    DEVICE_REBOOT("device_reboot"),
    DEVICE_SLEEP("device_sleep"),
    DEVICE_WALL_JUMP("device_wall_jump"),
    PC_SLEEP("pc_sleep"),
    CONNECTION_LOST("connection_lost"),
    STREAM_GAP("stream_gap"),
    STORAGE_PRESSURE_STOP("storage_pressure_stop"),
    CLOCK_SYNC_STALE("clock_sync_stale"),
    STATIC_HOLD("static_hold");

    companion object {
        fun fromWire(value: String): DiscontinuityKind =
            entries.firstOrNull { it.wire == value }
                ?: throw IllegalArgumentException("Unknown DiscontinuityKind: $value")
    }
}

enum class UncertaintyReason(val wire: String) {
    NO_CLOCK_ANCHOR("no_clock_anchor"),
    CROSS_BOOT_BOUNDARY("cross_boot_boundary"),
    DEVICE_SLEEP_SPANNED("device_sleep_spanned"),
    DEVICE_WALL_JUMP_SPANNED("device_wall_jump_spanned"),
    CLOCK_SYNC_STALE("clock_sync_stale"),
    PC_INGEST_ONLY("pc_ingest_only"),
    PARTIAL_WINDOW_OVERLAP("partial_window_overlap"),
    LEGACY_SENTINEL_UNKNOWN("legacy_sentinel_unknown"),
    UNBRACKETED_MONOTONIC_EXTRAPOLATION("unbracketed_monotonic_extrapolation");

    companion object {
        fun fromWire(value: String): UncertaintyReason =
            entries.firstOrNull { it.wire == value }
                ?: throw IllegalArgumentException("Unknown UncertaintyReason: $value")
    }
}

sealed class Provenance {
    data object Original : Provenance()
    data class Trimmed(
        val parentBundleId: String,
        val inFrameIndex: Long,
        val outFrameIndex: Long,
    ) : Provenance()
}

sealed class TimeAlignment {
    data class Anchored(
        val bootId: String,
        val boottimeNs: Long,
        val halfWidthNs: Long,
        val semantics: TimestampSemantics,
    ) : TimeAlignment()

    data class Bounded(
        val bootId: String,
        val minBoottimeNs: Long,
        val maxBoottimeNs: Long,
        val reason: UncertaintyReason,
    ) : TimeAlignment()

    data class Unanchored(
        val pcIngestContNs: Long,
        val reason: UncertaintyReason,
    ) : TimeAlignment()
}

data class DeviceInfo(
    val serial: String,
    val model: String,
    val apiLevel: Int,
    val isEmulator: Boolean,
)

data class EpochSpan(
    val bootId: String,
    val startBoottimeNs: Long,
    val endBoottimeNs: Long,
)

data class VideoStreamMeta(
    val status: StreamStatus,
    val frameCount: Long,
    val tailMayBeIncomplete: Boolean,
    val semantics: TimestampSemantics,
)

data class AudioStreamMeta(
    val status: StreamStatus,
    val avOffsetStatus: AvOffsetStatus,
    val avOffsetMs: Long,
    val tailMayBeIncomplete: Boolean = false,
)

data class LogcatStreamMeta(
    val status: StreamStatus,
    val completeness: LogCompleteness,
    val overflowEvents: Long,
    val tailMayBeIncomplete: Boolean = false,
)

data class InputStreamMeta(
    val status: StreamStatus,
    val tailMayBeIncomplete: Boolean = false,
)

data class StreamsSummary(
    val video: VideoStreamMeta? = null,
    val audio: AudioStreamMeta? = null,
    val logcat: LogcatStreamMeta? = null,
    val input: InputStreamMeta? = null,
)

data class TimelineSummaryResponse(
    val bundleId: String,
    val path: String,
    val format: BundleFormat,
    val bundleKind: BundleKind = BundleKind.FULL,
    val isLegacy: Boolean,
    val createdAtUtc: String? = null,
    val provenance: Provenance,
    val device: DeviceInfo? = null,
    val streams: StreamsSummary,
    val unanchoredSummary: UnanchoredSummary,
    val epochs: List<EpochSpan>,
    val frameCount: Int,
    val keyframeCount: Int,
    val logMainCount: Int,
    val logUnanchoredCount: Int,
    val tapMainCount: Int,
    val tapUnanchoredCount: Int,
    val discontinuityCount: Int,
    val videoPath: String? = null,
    val audioPath: String? = null,
)

data class LogRecord(
    val seq: Long,
    val bootId: String? = null,
    val headerEpochNs: Long? = null,
    val deviceReadBoottimeNs: Long? = null,
    val pcIngestContNs: Long,
    val alignment: TimeAlignment,
    val bucket: RecordBucket,
    val level: String? = null,
    val tag: String? = null,
    val pid: Int? = null,
    val tid: Int? = null,
    val message: String,
)

data class TimelineLogsResponse(
    val logs: List<LogRecord>,
)

data class TapRecord(
    val seq: Long,
    val bootId: String? = null,
    val eventMonoNs: Long? = null,
    val pcIngestContNs: Long,
    val alignment: TimeAlignment,
    val bucket: RecordBucket,
    val action: String,
    val x: Double? = null,
    val y: Double? = null,
    val pointerId: Int? = null,
)

data class TimelineTapsResponse(
    val taps: List<TapRecord>,
)

data class DiscontinuityEvent(
    val kind: DiscontinuityKind,
    val stream: StreamKind? = null,
    val bootId: String? = null,
    val startBoottimeNs: Long? = null,
    val endBoottimeNs: Long? = null,
    val pcContStartNs: Long,
    val pcContEndNs: Long? = null,
    val deltaNs: Long? = null,
    val detail: String? = null,
)

data class TimelineDiscontinuitiesResponse(
    val discontinuities: List<DiscontinuityEvent>,
)

data class CrashMarker(
    val boottimeNs: Long? = null,
    val frameIndexHint: Long? = null,
    val processName: String,
    val packageName: String,
    val pid: Int? = null,
    val exceptionClass: String,
    val message: String,
    val sourceLogSeq: Long,
)

data class TimelineCrashesResponse(
    val crashes: List<CrashMarker> = emptyList(),
)

data class ScreenSpan(
    val startNs: Long,
    val endNs: Long,
    val startFrameIndex: Long? = null,
    val endFrameIndex: Long? = null,
    val packageName: String,
    val activityName: String,
    val shortName: String,
)

data class TimelineScreensResponse(
    val screens: List<ScreenSpan> = emptyList(),
)

// ---------------- Edit Session ----------------

data class EditSessionView(
    val sessionId: String,
    val bundleId: String,
    val inFrameIndex: Long? = null,
    val outFrameIndex: Long? = null,
    val playheadFrameIndex: Long? = null,
    val avOffsetOverrideMs: Long? = null,
    val updatedAtUtc: String,
)

data class EditSessionEnvelope(
    val session: EditSessionView? = null,
)

data class EditUpdateSessionParams(
    val sessionId: String,
    val bundleId: String,
    val inFrameIndex: Long? = null,
    val outFrameIndex: Long? = null,
    val playheadFrameIndex: Long? = null,
    val avOffsetOverrideMs: Long? = null,
)

// ---------------- Export (Trim & PNG) ----------------

data class ExportTrimParams(
    val bundleIdOrPath: String,
    val inFrameIndex: Long,
    val outFrameIndex: Long,
    val destDir: String? = null,
    val holderPid: Long? = null,
)

data class TrimReport(
    val inFrameIndex: Long,
    val outFrameIndex: Long,
    val outputFrameCount: Long,
    val headDecodedFrames: Long,
    val reencodedFrames: Long,
    val streamCopiedFrames: Long,
    val backendName: String,
    val edtsBoxCount: Int,
    val elstBoxCount: Int,
)

data class ExportTrimResponse(
    val bundleId: String,
    val bundlePath: String,
    val trimReport: TrimReport,
    val unanchoredSummary: UnanchoredSummary,
    val mainLogCount: Int,
    val unanchoredLogCount: Int,
    val mainTapCount: Int,
    val unanchoredTapCount: Int,
    val warning: String? = null,
)

// Forward-compatible T-2B RPC models (export.share_mp4 & media.decode_audio_pcm)
data class ExportShareMp4Params(
    val bundleIdOrPath: String,
    val inFrameIndex: Long,
    val outFrameIndex: Long,
    val outputPath: String? = null,
    val holderPid: Long? = null,
)

data class ExportShareMp4Response(
    val shareMp4Path: String,
    val trimmedBundleId: String? = null,
    val trimmedBundlePath: String? = null,
    val audioIncluded: Boolean = false,
    val videoFrameCount: Long = 0L,
    val edtsBoxCount: Int = 0,
    val elstBoxCount: Int = 0,
    val outputBytes: Long = 0L,
    val originalTrashed: Boolean = false,
)

data class MediaDecodeAudioPcmParams(
    val bundleIdOrPath: String,
    val outputPath: String? = null,
)

data class MediaDecodeAudioPcmResponse(
    val available: Boolean = false,
    val wavPath: String? = null,
    val sampleRateHz: Int = 0,
    val channels: Int = 0,
    val durationNs: Long = 0L,
    val avOffsetMs: Long = 0L,
    val audioLeadInOffsetNs: Long = 0L,
    val cached: Boolean = false,
)

data class ExportPngParams(
    val bundleIdOrPath: String,
    val frameIndex: Long,
    val outputPath: String,
)

data class PngExportReport(
    val frameIndex: Long,
    val width: Int,
    val height: Int,
    val backendName: String,
    val outputBytes: Long,
)

data class ExportPngResponse(
    val outputPath: String,
    val report: PngExportReport,
)

// ---------------- Media / SHM Viewport ----------------

data class MediaOpenViewportResponse(
    val viewportId: String,
    val bundleId: String,
    val shmPath: String,
    val width: Int,
    val height: Int,
    val stride: Int,
    val pixelFormat: String,
    val slotCapacity: Int,
    val frameCount: Long,
    val keyframeCount: Int,
)

data class FilmstripEntryMeta(
    val frameIndex: Long,
    val ptsNs: Long,
    val width: Int,
    val height: Int,
)

data class MediaFilmstripResponse(
    val origin: String,
    val cachePath: String,
    val count: Int,
    val elapsedMs: Long,
    val entries: List<FilmstripEntryMeta>,
)

data class MediaScrubResponse(
    val generation: Long,
    val requestedFrameIndex: Long,
    val displayedFrameIndex: Long,
    val isApproximate: Boolean,
    val width: Int,
    val height: Int,
    val elapsedMs: Long,
)

data class MediaSeekFrameResponse(
    val generation: Long,
    val frameIndex: Long,
    val isApproximate: Boolean,
    val width: Int,
    val height: Int,
    val cacheHits: Long,
    val cacheMisses: Long,
    val elapsedMs: Long,
)

data class MediaCloseViewportResponse(
    val closed: Boolean,
    val viewportId: String,
)
