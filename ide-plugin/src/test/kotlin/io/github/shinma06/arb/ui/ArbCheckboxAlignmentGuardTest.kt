package io.github.shinma06.arb.ui

import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths

/**
 * Automated guard for the systemic checkbox + label vertical alignment fix (Owner trial feedback
 * T-6H item 1): every checkbox paired with a text label must go through the single shared
 * [ArbLabeledCheckbox] primitive, never a raw Jewel `CheckboxRow(...)` or an ad-hoc
 * `Checkbox(...) + Text(...)` pairing, so the optical alignment correction lives in ONE place.
 */
class ArbCheckboxAlignmentGuardTest {

    private fun repoRoot(): Path {
        System.getProperty("arb.repo.root")?.let { raw ->
            val p = Paths.get(raw).toAbsolutePath().normalize()
            if (Files.isDirectory(p.resolve("core")) && Files.isDirectory(p.resolve("ide-plugin"))) {
                return p
            }
        }
        var cur = Paths.get("").toAbsolutePath().normalize()
        repeat(8) {
            if (Files.isDirectory(cur.resolve("core")) && Files.isDirectory(cur.resolve("ide-plugin"))) {
                return cur
            }
            cur = cur.parent ?: return cur
        }
        error("Could not locate repository root")
    }

    @Test
    fun zeroRawCheckboxRowOrAdHocCheckboxCallsOutsideArbLabeledCheckbox() {
        val root = repoRoot()
        val srcRoot = root.resolve("ide-plugin/src/main")
        assertTrue(Files.isDirectory(srcRoot))

        // ArbLabeledCheckbox itself legitimately calls Jewel's `Checkbox(` once internally; every
        // other file must have zero `CheckboxRow(` and zero raw `Checkbox(` calls.
        val definitionFile = srcRoot.resolve("kotlin/io/github/shinma06/arb/ui/ArbJewelTheme.kt")
        assertTrue(Files.isRegularFile(definitionFile), "ArbJewelTheme.kt must define ArbLabeledCheckbox")
        val definitionContent = Files.readString(definitionFile)
        assertTrue(
            definitionContent.contains("fun ArbLabeledCheckbox("),
            "ArbJewelTheme.kt must define the shared ArbLabeledCheckbox(...) composable",
        )

        val violations = mutableListOf<String>()
        val checkboxRowPattern = Regex("""CheckboxRow\s*\(""")
        val rawCheckboxPattern = Regex("""(?<!Labeled)(?<!ArbLabeled)\bCheckbox\s*\(""")

        Files.walk(srcRoot).use { stream ->
            stream
                .filter { Files.isRegularFile(it) && it.toString().endsWith(".kt") }
                .forEach { file ->
                    val rel = root.relativize(file).toString().replace('\\', '/')
                    val isDefinitionFile = file == definitionFile
                    Files.readAllLines(file).forEachIndexed { idx, rawLine ->
                        if (checkboxRowPattern.containsMatchIn(rawLine)) {
                            violations += "$rel:${idx + 1}: raw CheckboxRow( call -> ${rawLine.trim()}"
                        }
                        val isCheckboxCall = rawCheckboxPattern.containsMatchIn(rawLine) &&
                            !rawLine.contains("ArbLabeledCheckbox") &&
                            !rawLine.contains("fun ArbLabeledCheckbox")
                        if (isCheckboxCall && !(isDefinitionFile && rawLine.trim().startsWith("Checkbox("))) {
                            violations += "$rel:${idx + 1}: ad-hoc Checkbox( call -> ${rawLine.trim()}"
                        }
                    }
                }
        }

        assertTrue(
            violations.isEmpty(),
            "Found raw CheckboxRow(/Checkbox( usage outside ArbLabeledCheckbox:\n" +
                violations.joinToString("\n"),
        )
    }
}
