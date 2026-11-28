package io.github.shinma06.arb.state

import io.github.shinma06.arb.model.ArbConfig
import io.github.shinma06.arb.model.ArbConfigPatch
import io.github.shinma06.arb.model.AudioCaptureMode
import io.github.shinma06.arb.model.CaptureMode
import io.github.shinma06.arb.model.IdeDefaultSaveAction
import io.github.shinma06.arb.model.OutputDirPatch
import io.github.shinma06.arb.model.StringPatch
import java.nio.file.Path

/**
 * Pure state tracker deciding whether closing an ARB Studio editor tab should prompt the user
 * about an unexported custom trim range (`VirtualFilePreCloseCheck`).
 *
 * Rules:
 * - Untouched full range `[0 .. totalFrames - 1]` -> clean (close silently without nagging).
 * - Initial range loaded from an existing edit session -> clean until modified in this tab.
 * - Custom narrower range adjusted by the user -> dirty until exported OR reset to full range.
 * - After a successful export covering the current range -> clean until edited again.
 */
class TrimCloseState {
    private val lock = Any()
    private var fullEnd: Long = 0L
    private var inFrame: Long = 0L
    private var outFrame: Long = 0L
    private var baselineRange: Pair<Long, Long>? = null
    private var lastExportedRange: Pair<Long, Long>? = null

    fun initialize(totalFrames: Long, initialIn: Long, initialOut: Long) =
        initializeTimeRange((totalFrames.coerceAtLeast(1L) - 1L), initialIn, initialOut)

    fun initializeTimeRange(end: Long, initialIn: Long, initialOut: Long) = synchronized(lock) {
        fullEnd = end
        this.inFrame = initialIn
        this.outFrame = initialOut
        this.baselineRange = initialIn to initialOut
        this.lastExportedRange = null
    }

    fun onRangeAdjusted(newIn: Long, newOut: Long) = synchronized(lock) {
        inFrame = newIn
        outFrame = newOut
        if (isFullRangeLocked()) {
            baselineRange = newIn to newOut
        }
    }

    fun markExported(exportedIn: Long, exportedOut: Long) = synchronized(lock) {
        lastExportedRange = exportedIn to exportedOut
    }

    fun currentRange(): Pair<Long, Long> = synchronized(lock) { inFrame to outFrame }

    fun hasUnexportedCustomTrim(): Boolean = synchronized(lock) {
        if (fullEnd == 0L) return false
        if (isFullRangeLocked()) return false
        val current = inFrame to outFrame
        if (current == baselineRange) return false
        if (current == lastExportedRange) return false
        true
    }

    private fun isFullRangeLocked(): Boolean =
        inFrame == 0L && outFrame == fullEnd
}

enum class StorageSizeUnit(val label: String, val bytesPerUnit: Long) {
    MIB("MiB", 1024L * 1024L),
    GIB("GiB", 1024L * 1024L * 1024L),
}

data class StorageSizeValue(
    val amount: Double,
    val unit: StorageSizeUnit,
) {
    fun toBytes(): Long = kotlin.math.round(amount * unit.bytesPerUnit.toDouble()).toLong()

    companion object {
        fun fromBytes(bytes: Long): StorageSizeValue {
            val gib = StorageSizeUnit.GIB.bytesPerUnit
            val mib = StorageSizeUnit.MIB.bytesPerUnit
            return if (bytes >= gib && bytes % (mib * 128L) == 0L) {
                StorageSizeValue(bytes.toDouble() / gib.toDouble(), StorageSizeUnit.GIB)
            } else {
                StorageSizeValue(bytes.toDouble() / mib.toDouble(), StorageSizeUnit.MIB)
            }
        }
    }
}

/**
 * Pure draft representation of the Android Studio Settings -> Tools -> Android Replay Buffer page.
 */
data class ArbSettingsDraft(
    val saveWindowSecs: Int = ArbConfig.DEFAULT_SAVE_WINDOW_SECS,
    val ringBudgetBytes: Long = ArbConfig.DEFAULT_RING_BUDGET_BYTES,
    val minFreeDiskBytes: Long = ArbConfig.DEFAULT_MIN_FREE_DISK_BYTES,
    val videoMaxSize: Int = ArbConfig.DEFAULT_VIDEO_MAX_SIZE,
    val videoMaxFps: Int = ArbConfig.DEFAULT_VIDEO_MAX_FPS,
    val audioEnabled: Boolean = true,
    val audioCaptureMode: AudioCaptureMode = AudioCaptureMode.PLAYBACK_DUP,
    val crashAutosaveEnabled: Boolean = true,
    val crashAutosavePostRollMs: Long = ArbConfig.DEFAULT_CRASH_AUTOSAVE_POST_ROLL_MS,
    val crashAutosaveCooldownSecs: Int = ArbConfig.DEFAULT_CRASH_AUTOSAVE_COOLDOWN_SECS,
    val outputDirOverride: String = "",
    val ideDefaultSaveAction: IdeDefaultSaveAction = IdeDefaultSaveAction.SAVE_AND_OPEN,
    val customArbdBinaryPath: String = "",
    val captureMode: CaptureMode = CaptureMode.VIDEO_AND_LOGS,
    val deviceSerial: String = "",
    val screenshotDirOverride: String = "",
) {
    fun normalizedOutputDir(): String? = outputDirOverride.trim().takeIf { it.isNotEmpty() }

    fun normalizedScreenshotDir(): String? = screenshotDirOverride.trim().takeIf { it.isNotEmpty() }

    fun normalizedDeviceSerial(): String? = deviceSerial.trim().takeIf { it.isNotEmpty() }

    fun normalizedCustomArbdPath(): String = customArbdBinaryPath.trim()

    fun validateDaemonFields(): String? {
        if (saveWindowSecs !in ArbConfig.MIN_SAVE_WINDOW_SECS..ArbConfig.MAX_SAVE_WINDOW_SECS) {
            return "保持ウィンドウは ${ArbConfig.MIN_SAVE_WINDOW_SECS}s 〜 ${ArbConfig.MAX_SAVE_WINDOW_SECS}s の範囲で指定してください。"
        }
        if (ringBudgetBytes !in ArbConfig.MIN_RING_BUDGET_BYTES..ArbConfig.MAX_RING_BUDGET_BYTES) {
            return "リングバッファ上限は 16 MiB 〜 64 GiB の範囲で指定してください。"
        }
        if (minFreeDiskBytes !in ArbConfig.MIN_FREE_DISK_FLOOR_BYTES..ArbConfig.MAX_FREE_DISK_FLOOR_BYTES) {
            return "最低空き容量フロアは 64 MiB 〜 1 TiB の範囲で指定してください。"
        }
        if (videoMaxSize !in ArbConfig.MIN_VIDEO_MAX_SIZE..ArbConfig.MAX_VIDEO_MAX_SIZE) {
            return "映像最大長辺は ${ArbConfig.MIN_VIDEO_MAX_SIZE} 〜 ${ArbConfig.MAX_VIDEO_MAX_SIZE} px の範囲で指定してください。"
        }
        if (videoMaxFps !in ArbConfig.MIN_VIDEO_MAX_FPS..ArbConfig.MAX_VIDEO_MAX_FPS) {
            return "映像最大 fps は ${ArbConfig.MIN_VIDEO_MAX_FPS} 〜 ${ArbConfig.MAX_VIDEO_MAX_FPS} の範囲で指定してください。"
        }
        if (crashAutosavePostRollMs !in 0..ArbConfig.MAX_CRASH_AUTOSAVE_POST_ROLL_MS) {
            return "クラッシュ後の記録時間は 0〜10,000 ミリ秒で指定してください。"
        }
        if (crashAutosaveCooldownSecs !in ArbConfig.MIN_CRASH_AUTOSAVE_COOLDOWN_SECS..ArbConfig.MAX_CRASH_AUTOSAVE_COOLDOWN_SECS) {
            return "クラッシュ自動保存のクールダウンは ${ArbConfig.MIN_CRASH_AUTOSAVE_COOLDOWN_SECS}s 〜 ${ArbConfig.MAX_CRASH_AUTOSAVE_COOLDOWN_SECS}s の範囲で指定してください。"
        }
        val serial = normalizedDeviceSerial()
        if (serial != null && !DeviceSelectionPolicy.isValidAdbSerial(serial)) {
            return "端末シリアルの形式が不正です（USB / Wi-Fi IP:ポート / mDNS シリアルのみ指定可能）。"
        }
        val dir = normalizedOutputDir()
        if (dir != null) {
            val isAbsolute = runCatching { Path.of(dir).isAbsolute }.getOrDefault(false)
            if (!isAbsolute) {
                return "保存先フォルダの上書きには絶対パスを指定してください。"
            }
        }
        val shotDir = normalizedScreenshotDir()
        if (shotDir != null) {
            val isAbsolute = runCatching { Path.of(shotDir).isAbsolute }.getOrDefault(false)
            if (!isAbsolute) {
                return "スクリーンショット保存先フォルダには絶対パスを指定してください。"
            }
        }
        return null
    }

    fun isDaemonModifiedAgainst(baseline: ArbConfig): Boolean =
        saveWindowSecs != baseline.saveWindowSecs ||
            ringBudgetBytes != baseline.ringBudgetBytes ||
            minFreeDiskBytes != baseline.minFreeDiskBytes ||
            videoMaxSize != baseline.videoMaxSize ||
            videoMaxFps != baseline.videoMaxFps ||
            audioCaptureMode != baseline.audioCaptureMode ||
            audioEnabled != baseline.audioEnabled ||
            captureMode != baseline.captureMode ||
            normalizedDeviceSerial() != baseline.deviceSerial ||
            crashAutosaveEnabled != baseline.crashAutosaveEnabled ||
            crashAutosaveCooldownSecs != baseline.crashAutosaveCooldownSecs ||
            crashAutosavePostRollMs != baseline.crashAutosavePostRollMs ||
            normalizedOutputDir() != baseline.outputDirOverride ||
            normalizedScreenshotDir() != baseline.screenshotDirOverride ||
            ideDefaultSaveAction != baseline.ideDefaultSaveAction

    /** Preserve edited controls and refresh untouched fields (including CLI-managed values). */
    fun rebase(previous: ArbConfig, refreshed: ArbConfig): ArbSettingsDraft {
        val base = fromConfig(previous)
        val next = fromConfig(refreshed, customArbdBinaryPath)
        return next.copy(
            saveWindowSecs = if (saveWindowSecs == base.saveWindowSecs) next.saveWindowSecs else saveWindowSecs,
            ringBudgetBytes = if (ringBudgetBytes == base.ringBudgetBytes) next.ringBudgetBytes else ringBudgetBytes,
            minFreeDiskBytes = if (minFreeDiskBytes == base.minFreeDiskBytes) next.minFreeDiskBytes else minFreeDiskBytes,
            videoMaxSize = if (videoMaxSize == base.videoMaxSize) next.videoMaxSize else videoMaxSize,
            videoMaxFps = if (videoMaxFps == base.videoMaxFps) next.videoMaxFps else videoMaxFps,
            audioCaptureMode = if (audioCaptureMode == base.audioCaptureMode) next.audioCaptureMode else audioCaptureMode,
            audioEnabled = if (audioEnabled == base.audioEnabled) next.audioEnabled else audioEnabled,
            captureMode = if (captureMode == base.captureMode) next.captureMode else captureMode,
            crashAutosaveEnabled = if (crashAutosaveEnabled == base.crashAutosaveEnabled) next.crashAutosaveEnabled else crashAutosaveEnabled,
            crashAutosaveCooldownSecs = if (crashAutosaveCooldownSecs == base.crashAutosaveCooldownSecs) next.crashAutosaveCooldownSecs else crashAutosaveCooldownSecs,
            crashAutosavePostRollMs = if (crashAutosavePostRollMs == base.crashAutosavePostRollMs) next.crashAutosavePostRollMs else crashAutosavePostRollMs,
            outputDirOverride = if (outputDirOverride == base.outputDirOverride) next.outputDirOverride else outputDirOverride,
            ideDefaultSaveAction = if (ideDefaultSaveAction == base.ideDefaultSaveAction) next.ideDefaultSaveAction else ideDefaultSaveAction,
        )
    }

    fun toConfigPatch(baseline: ArbConfig): ArbConfigPatch {
        val nextDir = normalizedOutputDir()
        val dirPatch = when {
            nextDir == baseline.outputDirOverride -> OutputDirPatch.Unchanged
            nextDir == null -> OutputDirPatch.Clear
            else -> OutputDirPatch.Set(nextDir)
        }
        val nextShotDir = normalizedScreenshotDir()
        val shotDirPatch = when {
            nextShotDir == baseline.screenshotDirOverride -> OutputDirPatch.Unchanged
            nextShotDir == null -> OutputDirPatch.Clear
            else -> OutputDirPatch.Set(nextShotDir)
        }
        val nextSerial = normalizedDeviceSerial()
        val serialPatch = when {
            nextSerial == baseline.deviceSerial -> StringPatch.Unchanged
            nextSerial == null -> StringPatch.Clear
            else -> StringPatch.Set(nextSerial)
        }
        return ArbConfigPatch(
            saveWindowSecs = saveWindowSecs.takeIf { it != baseline.saveWindowSecs },
            ringBudgetBytes = ringBudgetBytes.takeIf { it != baseline.ringBudgetBytes },
            minFreeDiskBytes = minFreeDiskBytes.takeIf { it != baseline.minFreeDiskBytes },
            videoMaxSize = videoMaxSize.takeIf { it != baseline.videoMaxSize },
            videoMaxFps = videoMaxFps.takeIf { it != baseline.videoMaxFps },
            audioCaptureMode = audioCaptureMode.takeIf { it != baseline.audioCaptureMode },
            audioEnabled = audioEnabled.takeIf { it != baseline.audioEnabled },
            captureMode = captureMode.takeIf { it != baseline.captureMode },
            deviceSerial = serialPatch,
            crashAutosaveEnabled = crashAutosaveEnabled.takeIf { it != baseline.crashAutosaveEnabled },
            crashAutosaveCooldownSecs = crashAutosaveCooldownSecs.takeIf { it != baseline.crashAutosaveCooldownSecs },
            crashAutosavePostRollMs = crashAutosavePostRollMs.takeIf { it != baseline.crashAutosavePostRollMs },
            outputDirOverride = dirPatch,
            screenshotDirOverride = shotDirPatch,
            ideDefaultSaveAction = ideDefaultSaveAction.takeIf { it != baseline.ideDefaultSaveAction },
        )
    }

    companion object {
        fun fromConfig(config: ArbConfig, customArbdBinaryPath: String = ""): ArbSettingsDraft = ArbSettingsDraft(
            saveWindowSecs = config.saveWindowSecs,
            ringBudgetBytes = config.ringBudgetBytes,
            minFreeDiskBytes = config.minFreeDiskBytes,
            videoMaxSize = config.videoMaxSize,
            videoMaxFps = config.videoMaxFps,
            audioCaptureMode = config.audioCaptureMode,
            audioEnabled = config.audioEnabled,
            crashAutosaveEnabled = config.crashAutosaveEnabled,
            crashAutosaveCooldownSecs = config.crashAutosaveCooldownSecs,
            crashAutosavePostRollMs = config.crashAutosavePostRollMs,
            outputDirOverride = config.outputDirOverride.orEmpty(),
            ideDefaultSaveAction = config.ideDefaultSaveAction,
            customArbdBinaryPath = customArbdBinaryPath,
            captureMode = config.captureMode,
            deviceSerial = config.deviceSerial.orEmpty(),
            screenshotDirOverride = config.screenshotDirOverride.orEmpty(),
        )
    }
}
