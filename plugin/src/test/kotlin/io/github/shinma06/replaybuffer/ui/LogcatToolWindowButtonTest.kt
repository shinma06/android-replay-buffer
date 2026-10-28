package io.github.shinma06.replaybuffer.ui

import com.intellij.openapi.wm.ToolWindow
import io.github.shinma06.replaybuffer.core.DeviceKind
import io.github.shinma06.replaybuffer.core.ReplayDevice
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Test
import java.lang.reflect.Proxy
import java.util.concurrent.CompletableFuture
import javax.swing.SwingUtilities

class LogcatToolWindowButtonTest {
    @Test
    fun `only the confirmed full Studio build can enter the target selection bridge`() {
        assertNull(logcatTargetBuildReason("AI-262.9437.185.2621.16467767"))
        assertNotNull(logcatTargetBuildReason("AI-262.9437.185.2621.16467768"))
        assertNotNull(logcatTargetBuildReason("IC-262.9437.185.2621.16467767"))
    }

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
            val button = LogcatToolWindowButton({ alive }, { ReplayDevice("A", "target", DeviceKind.PHYSICAL, true) }, { _, serial, _ ->
                assertEquals("A", serial)
                CompletableFuture.completedFuture(null)
            }) {
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

    @Test
    fun `opening rechecks canonical target and readiness while closing works without a target`() {
        SwingUtilities.invokeAndWait {
            var alive = true
            var visible = false
            var target: ReplayDevice? = null
            var reason: String? = "端末を切り替え中です。再試行してください。"
            var duringSelection: () -> Unit = {}
            val selected = mutableListOf<String>()
            val operations = mutableListOf<String>()
            val window = Proxy.newProxyInstance(ToolWindow::class.java.classLoader, arrayOf(ToolWindow::class.java)) { _, method, _ ->
                when (method.name) {
                    "isVisible" -> visible
                    "isAvailable" -> true
                    "isDisposed" -> false
                    "show", "hide" -> {
                        operations += method.name
                        visible = method.name == "show"
                        null
                    }
                    else -> error("Unexpected ToolWindow call: ${method.name}")
                }
            } as ToolWindow
            val button = LogcatToolWindowButton({ alive }, {
                check(alive)
                target
            }, { _, serial, _ ->
                selected += serial
                duringSelection()
                CompletableFuture.completedFuture(reason)
            }) { window }
            button.refresh()
            assertFalse(button.isEnabled)
            assertTrue(button.toolTipText.contains("対象端末"))

            target = ReplayDevice("A", "same-name", DeviceKind.PHYSICAL, false)
            button.refresh()
            assertFalse(button.isEnabled)
            assertTrue(button.toolTipText.contains("切断"))
            target = target.copy(connected = true)
            button.refresh()
            button.doClick(0)
            assertEquals(listOf("A"), selected)
            assertTrue(operations.isEmpty())
            assertEquals(reason, button.toolTipText)
            button.refresh() // A service refresh must not erase the visible failure.
            assertEquals(reason, button.toolTipText)

            reason = null
            duringSelection = { target = target!!.copy(serial = "B") }
            button.doClick(0)
            assertTrue(operations.isEmpty())
            assertTrue(button.toolTipText.contains("変わりました"))
            duringSelection = {}
            button.doClick(0)
            assertEquals("B", selected.last())
            assertEquals(listOf("show"), operations)

            target = null
            button.refresh()
            assertTrue(button.isEnabled)
            button.doClick(0)
            assertEquals(listOf("show", "hide"), operations)
            assertFalse(button.isEnabled)

            target = ReplayDevice("A", "target", DeviceKind.PHYSICAL, true)
            button.refresh()
            duringSelection = { throw NoSuchMethodError("fixture ABI mismatch") }
            button.doClick(0)
            assertTrue(button.toolTipText.contains("互換性"))
            assertEquals(listOf("show", "hide"), operations)
            duringSelection = { alive = false }
            button.doClick(0)
            assertFalse(button.isEnabled)
            assertEquals(listOf("show", "hide"), operations)
        }
    }

    @Test
    fun `one click waits for completion and cancellation or external visibility prevents a late open`() {
        SwingUtilities.invokeAndWait {
            var visible = false
            val operations = mutableListOf<String>()
            val window = Proxy.newProxyInstance(ToolWindow::class.java.classLoader, arrayOf(ToolWindow::class.java)) { _, method, _ ->
                when (method.name) {
                    "isVisible" -> visible
                    "isAvailable" -> true
                    "isDisposed" -> false
                    "show", "hide" -> { operations += method.name; visible = method.name == "show"; null }
                    else -> error("Unexpected ToolWindow call: ${method.name}")
                }
            } as ToolWindow
            var next = CompletableFuture<String?>()
            val button = LogcatToolWindowButton({ true }, { ReplayDevice("A", "target", DeviceKind.PHYSICAL, true) },
                { _, _, active -> assertTrue(active.asBoolean); next }) { window }
            button.refresh()
            button.doClick(0)
            assertTrue(operations.isEmpty())
            assertEquals("Logcatを開くのを中止", button.text)
            next.complete(null)
            assertEquals(listOf("show"), operations)
            button.doClick(0)
            next = CompletableFuture()
            button.doClick(0)
            button.doClick(0)
            assertTrue(next.isCancelled)
            next.complete(null)
            assertEquals(listOf("show", "hide"), operations)
            next = CompletableFuture()
            button.doClick(0)
            visible = true
            button.refresh()
            assertTrue(next.isCancelled)
            visible = false
            button.refresh()
            next = CompletableFuture()
            button.doClick(0)
            button.cancelOpen() // A queued IDE-show event still cancels after a fast external show/hide.
            assertTrue(next.isCancelled)
            next = CompletableFuture()
            button.doClick(0)
            button.dispose()
            assertTrue(next.isCancelled)
            button.doClick(0)
            assertFalse(button.isEnabled)
            assertEquals(listOf("show", "hide"), operations)
        }
    }
}
