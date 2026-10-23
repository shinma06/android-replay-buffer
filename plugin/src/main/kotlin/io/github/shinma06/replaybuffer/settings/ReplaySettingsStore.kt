package io.github.shinma06.replaybuffer.settings

import com.intellij.openapi.components.SerializablePersistentStateComponent
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.State
import com.intellij.openapi.components.Storage
import com.intellij.openapi.components.StoragePathMacros
import com.intellij.util.messages.Topic
import com.intellij.util.xmlb.annotations.OptionTag
import io.github.shinma06.replaybuffer.core.ApplicationMode
import io.github.shinma06.replaybuffer.core.ApplicationTarget
import io.github.shinma06.replaybuffer.core.CaptureMode
import io.github.shinma06.replaybuffer.core.ReplaySettings as CoreReplaySettings
import java.io.IOException
import java.nio.file.Files
import java.nio.file.InvalidPathException
import java.nio.file.NoSuchFileException
import java.nio.file.Path
import java.nio.file.attribute.BasicFileAttributes

enum class AppSelectionMode(private val label: String) {
    AUTOMATIC("自動（Android Studioの実行対象）"),
    MANUAL("手動"),
    ;

    override fun toString(): String = label
}

data class ReplaySettings(
    val retentionSeconds: Int = 180,
    val destination: String = "",
    val appSelection: AppSelectionMode = AppSelectionMode.AUTOMATIC,
    val manualPackage: String = "",
    val captureMode: CaptureMode = CaptureMode.VIDEO_AND_LOGS,
    val autoSaveOnCrash: Boolean = false,
    val screenshotUsesCommonDestination: Boolean = true,
    val screenshotDirectory: String = "",
) {
    val screenshotDestination: String get() = if (screenshotUsesCommonDestination) destination else screenshotDirectory

    fun validationError(): String? = validationError(validateDestination(destination), validateDestination(screenshotDestination))

    internal fun validationError(
        destinationValidation: DestinationValidation,
        screenshotValidation: DestinationValidation = validateDestination(screenshotDestination),
    ): String? {
        require(destinationValidation.destination == destination)
        if (retentionSeconds !in 1..CoreReplaySettings.MAX_REPLAY_SECONDS) return "保持時間は1〜900秒で指定してください。"
        destinationValidation.error?.let { return it }
        require(screenshotValidation.destination == screenshotDestination)
        if (!screenshotUsesCommonDestination) {
            if (screenshotDirectory.isEmpty()) return "スクリーンショット専用の保存先を指定してください。"
            screenshotValidation.error?.let { return "スクリーンショット: $it" }
        }
        if (appSelection == AppSelectionMode.MANUAL && !isApplicationId(manualPackage)) {
            return "package名は255文字以内で、英字で始まる各部分をピリオドで区切って指定してください。"
        }
        return null
    }
}

internal data class DestinationValidation(val destination: String, val error: String?)

/** Reads filesystem attributes; call off EDT for nonempty destinations. */
internal fun validateDestination(destination: String): DestinationValidation {
    val error = if (destination.isEmpty()) {
        null
    } else {
        try {
            val path = Path.of(destination)
            when {
                !path.isAbsolute -> "保存先は絶対パスで指定してください。"
                !Files.readAttributes(path, BasicFileAttributes::class.java).isDirectory -> "保存先はファイルではなくフォルダを指定してください。"
                else -> null
            }
        } catch (_: InvalidPathException) {
            "保存先のパスを確認してください。"
        } catch (_: NoSuchFileException) {
            "保存先フォルダが存在しません。フォルダを作成してから指定してください。"
        } catch (_: IOException) {
            "保存先を確認できません。パスとアクセス権を確認してください。"
        } catch (_: SecurityException) {
            "保存先を確認できません。パスとアクセス権を確認してください。"
        }
    }
    return DestinationValidation(destination, error)
}

internal fun isApplicationId(value: String): Boolean = try {
    ApplicationTarget(packageName = value, mode = ApplicationMode.MANUAL).validate()
    true
} catch (_: IllegalArgumentException) {
    false
}

/** Explicit option bindings keep legacy XML fields when the state becomes immutable. */
data class ReplaySettingsState(
    @field:OptionTag("enabled")
    @JvmField val enabled: Boolean = false,
    @field:OptionTag("retentionSeconds")
    @JvmField val retentionSeconds: Int = 180,
    @field:OptionTag("destination")
    @JvmField val destination: String = "",
    @field:OptionTag("appSelection")
    @JvmField val appSelection: AppSelectionMode = AppSelectionMode.AUTOMATIC,
    @field:OptionTag("manualPackage")
    @JvmField val manualPackage: String = "",
    @field:OptionTag("captureMode")
    @JvmField val captureMode: CaptureMode = CaptureMode.VIDEO_AND_LOGS,
    @field:OptionTag("autoSaveOnCrash")
    @JvmField val autoSaveOnCrash: Boolean = false,
    @field:OptionTag("screenshotUsesCommonDestination")
    @JvmField val screenshotUsesCommonDestination: Boolean = true,
    @field:OptionTag("screenshotDirectory")
    @JvmField val screenshotDirectory: String = "",
)

@Service(Service.Level.PROJECT)
@State(
    name = "io.github.shinma06.replaybuffer.settings.ReplaySettingsStore",
    storages = [Storage(StoragePathMacros.WORKSPACE_FILE)],
)
class ReplaySettingsStore : SerializablePersistentStateComponent<ReplaySettingsState>(ReplaySettingsState()) {

    val enabled: Boolean get() = state.enabled

    fun settings(): ReplaySettings {
        val snapshot = state
        return ReplaySettings(snapshot.retentionSeconds, snapshot.destination, snapshot.appSelection, snapshot.manualPackage,
            snapshot.captureMode, snapshot.autoSaveOnCrash, snapshot.screenshotUsesCommonDestination, snapshot.screenshotDirectory)
    }

    fun setEnabled(value: Boolean) {
        updateState { it.copy(enabled = value) }
    }

    fun apply(settings: ReplaySettings) {
        apply(settings, validateDestination(settings.destination), validateDestination(settings.screenshotDestination))
    }

    /** Configurable supplies the result already checked in the background for this exact path. */
    internal fun apply(
        settings: ReplaySettings,
        destinationValidation: DestinationValidation,
        screenshotValidation: DestinationValidation = validateDestination(settings.screenshotDestination),
    ) {
        val error = settings.validationError(destinationValidation, screenshotValidation)
        require(error == null) { error.orEmpty() }
        updateState {
            require(!it.enabled || settings.captureMode == it.captureMode) { "取得モードは取得をOFFにしてから変更してください。" }
            it.copy(
                retentionSeconds = settings.retentionSeconds,
                destination = settings.destination,
                appSelection = settings.appSelection,
                manualPackage = settings.manualPackage,
                captureMode = settings.captureMode,
                autoSaveOnCrash = settings.autoSaveOnCrash,
                screenshotUsesCommonDestination = settings.screenshotUsesCommonDestination,
                screenshotDirectory = settings.screenshotDirectory,
            )
        }
    }

    /** Imports only legacy capture values; concurrent GUI choices retain their own fields. */
    internal fun importLegacy(
        retentionSeconds: Int,
        destination: String,
        destinationValidation: DestinationValidation = validateDestination(destination),
    ) {
        ReplaySettings(retentionSeconds, destination).validationError(destinationValidation)?.let { throw IllegalArgumentException(it) }
        updateState {
            require(!it.enabled) { "旧設定の移行前に取得をOFFにしてください。" }
            it.copy(retentionSeconds = retentionSeconds, destination = destination)
        }
    }

    override fun loadState(state: ReplaySettingsState) {
        super.loadState(state.copy(retentionSeconds = state.retentionSeconds.takeIf { it in 1..CoreReplaySettings.MAX_REPLAY_SECONDS } ?: 180))
    }

    companion object {
        val CHANGED: Topic<Runnable> = Topic.create("Android Replay Buffer settings changed", Runnable::class.java)
    }
}
