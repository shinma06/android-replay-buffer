package io.github.shinma06.replaybuffer.ui

import com.intellij.openapi.actionSystem.AnAction
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import javax.xml.parsers.DocumentBuilderFactory
import org.w3c.dom.Element

class RunningDevicesActionsTest {
    @Test
    fun `Running Devices toolbar gets capture save and screenshot actions`() {
        val document = javaClass.getResourceAsStream("/META-INF/plugin.xml")!!.use {
            DocumentBuilderFactory.newInstance().newDocumentBuilder().parse(it)
        }
        val groups = document.getElementsByTagName("group")
        val group = (0 until groups.length).map { groups.item(it) as Element }
            .single { it.getAttribute("id") == "io.github.shinma06.replaybuffer.RunningDevices" }
        val target = group.getElementsByTagName("add-to-group").item(0) as Element
        assertEquals("StreamingToolbarSecondary", target.getAttribute("group-id"))
        val actions = group.getElementsByTagName("action")
        val classes = (0 until actions.length).map { (actions.item(it) as Element).getAttribute("class") }
        assertEquals(listOf(ReplayCaptureToggleAction::class, ReplaySaveAction::class, ReplayScreenshotAction::class).map { it.qualifiedName }, classes)
        classes.forEach { assertTrue(AnAction::class.java.isAssignableFrom(Class.forName(it))) }
    }

    @Test
    fun `Logcat gets the logs-only save in its public popup group and tool window header`() {
        val document = javaClass.getResourceAsStream("/META-INF/plugin.xml")!!.use {
            DocumentBuilderFactory.newInstance().newDocumentBuilder().parse(it)
        }
        val groups = document.getElementsByTagName("group")
        val group = (0 until groups.length).map { groups.item(it) as Element }
            .single { it.getAttribute("id") == "io.github.shinma06.replaybuffer.Logcat" }
        assertEquals("Logcat.PopupActions", (group.getElementsByTagName("add-to-group").item(0) as Element).getAttribute("group-id"))
        val action = group.getElementsByTagName("action").item(0) as Element
        assertEquals(1, group.getElementsByTagName("action").length)
        assertEquals(LOGCAT_SAVE_LOGS_ACTION_ID, action.getAttribute("id"))
        assertEquals(ReplayLogSaveAction::class.qualifiedName, action.getAttribute("class"))
        val listeners = document.getElementsByTagName("listener")
        val listener = (0 until listeners.length).map { listeners.item(it) as Element }
            .single { it.getAttribute("class") == LogcatTitleActions::class.qualifiedName }
        assertEquals("com.intellij.openapi.wm.ex.ToolWindowManagerListener", listener.getAttribute("topic"))
        assertEquals("projectListeners", listener.parentNode.nodeName)
    }
}
