package io.github.shinma06.replaybuffer.menubar

import java.awt.Image

/**
 * Applies the menu model to one tray surface. Free of IDE types so a fake surface can drive the same decisions in
 * unit tests: show only while requested with at least one project, otherwise hide and keep the reason visible.
 */
internal class MenuBarPresenter(
    private val surface: TraySurface,
    private val iconFor: (MenuBarIcon) -> Image = ::menuBarIconImage,
) : AutoCloseable {
    var shown: Boolean = false
        private set
    var lastReason: String? = null
        private set

    /** The reason the settings page can show: a missing destination first, then the last failed registration. */
    fun unavailableReason(): String? = surface.unavailableReason() ?: lastReason

    fun update(requested: Boolean, inputs: List<MenuProjectInput>, dispatch: MenuDispatch) {
        if (!requested || inputs.isEmpty()) {
            surface.hide()
            shown = false
            lastReason = null
            return
        }
        surface.unavailableReason()?.let {
            surface.hide()
            shown = false
            lastReason = it
            return
        }
        val menu = menuBarModel(inputs)
        val reason = surface.show(iconFor(menu.icon), menu.tooltip, menu, dispatch)
        shown = reason == null
        lastReason = reason
        if (reason != null) surface.hide()
    }

    override fun close() {
        runCatching { surface.hide() }
        runCatching { surface.close() }
    }
}
