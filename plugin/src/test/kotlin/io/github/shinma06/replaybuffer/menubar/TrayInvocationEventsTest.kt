package io.github.shinma06.replaybuffer.menubar

import java.awt.AWTEvent
import java.awt.Image
import java.awt.event.ActionEvent
import java.awt.event.InvocationEvent
import java.awt.event.MouseEvent
import javax.swing.JPanel
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertSame
import org.junit.jupiter.api.Test

class TrayInvocationEventsTest {
    // AwtTraySurface inherits this default policy for both template and non-template icons.
    private fun surface(owns: (Any) -> Boolean) = object : TraySurface {
        override fun unavailableReason(): String? = null
        override fun show(image: Image, tooltip: String, menu: MenuBarMenu, dispatch: MenuDispatch): String? = null
        override fun hide() {}
        override fun close() {}
        override fun ownsEventSource(source: Any): Boolean = owns(source)
    }

    @Test
    fun `owned invocations keep their identity class id and runnable`() {
        val icon = Any()
        val target = Any()
        var calls = 0
        val event = object : InvocationEvent(icon, Runnable { calls++ }) {}
        val identity = event
        val type = event.javaClass
        val id = event.id
        val whenPosted = event.`when`
        assertFalse(surface { it === icon }.interceptPostedEvent(event, target))
        assertSame(identity, event)
        assertSame(type, event.javaClass)
        assertSame(target, event.source)
        assertEquals(id, event.id)
        assertEquals(whenPosted, event.`when`)
        assertEquals(0, calls)
        assertFalse(event.isDispatched)
        event.dispatch()
        assertEquals(1, calls)
    }

    @Test
    fun `foreign invocations and non invocation tray events are untouched`() {
        val icon = Any()
        val foreign = Any()
        listOf(
            InvocationEvent(foreign, Runnable {}),
            InvocationEvent(foreign, Runnable {}).apply { source = null },
            ActionEvent(icon, ActionEvent.ACTION_PERFORMED, "open"),
            MouseEvent(JPanel(), MouseEvent.MOUSE_RELEASED, 1L, 0, 10, 10, 1, true, MouseEvent.BUTTON3).apply { source = icon },
            object : AWTEvent(icon, 42) {},
        ).forEach { event ->
            val before = event.source
            assertFalse(surface { it === icon }.interceptPostedEvent(event, Any()))
            assertSame(before, event.source)
        }
    }

    @Test
    fun `delayed events cannot use a hidden or replaced icons ownership`() {
        val old = Any()
        val current = Any()
        val target = Any()
        var owner: Any? = current
        val owns: (Any) -> Boolean = { it === owner }
        val stale = InvocationEvent(old, Runnable {})
        val surface = surface(owns)
        assertFalse(surface.interceptPostedEvent(stale, target))
        assertSame(old, stale.source)
        val active = InvocationEvent(current, Runnable {})
        assertFalse(surface.interceptPostedEvent(active, target))
        assertSame(target, active.source)
        owner = null
        val hidden = InvocationEvent(current, Runnable {})
        assertFalse(surface.interceptPostedEvent(hidden, target))
        assertSame(current, hidden.source)
    }
}
