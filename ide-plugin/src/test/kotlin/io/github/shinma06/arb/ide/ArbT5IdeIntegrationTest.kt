package io.github.shinma06.arb.ide

import com.intellij.openapi.options.ConfigurationException
import io.github.shinma06.arb.model.ArbConfig
import io.github.shinma06.arb.model.IdeDefaultSaveAction
import io.github.shinma06.arb.model.OutputDirPatch
import io.github.shinma06.arb.model.SystemHelloResponse
import io.github.shinma06.arb.state.ArbFormatters
import io.github.shinma06.arb.state.ArbSettingsDraft
import io.github.shinma06.arb.ui.StudioExportFormat
import io.github.shinma06.arb.ui.TimelineViewerPanel
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.io.IOException
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue

class ArbT5IdeIntegrationTest {
    private fun repoRoot(): Path {
        val prop = System.getProperty("arb.repo.root")
        if (!prop.isNullOrBlank()) return Paths.get(prop).toAbsolutePath().normalize()
        return Paths.get("").toAbsolutePath().normalize().parent
    }

    @Test
    fun fileTypeDetectorRecognizesArbBundlesAndStrictlyIgnoresOrdinaryJson(@TempDir tmp: Path) {
        // 1. Valid ARB v2 bundle manifest.json with sibling timeline.sqlite
        val v2Dir = Files.createDirectories(tmp.resolve("bundle-v2-valid"))
        Files.writeString(v2Dir.resolve("timeline.sqlite"), "stub")
        val v2Manifest = Files.writeString(
            v2Dir.resolve("manifest.json"),
            """{"schema_version":2,"bundle_id":"b-1","provenance":{"kind":"original"},"streams":{"video":{"status":"present"}}}""",
        )
        assertEquals(ArbBundleDetector.DetectedKind.V2_MANIFEST, ArbBundleDetector.detectPath(v2Manifest))

        // 2. Real repository Legacy Schema-4 fixture replay.json
        val legacyReplay = repoRoot().resolve(
            "plugin/src/test/resources/replay-trim-parity/replay-20240101-000000-manual-feedface/replay.json",
        )
        assertTrue(Files.isRegularFile(legacyReplay), "Legacy Schema-4 fixture must exist")
        assertEquals(ArbBundleDetector.DetectedKind.LEGACY_SCHEMA4_REPLAY, ArbBundleDetector.detectPath(legacyReplay))

        // 3. Valid Legacy Schema-3 session.json with sibling video.mp4
        val s3Dir = Files.createDirectories(tmp.resolve("bundle-s3"))
        Files.writeString(s3Dir.resolve("video.mp4"), "stub")
        val s3Session = Files.writeString(
            s3Dir.resolve("session.json"),
            """{"schema":3,"capture_mode":"VIDEO_AND_LOGS","replay_seconds":60}""",
        )
        assertEquals(ArbBundleDetector.DetectedKind.LEGACY_SCHEMA3_SESSION, ArbBundleDetector.detectPath(s3Session))

        // 4. Strictly ignore ordinary package.json
        val nodeDir = Files.createDirectories(tmp.resolve("web-app"))
        val pkgJson = Files.writeString(nodeDir.resolve("package.json"), """{"name":"app","version":"1.0.0"}""")
        assertNull(ArbBundleDetector.detectPath(pkgJson), "package.json must never be hijacked")

        // 5. Strictly ignore Web App / Chrome Extension manifest.json (even with sibling files!)
        val pwaDir = Files.createDirectories(tmp.resolve("pwa-app"))
        Files.writeString(pwaDir.resolve("video.mp4"), "promo-video")
        val pwaManifest = Files.writeString(
            pwaDir.resolve("manifest.json"),
            """{"manifest_version":3,"name":"My Extension","short_name":"Ext","version":"1.0"}""",
        )
        assertNull(ArbBundleDetector.detectPath(pwaManifest), "PWA/Chrome manifest.json must never be hijacked")

        // 6. Strictly ignore standalone manifest.json without sibling ARB files
        val loneDir = Files.createDirectories(tmp.resolve("lone-json"))
        val loneManifest = Files.writeString(
            loneDir.resolve("manifest.json"),
            """{"schema_version":2,"bundle_id":"doc-example","streams":{}}""",
        )
        assertNull(ArbBundleDetector.detectPath(loneManifest), "manifest.json without sibling bundle files must be ignored")

        // 7. Strictly ignore unrelated session.json / replay.json
        val randomDir = Files.createDirectories(tmp.resolve("random-json"))
        Files.writeString(randomDir.resolve("video.mp4"), "unrelated")
        val randomSession = Files.writeString(randomDir.resolve("session.json"), """{"user":"alice","token":"xyz"}""")
        val randomReplay = Files.writeString(randomDir.resolve("replay.json"), """{"game":"chess","moves":["e4"]}""")
        assertNull(ArbBundleDetector.detectPath(randomSession))
        assertNull(ArbBundleDetector.detectPath(randomReplay))
    }

    @Test
    fun preCloseCheckClosesSilentlyWhenCleanAndRespectsConfirmationWhenDirty() {
        var askCount = 0
        // Untouched / already exported -> silent close without asking
        assertTrue(
            ArbBundlePreCloseCheck.evaluatePreClose(hasUnexportedTrim = false) {
                askCount++
                false
            },
        )
        assertEquals(0, askCount, "clean editor must never prompt on close")

        // Dirty + user confirms close -> returns true
        assertTrue(
            ArbBundlePreCloseCheck.evaluatePreClose(hasUnexportedTrim = true) {
                askCount++
                true
            },
        )
        assertEquals(1, askCount)

        // Dirty + user cancels -> returns false (keeps tab open)
        assertFalse(
            ArbBundlePreCloseCheck.evaluatePreClose(hasUnexportedTrim = true) {
                askCount++
                false
            },
        )
        assertEquals(2, askCount)
    }

    @Test
    fun settingsConfigurableRoundTripsDaemonConfigAndHandlesOfflineGracefully(@TempDir tmp: Path) {
        var storedConfig = ArbConfig()
        var storedCustomBinary = ""
        var lastAppliedPatch: io.github.shinma06.arb.model.ArbConfigPatch? = null

        val configurable = ArbSettingsConfigurable(
            project = null,
            daemonConfigReader = { storedConfig },
            daemonConfigWriter = { baseline, draft ->
                val patch = draft.toConfigPatch(baseline)
                lastAppliedPatch = patch
                storedConfig = storedConfig.copy(
                    saveWindowSecs = patch.saveWindowSecs ?: storedConfig.saveWindowSecs,
                    ringBudgetBytes = patch.ringBudgetBytes ?: storedConfig.ringBudgetBytes,
                    minFreeDiskBytes = patch.minFreeDiskBytes ?: storedConfig.minFreeDiskBytes,
                    videoMaxSize = patch.videoMaxSize ?: storedConfig.videoMaxSize,
                    videoMaxFps = patch.videoMaxFps ?: storedConfig.videoMaxFps,
                    audioEnabled = patch.audioEnabled ?: storedConfig.audioEnabled,
                    crashAutosaveEnabled = patch.crashAutosaveEnabled ?: storedConfig.crashAutosaveEnabled,
                    crashAutosaveCooldownSecs = patch.crashAutosaveCooldownSecs ?: storedConfig.crashAutosaveCooldownSecs,
                    outputDirOverride = when (val p = patch.outputDirOverride) {
                        OutputDirPatch.Unchanged -> storedConfig.outputDirOverride
                        OutputDirPatch.Clear -> null
                        is OutputDirPatch.Set -> p.path
                    },
                    ideDefaultSaveAction = patch.ideDefaultSaveAction ?: storedConfig.ideDefaultSaveAction,
                )
                storedConfig
            },
            localBinaryReader = { storedCustomBinary },
            localBinaryWriter = { storedCustomBinary = it },
        )

        configurable.createComponent()
        val panel = requireNotNull(configurable.currentPanelForTest())
        assertFalse(panel.isOfflineBannerVisible())
        assertFalse(configurable.isModified())

        val outDir = tmp.resolve("saved-bundles").toAbsolutePath().normalize().toString()
        val customArbd = tmp.resolve("custom-arbd").toAbsolutePath().normalize().toString()
        panel.loadFromDraft(
            ArbSettingsDraft(
                saveWindowSecs = 90,
                ringBudgetBytes = 2L * 1024L * 1024L * 1024L,
                minFreeDiskBytes = 4L * 1024L * 1024L * 1024L,
                videoMaxSize = 1024,
                videoMaxFps = 15,
                audioEnabled = false,
                crashAutosaveEnabled = false,
                crashAutosaveCooldownSecs = 45,
                outputDirOverride = outDir,
                ideDefaultSaveAction = IdeDefaultSaveAction.SAVE_ONLY,
                customArbdBinaryPath = customArbd,
            ),
        )
        assertTrue(configurable.isModified())
        configurable.apply()
        assertFalse(configurable.isModified())

        assertEquals(90, storedConfig.saveWindowSecs)
        assertEquals(1024, storedConfig.videoMaxSize)
        assertEquals(15, storedConfig.videoMaxFps)
        assertFalse(storedConfig.audioEnabled)
        assertFalse(storedConfig.crashAutosaveEnabled)
        assertEquals(45, storedConfig.crashAutosaveCooldownSecs)
        assertEquals(outDir, storedConfig.outputDirOverride)
        assertEquals(IdeDefaultSaveAction.SAVE_ONLY, storedConfig.ideDefaultSaveAction)
        assertEquals(customArbd, storedCustomBinary)
        assertEquals(OutputDirPatch.Set(outDir), lastAppliedPatch?.outputDirOverride)

        // Relative custom arbd binary path rejected cleanly
        panel.loadFromDraft(panel.currentDraft().copy(customArbdBinaryPath = "relative/arbd"))
        assertFailsWith<ConfigurationException> { configurable.apply() }
        configurable.disposeUIResources()

        // Offline fallback: daemon unreachable -> banner shown, local binary path still editable!
        var offlineLocalBin = ""
        val offlineConfigurable = ArbSettingsConfigurable(
            project = null,
            daemonConfigReader = { throw IOException("socket not found") },
            daemonConfigWriter = { _, _ -> error("must not call daemonConfigWriter when offline") },
            localBinaryReader = { offlineLocalBin },
            localBinaryWriter = { offlineLocalBin = it },
        )
        offlineConfigurable.createComponent()
        val offlinePanel = requireNotNull(offlineConfigurable.currentPanelForTest())
        assertTrue(offlinePanel.isOfflineBannerVisible())
        assertFalse(ArbFormatters.containsEmoji(offlinePanel.offlineBannerText()))

        offlinePanel.loadFromDraft(offlinePanel.currentDraft().copy(customArbdBinaryPath = customArbd))
        assertTrue(offlineConfigurable.isModified())
        offlineConfigurable.apply()
        assertEquals(customArbd, offlineLocalBin)
        offlineConfigurable.disposeUIResources()
    }

    @Test
    fun aboutPanelDisplaysVersionsAndZeroGplOssLicenseAttribution() {
        val onlineInfo = ArbAboutInfo.fromDaemonService(
            pluginVersion = "2.0.0-SNAPSHOT",
            hello = SystemHelloResponse(
                protocolVersion = 1,
                schemaVersion = 2,
                pid = 4242L,
                dataDir = "/tmp/d",
                cacheDir = "/tmp/c",
                runtimeDir = "/tmp/r",
                daemonVersion = "0.2.0",
            ),
            connected = true,
        )
        val panel = ArbAboutPanel(onlineInfo)
        assertTrue(onlineInfo.daemonStatusSummary.contains("0.2.0"))
        assertTrue(onlineInfo.daemonStatusSummary.contains("4242"))
        val licenseText = panel.licenseTextArea.text
        assertTrue(licenseText.contains("MIT"))
        assertTrue(licenseText.contains("Apache-2.0"))
        assertTrue(licenseText.contains("BSD-2-Clause"))
        assertTrue(licenseText.contains("GPL / AGPL 依存: 0 件"))
        assertFalse(ArbFormatters.containsEmoji(licenseText))
        assertFalse(ArbFormatters.containsEmoji(onlineInfo.daemonStatusSummary))

        val offlineInfo = ArbAboutInfo.fromDaemonService("2.0.0-SNAPSHOT", hello = null, connected = false)
        assertTrue(offlineInfo.daemonStatusSummary.contains("未接続"))
        assertFalse(ArbFormatters.containsEmoji(offlineInfo.daemonStatusSummary))
    }

    @Test
    fun pluginXmlRegistersAllNativeIntegrationPointsWithZeroEmoji() {
        val pluginXmlPath = repoRoot().resolve("ide-plugin/src/main/resources/META-INF/plugin.xml")
        assertTrue(Files.isRegularFile(pluginXmlPath))
        val xml = Files.readString(pluginXmlPath)

        assertFalse(ArbFormatters.containsEmoji(xml), "plugin.xml must contain zero emoji")
        assertTrue(xml.contains("""group-id="StreamingToolbarSecondary""""))
        assertTrue(xml.contains("io.github.shinma06.arb.RunningDevices.ToggleCapture"))
        assertTrue(xml.contains("io.github.shinma06.arb.RunningDevices.Save"))
        assertTrue(xml.contains("io.github.shinma06.arb.RunningDevices.Screenshot"))
        assertTrue(xml.contains("""group-id="Logcat.PopupActions""""))
        assertTrue(xml.contains("io.github.shinma06.arb.Logcat.SaveLogs"))
        assertTrue(xml.contains("io.github.shinma06.arb.ide.ArbLogcatTitleActions"))
        assertTrue(xml.contains("""id="io.github.shinma06.arb.settings""""))
        assertTrue(xml.contains("""parentId="tools""""))
        assertTrue(xml.contains("io.github.shinma06.arb.ide.ArbBundleFileTypeDetector"))
        assertTrue(xml.contains("io.github.shinma06.arb.ide.ArbBundlePreCloseCheck"))
        assertTrue(xml.contains("Android Replay Buffer (v2) について"))

        // Export format selector defaults to SHARE_MP4 and has zero emoji
        val viewer = TimelineViewerPanel(
            onScrub = {},
            onSeekExact = {},
            onTrimExport = { _, _ -> },
            onPngExport = { _, _ -> },
        )
        assertEquals(StudioExportFormat.SHARE_MP4, viewer.selectedExportFormat())
        viewer.selectExportFormat(StudioExportFormat.WORM_BUNDLE_ONLY)
        assertEquals(StudioExportFormat.WORM_BUNDLE_ONLY, viewer.selectedExportFormat())
        for (fmt in StudioExportFormat.entries) {
            assertFalse(ArbFormatters.containsEmoji(fmt.label))
        }
    }
}
