package io.github.shinma06.replaybuffer.core

import java.nio.file.Files
import java.nio.file.Path

/** Applied values only. SDK/Run configuration resolution and draft settings belong to the IDE layer. */
data class ReplaySettings(
    val adbPath: Path? = null,
    val saveDirectory: Path? = null,
    val replaySeconds: Int = 180,
    val application: ApplicationTarget = ApplicationTarget(),
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
    }
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
enum class StreamState { WAITING, CAPTURING, RECOVERING, UNAVAILABLE }
enum class DeviceKind { PHYSICAL, EMULATOR }
enum class SavePhase { IDLE, WRITING, FAILED, COMPLETED }

data class ReplayDevice(val serial: String, val name: String, val kind: DeviceKind, val connected: Boolean)
data class CaptureGap(val stream: String, val fromNs: Long?, val toNs: Long?, val reason: String)
data class StreamSnapshot(
    val state: StreamState = StreamState.WAITING,
    val availableSeconds: Double = 0.0,
    val reason: String? = null,
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
)

data class ReplayOperation(val accepted: Boolean, val reason: String? = null, val requestId: String? = null)
