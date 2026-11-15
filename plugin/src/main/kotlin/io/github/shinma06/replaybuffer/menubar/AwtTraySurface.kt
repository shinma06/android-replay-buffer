package io.github.shinma06.replaybuffer.menubar

import java.awt.CheckboxMenuItem
import java.awt.GraphicsEnvironment
import java.awt.Image
import java.awt.Menu
import java.awt.MenuItem
import java.awt.PopupMenu
import java.awt.SystemTray
import java.awt.TrayIcon

/**
 * The shared AWT destination: macOS menu bar, Windows notification area and Linux tray all come through the same
 * java.awt.SystemTray API. [templateImages] asks JBR to treat the icon as a macOS template image; it is set before the
 * first TrayIcon so JBR can pick it up, and the drawn glyphs stay readable on either theme when it is ignored.
 */
internal class AwtTraySurface(templateImages: Boolean = false) : TraySurface {
    private var tray: SystemTray? = null
    private var icon: TrayIcon? = null

    init {
        if (templateImages && System.getProperty("apple.awt.enableTemplateImages") == null)
            System.setProperty("apple.awt.enableTemplateImages", "true")
    }

    override fun unavailableReason(): String? = when {
        GraphicsEnvironment.isHeadless() -> "画面のない環境では、メニューバー（通知領域・トレイ）アイコンを表示できません。"
        !SystemTray.isSupported() -> "このOS・デスクトップのトレイに対応していないため、メニューバー（通知領域・トレイ）アイコンを表示できません。"
        else -> null
    }

    override fun show(image: Image, tooltip: String, menu: MenuBarMenu, dispatch: MenuDispatch): String? {
        unavailableReason()?.let { return it }
        return try {
            val systemTray = tray ?: SystemTray.getSystemTray().also { tray = it }
            val current = icon ?: TrayIcon(image, tooltip).also { created ->
                created.isImageAutoSize = true
                systemTray.add(created)
                icon = created
            }
            current.setImage(image)
            current.toolTip = tooltip
            current.popupMenu = menu.toPopupMenu(dispatch)
            null
        } catch (_: Exception) {
            icon = null
            "OSのトレイへメニューバーアイコンを登録できませんでした。"
        }
    }

    override fun hide() {
        icon?.let { tray?.remove(it) }
        icon = null
    }

    override fun close() = hide()
}

internal fun MenuBarMenu.toPopupMenu(dispatch: MenuDispatch): PopupMenu = PopupMenu().also { it.fill(entries, dispatch) }

private fun Menu.fill(entries: List<MenuEntry>, dispatch: MenuDispatch) {
    entries.forEach { entry ->
        when (entry) {
            is MenuEntry.Separator -> addSeparator()
            is MenuEntry.Submenu -> add(Menu(entry.title).also { it.fill(entry.entries, dispatch) })
            is MenuEntry.Item -> {
                val model = entry.item
                val item = if (model.command == MenuBarCommand.TOGGLE_CAPTURE) CheckboxMenuItem(model.label, model.checked)
                    else MenuItem(model.label)
                item.isEnabled = model.enabled
                if (item is CheckboxMenuItem) item.addItemListener { dispatch.dispatch(model) }
                else if (model.command != null) item.addActionListener { dispatch.dispatch(model) }
                add(item)
            }
        }
    }
}
