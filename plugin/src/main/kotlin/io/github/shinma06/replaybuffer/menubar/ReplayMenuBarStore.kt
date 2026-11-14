package io.github.shinma06.replaybuffer.menubar

import com.intellij.openapi.components.SerializablePersistentStateComponent
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.State
import com.intellij.openapi.components.Storage
import com.intellij.util.messages.Topic
import com.intellij.util.xmlb.annotations.OptionTag

data class ReplayMenuBarState @JvmOverloads constructor(
    @field:OptionTag("showInMenuBar")
    @JvmField val showInMenuBar: Boolean = false,
)

/**
 * The IDE-wide "show in menu bar" choice; it applies to every open project, so it must not live in the per-project
 * ReplaySettingsStore. Off by default — the user, not the plugin, decides whether a menu bar icon appears.
 */
@Service(Service.Level.APP)
@State(
    name = "io.github.shinma06.replaybuffer.menubar.ReplayMenuBarStore",
    storages = [Storage("android-replay-buffer.xml")],
)
class ReplayMenuBarStore : SerializablePersistentStateComponent<ReplayMenuBarState>(ReplayMenuBarState()) {
    var showInMenuBar: Boolean
        get() = state.showInMenuBar
        set(value) {
            if (value != state.showInMenuBar) updateState { it.copy(showInMenuBar = value) }
        }

    companion object {
        val CHANGED: Topic<Runnable> = Topic.create("Android Replay Buffer menu bar setting changed", Runnable::class.java)
    }
}
