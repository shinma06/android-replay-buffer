package io.github.shinma06.arb.ide

import com.intellij.openapi.components.State
import com.intellij.util.xmlb.XmlSerializer
import io.github.shinma06.arb.model.ArbConfig
import org.jdom.Element
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertTrue

/**
 * G-M9 plugin-side upgrade regressions.
 *
 * - U2: v1 (`v0.2.0`) -> this plugin: the plugin id is unchanged (a different id would load both
 *   plugins side by side) and a project whose `.idea/workspace.xml` still carries the v1
 *   `ReplaySettingsStore` component starts safely with defaults even after the temporary
 *   [LegacyV1SettingsMigrator] has been deleted on `main` promotion.
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

    private val legacyWorkspaceXml = """
        <?xml version="1.0" encoding="UTF-8"?>
        <project version="4">
          <component name="io.github.shinma06.replaybuffer.settings.ReplaySettingsStore">
            <option name="enabled" value="true" />
            <option name="retentionSeconds" value="240" />
            <option name="captureMode" value="LOG_ONLY" />
            <option name="appSelection" value="MANUAL" />
            <option name="manualPackage" value="com.example.legacy" />
            <option name="showTouches" value="true" />
            <option name="tapDisplay" value="ON" />
          </component>
          <component name="SomeOtherComponent"><option name="x" value="y" /></component>
        </project>
    """.trimIndent()

    @Test
    fun pluginIdIsExactlyTheV1PluginIdSoAnUpdateReplacesInsteadOfLoadingTwice() {
        val pluginXml = Files.readString(repoRoot().resolve("ide-plugin/src/main/resources/META-INF/plugin.xml"))
        val ids = Regex("""<id>([^<]+)</id>""").findAll(pluginXml).map { it.groupValues[1].trim() }.toList()
        assertEquals(listOf("io.github.shinma06.android-replay-buffer"), ids, "plugin.xml must declare exactly the v1 plugin id")
        assertEquals("io.github.shinma06.android-replay-buffer", ArbdDaemonService.PLUGIN_ID)
        // The v1 settings component is keyed by its own name; this plugin's is a different, stable one.
        assertFalse(pluginXml.contains("ReplaySettingsStore"), "plugin.xml must not register the dropped v1 settings store")
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
            migratedFromV1 = true,
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
        assertTrue(component.migratedFromV1)

        // And the round trip within the same build keeps trayEnabled=true.
        val roundTripped = ArbLocalSettingsState()
        roundTripped.loadState(XmlSerializer.deserialize(XmlSerializer.serialize(saved), ArbLocalSettingsState.State::class.java))
        assertTrue(roundTripped.trayEnabled)
        assertFalse(roundTripped.burnTapsOnExport)
    }

    @Test
    fun legacyV1WorkspaceXmlIsHarmlessWhenTheMigratorIsDisabledAsAfterMainPromotion(@TempDir tmp: Path) {
        val idea = Files.createDirectories(tmp.resolve(".idea"))
        val workspace = idea.resolve("workspace.xml")
        Files.writeString(workspace, legacyWorkspaceXml)
        val before = Files.readAllBytes(workspace)

        // `migratedFromV1 = true` is exactly the state in which the migrator does nothing, which is what the
        // plugin behaves like once LegacyV1SettingsMigrator.kt is deleted: no read of workspace.xml, no RPC.
        val settings = ArbLocalSettingsState().also { it.migratedFromV1 = true }
        var rpcCalls = 0
        val result = LegacyV1SettingsMigrator.migrateOnceIfNeeded(tmp, settings) {
            rpcCalls += 1
            ArbConfig()
        }
        assertIs<LegacyV1SettingsMigrator.MigrationResult.AlreadyMigrated>(result)
        assertEquals(0, rpcCalls, "an already-handled project must never push v1 values into arbd")
        assertTrue(before.contentEquals(Files.readAllBytes(workspace)), "workspace.xml is never rewritten")

        // A fresh (default) settings object starts with safe defaults regardless of the leftover v1 component.
        val fresh = ArbLocalSettingsState()
        assertFalse(fresh.trayEnabled)
        assertTrue(fresh.burnTapsOnExport)
        assertTrue(fresh.screenshotInheritLogcat)
        assertEquals("", fresh.customArbdBinaryPath)
    }

    @Test
    fun noProductionCodeOutsideTheSingleMigratorFileReadsWorkspaceXmlOrTheV1Component() {
        val srcRoots = listOf("ide-plugin/src/main", "ide-plugin/client-core/src/main").map { repoRoot().resolve(it) }
        val offenders = mutableListOf<String>()
        for (srcRoot in srcRoots) {
            Files.walk(srcRoot).use { stream ->
                stream.filter { Files.isRegularFile(it) && (it.toString().endsWith(".kt") || it.toString().endsWith(".java")) }
                    .forEach { file ->
                        if (file.fileName.toString() == "LegacyV1SettingsMigrator.kt") return@forEach
                        val text = Files.readString(file)
                        if (text.contains("ReplaySettingsStore") || text.contains("workspace.xml")) {
                            offenders += repoRoot().relativize(file).toString()
                        }
                    }
            }
        }
        assertTrue(offenders.isEmpty(), "deleting LegacyV1SettingsMigrator.kt must be enough; other files read the v1 store: $offenders")
        // The only call site is the single startup line, so the promotion PR deletes one line + one file + one flag.
        val startup = Files.readString(repoRoot().resolve("ide-plugin/src/main/kotlin/io/github/shinma06/arb/ide/ArbStartupActivity.kt"))
        assertEquals(1, Regex("""LegacyV1SettingsMigrator\.migrateOnceIfNeeded""").findAll(startup).count())
    }
}
