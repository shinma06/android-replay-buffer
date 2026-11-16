package io.github.shinma06.replaybuffer.menubar

import java.awt.AWTEvent
import java.awt.event.ActionEvent
import java.awt.event.InvocationEvent
import java.awt.event.MouseEvent
import java.util.concurrent.atomic.AtomicReference
import javax.swing.JPanel
import javax.swing.SwingUtilities
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertSame
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/** Contract tests; actual native delivery still requires the exact CI ZIP in Studio. */
class WindowsTrayPostedEventsTest {
    private fun mouse(source: Any, id: Int, popup: Boolean, button: Int = MouseEvent.BUTTON3) =
        MouseEvent(JPanel(), id, 1L, 0, 10, 20, 10, 20, 1, popup, button).apply { this.source = source }

    @Test
    fun `consume the original owned press and release before queueing and open once`() {
        val icon = Any()
        for ((pressPopup, releasePopup) in listOf(true to false, false to true, true to true)) {
            val opens = mutableListOf<MouseEvent>()
            val gesture = WindowsPopupGesture(opens::add)
            val pressed = mouse(icon, MouseEvent.MOUSE_PRESSED, pressPopup)
            val released = mouse(icon, MouseEvent.MOUSE_RELEASED, releasePopup)
            var queuedOriginals = 0
            for (event in listOf(pressed, released)) {
                // The fixed SDK only queues an original if its post listener returns false.
                val consumed = gesture.dispatchPosted(event) { it === icon }
                assertTrue(consumed)
                if (!consumed) queuedOriginals++
                assertSame(icon, event.source)
                assertFalse(event.isConsumed, "Only the hook return value consumes; do not mutate the MouseEvent")
            }
            assertEquals(0, queuedOriginals)
            assertEquals(1, opens.size)
            assertSame(if (pressPopup) pressed else released, opens.single())
        }
    }

    @Test
    fun `other sources action invocation left click and other mouse ids stay untouched`() {
        val icon = Any()
        val equalButForeign = object { override fun equals(other: Any?) = true; override fun hashCode() = 0 }
        val gesture = WindowsPopupGesture { error("Unrelated event opened a popup") }
        val events = listOf(
            mouse(equalButForeign, MouseEvent.MOUSE_RELEASED, true),
            mouse(icon, MouseEvent.MOUSE_RELEASED, true, MouseEvent.BUTTON1),
            mouse(icon, MouseEvent.MOUSE_CLICKED, true),
            mouse(icon, MouseEvent.MOUSE_MOVED, false, MouseEvent.NOBUTTON),
            mouse(icon, MouseEvent.MOUSE_RELEASED, true).apply { source = null },
            ActionEvent(icon, ActionEvent.ACTION_PERFORMED, "open"),
            InvocationEvent(icon, Runnable { error("Must not dispatch invocations here") }),
            object : AWTEvent(icon, 42) {},
        )
        for (event in events) {
            val source = event.source
            assertFalse(gesture.dispatchPosted(event) { it === icon })
            assertSame(source, event.source)
        }
    }

    @Test
    fun `ownership is current identity so stale hidden and disposed sources cannot open`() {
        val old = Any()
        val replacement = Any()
        val current = AtomicReference<Any?>(old)
        val opens = mutableListOf<MouseEvent>()
        val gesture = WindowsPopupGesture(opens::add)
        fun release(source: Any) = gesture.dispatchPosted(mouse(source, MouseEvent.MOUSE_RELEASED, true)) { it === current.get() }
        assertTrue(release(old))
        current.set(replacement)
        assertFalse(release(old))
        assertTrue(release(replacement))
        current.set(null)
        assertFalse(release(old))
        assertFalse(release(replacement))
        assertEquals(2, opens.size)
    }

    @Test
    fun `the post hook runs synchronously on its posting thread without reposting the mouse event`() {
        val icon = Any()
        val event = mouse(icon, MouseEvent.MOUSE_RELEASED, true)
        val callbackThread = AtomicReference<Thread>()
        val delivered = AtomicReference<MouseEvent>()
        val failure = AtomicReference<Throwable>()
        val gesture = WindowsPopupGesture { delivered.set(it); callbackThread.set(Thread.currentThread()) }
        val posting = Thread {
            try {
                assertFalse(SwingUtilities.isEventDispatchThread())
                assertTrue(gesture.dispatchPosted(event) { it === icon })
                assertSame(event, delivered.get(), "Delivery completes before the hook returns true")
            } catch (error: Throwable) { failure.set(error) }
        }
        posting.start()
        posting.join(5000)
        assertFalse(posting.isAlive)
        failure.get()?.let { throw it }
        assertSame(posting, callbackThread.get())
        assertSame(event, delivered.get())
        assertSame(icon, event.source)
    }

    @Test
    fun `non popup right click is consumed without replacing the ordinary popup gesture`() {
        val icon = Any()
        val opens = mutableListOf<MouseEvent>()
        val gesture = WindowsPopupGesture(opens::add)
        assertTrue(gesture.dispatchPosted(mouse(icon, MouseEvent.MOUSE_PRESSED, false)) { it === icon })
        assertTrue(gesture.dispatchPosted(mouse(icon, MouseEvent.MOUSE_RELEASED, false)) { it === icon })
        assertTrue(opens.isEmpty())
    }

    @Test
    fun `a surface with no current registration leaves events alone before and after disposal`() {
        val surface = WindowsTraySurface()
        val source = Any()
        val event = mouse(source, MouseEvent.MOUSE_RELEASED, true)
        repeat(3) {
            assertFalse(surface.interceptPostedEvent(event, Any()))
            assertSame(source, event.source)
            if (it == 0) surface.hide() else surface.close()
        }
    }
}
