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
import java.util.concurrent.TimeUnit
import javax.swing.SwingUtilities

class LogcatToolWindowButtonTest {
    @Test
    fun `patch builds of a verified baseline and Android line can enter the target selection bridge`() {
        assertNull(logcatTargetBuildReason("AI-262.9437.185.2621.16467767"))
        assertNull(logcatTargetBuildReason("AI-262.9437.185.2621.16467768"))
        assertNull(logcatTargetBuildReason("AI-262.10968.63.2622.16730706"))
        assertNull(logcatTargetBuildReason("AI-262.10968.63.2622.16840690"), "Rabbit 2 Patch 1")
        assertNotNull(logcatTargetBuildReason("IC-262.9437.185.2621.16467767"))
        assertNotNull(logcatTargetBuildReason("IC-262.10968.63.2622.16730706"))
        assertNotNull(logcatTargetBuildReason("AI-262.10968.63"))
        assertNotNull(logcatTargetBuildReason("AI-262.10968.63.2622"))
        assertNotNull(logcatTargetBuildReason("AI-262.10968.63.2622."))
        assertNotNull(logcatTargetBuildReason("AI-262.10968.63.2622.SNAPSHOT"))
        assertNotNull(logcatTargetBuildReason("AI-262.10968.63.2622.16840690.1"))
        assertNotNull(logcatTargetBuildReason("AI-262.10968.64.2622.16840690"), "Another IntelliJ baseline")
        assertNotNull(logcatTargetBuildReason("AI-262.10968.63.2623.16840690"), "Another Android line")
        assertNotNull(logcatTargetBuildReason("AI-262.9437.185.2622.16730706"), "Baseline and line must come from one release")
        assertNotNull(logcatTargetBuildReason("262.10968.63.2622.16840690"))
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
            var resolvedAt = 0L
            val button = LogcatToolWindowButton({ alive }, { ReplayDevice("A", "target", DeviceKind.PHYSICAL, true) }, { _, serial, _, deadlineNs ->
                assertEquals("A", serial)
                assertTrue(deadlineNs - TimeUnit.SECONDS.toNanos(5) <= resolvedAt, "Budget starts before resolving or initializing the IDE")
                CompletableFuture.completedFuture(null)
            }, canSelectDevice = { true }) {
                check(alive) { "Disposed content must not resolve the project service" }
                resolvedAt = System.nanoTime()
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
            }, { _, serial, _, _ ->
                selected += serial
                duringSelection()
                CompletableFuture.completedFuture(reason)
            }, canSelectDevice = { true }) { window }
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
                { _, _, active, _ -> assertTrue(active.asBoolean); next }, canSelectDevice = { true }) { window }
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

    @Test
    fun `an empty Logcat is shown first so the IDE creates a tab and the selection survives the show`() {
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
            var empty = true
            var next = CompletableFuture<String?>()
            val button = LogcatToolWindowButton({ true }, { ReplayDevice("A", "target", DeviceKind.PHYSICAL, true) },
                { _, _, _, _ -> operations += "select"; next }, { empty }, { true }) { window }
            button.refresh()
            button.doClick(0)
            assertEquals(listOf("show", "select"), operations)
            button.cancelOpen()
            button.refresh()
            assertFalse(next.isCancelled)
            assertEquals("Logcatを開くのを中止", button.text)
            next.complete(null)
            assertEquals(listOf("show", "select"), operations)
            assertEquals("Logcatを閉じる", button.text)
            button.doClick(0)
            assertEquals(listOf("show", "select", "hide"), operations)
            next = CompletableFuture()
            button.doClick(0)
            visible = false
            button.refresh()
            assertTrue(next.isCancelled)
            empty = false
            operations.clear()
            next = CompletableFuture()
            button.doClick(0)
            assertEquals(listOf("select"), operations)
            next.complete(null)
            assertEquals(listOf("select", "show"), operations)
            button.doClick(0)
            empty = true
            operations.clear()
            next = CompletableFuture()
            button.doClick(0)
            next.complete("期限切れ")
            button.refresh()
            assertEquals(listOf("show", "select"), operations)
            assertEquals("Logcatを閉じる", button.text)
            assertTrue(button.toolTipText.startsWith("期限切れ"))
            button.doClick(0)
            assertEquals("期限切れ", button.toolTipText)
        }
    }

    private fun fakeWindow(operations: MutableList<String>): ToolWindow {
        var visible = false
        return Proxy.newProxyInstance(ToolWindow::class.java.classLoader, arrayOf(ToolWindow::class.java)) { _, method, _ ->
            when (method.name) {
                "isVisible" -> visible
                "isAvailable" -> true
                "isDisposed" -> false
                "show", "hide" -> { operations += method.name; visible = method.name == "show"; null }
                else -> error("Unexpected ToolWindow call: ${method.name}")
            }
        } as ToolWindow
    }

    @Test
    fun `an unverified build only opens and closes Logcat without touching the device selection`() {
        SwingUtilities.invokeAndWait {
            val operations = mutableListOf<String>()
            val window = fakeWindow(operations)
            var target: ReplayDevice? = null
            val button = LogcatToolWindowButton({ true }, { target },
                { _, _, _, _ -> error("The selection bridge must not load on an unverified build") },
                { error("The tab probe must not load on an unverified build") }, { false }) { window }
            button.refresh()
            assertTrue(button.isEnabled, "Opening needs no Replay Buffer target when no device is selected")
            assertEquals("Logcatを開く", button.text)
            assertTrue(button.toolTipText.contains("対象端末にLogcatを合わせられない"))
            assertTrue(button.toolTipText.contains("IDEで選択中の端末"))
            assertEquals(button.toolTipText, button.accessibleContext.accessibleDescription)
            button.doClick(0)
            assertEquals(listOf("show"), operations)
            assertEquals("Logcatを閉じる", button.text)
            assertTrue(button.toolTipText.startsWith("IDE標準Logcatパネルを非表示にします。"))
            assertTrue(button.toolTipText.contains("IDEで選択中の端末"))
            assertEquals(button.toolTipText, button.accessibleContext.accessibleDescription)
            button.doClick(0)
            target = ReplayDevice("A", "target", DeviceKind.PHYSICAL, true)
            button.refresh()
            button.doClick(0)
            target = ReplayDevice("A", "target", DeviceKind.PHYSICAL, false)
            button.refresh()
            assertTrue(button.isEnabled, "A disconnected target does not block closing or reopening")
            button.doClick(0)
            button.doClick(0)
            assertEquals(listOf("show", "hide", "show", "hide", "show"), operations)
        }
    }

    @Test
    fun `a selection bridge that fails to link falls back to opening Logcat for later clicks too`() {
        SwingUtilities.invokeAndWait {
            val operations = mutableListOf<String>()
            val window = fakeWindow(operations)
            val button = LogcatToolWindowButton({ true }, { ReplayDevice("A", "target", DeviceKind.PHYSICAL, true) },
                { _, _, _, _ -> operations += "select"; throw NoSuchMethodError("getLogcatPresenters") }, { false }, { true }) { window }
            button.refresh()
            assertEquals("Replay Bufferの対象端末（A）に合わせてIDE標準Logcatを表示します。", button.toolTipText)
            button.doClick(0)
            assertEquals(listOf("select", "show"), operations)
            assertEquals("Logcatを閉じる", button.text)
            assertTrue(button.toolTipText.contains("IDEで選択中の端末"))
            button.doClick(0)
            button.doClick(0)
            assertEquals(listOf("select", "show", "hide", "show"), operations)
            assertTrue(button.toolTipText.contains("IDEで選択中の端末"))
        }
    }

    @Test
    fun `an asynchronous link failure opens Logcat while other failures keep the retry reason`() {
        SwingUtilities.invokeAndWait {
            val operations = mutableListOf<String>()
            val window = fakeWindow(operations)
            var next = CompletableFuture<String?>()
            val button = LogcatToolWindowButton({ true }, { ReplayDevice("A", "target", DeviceKind.PHYSICAL, true) },
                { _, _, _, _ -> operations += "select"; next }, { false }, { true }) { window }
            button.refresh()
            button.doClick(0)
            next.completeExceptionally(IllegalStateException("model"))
            assertEquals(listOf("select"), operations)
            assertTrue(button.toolTipText.contains("再試行"))
            next = CompletableFuture()
            button.doClick(0)
            next.completeExceptionally(java.util.concurrent.CompletionException(NoClassDefFoundError("DeviceItem")))
            assertEquals(listOf("select", "select", "show"), operations)
            assertTrue(button.toolTipText.contains("IDEで選択中の端末"))
            button.doClick(0)
            button.doClick(0)
            assertEquals(listOf("select", "select", "show", "hide", "show"), operations)
        }
    }

    @Test
    fun `a failing build probe is treated as an unverified build`() {
        SwingUtilities.invokeAndWait {
            val operations = mutableListOf<String>()
            val window = fakeWindow(operations)
            val button = LogcatToolWindowButton({ true }, { null },
                { _, _, _, _ -> error("unreachable") }, { false }, { throw IllegalStateException("no application") }) { window }
            button.refresh()
            assertTrue(button.isEnabled)
            button.doClick(0)
            assertEquals(listOf("show"), operations)
        }
    }
}
