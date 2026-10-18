package io.github.shinma06.replaybuffer.core

import java.nio.file.Files
import java.nio.file.Path

/** Applied values only. SDK/Run configuration resolution and draft settings belong to the IDE layer. */
data class ReplaySettings(
    val adbPath: Path? = null,
    val saveDirectory: Path? = null,
    val replaySeconds: Int = 180,
    val application: ApplicationTarget = ApplicationTarget(),
    val captureMode: CaptureMode = CaptureMode.VIDEO_AND_LOGS,
    val autoSaveOnCrash: Boolean = false,
) {
    fun validate(): ReplaySettings {
        require(adbPath == null || adbPath.isAbsolute && Files.isRegularFile(adbPath) && Files.isExecutable(adbPath)) {
            "Android SDKのadb実行ファイルを指定してください"
        }
        require(saveDirectory == null || saveDirectory.isAbsolute) { "保存先は絶対パスで指定してください" }
        require(replaySeconds in 1..MAX_REPLAY_SECONDS) { "保持時間は1〜900秒で指定してください" }
        application.validate()
        return copy(adbPath = adbPath?.normalize(), saveDirectory = saveDirectory?.normalize())
    }

    companion object {
        const val MAX_REPLAY_SECONDS = 900
        // 900s at 8Mbps = 900MB, plus <=32MiB decoder GOP, below the 1GiB disk ring ceiling.
        const val VIDEO_BYTES = 1024L * 1024 * 1024
        const val LOG_BYTES = 32L * 1024 * 1024
        const val MIN_FREE_BYTES = 128L * 1024 * 1024
        const val MAX_VIDEO_PACKETS = 60_000
        const val MAX_CONFIG_PACKET_BYTES = 64 * 1024
        const val CONFIG_MEMORY_BYTES = 8L * 1024 * 1024
    }
}

enum class CaptureMode {
    VIDEO_AND_LOGS, LOG_ONLY;

    val videoRequested: Boolean get() = this == VIDEO_AND_LOGS
    val streams: List<String> get() = if (videoRequested) listOf("video", "device_log", "app_log") else listOf("device_log", "app_log")
}

enum class ApplicationMode { AUTO, MANUAL }

data class ApplicationTarget(
    val packageName: String? = null,
    val mode: ApplicationMode = ApplicationMode.AUTO,
    val unresolvedReason: String? = null,
) {
    fun validate() {
        require(packageName == null || packageName.length <= 255 && PACKAGE.matches(packageName)) {
            "package名を確認してください"
        }
        require(mode != ApplicationMode.MANUAL || packageName != null) { "手動指定にはpackage名が必要です" }
        require(unresolvedReason == null || unresolvedReason.length <= 512)
    }

    companion object {
        private val PACKAGE = Regex("[A-Za-z][A-Za-z0-9_]*(\\.[A-Za-z][A-Za-z0-9_]*)+")
    }
}

enum class CaptureState { DISABLED, WAITING, CAPTURING, RECOVERING, PARTIAL, MULTIPLE_DEVICES }
enum class StreamState { WAITING, CAPTURING, RECOVERING, UNAVAILABLE, NOT_REQUESTED }
enum class DeviceKind { PHYSICAL, EMULATOR }
enum class SavePhase { IDLE, WRITING, FAILED, COMPLETED, CANCELLED }

enum class AutoSaveState { OFF, WAITING, ARMED, SUSPENDED }

enum class AutoSaveReason(val description: String) {
    DETECTED("対象の例外報告を照合しました"),
    ACCEPTED_WRITING("自動保存中です"),
    SAVED("自動保存が完了しました"),
    SAVE_FAILED("自動保存に失敗しました"),
    CANCELLED("公開前に自動保存を取消しました"),
    UNKNOWN_FORMAT("対象の報告形式を確認できません"),
    UNKNOWN_IDENTITY("対象またはsystem_serverの帰属を確認できません"),
    UNKNOWN_UNCORROBORATED("対象の例外報告を相互確認できません"),
    UNKNOWN_AMBIGUOUS("複数の報告が対応し、同一事象を確定できません"),
    UNSUPPORTED_NATIVE("native crashは自動保存の対応外です"),
    NOT_A_SUPPORTED_FATAL_EVENT("回復可能な報告は自動保存しません"),
    NOT_SAVED_DISABLED("自動保存を無効にしました"),
    NOT_SAVED_TARGET_CHANGED("対象アプリが変更されたため保存しません"),
    NOT_SAVED_STALE_OWNER("取得接続が変更されたため保存しません"),
    NOT_SAVED_CLOCK_UNKNOWN("報告の時計対応を確認できません"),
    NOT_SAVED_PRE_ARM_EVENT("自動保存の適用前の報告は保存しません"),
    NOT_SAVED_BUSY("別の保存中のため自動保存しません"),
    NOT_SAVED_FAILED_PIN("失敗した固定対象を保全するため自動保存しません"),
    NOT_SAVED_COOLDOWN("前回の自動保存受付から30秒未満のため保存しません"),
    NOT_SAVED_DESTINATION_UNSET("保存先が未指定のため自動保存しません"),
    NOT_SAVED_NO_DATA("保存できる取得データがありません"),
    NOT_SAVED_EVIDENCE_LOST("例外報告の元データが保持されていないため保存しません"),
    NOT_SAVED_EVENT_OUTSIDE_WINDOW("例外報告が保持窓より前のため保存しません"),
    NOT_SAVED_WINDOW_UNKNOWN("例外報告が保持窓内と確定できないため保存しません"),
    DETECTOR_OVERFLOW("検出の保持上限です。自動保存をOFF→ONで再設定してください"),
    LIMIT_REACHED("自動保存の回数・記録上限です。OFF→ONで再設定してください"),
}

data class AutoSaveOutcome(
    val id: String,
    val event: Boolean,
    val reason: AutoSaveReason,
    val requestId: String? = null,
)

data class AutoSaveSnapshot(
    val state: AutoSaveState = AutoSaveState.OFF,
    val reason: String? = null,
    val admissionsRemaining: Int = 8,
    val notSavedCount: Int = 0,
    val lastOutcome: AutoSaveOutcome? = null,
    val warning: String? = null,
)

data class ReplayDevice(val serial: String, val name: String, val kind: DeviceKind, val connected: Boolean)
data class CaptureGap(
    val stream: String, val fromNs: Long?, val toNs: Long?, val reason: String,
    val boundaryUncertaintyNs: Long? = null, val generation: Long = 0, val clockEpoch: Int? = null,
)
data class StreamSnapshot(
    val state: StreamState = StreamState.WAITING,
    val availableSeconds: Double = 0.0,
    val reason: String? = null,
    val gaps: List<CaptureGap> = emptyList(),
)

data class ApplicationPeriodSnapshot(
    val packageName: String?, val fromNs: Long?, val toNs: Long?, val clockEpoch: Int,
    val resolved: Boolean,
)

/** Sequence-axis display of the last received image; no new image was confirmed in this range. */
data class VideoTailSnapshot(
    val fromNs: Long?, val toNs: Long?, val sourcePtsUs: Long, val sourceSequenceNs: Long?,
    val clockEpoch: Int, val generation: Long, val displayHeld: Boolean,
)

data class SaveSnapshot(
    val phase: SavePhase = SavePhase.IDLE,
    val requestId: String? = null,
    val sequenceId: String? = null,
    val windowStartNs: Long? = null,
    val windowEndNs: Long? = null,
    val replaySeconds: Int? = null,
    val directory: Path? = null,
    val error: String? = null,
    val missingKinds: List<String> = emptyList(),
    val device: ReplayDevice? = null,
    val application: ApplicationTarget? = null,
    val applicationHistory: List<ApplicationPeriodSnapshot> = emptyList(),
    val videoTail: VideoTailSnapshot? = null,
    val captureMode: CaptureMode = CaptureMode.VIDEO_AND_LOGS,
    val attempt: Long = 0,
    val autoEventId: String? = null,
    val retainedPath: Path? = null,
)

/** Callbacks run on replay-control, never EDT. Consumers must dispatch to EDT and reject stale revisions. */
data class ReplaySnapshot(
    val revision: Long,
    val generation: Long,
    val enabled: Boolean,
    val captureState: CaptureState,
    val settings: ReplaySettings,
    val settingsRevision: Long,
    val device: ReplayDevice? = null,
    val sequenceId: String? = null,
    val windowEndNs: Long? = null,
    val frozen: Boolean = false,
    val video: StreamSnapshot = StreamSnapshot(),
    val deviceLog: StreamSnapshot = StreamSnapshot(),
    val appLog: StreamSnapshot = StreamSnapshot(),
    val save: SaveSnapshot = SaveSnapshot(),
    val canSave: Boolean = false,
    val saveDisabledReason: String? = null,
    val error: String? = null,
    val windowStartNs: Long? = null,
    val cleanupPendingCount: Int = 0,
    val autoSave: AutoSaveSnapshot = AutoSaveSnapshot(),
    val lastSave: SaveSnapshot? = null,
)

data class ReplayOperation(val accepted: Boolean, val reason: String? = null, val requestId: String? = null)
