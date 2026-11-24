package io.github.shinma06.arb.ide

import com.intellij.openapi.components.State
import com.intellij.util.xmlb.XmlSerializer
import org.jdom.Element
import org.junit.jupiter.api.Test
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/**
 * G-M9 plugin-side upgrade regressions.
 *
 * - U3 / U4: v2-old -> v2-new plugin overwrite keeps the IDE-local settings (`trayEnabled`,
 *   `burnTapsOnExport`, ...) because the persistent-state component keeps its name and storage file.
 */
class ArbUpgradeRegressionTest {
    private fun repoRoot(): Path {
        System.getProperty("arb.repo.root")?.let { raw ->
            val p = Paths.get(raw).toAbsolutePath().normalize()
            if (Files.isDirectory(p.resolve("core")) && Files.isDirectory(p.resolve("ide-plugin"))) return p
        }
        var cur = Paths.get("").toAbsolutePath().normalize()
        repeat(8) {
            if (Files.isDirectory(cur.resolve("core")) && Files.isDirectory(cur.resolve("ide-plugin"))) return cur
            cur = cur.parent ?: return cur
        }
        error("Could not locate repository root")
    }

    @Test
    fun pluginIdIsStableAcrossOverwriteUpdates() {
        val pluginXml = Files.readString(repoRoot().resolve("ide-plugin/src/main/resources/META-INF/plugin.xml"))
        val ids = Regex("""<id>([^<]+)</id>""").findAll(pluginXml).map { it.groupValues[1].trim() }.toList()
        assertEquals(listOf("io.github.shinma06.android-replay-buffer"), ids, "plugin.xml must declare the installed plugin id")
        assertEquals("io.github.shinma06.android-replay-buffer", ArbdDaemonService.PLUGIN_ID)
    }

    @Test
    fun persistentStateKeepsItsComponentNameAndStorageFileAcrossPluginUpdates() {
        val annotation = ArbLocalSettingsState::class.java.getAnnotation(State::class.java)
        assertEquals("ArbLocalSettings", annotation.name, "renaming the state component would drop every saved IDE-local setting")
        assertEquals(listOf("arb-local.xml"), annotation.storages.map { it.value })
    }

    @Test
    fun ideLocalSettingsSurviveAnOverwriteUpdateEvenWhenFieldsWereAddedOrAreUnknown() {
        val saved = ArbLocalSettingsState.State(
            customArbdBinaryPath = "/opt/arbd",
            screenshotInheritLogcat = false,
            customScreenshotDir = "/shots",
            burnTapsOnExport = false,
            trayEnabled = true,
        )
        val written: Element = XmlSerializer.serialize(saved)
        // An older plugin build did not know `trayEnabled`; a newer one may add fields we do not know.
        written.children.removeIf { it.getAttributeValue("name") == "trayEnabled" }
        written.addContent(Element("option").setAttribute("name", "futureOnlyOption").setAttribute("value", "1"))
        val loaded = XmlSerializer.deserialize(written, ArbLocalSettingsState.State::class.java)
        val component = ArbLocalSettingsState()
        component.loadState(loaded)
        assertEquals("/opt/arbd", component.customArbdBinaryPath)
        assertFalse(component.burnTapsOnExport, "burnTapsOnExport must survive")
        assertEquals("/shots", component.customScreenshotDir)
        assertFalse(component.screenshotInheritLogcat)
        assertFalse(component.trayEnabled, "a field the old build never stored falls back to its safe default (off)")

        // And the round trip within the same build keeps trayEnabled=true.
        val roundTripped = ArbLocalSettingsState()
        roundTripped.loadState(XmlSerializer.deserialize(XmlSerializer.serialize(saved), ArbLocalSettingsState.State::class.java))
        assertTrue(roundTripped.trayEnabled)
        assertFalse(roundTripped.burnTapsOnExport)
    }
}
