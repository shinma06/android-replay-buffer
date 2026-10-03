package io.github.shinma06.replaybuffer.settings

import com.intellij.util.xmlb.XmlSerializer
import org.jdom.Element
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.nio.file.Path

class ReplaySettingsStoreTest {
    private val directory = Path.of(System.getProperty("java.io.tmpdir")).toAbsolutePath().toString()

    @Test
    fun `draft edits do not change applied settings and Apply preserves immediate toggle`() {
        val store = ReplaySettingsStore()
        assertFalse(store.enabled)
        assertEquals(180, store.settings().retentionSeconds)
        val initial = store.settings()
        val draft = initial.copy(retentionSeconds = 60, destination = directory)
        assertEquals(initial, store.settings())
        store.setEnabled(true)
        store.apply(draft)
        assertTrue(store.enabled)
        assertEquals(draft, store.settings())
        assertEquals(180, initial.retentionSeconds)
        store.setEnabled(false)
        assertEquals(draft, store.settings())
    }

    @Test
    fun `invalid Apply is atomic and persisted state is detached`() {
        val store = ReplaySettingsStore()
        val valid = ReplaySettings(destination = directory)
        store.apply(valid)
        store.setEnabled(true)
        assertThrows(IllegalArgumentException::class.java) {
            store.apply(valid.copy(retentionSeconds = 0, destination = "other"))
        }
        assertEquals(valid, store.settings())
        val saved = store.getState()
        saved.enabled = false
        saved.retentionSeconds = 5
        assertTrue(store.enabled)
        assertEquals(180, store.settings().retentionSeconds)
        val restored = ReplaySettingsStore()
        restored.loadState(XmlSerializer.deserialize(XmlSerializer.serialize(store.getState()), ReplaySettingsState::class.java))
        assertTrue(restored.enabled)
        assertEquals(valid, restored.settings())
        val oldXml = XmlSerializer.deserialize(Element("state"), ReplaySettingsState::class.java)
        assertEquals(180, oldXml.retentionSeconds)
        oldXml.retentionSeconds = -10
        restored.loadState(oldXml)
        oldXml.retentionSeconds = 10
        assertFalse(restored.enabled)
        assertEquals(180, restored.settings().retentionSeconds)
    }

    @Test
    fun `manual application IDs and absolute destination paths reject unsafe input`() {
        val valid = ReplaySettings(destination = directory, appSelection = AppSelectionMode.MANUAL, manualPackage = "com.example_app.debug")
        assertNull(valid.validationError())
        for (packageName in listOf("", "app", "1com.app", "com.1app", "com.app;echo", "com.app\n", "com.app/other")) {
            assertNotNull(valid.copy(manualPackage = packageName).validationError(), packageName)
        }
        assertNull(valid.copy(destination = "").validationError())
        assertNotNull(valid.copy(destination = "relative/path").validationError())
        assertNotNull(valid.copy(destination = "bad\u0000path").validationError())
        assertNull(valid.copy(appSelection = AppSelectionMode.AUTOMATIC, manualPackage = "").validationError())
    }
}
