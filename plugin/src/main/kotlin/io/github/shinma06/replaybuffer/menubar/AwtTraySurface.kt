package io.github.shinma06.replaybuffer.menubar

import java.awt.AWTEvent
import java.awt.CheckboxMenuItem
import java.awt.GraphicsEnvironment
import java.awt.Image
import java.awt.Menu
import java.awt.MenuItem
import java.awt.PopupMenu
import java.awt.SystemTray
import java.awt.TrayIcon
import java.awt.event.ItemEvent
import java.awt.event.InvocationEvent

/**
 * The shared AWT destination: macOS menu bar, Windows notification area and Linux tray all come through the same
 * java.awt.SystemTray API. Template images are used only when enabled before this surface is created; otherwise
 * the halo-backed image stays readable without relying on a late change to a JBR startup property.
 */
internal class AwtTraySurface(templateImages: Boolean = false) : TraySurface {
    private var tray: SystemTray? = null
    @Volatile private var icon: TrayIcon? = null
    private val useTemplateImages = templateImages && java.lang.Boolean.getBoolean("apple.awt.enableTemplateImages")

    override fun unavailableReason(): String? = when {
        GraphicsEnvironment.isHeadless() -> "画面のない環境では、メニューバー（通知領域・トレイ）アイコンを表示できません。"
        !SystemTray.isSupported() -> "このOS・デスクトップのトレイに対応していないため、メニューバー（通知領域・トレイ）アイコンを表示できません。"
        else -> null
    }

    override fun show(image: Image, tooltip: String, menu: MenuBarMenu, dispatch: MenuDispatch): String? {
        unavailableReason()?.let { return it }
        return try {
            val rendered = if (useTemplateImages) menuBarTemplateIconImage(menu.icon) else image
            val systemTray = tray ?: SystemTray.getSystemTray().also { tray = it }
            val current = icon ?: TrayIcon(rendered, tooltip).also { created ->
                created.isImageAutoSize = true
                systemTray.add(created)
                icon = created
            }
            current.setImage(rendered)
            current.toolTip = tooltip
            current.popupMenu = menu.toPopupMenu(dispatch)
            null
        } catch (_: Exception) {
            runCatching { hide() }
            "OSのトレイへメニューバーアイコンを登録できませんでした。"
        }
    }

    override fun hide() {
        val previous = icon
        icon = null
        previous?.let { tray?.remove(it) }
    }

    override fun ownsEventSource(source: Any): Boolean = source === icon

    override fun close() = hide()
}

internal fun retargetTrayInvocation(event: AWTEvent, ownsSource: (Any) -> Boolean, target: Any) {
    // IdeEventQueue drops TrayIcon-sourced invocations; retain the original event and its priority.
    if (event is InvocationEvent && event.source?.let(ownsSource) == true) event.source = target
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
                if (item is CheckboxMenuItem) item.addItemListener { dispatch.dispatch(model.copy(checked = it.stateChange == ItemEvent.SELECTED)) }
                else if (model.command != null) item.addActionListener { dispatch.dispatch(model) }
                add(item)
            }
        }
    }
}
