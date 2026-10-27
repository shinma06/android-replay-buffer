package io.github.shinma06.replaybuffer.ui

import com.intellij.openapi.wm.ToolWindow
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.lang.reflect.Proxy
import javax.swing.SwingUtilities

class LogcatToolWindowButtonTest {
    @Test
    fun `actual visibility and availability override stale presentation and disposed content stops lookup`() {
        SwingUtilities.invokeAndWait {
            var alive = true
            var visible = false
            var available = true
            var disposed = false
            val operations = mutableListOf<String>()
            val window = Proxy.newProxyInstance(ToolWindow::class.java.classLoader, arrayOf(ToolWindow::class.java)) { _, method, _ ->
                when (method.name) {
                    "isVisible" -> visible
                    "isAvailable" -> available
                    "isDisposed" -> disposed
                    "show", "hide" -> {
                        operations += method.name
                        visible = method.name == "show"
                        null
                    }
                    else -> error("Unexpected ToolWindow call: ${method.name}")
                }
            } as ToolWindow
            var registered: ToolWindow? = null
            val button = LogcatToolWindowButton({ alive }) {
                check(alive) { "Disposed content must not resolve the project service" }
                registered
            }
            button.refresh()
            assertFalse(button.isEnabled)
            assertTrue(button.toolTipText.contains("利用できません"))
            registered = window
            button.refresh()
            assertTrue(button.isEnabled)
            assertEquals("Logcatを開く", button.text)
            button.doClick(0)
            assertEquals(listOf("show"), operations)
            assertEquals("Logcatを閉じる", button.text)

            visible = false // External IDE close, before our queued presentation update.
            button.doClick(0)
            assertEquals(listOf("show", "show"), operations)
            visible = false
            button.refresh()
            assertEquals("Logcatを開く", button.text)
            visible = true // External IDE open, again before the queued update.
            button.doClick(0)
            assertEquals(listOf("show", "show", "hide"), operations)

            available = false // Availability can also change before the click.
            button.doClick(0)
            assertFalse(button.isEnabled)
            available = true
            button.refresh()
            disposed = true
            button.doClick(0)
            assertFalse(button.isEnabled)
            disposed = false
            button.refresh()
            alive = false
            button.doClick(0)
            assertFalse(button.isEnabled)
            assertEquals(listOf("show", "show", "hide"), operations)
            assertEquals(button.text, button.accessibleContext.accessibleName)
            assertEquals(button.toolTipText, button.accessibleContext.accessibleDescription)
        }
    }
}
