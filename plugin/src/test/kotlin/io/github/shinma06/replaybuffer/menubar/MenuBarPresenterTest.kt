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
    @Test
    fun `template icons use only ink alpha and never include the opaque fallback halo`() {
        val masks = MenuBarIcon.entries.map { icon ->
            val template = menuBarTemplateIconImage(icon)
            val fallback = menuBarIconImage(icon)
            val pixels = (0 until template.height).flatMap { y -> (0 until template.width).map { x -> template.getRGB(x, y) } }
            assertTrue(pixels.any { it ushr 24 != 0 })
            assertTrue(pixels.all { it and 0xFFFFFF == 0 }, "Templates must not merge opaque white halo into the glyph")
            assertTrue((0 until template.height).any { y -> (0 until template.width).any { x ->
                fallback.getRGB(x, y) ushr 24 > template.getRGB(x, y) ushr 24
            } })
            pixels.map { it ushr 24 }
        }
        assertEquals(MenuBarIcon.entries.size, masks.distinct().size)
    }

    @Test
    fun `an unchanged presentation leaves the native menu intact and uses the latest dispatcher`() {
        val surface = FakeSurface()
        val presenter = MenuBarPresenter(surface)
        val oldEvents = mutableListOf<MenuItemModel>()
        val newEvents = mutableListOf<MenuItemModel>()
        presenter.update(true, listOf(input()), MenuDispatch(oldEvents::add))
        val original = surface.lastMenu
        repeat(5) { presenter.update(true, listOf(input()), MenuDispatch(newEvents::add)) }
        assertEquals(1, surface.shows, "Snapshot refreshes must not replace an unchanged native popup or image")
        assertTrue(original === surface.lastMenu)
        val save = flatten(surface.lastMenu!!).single { it.command == MenuBarCommand.SAVE }
        surface.lastDispatch!!.dispatch(save)
        assertTrue(oldEvents.isEmpty())
        assertEquals(listOf(save), newEvents)
        presenter.update(true, listOf(input().copy(enabled = false)), MenuDispatch(newEvents::add))
        assertEquals(2, surface.shows, "A changed menu must still be rendered")
    }

    @Test
    fun `failed registration is retried and hiding invalidates the presentation cache`() {
        val surface = FakeSurface(failure = "登録失敗")
        val presenter = MenuBarPresenter(surface)
        repeat(2) { presenter.update(true, listOf(input()), MenuDispatch {}) }
        assertEquals(2, surface.shows)
        surface.failure = null
        presenter.update(true, listOf(input()), MenuDispatch {})
        assertEquals(3, surface.shows)
        presenter.update(false, listOf(input()), MenuDispatch {})
        presenter.update(true, listOf(input()), MenuDispatch {})
        assertEquals(4, surface.shows)
        presenter.update(true, emptyList(), MenuDispatch {})
        val delivered = mutableListOf<MenuItemModel>()
        presenter.update(true, listOf(input()), MenuDispatch(delivered::add))
        assertEquals(5, surface.shows)
        val staleDispatch = surface.lastDispatch!!
        presenter.close()
        assertFalse(presenter.shown)
        staleDispatch.dispatch(flatten(surface.lastMenu!!).first { it.command == MenuBarCommand.SAVE })
        assertTrue(delivered.isEmpty(), "An event from the removed icon must not reach project operations")
    }

    @Test
    fun `a rejected capture change resynchronizes the native checkbox even when the model is unchanged`() {
        val surface = FakeSurface()
        val presenter = MenuBarPresenter(surface)
        presenter.update(true, listOf(input()), MenuDispatch {})
        val toggle = flatten(surface.lastMenu!!).single { it.command == MenuBarCommand.TOGGLE_CAPTURE }
        surface.lastDispatch!!.dispatch(toggle.copy(checked = !toggle.checked))
        presenter.update(true, listOf(input()), MenuDispatch {})
        assertEquals(2, surface.shows, "A checkbox mutates before dispatch, so a rejected change must restore it")
        assertEquals(toggle.checked, flatten(surface.lastMenu!!).single { it.command == MenuBarCommand.TOGGLE_CAPTURE }.checked)
    }

    @Test
    fun `callbacks from a hidden presentation cannot reach a later presentation`() {
        listOf("off", "empty", "unavailable", "close").forEach { hidden ->
            val surface = FakeSurface()
            val presenter = MenuBarPresenter(surface)
            val delivered = mutableListOf<MenuItemModel>()
            presenter.update(true, listOf(input()), MenuDispatch(delivered::add))
            val oldDispatch = surface.lastDispatch!!
            val oldItems = flatten(surface.lastMenu!!).filter { it.command != null }
            when (hidden) {
                "off" -> presenter.update(false, listOf(input()), MenuDispatch(delivered::add))
                "empty" -> presenter.update(true, emptyList(), MenuDispatch(delivered::add))
                "unavailable" -> {
                    surface.unavailable = "利用不可"
                    presenter.update(true, listOf(input()), MenuDispatch(delivered::add))
                    surface.unavailable = null
                }
                "close" -> presenter.close()
            }
            presenter.update(true, listOf(input()), MenuDispatch(delivered::add))
            val shows = surface.shows
            oldItems.forEach(oldDispatch::dispatch)
            assertTrue(delivered.isEmpty(), hidden)
            presenter.update(true, listOf(input()), MenuDispatch(delivered::add))
            assertEquals(shows, surface.shows, "A stale toggle must not invalidate the current cache: $hidden")
            val save = flatten(surface.lastMenu!!).single { it.command == MenuBarCommand.SAVE }
            surface.lastDispatch!!.dispatch(save)
            assertEquals(listOf(save), delivered, hidden)
            presenter.close()
        }
    }

    @Test
    fun `replaced and failed presentations cannot dispatch into their successors`() {
        val surface = FakeSurface()
        val presenter = MenuBarPresenter(surface)
        val delivered = mutableListOf<MenuItemModel>()
        presenter.update(true, listOf(input()), MenuDispatch(delivered::add))
        val replaced = surface.lastDispatch!!
        val oldSave = flatten(surface.lastMenu!!).single { it.command == MenuBarCommand.SAVE }
        surface.failure = "登録失敗"
        presenter.update(true, listOf(input("Other")), MenuDispatch(delivered::add))
        val failed = surface.lastDispatch!!
        surface.failure = null
        presenter.update(true, listOf(input()), MenuDispatch(delivered::add))
        replaced.dispatch(oldSave)
        failed.dispatch(oldSave)
        assertTrue(delivered.isEmpty())
        surface.lastDispatch!!.dispatch(oldSave)
        assertEquals(listOf(oldSave), delivered)
        presenter.close()
    }

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

    private class FakeSurface(var unavailable: String? = null, var failure: String? = null) : TraySurface {
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
