package io.github.shinma06.replaybuffer.menubar

import java.awt.Image

/** Fires when a menu row is activated; the receiver resolves the project and runs the operation. */
internal fun interface MenuDispatch {
    fun dispatch(item: MenuItemModel)
}

/**
 * One OS destination for the tray icon and menu. HostPlatform picks the implementation once; callers never branch
 * on the OS. Both failure paths report a user-facing reason instead of raising IDE errors: a surface without a
 * destination reports it from [unavailableReason], and a failed registration returns it from [show].
 */
internal interface TraySurface : AutoCloseable {
    /** Why no icon can appear on this environment; null while the tray is usable. */
    fun unavailableReason(): String?

    /** Adds or updates the icon and menu; returns the reason when registration fails, null on success. */
    fun show(image: Image, tooltip: String, menu: MenuBarMenu, dispatch: MenuDispatch): String?

    fun hide()

    fun ownsEventSource(source: Any): Boolean = false

    override fun close()
}

/** The destination chosen for operating systems the tray does not cover at all. */
internal class NoTraySurface(private val reason: String) : TraySurface {
    override fun unavailableReason(): String = reason
    override fun show(image: Image, tooltip: String, menu: MenuBarMenu, dispatch: MenuDispatch): String = reason
    override fun hide() {}
    override fun close() {}
}
