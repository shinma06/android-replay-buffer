package io.github.shinma06.replaybuffer.ui

import com.intellij.openapi.wm.ToolWindow
import javax.swing.JButton

/** EDT-only view of the IDE-owned window; callbacks must stop resolving it after content/project disposal. */
internal class LogcatToolWindowButton(
    private val alive: () -> Boolean,
    private val logcat: () -> ToolWindow?,
) : JButton() {
    init {
        addActionListener {
            // Re-read on click: the IDE may have changed visibility or availability since the last notification.
            availableWindow()?.let { window ->
                if (window.isVisible) window.hide() else window.show()
            }
            refresh()
        }
    }

    fun refresh() {
        val window = availableWindow()
        isEnabled = window != null
        text = if (window?.isVisible == true) "Logcatを閉じる" else "Logcatを開く"
        toolTipText = when {
            window == null -> "このprojectではIDE標準Logcatを利用できません。"
            window.isVisible -> "IDE標準Logcatパネルを非表示にします。"
            else -> "IDE標準Logcatパネルを表示します。"
        }
        accessibleContext.accessibleName = text
        accessibleContext.accessibleDescription = toolTipText
    }

    private fun availableWindow(): ToolWindow? = if (alive()) {
        logcat()?.takeUnless { it.isDisposed || !it.isAvailable }
    } else {
        null
    }
}
