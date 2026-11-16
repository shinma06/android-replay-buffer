package io.github.shinma06.replaybuffer.menubar

import java.awt.AWTEvent
import java.awt.Component
import java.awt.Dialog
import java.awt.Graphics
import java.awt.Graphics2D
import java.awt.GraphicsConfiguration
import java.awt.GraphicsEnvironment
import java.awt.Image
import java.awt.Point
import java.awt.Rectangle
import java.awt.RenderingHints
import java.awt.SystemTray
import java.awt.Toolkit
import java.awt.TrayIcon
import java.awt.Window
import java.awt.event.ActionEvent
import java.awt.event.MouseAdapter
import java.awt.event.MouseEvent
import java.awt.event.MouseMotionAdapter
import java.awt.event.WindowAdapter
import java.awt.event.WindowEvent
import javax.swing.AbstractAction
import javax.swing.BorderFactory
import javax.swing.DefaultListCellRenderer
import javax.swing.JDialog
import javax.swing.JList
import javax.swing.JScrollPane
import javax.swing.ListSelectionModel
import javax.swing.SwingUtilities
import javax.swing.UIManager
import javax.swing.WindowConstants
import javax.swing.border.EmptyBorder

/** Only the actual menus are windows. No IDE frame or auxiliary owner participates. */
internal class WindowsTraySurface : TraySurface {
    private var tray: SystemTray? = null
    @Volatile private var icon: TrayIcon? = null
    @Volatile private var popupGesture: WindowsPopupGesture? = null
    private val state = WindowsMenuState()
    private var root: MenuWindow? = null
    private var menuFailure: String? = null

    override fun unavailableReason(): String? = when {
        state.closed -> "メニューバーの表示は終了しています。"
        GraphicsEnvironment.isHeadless() -> "画面のない環境では、メニューバー（通知領域・トレイ）アイコンを表示できません。"
        !SystemTray.isSupported() -> "このOS・デスクトップのトレイに対応していないため、メニューバー（通知領域・トレイ）アイコンを表示できません。"
        else -> menuFailure
    }

    override fun show(image: Image, tooltip: String, menu: MenuBarMenu, dispatch: MenuDispatch): String? {
        unavailableReason()?.let { return it }
        return try {
            val systemTray = tray ?: SystemTray.getSystemTray().also { tray = it }
            val current = icon ?: TrayIcon(image, tooltip).also { created ->
                created.isImageAutoSize = true
                val gesture = WindowsPopupGesture { event ->
                    val revision = state.revision
                    val point = event.locationOnScreen
                    SwingUtilities.invokeLater { if (icon === created) open(revision, point) }
                }
                created.addMouseListener(gesture)
                systemTray.add(created)
                popupGesture = gesture
                icon = created
            }
            current.image = image
            current.toolTip = tooltip
            if (state.update(menu, dispatch)) disposeMenus()
            null
        } catch (_: Exception) {
            runCatching { hide() }
            "OSのトレイへメニューバーアイコンを登録できませんでした。"
        }
    }

    override fun ownsEventSource(source: Any): Boolean = source === icon

    override fun interceptPostedEvent(event: AWTEvent, target: Any): Boolean {
        val current = icon
        val gesture = popupGesture
        if (current != null && gesture?.dispatchPosted(event) { it === current && ownsEventSource(it) } == true) return true
        return super.interceptPostedEvent(event, target)
    }

    override fun hide() {
        state.hide()
        menuFailure = null
        val previous = icon
        icon = null
        popupGesture = null
        try { disposeMenus() } finally { previous?.let { tray?.remove(it) } }
    }

    override fun close() { state.close(); hide() }

    private fun open(revision: Long, point: Point) {
        val session = state.open(revision) ?: return
        guarded {
            val devices = GraphicsEnvironment.getLocalGraphicsEnvironment().screenDevices
            val gc = devices.map { it.defaultConfiguration }.firstOrNull { it.bounds.contains(point) }
                ?: devices.first().defaultConfiguration
            val window = MenuWindow(null, session, session.menu.entries, gc)
            root = window
            window.fit()
            window.showAt(point.x - window.width + 16, point.y - window.height - 8)
        }
    }

    private fun guarded(action: () -> Unit) {
        try { action() } catch (_: Exception) {
            runCatching { hide() }
            menuFailure = "通知領域のメニューを表示できませんでした。設定から表示を切り替えて再試行してください。"
        }
    }

    private fun dismiss() { state.dismiss(); disposeMenus() }

    private fun disposeMenus() {
        val previous = root
        root = null
        previous?.disposeTree()
    }

    private fun ownsMenu(window: Window?): Boolean {
        var current = window
        while (current != null) {
            if (current === root) return true
            current = current.owner
        }
        return false
    }

    private inner class MenuWindow(
        val parentMenu: MenuWindow?,
        val session: WindowsMenuSession,
        entries: List<MenuEntry>,
        gc: GraphicsConfiguration,
    ) : JDialog(parentMenu as Window?, "Android Replay Buffer", Dialog.ModalityType.MODELESS, gc) {
        val list = JList(entries.toTypedArray())
        private var child: MenuWindow? = null
        private val input = WindowsMenuInput(state, session)

        init {
            isUndecorated = true
            type = Window.Type.POPUP
            defaultCloseOperation = WindowConstants.DO_NOTHING_ON_CLOSE
            focusableWindowState = true
            isAutoRequestFocus = true
            list.selectionMode = ListSelectionModel.SINGLE_SELECTION
            list.font = UIManager.getFont("MenuItem.font") ?: list.font
            list.visibleRowCount = entries.size.coerceAtLeast(1)
            list.cellRenderer = MenuCell()
            list.addListSelectionListener { if (!it.valueIsAdjusting) closeChild() }
            contentPane = JScrollPane(list, JScrollPane.VERTICAL_SCROLLBAR_AS_NEEDED, JScrollPane.HORIZONTAL_SCROLLBAR_NEVER).apply {
                border = BorderFactory.createLineBorder(UIManager.getColor("Separator.foreground") ?: list.foreground)
            }
            bind("UP") { move(-1) }
            bind("DOWN") { move(1) }
            bind("RIGHT") { guarded { openChild() } }
            bind("LEFT") { parentMenu?.let { it.closeChild(); it.list.requestFocusInWindow() } }
            bind("ENTER") { activate() }
            bind("SPACE") { activate() }
            bind("ESCAPE") { dismiss() }
            list.addMouseMotionListener(object : MouseMotionAdapter() {
                override fun mouseMoved(e: MouseEvent) {
                    input.run {
                        rowAt(e.point)?.takeIf { list.model.getElementAt(it).selectable() }?.let { list.selectedIndex = it }
                    }
                }
            })
            list.addMouseListener(object : MouseAdapter() {
                override fun mouseReleased(e: MouseEvent) {
                    if (!SwingUtilities.isLeftMouseButton(e)) return
                    input.run {
                        rowAt(e.point)?.takeIf { list.model.getElementAt(it).selectable() }?.let {
                            list.selectedIndex = it
                            activate()
                        }
                    }
                }
            })
            addWindowFocusListener(object : WindowAdapter() {
                override fun windowGainedFocus(e: WindowEvent) { input.run { list.requestFocusInWindow() } }
                override fun windowLostFocus(e: WindowEvent) {
                    input.run { if (root != null && !ownsMenu(e.oppositeWindow)) dismiss() }
                }
            })
            move(1)
        }

        private fun bind(key: String, action: () -> Unit) = WindowsMenuKeyBindings.bind(list, key, object : AbstractAction() {
            override fun actionPerformed(e: ActionEvent) = input.run(action)
        })

        private fun rowAt(point: Point): Int? = list.locationToIndex(point).takeIf {
            it >= 0 && list.getCellBounds(it, it).contains(point)
        }

        private fun move(direction: Int) {
            val size = list.model.size
            for (offset in 1..size) {
                val index = Math.floorMod(list.selectedIndex + direction * offset, size)
                if (list.model.getElementAt(index).selectable()) {
                    list.selectedIndex = index
                    list.ensureIndexIsVisible(index)
                    return
                }
            }
        }

        private fun activate() {
            when (val entry = list.selectedValue) {
                is MenuEntry.Submenu -> guarded { openChild() }
                is MenuEntry.Item -> state.activate(session, entry.item, ::disposeMenus)
                else -> {}
            }
        }

        private fun openChild() {
            val entry = list.selectedValue as? MenuEntry.Submenu ?: return
            closeChild()
            val window = MenuWindow(this, session, entry.entries, graphicsConfiguration)
            child = window
            window.fit()
            val bounds = usable(graphicsConfiguration)
            val childX = if (x + width + window.width > bounds.x + bounds.width) x - window.width + 1 else x + width - 1
            window.showAt(childX, list.locationOnScreen.y + list.getCellBounds(list.selectedIndex, list.selectedIndex).y)
        }

        private fun closeChild() { val previous = child; child = null; previous?.disposeTree() }
        fun disposeTree() { input.close(); closeChild(); dispose() }

        fun fit() {
            pack()
            val bounds = usable(graphicsConfiguration)
            setSize(width.coerceAtMost(bounds.width), height.coerceAtMost(bounds.height))
        }

        fun showAt(x: Int, y: Int) {
            val bounds = usable(graphicsConfiguration)
            setLocation(x.coerceIn(bounds.x, bounds.x + bounds.width - width), y.coerceIn(bounds.y, bounds.y + bounds.height - height))
            isVisible = true
            list.requestFocusInWindow()
        }
    }
}

private fun usable(gc: GraphicsConfiguration): Rectangle = Rectangle(gc.bounds).apply {
    val insets = Toolkit.getDefaultToolkit().getScreenInsets(gc)
    x += insets.left; y += insets.top
    width -= insets.left + insets.right; height -= insets.top + insets.bottom
}

private fun MenuEntry.selectable(): Boolean = when (this) {
    is MenuEntry.Item -> item.enabled && item.command != null
    is MenuEntry.Submenu -> true
    MenuEntry.Separator -> false
}

private class MenuCell : DefaultListCellRenderer() {
    private var entry: MenuEntry = MenuEntry.Separator

    init { putClientProperty("html.disable", true) }

    override fun getListCellRendererComponent(list: JList<*>?, value: Any?, index: Int, selected: Boolean, focused: Boolean): Component {
        entry = value as MenuEntry
        super.getListCellRendererComponent(list, value, index, selected && entry.selectable(), focused)
        text = when (val row = entry) {
            is MenuEntry.Item -> row.item.label
            is MenuEntry.Submenu -> row.title
            MenuEntry.Separator -> ""
        }
        val unit = getFontMetrics(font).height
        border = EmptyBorder(if (entry == MenuEntry.Separator) 0 else 4, unit + 12, 4, unit + 6)
        if (!entry.selectable()) foreground = UIManager.getColor("Label.disabledForeground") ?: foreground
        val toggle = (entry as? MenuEntry.Item)?.item?.takeIf { it.command == MenuBarCommand.TOGGLE_CAPTURE }
        accessibleContext.accessibleName = text
        accessibleContext.accessibleDescription = toggle?.let { if (it.checked) "チェックあり" else "チェックなし" }
        toolTipText = text
        return this
    }

    override fun paintComponent(graphics: Graphics) {
        super.paintComponent(graphics)
        val g = graphics.create() as Graphics2D
        try {
            g.setRenderingHint(RenderingHints.KEY_ANTIALIASING, RenderingHints.VALUE_ANTIALIAS_ON)
            g.color = foreground
            val y = height / 2
            val size = (getFontMetrics(font).height * 0.65).toInt().coerceAtLeast(6)
            if (entry == MenuEntry.Separator) g.drawLine(5, y, width - 5, y)
            (entry as? MenuEntry.Item)?.item?.takeIf { it.command == MenuBarCommand.TOGGLE_CAPTURE }?.let {
                g.drawRect(6, y - size / 2, size, size)
                if (it.checked) { g.drawLine(8, y, 6 + size / 2, y + size / 3); g.drawLine(6 + size / 2, y + size / 3, 5 + size, y - size / 3) }
            }
            if (entry is MenuEntry.Submenu) g.fillPolygon(intArrayOf(width - 12, width - 7, width - 12), intArrayOf(y - 4, y, y + 4), 3)
        } finally { g.dispose() }
    }
}

internal data class WindowsMenuSession(val revision: Long, val menu: MenuBarMenu)

/** A child may be disposed while its root session is still active. Reject its queued input too. */
internal class WindowsMenuInput(private val state: WindowsMenuState, private val session: WindowsMenuSession) {
    private var closed = false
    fun run(action: () -> Unit) { if (!closed && state.isActive(session)) action() }
    fun close() { closed = true }
}

/** Mutated on the EDT. The posting thread may read revision before queuing the existing menu-open callback. */
internal class WindowsMenuState {
    @Volatile var revision: Long = 0
        private set
    var closed: Boolean = false
        private set
    private var menu: MenuBarMenu? = null
    private var dispatch: MenuDispatch? = null
    private var active: WindowsMenuSession? = null

    fun update(menu: MenuBarMenu, dispatch: MenuDispatch): Boolean {
        if (closed) return false
        val changed = this.menu != menu
        if (changed) dismiss()
        this.menu = menu
        this.dispatch = dispatch
        return changed
    }

    fun open(expectedRevision: Long): WindowsMenuSession? {
        if (closed || expectedRevision != revision || active != null) return null
        return menu?.let { WindowsMenuSession(++revision, it).also { session -> active = session } }
    }

    fun dismiss() { active = null; revision++ }
    fun hide() { dismiss(); menu = null; dispatch = null }
    fun close() { closed = true; hide() }

    fun isActive(session: WindowsMenuSession): Boolean = !closed && active === session

    fun activate(session: WindowsMenuSession, item: MenuItemModel, closeMenus: () -> Unit) {
        if (!isActive(session) || !item.enabled || item.command == null) return
        val callback = dispatch ?: return
        val outgoing = if (item.command == MenuBarCommand.TOGGLE_CAPTURE) item.copy(checked = !item.checked) else item
        dismiss()
        val afterDismiss = revision
        closeMenus()
        if (revision == afterDismiss && dispatch === callback) callback.dispatch(outgoing)
    }
}

/** Platforms may flag press, release, or both; one physical popup gesture opens once. */
internal class WindowsPopupGesture(private val open: (MouseEvent) -> Unit) : MouseAdapter() {
    private var openedOnPress = false

    /** The IDE drops TrayIcon MouseEvents. Consume only our ordinary right click, without reposting it. */
    fun dispatchPosted(event: AWTEvent, ownsSource: (Any) -> Boolean): Boolean {
        if (event !is MouseEvent || event.button != MouseEvent.BUTTON3 || event.source?.let(ownsSource) != true) return false
        when (event.id) {
            MouseEvent.MOUSE_PRESSED -> mousePressed(event)
            MouseEvent.MOUSE_RELEASED -> mouseReleased(event)
            else -> return false
        }
        return true
    }

    // Post-event hooks run on the posting thread, not necessarily the EDT. Opening already queues UI work.
    @Synchronized override fun mousePressed(e: MouseEvent) { openedOnPress = e.isPopupTrigger; if (openedOnPress) open(e) }
    @Synchronized
    override fun mouseReleased(e: MouseEvent) {
        if (e.isPopupTrigger && !openedOnPress) open(e)
        openedOnPress = false
    }
}
