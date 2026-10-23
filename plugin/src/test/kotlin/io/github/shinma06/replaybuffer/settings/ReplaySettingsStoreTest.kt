package io.github.shinma06.replaybuffer.settings

import com.intellij.util.xmlb.XmlSerializer
import io.github.shinma06.replaybuffer.core.ApplicationMode
import io.github.shinma06.replaybuffer.core.ApplicationTarget
import io.github.shinma06.replaybuffer.core.CaptureMode
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
    fun `mode persists and changes are rejected atomically while enabled`() {
        val store = ReplaySettingsStore()
        val selected = ReplaySettings(captureMode = CaptureMode.LOG_ONLY, autoSaveOnCrash = true)
        store.apply(selected)
        store.setEnabled(true)
        val restored = ReplaySettingsStore()
        restored.loadState(XmlSerializer.deserialize(XmlSerializer.serialize(store.state), ReplaySettingsState::class.java))
        assertEquals(selected, restored.settings())
        assertTrue(restored.enabled)
        val before = store.stateModificationCount
        assertThrows(IllegalArgumentException::class.java) {
            store.apply(selected.copy(captureMode = CaptureMode.VIDEO_AND_LOGS, retentionSeconds = 12))
        }
        assertEquals(before, store.stateModificationCount)
        assertEquals(selected, store.settings())
        store.apply(selected.copy(retentionSeconds = 60))
        assertEquals(CaptureMode.LOG_ONLY, store.settings().captureMode)
        store.setEnabled(false)
        store.apply(selected.copy(captureMode = CaptureMode.VIDEO_AND_LOGS))
        assertEquals(CaptureMode.VIDEO_AND_LOGS, store.settings().captureMode)
    }

    @Test
    fun `CLI import changes only legacy values while OFF and invalid input is atomic`() {
        val store = ReplaySettingsStore()
        val selected = ReplaySettings(180, "", AppSelectionMode.MANUAL, "com.example.app", CaptureMode.LOG_ONLY, autoSaveOnCrash = true,
            screenshotUsesLogcatDestination = false, screenshotDirectory = destinationRoot.toString())
        store.apply(selected)
        store.importLegacy(60, destinationRoot.toString())
        assertEquals(selected.copy(retentionSeconds = 60, destination = destinationRoot.toString()), store.settings())
        assertFalse(store.enabled)
        val before = store.state
        assertThrows(IllegalArgumentException::class.java) { store.importLegacy(901, destinationRoot.toString()) }
        assertThrows(IllegalArgumentException::class.java) { store.importLegacy(12, destinationRoot.resolve("missing").toString()) }
        assertEquals(before, store.state)
        store.setEnabled(true)
        assertThrows(IllegalArgumentException::class.java) { store.importLegacy(12, destinationRoot.toString()) }
        assertTrue(store.enabled)
        assertEquals(60, store.settings().retentionSeconds)
    }

    @Test
    fun `legacy common and absent screenshot mode follow Logcat while manual settings persist without fallback`() {
        val oldXml = XmlSerializer.deserialize(Element("state").apply {
            addContent(Element("option").setAttribute("name", "destination").setAttribute("value", destinationRoot.toString()))
        }, ReplaySettingsState::class.java)
        val store = ReplaySettingsStore()
        store.loadState(oldXml)
        assertTrue(store.settings().screenshotUsesLogcatDestination)
        assertEquals("", store.settings().manualScreenshotDestination)
        for (mode in listOf(true, false)) {
            val legacy = XmlSerializer.deserialize(Element("state").apply {
                addContent(Element("option").setAttribute("name", "destination").setAttribute("value", destinationRoot.toString()))
                addContent(Element("option").setAttribute("name", "screenshotUsesCommonDestination").setAttribute("value", mode.toString()))
                addContent(Element("option").setAttribute("name", "screenshotDirectory").setAttribute("value", destinationRoot.toString()))
            }, ReplaySettingsState::class.java)
            store.loadState(legacy)
            assertEquals(mode, store.settings().screenshotUsesLogcatDestination)
            assertEquals(destinationRoot.toString(), store.settings().screenshotDirectory)
            assertEquals(if (mode) "" else destinationRoot.toString(), store.settings().manualScreenshotDestination)
            store.apply(store.settings())
        }
        val dedicated = Files.createDirectory(destinationRoot.resolve("screenshots")).toString()
        val selected = store.settings().copy(screenshotUsesLogcatDestination = false, screenshotDirectory = dedicated)
        store.setEnabled(true)
        store.apply(selected)
        val restored = ReplaySettingsStore()
        restored.loadState(XmlSerializer.deserialize(XmlSerializer.serialize(store.state), ReplaySettingsState::class.java))
        assertEquals(selected, restored.settings())
        assertEquals(dedicated, restored.settings().manualScreenshotDestination)
        assertTrue(restored.enabled)
        val checked = validateDestination(dedicated)
        for (invalid in listOf("", "relative", destinationRoot.resolve("missing").toString(), Files.createFile(destinationRoot.resolve("file")).toString())) {
            val draft = selected.copy(screenshotDirectory = invalid)
            assertEquals(invalid, draft.manualScreenshotDestination, "invalid dedicated path must never fall back")
            assertNotNull(draft.validationError())
            assertThrows(IllegalArgumentException::class.java) { store.apply(draft) }
            assertThrows(IllegalArgumentException::class.java) { store.apply(draft, validateDestination(selected.destination), checked) }
            assertEquals(selected, store.settings(), "invalid Apply must be atomic")
        }
        val follow = selected.copy(screenshotUsesLogcatDestination = true, screenshotDirectory = "unused-relative")
        store.apply(follow)
        assertTrue(store.settings().screenshotUsesLogcatDestination)
        assertEquals("", store.settings().manualScreenshotDestination, "Logcat location must never use the replay root")
        store.apply(selected.copy(screenshotDirectory = selected.destination))
        assertEquals(selected.destination, store.settings().manualScreenshotDestination, "manual root may equal replay root")
        store.apply(selected.copy(destination = ""))
        assertEquals(dedicated, store.settings().manualScreenshotDestination, "screenshot-only folder works without a video/log destination")
    }

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
    fun `invalid Apply is atomic and persisted state is an immutable snapshot`() {
        val store = ReplaySettingsStore()
        val valid = ReplaySettings(destination = directory)
        store.apply(valid)
        store.setEnabled(true)
        assertThrows(IllegalArgumentException::class.java) {
            store.apply(valid.copy(retentionSeconds = 0, destination = "other"))
        }
        assertEquals(valid, store.settings())
        val saved = store.getState()
        store.setEnabled(false)
        store.apply(valid.copy(retentionSeconds = 5))
        assertTrue(saved.enabled)
        assertEquals(180, saved.retentionSeconds)
        store.loadState(saved)
        assertTrue(store.enabled)
        assertEquals(180, store.settings().retentionSeconds)
        val restored = ReplaySettingsStore()
        restored.loadState(XmlSerializer.deserialize(XmlSerializer.serialize(store.getState()), ReplaySettingsState::class.java))
        assertTrue(restored.enabled)
        assertEquals(valid, restored.settings())
        val oldXml = XmlSerializer.deserialize(Element("state"), ReplaySettingsState::class.java)
        assertEquals(180, oldXml.retentionSeconds)
        assertEquals(CaptureMode.VIDEO_AND_LOGS, oldXml.captureMode)
        assertFalse(oldXml.autoSaveOnCrash)
        restored.loadState(oldXml.copy(retentionSeconds = -10))
        assertFalse(restored.enabled)
        assertEquals(180, restored.settings().retentionSeconds)
    }

    @Test
    fun `legacy XML fields load without migration and changes advance the IDE modification tracker`() {
        val xml = Element("state").apply {
            for ((name, value) in mapOf("enabled" to "true", "retentionSeconds" to "60",
                "destination" to directory, "appSelection" to "手動", "manualPackage" to "com.example.app")) {
                addContent(Element("option").setAttribute("name", name).setAttribute("value", value))
            }
        }
        val store = ReplaySettingsStore()
        store.loadState(XmlSerializer.deserialize(xml, ReplaySettingsState::class.java))
        val expected = ReplaySettings(60, directory, AppSelectionMode.MANUAL, "com.example.app")
        assertEquals(expected, store.settings())
        assertTrue(store.enabled)
        val before = store.stateModificationCount
        store.setEnabled(false)
        assertTrue(store.stateModificationCount > before)
        assertEquals(expected, store.settings())
        val restored = ReplaySettingsStore()
        restored.loadState(XmlSerializer.deserialize(XmlSerializer.serialize(store.state), ReplaySettingsState::class.java))
        assertEquals(store.state, restored.state)
    }

    @Test
    fun `concurrent toggle and settings Apply preserve both updates`() {
        val store = ReplaySettingsStore()
        java.util.concurrent.Executors.newFixedThreadPool(2).use { executor ->
            repeat(100) { index ->
                store.setEnabled(false)
                val start = java.util.concurrent.CyclicBarrier(2)
                val settings = ReplaySettings(retentionSeconds = index + 1)
                val toggle = executor.submit { start.await(); store.setEnabled(true) }
                val apply = executor.submit { start.await(); store.apply(settings) }
                toggle.get(5, java.util.concurrent.TimeUnit.SECONDS)
                apply.get(5, java.util.concurrent.TimeUnit.SECONDS)
                assertTrue(store.enabled)
                assertEquals(settings, store.settings())
            }
        }
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
