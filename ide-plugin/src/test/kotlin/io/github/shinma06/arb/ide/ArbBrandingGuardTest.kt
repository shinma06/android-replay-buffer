package io.github.shinma06.arb.ide

import io.github.shinma06.arb.model.BundleFormat
import io.github.shinma06.arb.model.BundleLifecycleState
import io.github.shinma06.arb.model.Provenance
import io.github.shinma06.arb.model.StreamsSummary
import io.github.shinma06.arb.model.TimelineSummaryResponse
import io.github.shinma06.arb.model.UnanchoredSummary
import io.github.shinma06.arb.state.BundleCatalogItem
import io.github.shinma06.arb.state.ProvenanceFormatter
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths

/**
 * Automated branding guard preventing product-version `"v2"` labels from re-entering user-facing
 * IDE plugin metadata, actions, dialogs, settings, notifications, or view-model copy.
 *
 * Internal wire/storage identifiers (such as `schema_version = 2`, `BundleFormat.V2`,
 * `CatalogFormatVersion.V2`, and `DetectedKind.V2_MANIFEST`) remain untouched for storage/IPC
 * compatibility.
 */
class ArbBrandingGuardTest {

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
    fun pluginXmlAndGradleBuildUsePureAndroidReplayBufferBranding() {
        val root = repoRoot()
        val pluginXml = Files.readString(root.resolve("ide-plugin/src/main/resources/META-INF/plugin.xml"))
        val buildGradle = Files.readString(root.resolve("ide-plugin/build.gradle.kts"))

        assertTrue(pluginXml.contains("<name>Android Replay Buffer</name>"))
        assertTrue(pluginXml.contains("""<notificationGroup id="Android Replay Buffer""""))
        assertTrue(pluginXml.contains("""text="Android Replay Buffer""""))
        assertTrue(pluginXml.contains("""text="Android Replay Buffer について""""))
        assertTrue(buildGradle.contains("""name = "Android Replay Buffer""""))

        for (forbidden in FORBIDDEN_BRANDING_PATTERNS) {
            assertFalse(
                forbidden.containsMatchIn(pluginXml),
                "plugin.xml must not contain forbidden product-v2 branding (${forbidden.pattern})",
            )
            assertFalse(
                forbidden.containsMatchIn(buildGradle),
                "ide-plugin/build.gradle.kts must not contain forbidden product-v2 branding (${forbidden.pattern})",
            )
        }
    }

    @Test
    fun allMainKotlinSourcesContainZeroProductV2Branding() {
        val root = repoRoot()
        val sourceRoots = listOf(
            root.resolve("ide-plugin/src/main"),
            root.resolve("ide-plugin/client-core/src/main"),
        )
        val violations = mutableListOf<String>()

        for (srcRoot in sourceRoots) {
            Files.walk(srcRoot).use { stream ->
                stream
                    .filter { Files.isRegularFile(it) }
                    .forEach { file ->
                        val rel = root.relativize(file).toString().replace('\\', '/')
                        val lines = Files.readAllLines(file)
                        lines.forEachIndexed { idx, line ->
                            for (pattern in FORBIDDEN_BRANDING_PATTERNS) {
                                if (pattern.containsMatchIn(line)) {
                                    violations += "$rel:${idx + 1}: matched /${pattern.pattern}/ -> ${line.trim()}"
                                }
                            }
                        }
                    }
            }
        }

        assertTrue(
            violations.isEmpty(),
            "Found forbidden user-facing 'v2' product branding:\n${violations.joinToString("\n")}",
        )
    }

    @Test
    fun runtimeUiLabelsAndFormattersExposeZeroProductV2Strings() {
        assertEquals("ARB バンドル", ArbBundleFileType.displayName)
        assertEquals("Android Replay Buffer バンドルマニフェスト", ArbBundleFileType.description)

        val item = BundleCatalogItem(
            bundleId = "bundle-20261124",
            path = "/tmp/bundle-20261124",
            isLegacyReadOnly = false,
            state = BundleLifecycleState.ACTIVE,
            activeLeases = 0,
            createdAtUtc = "2026-11-24T00:00:00Z",
            parentBundleId = null,
        )
        assertEquals("ARB", item.formatBadge)

        val provenance = ProvenanceFormatter.format(
            TimelineSummaryResponse(
                bundleId = "bundle-20261124",
                path = "/tmp/bundle-20261124",
                format = BundleFormat.V2,
                isLegacy = false,
                provenance = Provenance.Original,
                streams = StreamsSummary(),
                unanchoredSummary = UnanchoredSummary(0L, 0L),
                epochs = emptyList(),
                frameCount = 30,
                keyframeCount = 1,
                logMainCount = 0,
                logUnanchoredCount = 0,
                tapMainCount = 0,
                tapUnanchoredCount = 0,
                discontinuityCount = 0,
            ),
        )
        assertEquals("来歴: オリジナル保存 (WORM)", provenance.provenanceLine)

        val licenseReport = ArbAboutInfo.formatLicenseReport()
        val runtimeTexts = listOf(
            ArbBundleFileType.displayName,
            ArbBundleFileType.description,
            item.formatBadge,
            provenance.provenanceLine,
            licenseReport,
        )
        for (text in runtimeTexts) {
            for (pattern in FORBIDDEN_BRANDING_PATTERNS) {
                assertFalse(
                    pattern.containsMatchIn(text),
                    "Runtime UI text '$text' matched forbidden pattern /${pattern.pattern}/",
                )
            }
            assertFalse(
                Regex("""\bv2\b""", RegexOption.IGNORE_CASE).containsMatchIn(text),
                "Runtime UI text '$text' must not expose standalone 'v2'",
            )
        }
    }

    @Test
    fun docsDirectoryIsFlattenedWithNoDocsV2Subtree() {
        val root = repoRoot()
        assertFalse(Files.exists(root.resolve("docs/v2")), "docs/v2/ must no longer exist")
        assertTrue(Files.isRegularFile(root.resolve("docs/sara.toml")), "docs/sara.toml must exist")
        assertTrue(Files.isRegularFile(root.resolve("docs/model.yaml")), "docs/model.yaml must exist")
    }

    companion object {
        private val FORBIDDEN_BRANDING_PATTERNS: List<Regex> = listOf(
            Regex("""ARB\s+v2""", RegexOption.IGNORE_CASE),
            Regex("""\(v2\)""", RegexOption.IGNORE_CASE),
            Regex("""Android Replay Buffer\s+v2""", RegexOption.IGNORE_CASE),
            Regex("""v2\s*バンドル""", RegexOption.IGNORE_CASE),
            Regex("""v2\s+WORM""", RegexOption.IGNORE_CASE),
            Regex("""arb-v2-""", RegexOption.IGNORE_CASE),
        )
    }
}
