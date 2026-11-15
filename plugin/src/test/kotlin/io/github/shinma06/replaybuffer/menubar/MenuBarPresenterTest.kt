package io.github.shinma06.replaybuffer.menubar

import io.github.shinma06.replaybuffer.core.ApplicationTarget
import io.github.shinma06.replaybuffer.core.CaptureState
import io.github.shinma06.replaybuffer.core.DeviceKind
import io.github.shinma06.replaybuffer.core.ReplayDevice
import io.github.shinma06.replaybuffer.core.ReplaySettings as CoreSettings
import io.github.shinma06.replaybuffer.core.ReplaySnapshot
import io.github.shinma06.replaybuffer.core.SaveOrigin
import io.github.shinma06.replaybuffer.core.SaveRequest
import io.github.shinma06.replaybuffer.settings.ReplaySettings
import java.awt.GraphicsEnvironment
import java.awt.Image
import java.awt.SystemTray
import java.nio.file.Path
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class MenuBarPresenterTest {
    private fun input(name: String = "MyApp") = MenuProjectInput(
        "id-$name", name,
        ReplaySnapshot(1, 1, true, CaptureState.CAPTURING,
            CoreSettings(replaySeconds = 30, application = ApplicationTarget("com.example.app")), 1,
            device = ReplayDevice("emulator-5554", "Pixel 9", DeviceKind.EMULATOR, true), canSave = true),
        message = null, initializationError = null, busy = false, enabled = true,
        settings = ReplaySettings(retentionSeconds = 30),
        screenshotDisabledReason = null, screenshotBusy = false, savedAt = null, screenshot = null)

    @Test
    fun `a usable surface shows the built menu and routes its commands`() {
        val surface = FakeSurface()
        val presenter = MenuBarPresenter(surface)
        val ops = FakeOps()
        val dispatched = mutableListOf<MenuItemModel>()
        presenter.update(requested = true, inputs = listOf(input()), MenuDispatch(dispatched::add))
        assertTrue(presenter.shown)
        assertNull(presenter.lastReason)
        assertEquals(1, surface.shows)
        val menu = surface.lastMenu!!
        val save = surface.lastDispatch
        // The presenter hands the surface one dispatch entry point; items reach the project's operations.
        val item = flatten(menu).single { it.command == MenuBarCommand.SAVE }
        save?.dispatch(item)
        assertEquals(dispatched.single(), item)
        item.run(ops)
        assertEquals(SaveRequest(origin = SaveOrigin.MENU_BAR), ops.savedRequest)
    }

    @Test
    fun `an environment without a tray hides the icon and keeps the reason`() {
        val surface = FakeSurface(unavailable = "このOS・デスクトップのトレイに対応していないため")
        val presenter = MenuBarPresenter(surface)
        presenter.update(requested = true, inputs = listOf(input()), MenuDispatch {})
        assertFalse(presenter.shown)
        assertEquals(surface.unavailable, presenter.lastReason)
        assertEquals(surface.unavailable, presenter.unavailableReason())
        assertTrue(surface.hideCalls >= 1)
        assertEquals(0, surface.shows)
    }

    @Test
    fun `a failed registration reports a reason instead of throwing`() {
        val surface = FakeSurface(failure = "OSのトレイへメニューバーアイコンを登録できませんでした。")
        val presenter = MenuBarPresenter(surface)
        presenter.update(requested = true, inputs = listOf(input()), MenuDispatch {})
        assertFalse(presenter.shown)
        assertEquals(surface.failure, presenter.lastReason)
        assertEquals(1, surface.shows)
        assertEquals(1, surface.hideCalls)
    }

    @Test
    fun `off setting or no projects removes the icon`() {
        val surface = FakeSurface()
        val presenter = MenuBarPresenter(surface)
        presenter.update(requested = true, inputs = listOf(input()), MenuDispatch {})
        assertTrue(presenter.shown)
        presenter.update(requested = false, inputs = listOf(input()), MenuDispatch {})
        assertFalse(presenter.shown)
        presenter.update(requested = true, inputs = emptyList(), MenuDispatch {})
        assertFalse(presenter.shown)
        assertNull(presenter.lastReason)
        assertEquals(1, surface.shows)
    }

    @Test
    fun `the icon image stays readable as ink on a light and a dark halo`() {
        val images = MenuBarIcon.entries.associateWith(::menuBarIconImage)
        assertEquals(MenuBarIcon.entries.toSet(), images.keys)
        images.forEach { (icon, image) ->
            var ink = 0
            var halo = 0
            var transparent = 0
            for (y in 0 until image.height) for (x in 0 until image.width) {
                val argb = image.getRGB(x, y)
                when {
                    argb ushr 24 == 0 -> transparent++
                    argb and 0xFFFFFF == 0xFFFFFF -> halo++
                    else -> ink++
                }
            }
            assertTrue(ink > 0, "$icon needs visible ink")
            assertTrue(halo > 0, "$icon needs the light fallback halo")
            assertTrue(transparent > 0, "$icon keeps a transparent background")
        }
        val pixels = images.values.map { image -> (0 until image.width).map { x -> image.getRGB(x, image.height / 2) } }
        assertTrue(pixels.distinct().size > 1, "icons must differ by shape, not only by colour")
    }

    @Test
    fun `the platform surface reports a reason exactly when the tray is unusable`() {
        val reason = AwtTraySurface().unavailableReason()
        val usable = !GraphicsEnvironment.isHeadless() && SystemTray.isSupported()
        assertEquals(usable, reason == null)
        val absent = NoTraySurface("理由")
        assertEquals("理由", absent.unavailableReason())
        assertEquals("理由", absent.show(stubImage(), "", menuBarModel(listOf(input())), MenuDispatch {}))
    }

    private fun flatten(menu: MenuBarMenu): List<MenuItemModel> = menu.entries.flatMap {
        when (it) {
            is MenuEntry.Item -> listOf(it.item)
            is MenuEntry.Submenu -> it.entries.filterIsInstance<MenuEntry.Item>().map(MenuEntry.Item::item)
            is MenuEntry.Separator -> emptyList()
        }
    }

    private fun stubImage(): Image = java.awt.image.BufferedImage(4, 4, java.awt.image.BufferedImage.TYPE_INT_ARGB)

    private class FakeSurface(val unavailable: String? = null, val failure: String? = null) : TraySurface {
        var shows = 0
        var hideCalls = 0
        var closed = false
        var lastMenu: MenuBarMenu? = null
        var lastDispatch: MenuDispatch? = null
        override fun unavailableReason(): String? = unavailable
        override fun show(image: Image, tooltip: String, menu: MenuBarMenu, dispatch: MenuDispatch): String? {
            shows++
            lastMenu = menu
            lastDispatch = dispatch
            return failure
        }
        override fun hide() { hideCalls++ }
        override fun close() { closed = true }
    }

    private class FakeOps(var captureEnabled: Boolean = true) : MenuBarOperations {
        var savedRequest: SaveRequest? = null
        override fun setEnabled(value: Boolean) { captureEnabled = value }
        override fun save(request: SaveRequest) { savedRequest = request }
        override fun screenshot() {}
        override fun openDirectory(directory: Path) {}
        override fun showToolWindow() {}
        override fun openSettings() {}
    }
}
