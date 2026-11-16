package io.github.shinma06.replaybuffer.menubar

import javax.swing.JList
import javax.swing.SwingUtilities
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertSame
import org.junit.jupiter.api.Test

class WindowsMenuCellTest {
    private val ordinary = MenuEntry.Item(MenuItemModel("保存 <画面>", true, MenuBarCommand.SAVE))
    private val checked = MenuEntry.Item(MenuItemModel("取得", true, MenuBarCommand.TOGGLE_CAPTURE, true))
    private val unchecked = checked.copy(item = checked.item.copy(checked = false))
    private val disabled = MenuEntry.Item(MenuItemModel("保存不可", false, MenuBarCommand.SAVE))
    private val submenu = MenuEntry.Submenu("プロジェクト", listOf(ordinary))
    private val rows = listOf(
        Triple(ordinary, "保存 <画面>", null),
        Triple(checked, "取得", "チェックあり"),
        Triple(unchecked, "取得", "チェックなし"),
        Triple(disabled, "保存不可", null),
        Triple(submenu, "プロジェクト", null),
        Triple(MenuEntry.Separator, "", null),
    )

    @Test
    fun `first render initializes accessibility without a prior getter call for every row kind`() {
        SwingUtilities.invokeAndWait {
            for ((row, caption, description) in rows) {
                val renderer = MenuCell()
                val list = JList(arrayOf(row))
                // Do not prime the renderer's lazy AccessibleContext before the real first render.
                assertSame(renderer, renderer.getListCellRendererComponent(list, row, 0, true, false))
                assertPresentation(renderer, caption, description)
            }
        }
    }

    @Test
    fun `reused renderer updates toggle state and clears descriptions on other rows`() {
        SwingUtilities.invokeAndWait {
            val renderer = MenuCell()
            val list = JList(rows.map { it.first }.toTypedArray())
            for ((row, caption, description) in listOf(rows[1], rows[2], rows[0], rows[1], rows[3], rows[1], rows[4], rows[1], rows[5])) {
                renderer.getListCellRendererComponent(list, row, 0, true, false)
                assertPresentation(renderer, caption, description)
            }
        }
    }

    private fun assertPresentation(renderer: MenuCell, caption: String, description: String?) {
        assertEquals(caption, renderer.text)
        assertEquals(caption, renderer.toolTipText)
        assertEquals(caption, renderer.getAccessibleContext().accessibleName)
        // With no explicit description, Swing exposes the tooltip; a reused toggle must not leak its state.
        assertEquals(description ?: caption, renderer.getAccessibleContext().accessibleDescription)
        assertEquals(true, renderer.getClientProperty("html.disable"))
    }
}
