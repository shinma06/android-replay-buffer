package io.github.shinma06.arb

import com.lemonappdev.konsist.api.Konsist
import com.lemonappdev.konsist.api.declaration.KoFileDeclaration
import org.junit.jupiter.api.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

/**
 * Layer-4 PDD (KON-1..KON-6): architecture rules over the plugin and `client-core` sources.
 * Each rule reports the offending files so a regression names its own fix site.
 */
class ArchitectureKonsistTest {
    private val scope = Konsist.scopeFromProject()

    private fun main(): List<KoFileDeclaration> =
        scope.files.filter { it.path.replace('\\', '/').contains("/src/main/kotlin/") }

    private fun KoFileDeclaration.rel(): String = path.replace('\\', '/').substringAfter("ide-plugin/")

    private fun KoFileDeclaration.code(): String =
        text.lineSequence().filterNot { it.trimStart().startsWith("//") || it.trimStart().startsWith("*") }
            .joinToString("\n")

    private fun assertNone(rule: String, offenders: List<String>) =
        assertTrue(offenders.isEmpty(), "$rule violated by:\n" + offenders.joinToString("\n"))

    @Test
    fun scopeSeesBothModules() {
        val files = main().map { it.rel() }
        assertTrue(files.any { it.startsWith("src/main/") }, "plugin sources in scope")
        assertTrue(files.any { it.startsWith("client-core/src/main/") }, "client-core sources in scope")
    }

    /** KON-1: UI never writes files, spawns processes or touches SQLite directly. */
    @Test
    fun kon1UiHasNoDirectPersistenceOrProcessControl() {
        val banned = Regex(
            """\bFiles\.(write|writeString|writeBytes|delete|deleteIfExists|move|copy|createFile|newOutputStream)\b|""" +
                """\bFileOutputStream\b|\bFileWriter\b|\bProcessBuilder\b|\bRuntime\.getRuntime\b|""" +
                """\bjava\.sql\b|sqlite""",
            RegexOption.IGNORE_CASE,
        )
        assertNone("KON-1", main().filter { "/arb/ui/" in it.path.replace('\\', '/') && banned.containsMatchIn(it.code()) }.map { it.rel() })
    }

    /** KON-2: actions update on BGT; views/updates never block; process ownership is app-scoped. */
    @Test
    fun kon2ActionsAreBgtAndProcessOwnershipIsApplicationScoped() {
        val actionDecl = Regex("""class\s+(\w+)\s*(\([^)]*\))?\s*:\s*\n?\s*(AnAction|DumbAwareAction|DumbAwareToggleAction|ToggleAction|ToggleOptionAction)\(""")
        val missingBgt = main().flatMap { f ->
            actionDecl.findAll(f.code()).map { it.groupValues[1] }.filter { name ->
                val body = f.code().substringAfter("class $name").substringBefore("\nclass ")
                !Regex("""getActionUpdateThread\(\)\s*(:\s*ActionUpdateThread)?\s*=\s*ActionUpdateThread\.BGT""").containsMatchIn(body)
            }.map { "${f.rel()}::$it" }.toList()
        }
        assertNone("KON-2 (BGT)", missingBgt)
        val actionCount = main().sumOf { actionDecl.findAll(it.code()).count() }
        assertTrue(actionCount >= 7, "expected the plugin's actions to be discovered, found $actionCount")

        val blocking = Regex("""\bArbdRpcClient\b|\bProcessBuilder\b|\.waitFor\(|\bFuture\b.*\.get\(""")
        assertNone("KON-2 (Compose views)", main().filter { it.name.endsWith("Content") && blocking.containsMatchIn(it.code()) }.map { it.rel() })
        val updateBlocks = main().flatMap { f ->
            Regex("""override fun update\(e: AnActionEvent\)\s*\{""").findAll(f.code()).mapNotNull { m ->
                val body = f.code().substring(m.range.last).substringBefore("\n    }")
                if (blocking.containsMatchIn(body)) f.rel() else null
            }.toList()
        }
        assertNone("KON-2 (AnAction.update)", updateBlocks)

        val owners = setOf("ArbdDaemonService", "ArbdProcessManager")
        assertNone(
            "KON-2 (process ownership)",
            main().filter { Regex("""\bProcessBuilder\(""").containsMatchIn(it.code()) && it.name !in owners }.map { it.rel() },
        )
        val daemonService = main().single { it.name == "ArbdDaemonService" }.code()
        assertTrue(Regex("""@Service\(\s*(Service\.)?Level\.APP\s*\)""").containsMatchIn(daemonService) || "@Service\n" in daemonService || "@Service()" in daemonService,
            "ArbdDaemonService must be an application-level service")
        assertTrue("Level.PROJECT" !in daemonService, "ArbdDaemonService must not be project-scoped")
    }

    /** KON-3: no Swing widgets regress into the Compose/Jewel UI. */
    @Test
    fun kon3NoSwingWidgets() {
        val widgets = Regex("""\b(JButton|JLabel|JTable|JTextField|JComboBox|JCheckBox|JDialog|JList|JScrollPane|JOptionPane|JBLabel|JBTable|JBList|JBScrollPane)\b|com\.intellij\.ui\.dsl""")
        assertNone("KON-3", main().filter { widgets.containsMatchIn(it.code()) }.map { it.rel() })
    }

    /** KON-4: `HostOs` is the single OS detection point. */
    @Test
    fun kon4HostOsIsTheSingleDetectionPoint() {
        val detect = Regex("""System\.getProperty\("os\.(name|arch)"\)|SystemInfo\.(isWindows|isMac|isLinux)""")
        assertNone("KON-4 (detection)", main().filter { it.name != "HostOs" && detect.containsMatchIn(it.code()) }.map { it.rel() })
        val branchAllowed = setOf("HostOs", "ArbPaths", "LocalIpcTransport", "Win32NamedPipes")
        val branch = Regex("""HostOs\.(WINDOWS|MACOS|LINUX)\b""")
        assertNone("KON-4 (branching)", main().filter { it.name !in branchAllowed && branch.containsMatchIn(it.code()) }.map { it.rel() })
    }

    /** KON-5: `client-core` is IDE/UI-free and the legacy v1 migrator stays one deletable file. */
    @Test
    fun kon5ClientCorePurityAndLegacyMigratorIsolation() {
        val clientCore = main().filter { it.rel().startsWith("client-core/") }
        assertTrue(clientCore.isNotEmpty())
        val impure = listOf("com.intellij", "org.jetbrains.jewel", "androidx.compose", "javax.swing")
        assertNone(
            "KON-5 (client-core purity)",
            clientCore.filter { f -> f.imports.any { i -> impure.any { i.name.startsWith(it) } } }.map { it.rel() },
        )
        val legacyUsers = scope.files
            .filter { it.name != "ArchitectureKonsistTest" }
            .filter { Regex("""\b(LegacyV1SettingsMigrator|ReplaySettingsStore)\b""").containsMatchIn(it.code()) }
            .map { it.name }
            .toSet()
        assertEquals(setOf("LegacyV1SettingsMigrator", "ArbStartupActivity", "LegacyV1SettingsMigratorTest"), legacyUsers)
    }

    /**
     * KON-6 (PDD G8-7): audio "unsupported" wording is produced only by `AudioPolicyTable`, which
     * shows it solely for `AudioCause.API_BELOW_MIN`; no other UI code may label audio unsupported.
     */
    @Test
    fun kon6AudioUnsupportedWordingLivesOnlyInAudioPolicyTable() {
        val wording = Regex("""音声(?:キャプチャ)?[のは：:\s]*非対応""")
        val table = main().single { it.rel() == "client-core/src/main/kotlin/io/github/shinma06/arb/state/AudioPolicyTable.kt" }
        // Skip comments before inspecting string literals, including multiline strings.
        val tokens = Regex("""//[^\n]*|/\*[\s\S]*?\*/|"{3}[\s\S]*?"{3}|"(?:\\.|[^"\\])*" """.trimEnd())
        fun hasWording(file: KoFileDeclaration) =
            tokens.findAll(file.text).any { it.value.startsWith('"') && wording.containsMatchIn(it.value) }
        assertNone("KON-6", main().filter { it.rel() != table.rel() && hasWording(it) }.map { it.rel() })
        assertTrue(hasWording(table), "AudioPolicyTable owns the unsupported wording")
        val classOf = table.objects().single { it.name == "AudioPolicyTable" }.functions().single { it.name == "classOf" }
        assertTrue(!Regex("""\belse\s*->""").containsMatchIn(classOf.text), "classOf handles every AudioCause explicitly")
    }
}
