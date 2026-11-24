package io.github.shinma06.arb.ide

import com.intellij.openapi.diagnostic.Logger
import com.intellij.openapi.project.Project
import io.github.shinma06.arb.model.ArbConfig
import io.github.shinma06.arb.model.ArbConfigPatch
import io.github.shinma06.arb.model.ArbdRpcException
import io.github.shinma06.arb.model.CaptureMode
import io.github.shinma06.arb.model.OutputDirPatch
import io.github.shinma06.arb.model.StringPatch
import org.w3c.dom.Element
import org.xml.sax.InputSource
import java.io.StringReader
import java.nio.charset.StandardCharsets
import java.nio.file.Files
import java.nio.file.Path
import javax.xml.XMLConstants
import javax.xml.parsers.DocumentBuilderFactory

/**
 * TEMPORARY ONE-TIME LEGACY v1 SETTINGS MIGRATOR — SCHEDULED FOR CLEAN DELETION UPON `main` PROMOTION.
 *
 * Strictly isolated in this single file (plus one startup call site in [ArbStartupActivity] and
 * its dedicated unit test `LegacyV1SettingsMigratorTest`) so removing it when promoting to `main`
 * requires deleting this file, its test, the single call-site line, and `migratedFromV1`.
 *
 * Behavior:
 * - Runs at most once while [ArbLocalSettingsState.migratedFromV1] is `false`.
 * - Strictly read-only on `<projectBasePath>/.idea/workspace.xml`, parsed with an XXE-disabled
 *   [DocumentBuilderFactory].
 * - Reads `<component name="io.github.shinma06.replaybuffer.settings.ReplaySettingsStore">` if
 *   present, clamps `retentionSeconds` into `5..600`, validates absolute paths and serials safely,
 *   and ignores dropped legacy fields (`appSelection`, `manualPackage`, `showTouches`, `tapDisplay`).
 * - Applies valid daemon settings via `config.update`, updates IDE-local screenshot inheritance
 *   settings, and flips `migratedFromV1 = true` so subsequent launches never overwrite user edits.
 */
object LegacyV1SettingsMigrator {
    private val logger = Logger.getInstance(LegacyV1SettingsMigrator::class.java)

    internal const val LEGACY_COMPONENT_NAME: String =
        "io.github.shinma06.replaybuffer.settings.ReplaySettingsStore"

    private val SAFE_SERIAL_REGEX = Regex("^[A-Za-z0-9._:-]{1,256}$")
    private const val MAX_WORKSPACE_XML_BYTES: Long = 16L * 1024L * 1024L

    sealed interface MigrationResult {
        data object AlreadyMigrated : MigrationResult
        data object NoLegacySettingsFound : MigrationResult
        data class MalformedXml(val reason: String) : MigrationResult
        data class DaemonUnavailable(val reason: String) : MigrationResult
        data class Migrated(
            val appliedDaemonPatch: ArbConfigPatch?,
            val screenshotInheritLogcat: Boolean?,
            val customScreenshotDir: String?,
        ) : MigrationResult
    }

    internal data class ParsedLegacySettings(
        val daemonPatch: ArbConfigPatch?,
        val screenshotInheritLogcat: Boolean?,
        val customScreenshotDir: String?,
    )

    /** Production entry point invoked once on project open off the EDT. */
    fun migrateOnceIfNeeded(project: Project): MigrationResult {
        val localSettings = ArbLocalSettingsState.getInstanceOrNull() ?: return MigrationResult.NoLegacySettingsFound
        if (localSettings.migratedFromV1) return MigrationResult.AlreadyMigrated
        val basePath = project.basePath?.takeIf { it.isNotBlank() }?.let { raw ->
            runCatching { Path.of(raw) }.getOrNull()
        } ?: return MigrationResult.NoLegacySettingsFound
        val daemonService = runCatching { ArbdDaemonService.getInstance() }.getOrNull()
            ?: return MigrationResult.DaemonUnavailable("ArbdDaemonService unavailable")
        return migrateOnceIfNeeded(
            projectBasePath = basePath,
            localSettings = localSettings,
            configUpdater = { patch ->
                val updated = daemonService.withClient { it.configUpdate(patch).config }
                daemonService.notifyConfigUpdated(updated)
                updated
            },
        )
    }

    /** Pure, headless-testable migration entry point. Never modifies `workspace.xml`. */
    internal fun migrateOnceIfNeeded(
        projectBasePath: Path,
        localSettings: ArbLocalSettingsState,
        configUpdater: (ArbConfigPatch) -> ArbConfig,
    ): MigrationResult {
        if (localSettings.migratedFromV1) {
            return MigrationResult.AlreadyMigrated
        }
        val workspaceXml = projectBasePath.resolve(".idea").resolve("workspace.xml")
        if (!Files.isRegularFile(workspaceXml)) {
            return MigrationResult.NoLegacySettingsFound
        }
        val xmlBytes = runCatching {
            val size = Files.size(workspaceXml)
            if (size <= 0L || size > MAX_WORKSPACE_XML_BYTES) return MigrationResult.NoLegacySettingsFound
            Files.readAllBytes(workspaceXml)
        }.getOrElse { err ->
            logger.debug("ARB legacy settings read skipped: ${err.message}")
            return MigrationResult.MalformedXml(err.message ?: "read error")
        }
        val xmlText = String(xmlBytes, StandardCharsets.UTF_8)
        // Fast path: skip DOM parsing when the legacy component name does not appear at all.
        if (!xmlText.contains(LEGACY_COMPONENT_NAME)) {
            return MigrationResult.NoLegacySettingsFound
        }

        val parsed = when (val parseOutcome = parseLegacyWorkspaceXml(xmlText)) {
            is ParseOutcome.Found -> parseOutcome.settings
            ParseOutcome.NotFound -> return MigrationResult.NoLegacySettingsFound
            is ParseOutcome.Malformed -> {
                logger.warn("ARB legacy settings XML malformed; skipping migration: ${parseOutcome.reason}")
                return MigrationResult.MalformedXml(parseOutcome.reason)
            }
        }

        var patch = parsed.daemonPatch
        if (patch != null) {
            try {
                configUpdater(patch)
            } catch (rejected: ArbdRpcException) {
                // An arbd that predates always-on capture rejects the whole patch because of the
                // unknown `capture_enabled` field: migrate everything else rather than nothing.
                if (patch.captureEnabled == null) {
                    logger.warn("ARB legacy settings rejected by arbd: ${rejected.message}")
                    return MigrationResult.DaemonUnavailable(rejected.message)
                }
                val withoutCaptureEnabled = patch.copy(captureEnabled = null)
                try {
                    configUpdater(withoutCaptureEnabled)
                } catch (t: Throwable) {
                    logger.warn("ARB legacy settings daemon migration deferred: ${t.message}")
                    return MigrationResult.DaemonUnavailable(t.message ?: "daemon error")
                }
                patch = withoutCaptureEnabled
            } catch (t: Throwable) {
                logger.warn("ARB legacy settings daemon migration deferred (arbd unavailable): ${t.message}")
                return MigrationResult.DaemonUnavailable(t.message ?: "daemon error")
            }
        }

        parsed.screenshotInheritLogcat?.let { localSettings.screenshotInheritLogcat = it }
        parsed.customScreenshotDir?.let { localSettings.customScreenshotDir = it }
        localSettings.migratedFromV1 = true

        return MigrationResult.Migrated(
            appliedDaemonPatch = patch,
            screenshotInheritLogcat = parsed.screenshotInheritLogcat,
            customScreenshotDir = parsed.customScreenshotDir,
        )
    }

    private sealed interface ParseOutcome {
        data class Found(val settings: ParsedLegacySettings) : ParseOutcome
        data object NotFound : ParseOutcome
        data class Malformed(val reason: String) : ParseOutcome
    }

    private fun parseLegacyWorkspaceXml(xmlText: String): ParseOutcome {
        val doc = try {
            val factory = DocumentBuilderFactory.newInstance().apply {
                isNamespaceAware = false
                setFeature(XMLConstants.FEATURE_SECURE_PROCESSING, true)
                setFeature("http://apache.org/xml/features/disallow-doctype-decl", true)
                setFeature("http://xml.org/sax/features/external-general-entities", false)
                setFeature("http://xml.org/sax/features/external-parameter-entities", false)
                setFeature("http://apache.org/xml/features/nonvalidating/load-external-dtd", false)
                isXIncludeAware = false
                isExpandEntityReferences = false
            }
            factory.newDocumentBuilder().parse(InputSource(StringReader(xmlText)))
        } catch (t: Throwable) {
            return ParseOutcome.Malformed(t.message ?: t.javaClass.simpleName)
        }

        val components = doc.getElementsByTagName("component")
        var legacyComponent: Element? = null
        for (i in 0 until components.length) {
            val elem = components.item(i) as? Element ?: continue
            if (elem.getAttribute("name") == LEGACY_COMPONENT_NAME) {
                legacyComponent = elem
                break
            }
        }
        if (legacyComponent == null) {
            return ParseOutcome.NotFound
        }

        val options = linkedMapOf<String, String>()
        // Support both IntelliJ <option name="..." value="..." /> children and direct attributes.
        val attrs = legacyComponent.attributes
        for (i in 0 until attrs.length) {
            val attr = attrs.item(i)
            val key = attr.nodeName
            if (key != "name") {
                options[key] = attr.nodeValue
            }
        }
        val childNodes = legacyComponent.childNodes
        for (i in 0 until childNodes.length) {
            val child = childNodes.item(i) as? Element ?: continue
            if (child.tagName == "option") {
                val key = child.getAttribute("name")
                if (key.isNotBlank() && child.hasAttribute("value")) {
                    options[key] = child.getAttribute("value")
                }
            }
        }

        // Explicitly ignored dropped v1 fields: appSelection, manualPackage, showTouches, tapDisplay.
        // IntelliJ's XmlSerializer omits options equal to their defaults, so an option missing from a
        // present component means the v1 default (ReplaySettingsState), not "leave arbd unchanged".
        val saveWindowSecs = (options["retentionSeconds"]?.trim()?.toIntOrNull() ?: V1_DEFAULT_RETENTION_SECS)
            .coerceIn(ArbConfig.MIN_SAVE_WINDOW_SECS, ArbConfig.MAX_SAVE_WINDOW_SECS)
        val outputDirPatch = options["destination"]?.let(::normalizeValidAbsolutePath)
            ?.let { OutputDirPatch.Set(it) }
            ?: OutputDirPatch.Unchanged
        val captureMode = when (options["captureMode"]?.trim()?.uppercase()) {
            null, "VIDEO_AND_LOGS" -> CaptureMode.VIDEO_AND_LOGS
            "LOG_ONLY", "LOGS_ONLY" -> CaptureMode.LOGS_ONLY
            else -> null
        }
        val (videoMaxSize, videoMaxFps) = parseV1VideoQuality(options["videoQuality"])
        val audioEnabled = parseStrictBoolean(options["recordAudio"], default = false)
        val crashAutosaveEnabled = parseStrictBoolean(options["autoSaveOnCrash"], default = false)
        val captureEnabled = parseStrictBoolean(options["enabled"], default = false)
        val deviceSerialPatch = options["deviceSerial"]?.trim()
            ?.takeIf { it.isNotEmpty() && SAFE_SERIAL_REGEX.matches(it) }
            ?.let { StringPatch.Set(it) }
            ?: StringPatch.Unchanged

        val screenshotInherit = parseStrictBoolean(
            options["screenshotUsesCommonDestination"] ?: options["screenshotUsesLogcatDestination"],
            default = true,
        )
        val customScreenshotDir = options["screenshotDirectory"]?.let(::normalizeValidAbsolutePath)

        val candidatePatch = ArbConfigPatch(
            saveWindowSecs = saveWindowSecs,
            videoMaxSize = videoMaxSize,
            videoMaxFps = videoMaxFps,
            audioEnabled = audioEnabled,
            captureMode = captureMode,
            deviceSerial = deviceSerialPatch,
            crashAutosaveEnabled = crashAutosaveEnabled,
            outputDirOverride = outputDirPatch,
            captureEnabled = captureEnabled,
        )
        return ParseOutcome.Found(
            ParsedLegacySettings(
                daemonPatch = candidatePatch,
                screenshotInheritLogcat = screenshotInherit,
                customScreenshotDir = customScreenshotDir,
            ),
        )
    }

    /** `null` (absent) yields the v1 [default]; an unparseable value yields `null` (leave unchanged). */
    private fun parseStrictBoolean(raw: String?, default: Boolean): Boolean? = when (raw?.trim()?.lowercase()) {
        null -> default
        "true" -> true
        "false" -> false
        else -> null
    }

    /**
     * v1 `VideoQuality` overrides `toString()`, which IntelliJ's XmlSerializer writes, e.g.
     * `軽量（1024px / 15fps / 2Mbps）`; enum names are accepted too. Absent means v1 `STANDARD`.
     */
    internal fun parseV1VideoQuality(raw: String?): Pair<Int?, Int?> {
        val value = raw?.trim() ?: return 1920 to 30
        return when {
            value.equals("STANDARD", ignoreCase = true) || value.startsWith("標準") -> 1920 to 30
            value.equals("LIGHTWEIGHT", ignoreCase = true) || value.startsWith("軽量") -> 1024 to 15
            else -> null to null
        }
    }

    /** v1 `ReplaySettingsState.retentionSeconds` default. */
    private const val V1_DEFAULT_RETENTION_SECS: Int = 180

    private fun normalizeValidAbsolutePath(raw: String): String? {
        val trimmed = raw.trim()
        if (trimmed.isEmpty() || trimmed.indexOf('\u0000') >= 0) return null
        val path = runCatching { Path.of(trimmed) }.getOrNull() ?: return null
        if (!path.isAbsolute) return null
        return path.normalize().toString()
    }
}
