package io.github.shinma06.replaybuffer.menubar

import java.awt.event.ActionEvent
import javax.swing.AbstractAction
import javax.swing.JComponent
import javax.swing.JList
import javax.swing.KeyStroke
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertSame
import org.junit.jupiter.api.Test

class WindowsMenuKeyBindingsTest {
    @Test
    fun `root and child keep Swing keystrokes and the IDE standard list actions on the same command`() {
        val aliases = mapOf("UP" to "selectPreviousRow", "DOWN" to "selectNextRow",
            "RIGHT" to "selectNextColumn", "LEFT" to "selectPreviousColumn")
        val calls = mutableListOf<String>()
        val root = JList(arrayOf("root"))
        val child = JList(arrayOf("child"))
        for ((name, list) in listOf("root" to root, "child" to child)) {
            for (key in aliases.keys + listOf("ENTER", "SPACE", "ESCAPE")) {
                WindowsMenuKeyBindings.bind(list, key, object : AbstractAction() {
                    override fun actionPerformed(e: ActionEvent) { calls += "$name:$key" }
                })
            }
        }
        for ((name, list) in listOf("root" to root, "child" to child)) {
            for (key in aliases.keys + listOf("ENTER", "SPACE", "ESCAPE")) {
                val stroke = list.getInputMap(JComponent.WHEN_FOCUSED).get(KeyStroke.getKeyStroke(key))
                val action = checkNotNull(list.actionMap.get(stroke))
                action.actionPerformed(ActionEvent(list, ActionEvent.ACTION_PERFORMED, key))
                assertEquals("$name:$key", calls.last())
                aliases[key]?.let { alias ->
                    assertSame(action, list.actionMap.get(alias), "The IDE bypasses InputMap for $alias")
                    list.actionMap.get(alias).actionPerformed(ActionEvent(list, ActionEvent.ACTION_PERFORMED, alias))
                    assertEquals("$name:$key", calls.last())
                }
            }
        }
        assertEquals(22, calls.size)
    }
}
