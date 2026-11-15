package io.github.shinma06.replaybuffer.menubar

import java.awt.AWTEvent
import java.awt.event.ActionEvent
import java.awt.event.InvocationEvent
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertSame
import org.junit.jupiter.api.Test

class TrayInvocationEventsTest {
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
        retargetTrayInvocation(event, { it === icon }, target)
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
            object : AWTEvent(icon, 42) {},
        ).forEach { event ->
            val before = event.source
            retargetTrayInvocation(event, { it === icon }, Any())
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
        retargetTrayInvocation(stale, owns, target)
        assertSame(old, stale.source)
        val active = InvocationEvent(current, Runnable {})
        retargetTrayInvocation(active, owns, target)
        assertSame(target, active.source)
        owner = null
        val hidden = InvocationEvent(current, Runnable {})
        retargetTrayInvocation(hidden, owns, target)
        assertSame(current, hidden.source)
    }
}
