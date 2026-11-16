package io.github.shinma06.replaybuffer.menubar

import io.github.shinma06.replaybuffer.core.SaveOrigin
import io.github.shinma06.replaybuffer.core.SaveRequest
import java.awt.event.MouseEvent
import java.nio.file.Path
import javax.swing.JPanel
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertSame
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class WindowsTraySurfaceTest {
    private fun item(command: MenuBarCommand, checked: Boolean = false, project: String = "project-a") =
        MenuItemModel(command.name, true, command, checked, project, Path.of("saved", project))

    private fun menu(vararg items: MenuItemModel) = MenuBarMenu(MenuBarIcon.OFF, "menu", items.map(MenuEntry::Item))
    private fun WindowsMenuState.openNow() = checkNotNull(open(revision))
    private fun mouse(id: Int, popup: Boolean, button: Int = MouseEvent.BUTTON3, clicks: Int = 1) =
        MouseEvent(JPanel(), id, 1L, 0, 10, 10, clicks, popup, button)

    @Test
    fun `popup trigger on press release or both opens once per ordinary right click`() {
        for ((press, release) in listOf(true to false, false to true, true to true)) {
            var opens = 0
            val gesture = WindowsPopupGesture { opens++ }
            repeat(3) {
                gesture.mousePressed(mouse(MouseEvent.MOUSE_PRESSED, press))
                gesture.mouseReleased(mouse(MouseEvent.MOUSE_RELEASED, release))
            }
            assertEquals(3, opens, "press=$press release=$release")
        }
    }

    @Test
    fun `left click double click and non popup mouse events do not replace the popup gesture`() {
        var opens = 0
        val gesture = WindowsPopupGesture { opens++ }
        for (clicks in 1..2) {
            gesture.mousePressed(mouse(MouseEvent.MOUSE_PRESSED, false, MouseEvent.BUTTON1, clicks))
            gesture.mouseReleased(mouse(MouseEvent.MOUSE_RELEASED, false, MouseEvent.BUTTON1, clicks))
            gesture.mouseClicked(mouse(MouseEvent.MOUSE_CLICKED, false, MouseEvent.BUTTON1, clicks))
        }
        gesture.mousePressed(mouse(MouseEvent.MOUSE_PRESSED, false))
        gesture.mouseReleased(mouse(MouseEvent.MOUSE_RELEASED, false))
        assertEquals(0, opens)
    }

    @Test
    fun `one popup gesture then one save activation reaches the existing MENU_BAR operation`() {
        val save = item(MenuBarCommand.SAVE)
        val state = WindowsMenuState()
        val order = mutableListOf<String>()
        var request: SaveRequest? = null
        val operations = object : MenuBarOperations {
            override fun save(value: SaveRequest) { request = value; order += "save" }
            override fun setEnabled(value: Boolean) = error("wrong operation")
            override fun screenshot() = error("wrong operation")
            override fun openDirectory(directory: Path) = error("wrong operation")
            override fun showToolWindow() = error("wrong operation")
            override fun openSettings() = error("wrong operation")
        }
        state.update(menu(save), MenuDispatch { it.run(operations) })
        var opened: WindowsMenuSession? = null
        val gesture = WindowsPopupGesture { opened = state.open(state.revision) }
        gesture.mousePressed(mouse(MouseEvent.MOUSE_PRESSED, false))
        gesture.mouseReleased(mouse(MouseEvent.MOUSE_RELEASED, true))
        state.activate(checkNotNull(opened), save) { order += "closed" }
        assertEquals(listOf("closed", "save"), order)
        assertEquals(SaveOrigin.MENU_BAR, checkNotNull(request).origin)
        // This is the event contract; real tray delivery and MB-04 still require the CI ZIP in Studio.
    }

    @Test
    fun `toggle sends the new checked value and preserves project directory and source snapshot`() {
        for (before in listOf(false, true)) {
            val a = item(MenuBarCommand.TOGGLE_CAPTURE, before)
            val b = item(MenuBarCommand.TOGGLE_CAPTURE, !before, "project-b")
            val original = menu(a, b)
            val events = mutableListOf<MenuItemModel>()
            val state = WindowsMenuState()
            state.update(original, MenuDispatch(events::add))
            val session = state.openNow()
            state.activate(session, b) {}
            state.activate(session, b) { error("must not close twice") }
            assertEquals(listOf(b.copy(checked = before)), events)
            assertEquals(original, session.menu)
            assertEquals(!before, b.checked)
            assertEquals(before, a.checked)
        }
    }

    @Test
    fun `disabled and information rows cannot close or dispatch and normal items retain identity`() {
        val enabled = item(MenuBarCommand.OPEN_DIRECTORY)
        val disabled = item(MenuBarCommand.SCREENSHOT).copy(enabled = false)
        val information = MenuItemModel("state")
        val events = mutableListOf<MenuItemModel>()
        val state = WindowsMenuState()
        state.update(menu(enabled, disabled, information), MenuDispatch(events::add))
        val session = state.openNow()
        for (row in listOf(disabled, information)) state.activate(session, row) { error("disabled close") }
        assertTrue(events.isEmpty())
        state.activate(session, enabled) {}
        assertSame(enabled, events.single())
    }

    @Test
    fun `unchanged refresh preserves an open menu but uses the latest dispatcher`() {
        val row = item(MenuBarCommand.SCREENSHOT)
        val model = menu(row)
        val state = WindowsMenuState()
        state.update(model, MenuDispatch { error("stale dispatcher") })
        val session = state.openNow()
        val events = mutableListOf<MenuItemModel>()
        repeat(20) { assertFalse(state.update(model.copy(), MenuDispatch(events::add))) }
        state.activate(session, row) {}
        assertEquals(listOf(row), events)
    }

    @Test
    fun `changed models invalidate visible rows and reopening uses the latest complete tree`() {
        val a = item(MenuBarCommand.TOGGLE_CAPTURE, false)
        val b = a.copy(checked = true)
        val events = mutableListOf<MenuItemModel>()
        val state = WindowsMenuState()
        state.update(menu(a), MenuDispatch(events::add))
        val old = state.openNow()
        assertTrue(state.update(menu(b), MenuDispatch(events::add)))
        state.activate(old, a) { error("stale row") }
        val next = state.openNow()
        assertEquals(menu(b), next.menu)
        state.activate(next, b) {}
        assertEquals(listOf(a), events)
    }

    @Test
    fun `dismiss hide replacement and close invalidate queued opens and stale actions`() {
        val row = item(MenuBarCommand.SAVE)
        val state = WindowsMenuState()
        val events = mutableListOf<MenuItemModel>()
        state.update(menu(row), MenuDispatch(events::add))
        val queued = state.revision
        val first = state.openNow()
        assertNull(state.open(state.revision), "A duplicate event must not create a second root")
        state.dismiss()
        assertNull(state.open(queued))
        state.activate(first, row) { error("dismissed") }
        val second = state.openNow()
        state.hide()
        assertNull(state.open(state.revision))
        state.update(menu(row.copy(projectId = "replacement")), MenuDispatch(events::add))
        state.activate(second, row) { error("hidden") }
        assertNull(state.open(queued), "An event from an earlier icon must not open a replacement")
        val third = state.openNow()
        state.close()
        assertFalse(state.update(menu(row), MenuDispatch(events::add)))
        assertNull(state.open(state.revision))
        state.activate(third, row) { error("closed") }
        assertTrue(events.isEmpty())
    }

    @Test
    fun `disposal while closing the menu prevents dispatch into a disposed surface`() {
        val row = item(MenuBarCommand.SAVE)
        val state = WindowsMenuState()
        state.update(menu(row), MenuDispatch { error("disposed dispatch") })
        state.activate(state.openNow(), row, state::close)
        assertTrue(state.closed)
    }

    @Test
    fun `disposed child callbacks cannot dispatch or reopen windows while the parent stays active`() {
        val row = item(MenuBarCommand.SAVE)
        val state = WindowsMenuState()
        val events = mutableListOf<MenuItemModel>()
        state.update(menu(row), MenuDispatch(events::add))
        val session = state.openNow()
        val root = WindowsMenuInput(state, session)
        val child = WindowsMenuInput(state, session)
        var windows = 0
        child.close() // LEFT closes only this child; the root is still usable.
        child.run { state.activate(session, row) {} }
        child.run { windows++ }
        assertTrue(events.isEmpty())
        root.run { windows++ }
        assertEquals(1, windows)
        state.dismiss()
        root.close()
        root.run { windows++ }
        val replacement = WindowsMenuInput(state, state.openNow())
        root.run { windows++ }
        replacement.run { windows++ }
        assertEquals(2, windows)
        state.hide()
        replacement.run { windows++ }
        assertEquals(2, windows)
    }
}
