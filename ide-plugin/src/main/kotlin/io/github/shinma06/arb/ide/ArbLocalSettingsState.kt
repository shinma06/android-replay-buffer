package io.github.shinma06.arb.ide

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.components.PersistentStateComponent
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.State
import com.intellij.openapi.components.Storage
import java.nio.file.Path

/**
 * Application-level IntelliJ [PersistentStateComponent] storing IDE-local preferences that
 * must remain available even when `arbd` is offline (e.g. a manual `arbd` binary path override
 * and Android Studio Logcat screenshot destination inheritance).
 */
@Service(Service.Level.APP)
@State(
    name = "ArbLocalSettings",
    storages = [Storage("arb-local.xml")],
)
class ArbLocalSettingsState : PersistentStateComponent<ArbLocalSettingsState.State> {
    data class State(
        var customArbdBinaryPath: String = "",
        var screenshotInheritLogcat: Boolean = true,
        var customScreenshotDir: String = "",
        var burnTapsOnExport: Boolean = true,
    )

    private var internalState = State()

    var customArbdBinaryPath: String
        get() = internalState.customArbdBinaryPath
        set(value) {
            internalState.customArbdBinaryPath = value.trim()
        }

    var screenshotInheritLogcat: Boolean
        get() = internalState.screenshotInheritLogcat
        set(value) {
            internalState.screenshotInheritLogcat = value
        }

    var customScreenshotDir: String
        get() = internalState.customScreenshotDir
        set(value) {
            internalState.customScreenshotDir = value.trim()
        }

    var burnTapsOnExport: Boolean
        get() = internalState.burnTapsOnExport
        set(value) {
            internalState.burnTapsOnExport = value
        }

    fun customArbdBinaryPathOrNull(): Path? =
        internalState.customArbdBinaryPath.trim().takeIf { it.isNotEmpty() }?.let { raw ->
            runCatching { Path.of(raw) }.getOrNull()
        }

    fun customScreenshotDirOrNull(): Path? =
        internalState.customScreenshotDir.trim().takeIf { it.isNotEmpty() }?.let { raw ->
            runCatching { Path.of(raw) }.getOrNull()
        }

    override fun getState(): State = internalState

    override fun loadState(state: State) {
        internalState = State(
            customArbdBinaryPath = state.customArbdBinaryPath.trim(),
            screenshotInheritLogcat = state.screenshotInheritLogcat,
            customScreenshotDir = state.customScreenshotDir.trim(),
            burnTapsOnExport = state.burnTapsOnExport,
        )
    }

    companion object {
        fun getInstance(): ArbLocalSettingsState =
            ApplicationManager.getApplication().getService(ArbLocalSettingsState::class.java)

        fun getInstanceOrNull(): ArbLocalSettingsState? = runCatching {
            ApplicationManager.getApplication()?.getService(ArbLocalSettingsState::class.java)
        }.getOrNull()
    }
}
