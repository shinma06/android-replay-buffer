package io.github.shinma06.arb.ui

import com.intellij.openapi.util.IconLoader
import io.github.shinma06.arb.ide.ArbToolbarDaemonSnapshot
import io.github.shinma06.arb.ide.ArbToolbarPresentationState
import org.jetbrains.jewel.bridge.icon.fromPlatformIcon
import org.jetbrains.jewel.ui.icon.IntelliJIconKey
import org.junit.jupiter.api.Test
import java.nio.file.Files
import java.nio.file.Path
import javax.xml.parsers.DocumentBuilderFactory
import kotlin.test.assertContains
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertSame

class ArbIconsTest {
    private val main = Path.of(System.getProperty("arb.repo.root"), "ide-plugin/src/main")

    @Test
    fun platformIconSelectionIsConfinedToTheSemanticCatalog() {
        Files.walk(main).use { files ->
            files.filter { Files.isRegularFile(it) && it.fileName.toString().substringAfterLast('.') in setOf("kt", "java", "xml") }
                .filter { it.fileName.toString() != "ArbIcons.kt" }
                .forEach { file ->
                    assertFalse(Regex("""\bAllIcons(?:Keys)?\b""").containsMatchIn(Files.readString(file)), file.toString())
                }
        }
    }

    @Test
    fun swingAndJewelResolveTheSameAssetForBothUiGenerations() {
        for (icon in ArbIcons.entries) {
            val expected = IntelliJIconKey.fromPlatformIcon(icon.swing)
            for (newUi in listOf(false, true)) {
                assertEquals(expected.path(newUi), icon.key.path(newUi), icon.name)
            }
        }
        val cold = ArbToolbarDaemonSnapshot(probed = false)
        assertSame(ArbIcons.Save.swing, ArbToolbarPresentationState.quickSave(cold, null).icon)
        assertSame(ArbIcons.Screenshot.swing, ArbToolbarPresentationState.screenshot(cold).icon)
        assertSame(ArbIcons.StartCapture.swing, ArbToolbarPresentationState.toggleCapture(cold).icon)
    }

    @Test
    fun composeActionsUseTheSameMeaningAsTheToolbarAndMenus() {
        fun assertActions(file: String, vararg bindings: Pair<String, ArbIcons>) {
            val source = Files.readString(main.resolve("kotlin/io/github/shinma06/arb/ui/$file.kt"))
            for ((action, icon) in bindings) {
                val click = "onClick = { controller.$action() }"
                assertContains(source, click)
                val button = source.substringBefore(click).substringAfterLast("ArbSubtleAction(")
                assertContains(button, "ArbIcons.${icon.name}.key", message = "$file $action")
            }
        }
        assertActions(
            "ArbToolWindowContent", "captureScreenshot" to ArbIcons.Screenshot, "triggerSecondarySave" to ArbIcons.Save,
            "saveLogsOnly" to ArbIcons.SaveLogs, "toggleCapture" to ArbIcons.StartCapture,
            "toggleCapture" to ArbIcons.StopCapture, "launchTray" to ArbIcons.LaunchTray,
        )
        assertActions("ArbStudioContent", "requestPngExport" to ArbIcons.Screenshot, "revealBundleInFinder" to ArbIcons.OpenFolder)
    }

    @Test
    fun xmlIconsResolveThroughPlatformLoaderToTheirSemanticResources() {
        val expected = mapOf(
            "toolWindow" to ArbIcons.Panel,
            "ToggleCapture" to ArbIcons.StartCapture, "QuickSave" to ArbIcons.Save, "Save" to ArbIcons.Save,
            "Screenshot" to ArbIcons.Screenshot, "SaveLogs" to ArbIcons.SaveLogs,
            "LaunchTray" to ArbIcons.LaunchTray, "OpenBundleDir" to ArbIcons.OpenFolder, "About" to ArbIcons.About,
        )
        for (descriptor in listOf("plugin.xml", "arb-android.xml")) {
            val doc = DocumentBuilderFactory.newInstance().newDocumentBuilder()
                .parse(main.resolve("resources/META-INF/$descriptor").toFile())
            val nodes = doc.getElementsByTagName("*")
            for (i in 0 until nodes.length) {
                val node = nodes.item(i)
                val icon = node.attributes.getNamedItem("icon")?.nodeValue ?: continue
                val meaning = if (node.nodeName == "toolWindow") "toolWindow" else node.attributes.getNamedItem("id").nodeValue.substringAfterLast('.')
                assertSame(expected.getValue(meaning).swing, IconLoader.findIcon(icon, javaClass), icon)
            }
        }
    }
}
