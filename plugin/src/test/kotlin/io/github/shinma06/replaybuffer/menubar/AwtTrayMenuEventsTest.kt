package io.github.shinma06.replaybuffer.menubar

import java.awt.AWTEvent
import java.awt.CheckboxMenuItem
import java.awt.MenuComponent
import java.awt.event.ActionEvent
import java.awt.event.ItemEvent
import java.io.File
import java.net.URI
import java.nio.file.Path
import java.util.concurrent.TimeUnit
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class AwtTrayMenuEventsTest {
    @Test
    fun `real AWT checkbox item events and ordinary action events reach the dispatch`() {
        val locations = listOf(AwtTrayMenuEventFixture::class.java, MenuBarMenu::class.java, Unit::class.java).map { type ->
            val suffix = "/${type.name.replace('.', '/')}.class"
            val resource = checkNotNull(type.getResource(suffix))
            if (resource.protocol == "jar") Path.of(URI.create(resource.toString().removePrefix("jar:").substringBefore("!/"))).toString()
            else Path.of(resource.toURI()).toString().removeSuffix(suffix.replace('/', File.separatorChar))
        }
        // Construct peerless AWT menus even on headless CI, without initializing an OS toolkit or showing a window.
        val process = ProcessBuilder(
            Path.of(System.getProperty("java.home"), "bin", "java").toString(),
            "-Djava.awt.headless=false", "--add-opens=java.desktop/java.awt=ALL-UNNAMED",
            "-cp", locations.distinct().joinToString(File.pathSeparator), AwtTrayMenuEventFixture::class.java.name,
        ).redirectErrorStream(true).start()
        try {
            assertTrue(process.waitFor(30, TimeUnit.SECONDS), "AWT event fixture did not finish")
            assertEquals(0, process.exitValue(), process.inputStream.bufferedReader().readText())
        } finally {
            process.destroyForcibly()
        }
    }
}

object AwtTrayMenuEventFixture {
    @JvmStatic
    fun main(args: Array<String>) {
        val toggle = MenuItemModel("取得する", true, MenuBarCommand.TOGGLE_CAPTURE, projectId = "project")
        val save = MenuItemModel("保存", true, MenuBarCommand.SAVE, projectId = "project")
        val events = mutableListOf<MenuItemModel>()
        val popup = MenuBarMenu(MenuBarIcon.OFF, "Replay", listOf(
            MenuEntry.Item(toggle), MenuEntry.Item(save), MenuEntry.Item(MenuItemModel("取得OFF")),
        )).toPopupMenu(MenuDispatch(events::add))
        val checkbox = popup.getItem(0) as CheckboxMenuItem
        check(!checkbox.state && checkbox.isEnabled)
        check(!popup.getItem(2).isEnabled)

        val processEvent = MenuComponent::class.java.getDeclaredMethod("processEvent", AWTEvent::class.java).apply { isAccessible = true }
        listOf(ItemEvent.SELECTED, ItemEvent.DESELECTED).forEach { state ->
            checkbox.state = state == ItemEvent.SELECTED
            processEvent.invoke(checkbox, ItemEvent(checkbox, ItemEvent.ITEM_STATE_CHANGED, checkbox.label, state))
        }
        val saveItem = popup.getItem(1)
        processEvent.invoke(saveItem, ActionEvent(saveItem, ActionEvent.ACTION_PERFORMED, saveItem.actionCommand))
        check(events == listOf(toggle, toggle, save)) { "Unexpected menu dispatch: $events" }
    }
}
