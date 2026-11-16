package io.github.shinma06.replaybuffer.menubar

import javax.swing.Action
import javax.swing.JComponent
import javax.swing.KeyStroke

/** The IDE may invoke standard JList actions before Swing looks up an arrow keystroke. */
internal object WindowsMenuKeyBindings {
    fun bind(component: JComponent, key: String, action: Action) {
        component.getInputMap(JComponent.WHEN_FOCUSED).put(KeyStroke.getKeyStroke(key), key)
        component.actionMap.put(key, action)
        val listAction = when (key) {
            "UP" -> "selectPreviousRow"
            "DOWN" -> "selectNextRow"
            "RIGHT" -> "selectNextColumn"
            "LEFT" -> "selectPreviousColumn"
            else -> null
        }
        if (listAction != null) component.actionMap.put(listAction, action)
    }
}
