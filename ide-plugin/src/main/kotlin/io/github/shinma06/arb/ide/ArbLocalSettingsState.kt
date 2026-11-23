package io.github.shinma06.arb.ide

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.components.PersistentStateComponent
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.State
import com.intellij.openapi.components.Storage
import java.nio.file.Path

/**
 * Application-level IntelliJ [PersistentStateComponent] storing IDE-local preferences that
 * must remain available even when `arbd` is offline (e.g. a manual `arbd` binary path override).
 */
@Service(Service.Level.APP)
@State(
    name = "ArbLocalSettings",
    storages = [Storage("arb-v2-local.xml")],
)
class ArbLocalSettingsState : PersistentStateComponent<ArbLocalSettingsState.State> {
    data class State(
        var customArbdBinaryPath: String = "",
    )

    private var internalState = State()

    var customArbdBinaryPath: String
        get() = internalState.customArbdBinaryPath
        set(value) {
            internalState.customArbdBinaryPath = value.trim()
        }

    fun customArbdBinaryPathOrNull(): Path? =
        internalState.customArbdBinaryPath.trim().takeIf { it.isNotEmpty() }?.let { raw ->
            runCatching { Path.of(raw) }.getOrNull()
        }

    override fun getState(): State = internalState

    override fun loadState(state: State) {
        internalState = State(customArbdBinaryPath = state.customArbdBinaryPath.trim())
    }

    companion object {
        fun getInstance(): ArbLocalSettingsState =
            ApplicationManager.getApplication().getService(ArbLocalSettingsState::class.java)

        fun getInstanceOrNull(): ArbLocalSettingsState? = runCatching {
            ApplicationManager.getApplication()?.getService(ArbLocalSettingsState::class.java)
        }.getOrNull()
    }
}
