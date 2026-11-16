package io.github.shinma06.replaybuffer.menubar

import java.awt.event.MouseEvent
import javax.swing.JScrollPane
import javax.swing.SwingUtilities
import javax.swing.UIManager
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class WindowsMenuListTest {
    private val permission = "取得の初期化に失敗しました。IDE設定領域（.）は本人所有で他の利用者の書込み不可である必要があります。" +
        "現在の権限を確認し、修正後に取得ONまたはreplay startで再試行してください。"
    private val notice = MenuEntry.Item(MenuItemModel(permission, false, null))
    private val save = MenuEntry.Item(MenuItemModel("直前30秒を保存", true, MenuBarCommand.SAVE))
    private val checked = MenuEntry.Item(MenuItemModel("取得", true, MenuBarCommand.TOGGLE_CAPTURE, true))
    private val unchecked = checked.copy(item = checked.item.copy(checked = false))
    private val submenu = MenuEntry.Submenu("プロジェクト", listOf(save))
    private val rows = listOf(notice, save, checked, unchecked, submenu, MenuEntry.Separator)

    @Test
    fun `long rows follow a narrow viewport when it is resized`() {
        SwingUtilities.invokeAndWait {
            val list = menuList()
            val scroll = scrollPane(list)
            for (width in listOf(320, 200, 360)) {
                layout(scroll, width)
                val extent = scroll.viewport.extentSize.width
                assertTrue(list.preferredSize.width > extent, "Precondition: a caption is wider than the viewport")
                assertEquals(extent, list.width, "The list must use the available viewport width")
                for (index in rows.indices) {
                    val cell = list.getCellBounds(index, index)
                    assertTrue(cell.x >= 0 && cell.x + cell.width <= extent, "Every renderer must receive a bounded cell")
                }
            }
        }
    }

    @Test
    fun `bounded cells retain full captions accessibility and tooltips across renderer reuse`() {
        SwingUtilities.invokeAndWait {
            val list = menuList()
            val scroll = scrollPane(list)
            layout(scroll, 200)
            val expected = listOf(
                permission to permission,
                "直前30秒を保存" to "直前30秒を保存",
                "取得" to "チェックあり",
                "取得" to "チェックなし",
                "プロジェクト" to "プロジェクト",
                "" to "",
            )
            for ((index, presentation) in expected.withIndex()) {
                val (caption, description) = presentation
                val renderer = list.cellRenderer.getListCellRendererComponent(list, rows[index], index, true, false) as MenuCell
                assertEquals(caption, renderer.text)
                assertEquals(caption, renderer.getAccessibleContext().accessibleName)
                assertEquals(description, renderer.getAccessibleContext().accessibleDescription)
                assertEquals(caption, renderer.toolTipText)
                val cell = list.getCellBounds(index, index)
                val hover = MouseEvent(list, MouseEvent.MOUSE_MOVED, 0, 0, cell.x + 1, cell.y + cell.height / 2, 0, false)
                assertEquals(caption, list.getToolTipText(hover))
            }
        }
    }

    private fun menuList() = WindowsMenuList(rows).apply {
        font = UIManager.getFont("MenuItem.font") ?: font
        visibleRowCount = rows.size
        cellRenderer = MenuCell()
    }

    private fun scrollPane(list: WindowsMenuList) = JScrollPane(
        list, JScrollPane.VERTICAL_SCROLLBAR_AS_NEEDED, JScrollPane.HORIZONTAL_SCROLLBAR_NEVER,
    )

    private fun layout(scroll: JScrollPane, width: Int) {
        scroll.setSize(width, 90)
        scroll.doLayout()
        scroll.viewport.doLayout()
    }
}
