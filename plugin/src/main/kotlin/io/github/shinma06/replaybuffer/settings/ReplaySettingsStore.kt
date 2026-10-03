package io.github.shinma06.replaybuffer.settings

import com.intellij.openapi.components.PersistentStateComponent
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.State
import com.intellij.openapi.components.Storage
import com.intellij.openapi.components.StoragePathMacros
import com.intellij.util.messages.Topic
import io.github.shinma06.replaybuffer.core.ApplicationMode
import io.github.shinma06.replaybuffer.core.ApplicationTarget
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
) {
    fun validationError(): String? = validationError(validateDestination(destination))

    internal fun validationError(destinationValidation: DestinationValidation): String? {
        require(destinationValidation.destination == destination)
        if (retentionSeconds !in 1..CoreReplaySettings.MAX_REPLAY_SECONDS) return "保持時間は1〜900秒で指定してください。"
        destinationValidation.error?.let { return it }
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
            null // A new destination may be created when saving.
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

/** Keep mutable XML state detached from the immutable settings consumed by capture. */
data class ReplaySettingsState(
    var enabled: Boolean = false,
    var retentionSeconds: Int = 180,
    var destination: String = "",
    var appSelection: AppSelectionMode = AppSelectionMode.AUTOMATIC,
    var manualPackage: String = "",
)

@Service(Service.Level.PROJECT)
@State(
    name = "io.github.shinma06.replaybuffer.settings.ReplaySettingsStore",
    storages = [Storage(StoragePathMacros.WORKSPACE_FILE)],
)
class ReplaySettingsStore : PersistentStateComponent<ReplaySettingsState> {
    @Volatile
    private var stored = ReplaySettingsState()

    val enabled: Boolean get() = stored.enabled

    fun settings(): ReplaySettings {
        val state = stored
        return ReplaySettings(state.retentionSeconds, state.destination, state.appSelection, state.manualPackage)
    }

    @Synchronized
    fun setEnabled(value: Boolean) {
        stored = stored.copy(enabled = value)
    }

    fun apply(settings: ReplaySettings) {
        apply(settings, validateDestination(settings.destination))
    }

    /** Configurable supplies the result already checked in the background for this exact path. */
    @Synchronized
    internal fun apply(settings: ReplaySettings, destinationValidation: DestinationValidation) {
        val error = settings.validationError(destinationValidation)
        require(error == null) { error.orEmpty() }
        stored = stored.copy(
            retentionSeconds = settings.retentionSeconds,
            destination = settings.destination,
            appSelection = settings.appSelection,
            manualPackage = settings.manualPackage,
        )
    }

    override fun getState(): ReplaySettingsState = stored.copy()

    @Synchronized
    override fun loadState(state: ReplaySettingsState) {
        stored = state.copy(retentionSeconds = state.retentionSeconds.takeIf { it in 1..CoreReplaySettings.MAX_REPLAY_SECONDS } ?: 180)
    }

    companion object {
        val CHANGED: Topic<Runnable> = Topic.create("Android Replay Buffer settings changed", Runnable::class.java)
    }
}
