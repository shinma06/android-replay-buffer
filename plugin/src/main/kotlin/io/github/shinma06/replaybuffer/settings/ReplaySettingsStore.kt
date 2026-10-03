package io.github.shinma06.replaybuffer.settings

import com.intellij.openapi.components.PersistentStateComponent
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.State
import com.intellij.openapi.components.Storage
import com.intellij.openapi.components.StoragePathMacros
import com.intellij.util.messages.Topic
import java.nio.file.InvalidPathException
import java.nio.file.Path

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
    fun validationError(): String? {
        if (retentionSeconds <= 0) return "保持時間は正の整数を入力してください。"
        if (destination.isNotEmpty()) {
            try {
                if (!Path.of(destination).isAbsolute) return "保存先は絶対パスで指定してください。"
            } catch (_: InvalidPathException) {
                return "保存先のパスを確認してください。"
            }
        }
        if (appSelection == AppSelectionMode.MANUAL && !isApplicationId(manualPackage)) {
            return "package名は英字で始まる各部分をピリオドで区切って指定してください。"
        }
        return null
    }
}

internal fun isApplicationId(value: String): Boolean =
    value.matches(Regex("[A-Za-z][A-Za-z0-9_]*(\\.[A-Za-z][A-Za-z0-9_]*)+"))

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

    @Synchronized
    fun apply(settings: ReplaySettings) {
        require(settings.validationError() == null) { settings.validationError().orEmpty() }
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
        stored = state.copy(retentionSeconds = state.retentionSeconds.takeIf { it > 0 } ?: 180)
    }

    companion object {
        val CHANGED: Topic<Runnable> = Topic.create("Android Replay Buffer settings changed", Runnable::class.java)
    }
}
