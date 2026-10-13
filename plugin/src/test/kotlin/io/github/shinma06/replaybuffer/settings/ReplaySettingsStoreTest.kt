package io.github.shinma06.replaybuffer.settings

import com.intellij.util.xmlb.XmlSerializer
import io.github.shinma06.replaybuffer.core.ApplicationMode
import io.github.shinma06.replaybuffer.core.ApplicationTarget
import io.github.shinma06.replaybuffer.core.ReplaySettings as CoreReplaySettings
import org.jdom.Element
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path

class ReplaySettingsStoreTest {
    @TempDir
    lateinit var destinationRoot: Path

    private val directory = Path.of(System.getProperty("java.io.tmpdir")).toAbsolutePath().toString()

    @Test
    fun `existing regular file is rejected before Apply while directory and unset destination are accepted`() {
        val file = Files.createFile(destinationRoot.resolve("recording.txt"))
        val store = ReplaySettingsStore()
        val valid = ReplaySettings(destination = destinationRoot.toString())
        assertNull(valid.validationError())
        store.apply(valid)
        store.setEnabled(true)
        val invalid = valid.copy(destination = file.toString())
        assertEquals("保存先はファイルではなくフォルダを指定してください。", invalid.validationError())
        assertThrows(IllegalArgumentException::class.java) { store.apply(invalid) }
        assertEquals(valid, store.settings())
        assertTrue(store.enabled)
        val missing = valid.copy(destination = destinationRoot.resolve("new-folder").toString())
        assertEquals("保存先フォルダが存在しません。フォルダを作成してから指定してください。", missing.validationError())
        assertThrows(IllegalArgumentException::class.java) { store.apply(missing) }
        assertEquals(valid, store.settings())
        assertTrue(store.enabled)
        assertFalse(Files.exists(Path.of(missing.destination)))
        store.apply(valid.copy(destination = ""))
        assertEquals("", store.settings().destination)
    }

    @Test
    fun `background destination result cannot authorize a different path`() {
        val file = Files.createFile(destinationRoot.resolve("recording.txt"))
        val store = ReplaySettingsStore()
        val valid = ReplaySettings(destination = destinationRoot.toString())
        val checked = validateDestination(valid.destination)
        store.apply(valid, checked)
        assertThrows(IllegalArgumentException::class.java) {
            store.apply(valid.copy(destination = file.toString()), checked)
        }
        assertEquals(valid, store.settings())
        assertThrows(IllegalArgumentException::class.java) {
            store.apply(valid.copy(destination = file.toString()), validateDestination(file.toString()))
        }
        assertEquals(valid, store.settings())
    }

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
    fun `IDE validation uses core retention and package boundaries with unset destination allowed`() {
        for (seconds in listOf(Int.MIN_VALUE, 0, 1, 180, 900, 901, Int.MAX_VALUE)) {
            val settings = ReplaySettings(retentionSeconds = seconds)
            val coreAccepted = runCatching { CoreReplaySettings(replaySeconds = seconds).validate() }.isSuccess
            assertEquals(coreAccepted, settings.validationError() == null, "seconds=$seconds")
        }
        for (value in listOf("com.a", "com." + "a".repeat(251), "com." + "a".repeat(252), "com.1app", "com._app")) {
            val coreAccepted = runCatching { ApplicationTarget(value, ApplicationMode.MANUAL).validate() }.isSuccess
            assertEquals(coreAccepted, isApplicationId(value), "length=${value.length}")
        }
        val store = ReplaySettingsStore()
        store.apply(ReplaySettings(retentionSeconds = 900))
        store.setEnabled(true)
        assertTrue(store.enabled)
        assertEquals("", store.settings().destination)
        store.loadState(ReplaySettingsState(retentionSeconds = 901))
        assertEquals(180, store.settings().retentionSeconds)
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
