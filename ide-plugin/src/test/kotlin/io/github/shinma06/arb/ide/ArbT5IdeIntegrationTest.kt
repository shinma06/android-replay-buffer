package io.github.shinma06.arb.ide

import com.google.gson.JsonParser
import com.intellij.openapi.fileTypes.PlainTextFileType
import com.intellij.openapi.options.ConfigurationException
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.io.ByteArraySequence
import com.intellij.testFramework.LightVirtualFile
import io.github.shinma06.arb.ipc.HostOs
import io.github.shinma06.arb.model.AdbDeviceInfo
import io.github.shinma06.arb.model.AdbDeviceState
import io.github.shinma06.arb.model.ArbConfig
import io.github.shinma06.arb.model.ArbConfigPatch
import io.github.shinma06.arb.model.ArbdRpcException
import io.github.shinma06.arb.model.IdeDefaultSaveAction
import io.github.shinma06.arb.model.OutputDirPatch
import io.github.shinma06.arb.model.SystemHelloResponse
import io.github.shinma06.arb.state.ArbFormatters
import io.github.shinma06.arb.state.ArbSettingsDraft
import io.github.shinma06.arb.state.CaptureBarState
import io.github.shinma06.arb.ui.ArbStudioController
import io.github.shinma06.arb.ui.ArbToolWindowController
import io.github.shinma06.arb.ui.StudioExportFormat
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.io.IOException
import java.lang.reflect.Proxy
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

class ArbT5IdeIntegrationTest {
    @Test
    fun studioTabIdentityAndTypeLeaveFlutterTranslationsAlone(@TempDir dir: Path) {
        val file = ArbBundleVirtualFile("id", "収録.arb")
        val legacy = ArbBundleVirtualFile("id", "旧形式", isLegacyHint = true)
        assertEquals("収録.arb", file.presentableName)
        assertEquals("旧形式 [Legacy (RO)]", legacy.presentableName)
        assertEquals(file, legacy)
        assertEquals(file.hashCode(), legacy.hashCode())
        assertEquals("arbstudio", file.extension)
        // Platform FileTypeManager reads the assigned type. The headless registry returns a dummy.
        assertEquals(ArbBundleFileType, file.assignedFileType)
        val project = Proxy.newProxyInstance(
            javaClass.classLoader, arrayOf(Project::class.java),
        ) { _, _, _ -> error("accept must only inspect the file") } as Project
        val json = """{"@@locale":"en","hello":"Hello"}"""
        val flutter = LightVirtualFile("app_en.arb", PlainTextFileType.INSTANCE, json)
        assertNull(ArbBundleDetector.detectPath(Files.writeString(dir.resolve("app_en.arb"), json)))
        val bytes = ByteArraySequence(json.toByteArray())
        assertTrue(ArbBundleFileEditorProvider().accept(project, file))
        assertFalse(ArbBundleFileEditorProvider().accept(project, flutter))
        assertNull(ArbBundleFileTypeDetector().detect(flutter, bytes, json))
    }

    private fun repoRoot(): Path {
        val prop = System.getProperty("arb.repo.root")
        if (!prop.isNullOrBlank()) return Paths.get(prop).toAbsolutePath().normalize()
        return Paths.get("").toAbsolutePath().normalize().parent
    }

    @Test
    fun fileTypeDetectorRecognizesArbBundlesAndStrictlyIgnoresOrdinaryJson(@TempDir tmp: Path) {
        val v2Dir = Files.createDirectories(tmp.resolve("bundle-v2-valid"))
        Files.writeString(v2Dir.resolve("timeline.sqlite"), "stub")
        val v2Manifest = Files.writeString(
            v2Dir.resolve("manifest.json"),
            """{"schema_version":2,"bundle_id":"b-1","provenance":{"kind":"original"},"streams":{"video":{"status":"present"}}}""",
        )
        assertEquals(ArbBundleDetector.DetectedKind.V2_MANIFEST, ArbBundleDetector.detectPath(v2Manifest))

        val legacyReplay = repoRoot().resolve(
            "fixtures/legacy-schema4/replay-trim-parity/replay-20240101-000000-manual-feedface/replay.json",
        )
        assertTrue(Files.isRegularFile(legacyReplay), "Legacy Schema-4 fixture must exist")
        assertEquals(ArbBundleDetector.DetectedKind.LEGACY_SCHEMA4_REPLAY, ArbBundleDetector.detectPath(legacyReplay))

        val s3Dir = Files.createDirectories(tmp.resolve("bundle-s3"))
        Files.writeString(s3Dir.resolve("video.mp4"), "stub")
        val s3Session = Files.writeString(
            s3Dir.resolve("session.json"),
            """{"schema":3,"capture_mode":"VIDEO_AND_LOGS","replay_seconds":60}""",
        )
        assertEquals(ArbBundleDetector.DetectedKind.LEGACY_SCHEMA3_SESSION, ArbBundleDetector.detectPath(s3Session))

        val nodeDir = Files.createDirectories(tmp.resolve("web-app"))
        val pkgJson = Files.writeString(nodeDir.resolve("package.json"), """{"name":"app","version":"1.0.0"}""")
        assertNull(ArbBundleDetector.detectPath(pkgJson), "package.json must never be hijacked")

        val pwaDir = Files.createDirectories(tmp.resolve("pwa-app"))
        Files.writeString(pwaDir.resolve("video.mp4"), "promo-video")
        val pwaManifest = Files.writeString(
            pwaDir.resolve("manifest.json"),
            """{"manifest_version":3,"name":"My Extension","short_name":"Ext","version":"1.0"}""",
        )
        assertNull(ArbBundleDetector.detectPath(pwaManifest), "PWA/Chrome manifest.json must never be hijacked")

        val loneDir = Files.createDirectories(tmp.resolve("lone-json"))
        val loneManifest = Files.writeString(
            loneDir.resolve("manifest.json"),
            """{"schema_version":2,"bundle_id":"doc-example","streams":{}}""",
        )
        assertNull(ArbBundleDetector.detectPath(loneManifest), "manifest.json without sibling bundle files must be ignored")

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
        assertTrue(
            ArbBundlePreCloseCheck.evaluatePreClose(hasUnexportedTrim = false) {
                askCount++
                false
            },
        )
        assertEquals(0, askCount, "clean editor must never prompt on close")

        assertTrue(
            ArbBundlePreCloseCheck.evaluatePreClose(hasUnexportedTrim = true) {
                askCount++
                true
            },
        )
        assertEquals(1, askCount)

        assertFalse(
            ArbBundlePreCloseCheck.evaluatePreClose(hasUnexportedTrim = true) {
                askCount++
                false
            },
        )
        assertEquals(2, askCount)
    }

    @Test
    fun settingsConfigurableRoundTripsDaemonConfigScreenshotInheritanceAndCliMcpCopy(@TempDir tmp: Path) {
        var storedConfig = ArbConfig()
        var storedCustomBinary = ""
        var storedScreenshotInherit = true
        var storedCustomScreenshotDir = ""
        var lastAppliedPatch: ArbConfigPatch? = null
        val copiedTexts = mutableListOf<String>()

        val fakeCliPath = tmp.resolve("native/macos-arm64/arb-cli").toAbsolutePath().normalize().toString()
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
            localScreenshotReader = { storedScreenshotInherit to storedCustomScreenshotDir },
            localScreenshotWriter = { inherit, dir ->
                storedScreenshotInherit = inherit
                storedCustomScreenshotDir = dir
            },
            cliPathProvider = { fakeCliPath },
            clipboardWriter = { copiedTexts.add(it) },
        )

        val form = configurable.createFormModel()
        assertFalse(form.isBannerVisible)
        assertFalse(configurable.isModified())
        assertTrue(form.screenshotInheritLogcat, "Logcat screenshot inheritance must default to true")

        // 1-Click CLI / MCP copy verification
        assertTrue(configurable.copyCliPath(form))
        assertEquals(fakeCliPath, copiedTexts.last())
        assertTrue(form.copyFeedback.contains("CLI"))
        assertFalse(ArbFormatters.containsEmoji(form.copyFeedback))

        assertTrue(configurable.copyMcpJson(form))
        val parsedMcp = JsonParser.parseString(copiedTexts.last()).asJsonObject
        assertEquals(fakeCliPath, parsedMcp.get("command").asString)
        assertEquals("mcp", parsedMcp.getAsJsonArray("args").get(0).asString)
        assertTrue(form.copyFeedback.contains("MCP"))
        assertFalse(ArbFormatters.containsEmoji(form.copyFeedback))

        // Full config + local screenshot override round-trip
        val outDir = tmp.resolve("saved-bundles").toAbsolutePath().normalize().toString()
        val customArbd = tmp.resolve("custom-arbd").toAbsolutePath().normalize().toString()
        val customShotDir = tmp.resolve("screenshots").toAbsolutePath().normalize().toString()
        form.loadFromDraft(
            draft = ArbSettingsDraft(
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
            screenshotInheritLogcat = false,
            customScreenshotDir = customShotDir,
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
        assertFalse(storedScreenshotInherit)
        assertEquals(customShotDir, storedCustomScreenshotDir)
        assertEquals(OutputDirPatch.Set(outDir), lastAppliedPatch?.outputDirOverride)

        // Relative custom arbd binary path rejected cleanly
        form.loadFromDraft(form.currentDraft().copy(customArbdBinaryPath = "relative/arbd"))
        assertFailsWith<ConfigurationException> { configurable.apply() }

        // Relative custom screenshot folder rejected cleanly when inheritance is unchecked
        form.loadFromDraft(
            draft = form.currentDraft().copy(customArbdBinaryPath = customArbd),
            screenshotInheritLogcat = false,
            customScreenshotDir = "relative/shots",
        )
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
            cliPathProvider = { fakeCliPath },
        )
        val offlineForm = offlineConfigurable.createFormModel()
        assertTrue(offlineForm.isBannerVisible)
        assertFalse(offlineForm.isDaemonControlsEnabled)
        assertFalse(ArbFormatters.containsEmoji(offlineForm.daemonBannerMessage))

        offlineForm.loadFromDraft(offlineForm.currentDraft().copy(customArbdBinaryPath = customArbd))
        assertTrue(offlineConfigurable.isModified())
        offlineConfigurable.apply()
        assertEquals(customArbd, offlineLocalBin)
        offlineConfigurable.disposeUIResources()
    }

    @Test
    fun screenshotDestinationResolverInheritsLogcatAndFallsBackSafely(@TempDir tmp: Path) {
        val logcatDir = tmp.resolve("studio-desktop").toAbsolutePath().normalize()
        val customDir = tmp.resolve("custom-shots").toAbsolutePath().normalize()

        // 1. Inherit Logcat -> uses resolved Studio directory
        val inherited = ArbScreenshotDestinationResolver.resolveDirectory(
            project = null,
            inheritLogcat = true,
            customScreenshotDir = "",
            logcatLocationProvider = { "\$DESKTOP_DIR$" },
            logcatDirectoryResolver = { _, _ -> logcatDir },
        )
        assertTrue(inherited.inheritedFromLogcat)
        assertEquals(logcatDir, inherited.directory)
        assertFalse(ArbFormatters.containsEmoji(inherited.statusText))

        // 2. Inherit Logcat when Studio ABI unavailable -> clean fallback to null (arbd output_dir)
        val fallback = ArbScreenshotDestinationResolver.resolveDirectory(
            project = null,
            inheritLogcat = true,
            customScreenshotDir = "",
            logcatLocationProvider = { null },
        )
        assertFalse(fallback.inheritedFromLogcat)
        assertNull(fallback.directory)
        assertTrue(fallback.statusText.contains("output_dir"))

        // 3. Custom folder override when inheritance unchecked
        val overridden = ArbScreenshotDestinationResolver.resolveDirectory(
            project = null,
            inheritLogcat = false,
            customScreenshotDir = customDir.toString(),
        )
        assertFalse(overridden.inheritedFromLogcat)
        assertEquals(customDir, overridden.directory)

        // 4. LogcatScreenshotSettings fallback macro expansion never crashes outside Studio
        val resolvedHomeSub = LogcatScreenshotSettings.resolveDirectoryOrNull(null, "\$USER_HOME$/Pictures")
        assertNotNull(resolvedHomeSub)
        assertTrue(resolvedHomeSub.isAbsolute)
    }

    @Test
    fun cliMcpPathResolverFindsBundledCliAndEscapesWindowsPathsInJson(@TempDir tmp: Path) {
        val pluginRoot = Files.createDirectories(tmp.resolve("plugin-root"))
        val macCli = Files.createDirectories(pluginRoot.resolve("native/macos-arm64")).resolve("arb-cli")
        Files.writeString(macCli, "#!/bin/sh\n")
        val resolvedMac = ArbCliMcpPathResolver.resolveCliPath(
            pluginDir = pluginRoot,
            os = HostOs.MACOS,
            arch = "arm64",
        )
        assertEquals(macCli.toAbsolutePath().normalize(), resolvedMac)

        val winPath = """C:\Users\dev\AndroidStudio\plugins\arb\native\windows-x86_64\arb-cli.exe"""
        val winJson = ArbCliMcpPathResolver.formatMcpConfigJson(winPath)
        val parsed = JsonParser.parseString(winJson).asJsonObject
        assertEquals(winPath, parsed.get("command").asString)
        assertEquals(1, parsed.getAsJsonArray("args").size())
        assertEquals("mcp", parsed.getAsJsonArray("args").get(0).asString)
    }

    @Test
    fun aboutInfoDisplaysVersionsAndZeroGplOssLicenseAttribution() {
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
        assertTrue(onlineInfo.daemonStatusSummary.contains("0.2.0"))
        assertTrue(onlineInfo.daemonStatusSummary.contains("4242"))
        val licenseText = ArbAboutInfo.formatLicenseReport()
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
    fun pluginXmlEnablesComposeKeepsRunningDevicesAndRemovesLogcatActionsWithZeroEmoji() {
        val pluginXmlPath = repoRoot().resolve("ide-plugin/src/main/resources/META-INF/plugin.xml")
        assertTrue(Files.isRegularFile(pluginXmlPath))
        // plugin.xml plus the optional Android descriptor (Running Devices group, `arb-android.xml`).
        val xml = Files.readString(pluginXmlPath) + "\n" +
            Files.readString(pluginXmlPath.resolveSibling("arb-android.xml"))

        assertFalse(ArbFormatters.containsEmoji(xml), "plugin.xml must contain zero emoji")
        assertTrue(xml.contains("<depends>com.intellij.modules.compose</depends>"), "compose dependency required")
        assertTrue(xml.contains("""group-id="StreamingToolbarSecondary""""))
        assertTrue(xml.contains("io.github.shinma06.arb.RunningDevices.ToggleCapture"))
        assertTrue(xml.contains("io.github.shinma06.arb.RunningDevices.Save"))
        assertTrue(xml.contains("io.github.shinma06.arb.RunningDevices.Screenshot"))
        assertFalse(xml.contains("Logcat.PopupActions"), "Logcat popup action must be removed per owner decision")
        assertFalse(xml.contains("ArbLogcatTitleActions"), "Logcat title bar listener must be removed per owner decision")
        assertTrue(xml.contains("""id="io.github.shinma06.arb.settings""""))
        assertTrue(xml.contains("""parentId="tools""""))
        val extensionLists = Regex("""\bextensions="([^"]*)"""").findAll(xml)
        val registeredExtensions = extensionLists.flatMap { it.groupValues[1].split(';') }
        assertFalse(registeredExtensions.any { it.trim().equals("arb", ignoreCase = true) })
        assertTrue(xml.contains("io.github.shinma06.arb.ide.ArbBundleFileTypeDetector"))
        assertTrue(xml.contains("io.github.shinma06.arb.ide.ArbBundlePreCloseCheck"))
        assertTrue(xml.contains("io.github.shinma06.arb.LaunchTray"))
        assertTrue(xml.contains("Android Replay Buffer について"))

        val studio = ArbStudioController(
            onScrub = {},
            onSeekExact = {},
            onTrimExport = { _, _ -> },
            onPngExport = {},
        )
        assertEquals(StudioExportFormat.SHARE_MP4, studio.selectedExportFormat)
        studio.selectExportFormat(StudioExportFormat.WORM_BUNDLE_ONLY)
        assertEquals(StudioExportFormat.WORM_BUNDLE_ONLY, studio.selectedExportFormat)
        for (fmt in StudioExportFormat.entries) {
            assertFalse(ArbFormatters.containsEmoji(fmt.label))
        }
    }

    @Test
    fun studioControllerScalesTapsWithDisplaySizeSkipsUnmappableAndFallsBackForLegacyBundles() {
        val drawRect = java.awt.Rectangle(10, 20, 460, 1024)
        val (scaledX, scaledY) = assertNotNull(ArbStudioController.mapTapToCanvasPoint(
            tapX = 540.0,
            tapY = 1200.0,
            drawRect = drawRect,
            videoW = 460,
            videoH = 1024,
            displayW = 1080,
            displayH = 2400,
        ))
        assertEquals(240, scaledX)
        assertEquals(532, scaledY)

        // Mirror arb-core gesture::map_touch_to_frame, including partially missing geometry.
        fun map(x: Double, y: Double, videoW: Int = 460, videoH: Int = 1024, displayW: Int? = 1080, displayH: Int? = 2400) =
            ArbStudioController.mapTapToCanvasPoint(x, y, drawRect, videoW, videoH, displayW, displayH)
        assertNotNull(map(500.0, 500.0, videoW = 1024, videoH = 460))
        assertNull(map(0.5, 0.5, displayW = null))
        assertNull(map(0.5, 0.5, displayH = 0))
        assertNull(map(4000.0, 1200.0))
        assertNull(map(-1.0, 10.0), "negative coordinates are unmappable")
        assertNotNull(map(540.0, 1200.0))
    }

    /**
     * CE-01: with a known display size a tap is always physical pixels. A tap at `(1, 1)` (status
     * bar corner) used to be read as normalized and drawn at the opposite bottom-right corner.
     */
    @Test
    fun physicalTapAtOneOneStaysTopLeftWhenTheDisplaySizeIsKnown() {
        val drawRect = java.awt.Rectangle(10, 20, 460, 1024)
        fun map(x: Double, y: Double, displayW: Int?, displayH: Int?) =
            ArbStudioController.mapTapToCanvasPoint(x, y, drawRect, 460, 1024, displayW, displayH)

        assertEquals(10 to 20, map(0.0, 0.0, 1080, 2400))
        assertEquals(10 to 20, map(1.0, 1.0, 1080, 2400), "physical (1,1) is the top-left corner")
        assertEquals(10 to 20, map(2.0, 2.0, 1080, 2400))
        assertEquals(10 + 459 to 20 + 1023, map(1080.0, 2400.0, 1080, 2400))
        // Monotonic in x: 0 <= 1 <= 2 never jumps to the far edge.
        val xs = listOf(0.0, 0.5, 1.0, 2.0, 3.0).map { assertNotNull(map(it, 100.0, 1080, 2400)).first }
        assertEquals(xs.sorted(), xs)

        // Unknown display size (legacy): only normalized coordinates are defined.
        assertEquals(10 + 459 to 20 + 1023, map(1.0, 1.0, null, null))
        assertEquals(10 + 230 to 20 + 512, map(0.5, 0.5, null, null))
        assertNull(map(230.0, 512.0, null, null))
        assertNull(map(16000.0, 16000.0, null, null), "raw evdev values are skipped, never edge-clamped")
        assertNull(map(Double.NaN, 1.0, 1080, 2400))
    }

    @Test
    fun daemonServiceBroadcastsConfigUpdateAndBinaryChangeImmediately() {
        val service = ArbdDaemonService()
        var receivedConfig: ArbConfig? = null
        var connectionChangedCount = 0
        val listener = object : ArbDaemonStateListener {
            override fun onConfigUpdated(config: ArbConfig) {
                receivedConfig = config
            }
            override fun onDaemonConnectionChanged() {
                connectionChangedCount++
            }
        }
        service.addStateListener(listener)
        try {
            val updated = ArbConfig(saveWindowSecs = 75, ideDefaultSaveAction = IdeDefaultSaveAction.SAVE_ONLY)
            service.notifyConfigUpdated(updated)
            assertEquals(updated, service.cachedConfig)
            assertEquals(updated, receivedConfig)

            service.notifyBinaryPathChanged()
            assertEquals(1, connectionChangedCount)
        } finally {
            service.removeStateListener(listener)
            service.dispose()
        }
    }

    @Test
    fun toolWindowControllerEmptyDevicePlaceholderAndManualSelection() {
        val service = ArbdDaemonService()
        val controller = ArbToolWindowController(
            project = null,
            daemonService = service,
            onOpenBundle = { _, _, _ -> },
            pollIntervalMs = 0L,
        )
        try {
            // 1. Initial empty state: disabled placeholder, null serial
            assertFalse(controller.deviceDropdownEnabled, "empty device dropdown must be disabled")
            assertEquals(listOf(ArbToolWindowController.NO_DEVICES_PLACEHOLDER), controller.deviceDropdownLabels)
            assertNull(controller.selectedDeviceSerial, "placeholder text must never be returned as serial")
            assertFalse(ArbFormatters.containsEmoji(ArbToolWindowController.NO_DEVICES_PLACEHOLDER))

            // 2. Real device connects -> dropdown enables and selects real serial
            val withDevice = CaptureBarState(
                daemonConnected = true,
                devices = listOf(AdbDeviceInfo(serial = "emulator-5554", state = AdbDeviceState.Device)),
                selectedSerial = "emulator-5554",
            )
            controller.applyStateForTest(capture = withDevice)
            assertTrue(controller.deviceDropdownEnabled)
            assertEquals("emulator-5554", controller.selectedDeviceSerial)
            assertTrue(controller.startStopEnabled)
            assertTrue(controller.screenshotEnabled)

            // 3. Device disconnects -> reverts cleanly to disabled placeholder
            controller.applyStateForTest(capture = withDevice.copy(devices = emptyList(), selectedSerial = null))
            assertFalse(controller.deviceDropdownEnabled)
            assertEquals(listOf(ArbToolWindowController.NO_DEVICES_PLACEHOLDER), controller.deviceDropdownLabels)
            assertNull(controller.selectedDeviceSerial)
        } finally {
            controller.dispose()
            service.dispose()
        }
    }

    @Test
    fun settingsConfigurableDetectsStaleDaemonMethodNotFoundAndSupportsOneClickRestart() {
        var daemonUpgraded = false
        var restartCalls = 0
        val upgradedConfig = ArbConfig(saveWindowSecs = 120, videoMaxFps = 60)

        val staleError = ArbdRpcException(
            code = ArbdRpcException.RPC_ERR_METHOD_NOT_FOUND,
            message = "method not found: config.get",
            method = "config.get",
        )
        val diag = ArbSettingsConfigurable.classifyDaemonError(staleError)
        assertEquals(ArbSettingsConfigurable.DaemonDiagnosticKind.STALE_OR_MISMATCHED, diag.kind)
        assertTrue(diag.message.contains("古いバージョンの arbd"), diag.message)
        assertTrue(diag.message.contains("config.get 非対応"), diag.message)
        assertFalse(ArbFormatters.containsEmoji(diag.message))

        val configurable = ArbSettingsConfigurable(
            project = null,
            daemonConfigReader = {
                if (!daemonUpgraded) throw staleError
                upgradedConfig
            },
            daemonConfigWriter = { _, _ -> upgradedConfig },
            localBinaryReader = { "" },
            localBinaryWriter = {},
            daemonRestarter = {
                restartCalls++
                daemonUpgraded = true
            },
        )

        val form = configurable.createFormModel()
        assertTrue(form.isBannerVisible)
        assertTrue(form.showRestartAction)
        assertFalse(form.isDaemonControlsEnabled)
        assertTrue(form.daemonBannerMessage.contains("古いバージョンの arbd"))
        assertFalse(form.daemonBannerMessage.contains("接続できません"))

        configurable.restartDaemonAndReload()
        assertEquals(1, restartCalls)
        assertFalse(form.isBannerVisible)
        assertTrue(form.isDaemonControlsEnabled)
        assertEquals(120, form.currentDraft().saveWindowSecs)
        assertEquals(60, form.currentDraft().videoMaxFps)
        configurable.disposeUIResources()
    }

    @Test
    fun backgroundDevicePollingAutoSelectsNewlyBootedEmulatorFromZeroToOne() {
        val currentDevices = java.util.concurrent.atomic.AtomicReference<List<AdbDeviceInfo>>(emptyList())
        val stubTransport = object : io.github.shinma06.arb.ipc.LocalIpcTransport {
            override val endpoint: String = "mock://device-poll"
            private val responseQueue = java.util.concurrent.LinkedBlockingQueue<String>()
            override fun writeLine(jsonLine: String) {
                val obj = JsonParser.parseString(jsonLine).asJsonObject
                val id = obj.get("id").asLong
                val method = obj.get("method").asString
                val resultJson = when (method) {
                    "system.status" -> """{"pid":999,"uptime_ms":1000,"capture":{"active":false,"connected":false,"storage_pressure_stopped":false,"video_frames":0,"audio_frames":0,"log_records":0,"tap_records":0,"discontinuities":0,"static_hold_events":0},"ring_chunk_count":0,"ring_total_bytes":0,"ring_pinned_chunk_count":0,"buffered_secs":0,"active_viewports":0,"active_save_jobs":0,"recovery":{}}"""
                    "config.get" -> """{"config":{"config_version":1,"save_window_secs":300,"ring_budget_bytes":1610612736,"min_free_disk_bytes":3221225472,"video_max_size":1920,"video_max_fps":30,"audio_enabled":true,"crash_autosave_enabled":true,"crash_autosave_cooldown_secs":15,"ide_default_save_action":"save_and_open"},"config_path":"/tmp/config.json"}"""
                    "device.list" -> {
                        val devs = currentDevices.get().joinToString(",") { d ->
                            """{"serial":"${d.serial}","state":"${d.state.wireName}"}"""
                        }
                        """{"devices":[$devs]}"""
                    }
                    "save.list_jobs" -> """{"jobs":[]}"""
                    "bundle.list" -> """{"bundles":[]}"""
                    else -> """{}"""
                }
                responseQueue.put("""{"jsonrpc":"2.0","id":$id,"result":$resultJson}""")
            }
            override fun readLine(): String? = responseQueue.take()
            override fun close() = Unit
        }
        val rpcClient = io.github.shinma06.arb.ipc.ArbdRpcClient(stubTransport)
        val fakeService = object : ArbdDaemonService() {
            override fun <T> withClient(block: (io.github.shinma06.arb.ipc.ArbdRpcClient) -> T): T = block(rpcClient)
        }

        val controller = ArbToolWindowController(
            project = null,
            daemonService = fakeService,
            onOpenBundle = { _, _, _ -> },
            pollIntervalMs = 60L,
        )
        try {
            val deadlineEmpty = System.currentTimeMillis() + 3_000L
            while (System.currentTimeMillis() < deadlineEmpty) {
                javax.swing.SwingUtilities.invokeAndWait {}
                if (controller.captureState.statusBadgeText == "待機中") break
                Thread.sleep(25L)
            }
            assertEquals("待機中", controller.captureState.statusBadgeText)
            assertFalse(controller.deviceDropdownEnabled)
            assertNull(controller.selectedDeviceSerial)

            currentDevices.set(listOf(AdbDeviceInfo(serial = "emulator-5554", state = AdbDeviceState.Device)))
            val deadlineDetected = System.currentTimeMillis() + 3_000L
            while (System.currentTimeMillis() < deadlineDetected) {
                javax.swing.SwingUtilities.invokeAndWait {}
                if (controller.selectedDeviceSerial == "emulator-5554") break
                Thread.sleep(25L)
            }
            assertTrue(controller.deviceDropdownEnabled, "newly booted emulator must enable dropdown automatically")
            assertEquals("emulator-5554", controller.selectedDeviceSerial)

            currentDevices.set(listOf(AdbDeviceInfo(serial = "PIXEL9-USB-01", state = AdbDeviceState.Device)))
            val deadlineSwitched = System.currentTimeMillis() + 3_000L
            while (System.currentTimeMillis() < deadlineSwitched) {
                javax.swing.SwingUtilities.invokeAndWait {}
                if (controller.selectedDeviceSerial == "PIXEL9-USB-01") break
                Thread.sleep(25L)
            }
            assertEquals("PIXEL9-USB-01", controller.selectedDeviceSerial)
        } finally {
            controller.dispose()
            rpcClient.close()
            fakeService.dispose()
        }
    }

    @Test
    fun physicalUsbUnauthorizedDeviceTransitionsCleanlyToOnlineAndEnablesControls() {
        val currentDevices = java.util.concurrent.atomic.AtomicReference<List<AdbDeviceInfo>>(emptyList())
        val stubTransport = object : io.github.shinma06.arb.ipc.LocalIpcTransport {
            override val endpoint: String = "mock://usb-unauth"
            private val responseQueue = java.util.concurrent.LinkedBlockingQueue<String>()
            override fun writeLine(jsonLine: String) {
                val obj = JsonParser.parseString(jsonLine).asJsonObject
                val id = obj.get("id").asLong
                val method = obj.get("method").asString
                val resultJson = when (method) {
                    "system.status" -> """{"pid":998,"uptime_ms":1000,"capture":{"active":false,"connected":false,"storage_pressure_stopped":false,"video_frames":0,"audio_frames":0,"log_records":0,"tap_records":0,"discontinuities":0,"static_hold_events":0},"ring_chunk_count":0,"ring_total_bytes":0,"ring_pinned_chunk_count":0,"buffered_secs":0,"active_viewports":0,"active_save_jobs":0,"recovery":{}}"""
                    "config.get" -> """{"config":{"config_version":1,"save_window_secs":300,"ring_budget_bytes":1610612736,"min_free_disk_bytes":3221225472,"video_max_size":1920,"video_max_fps":30,"audio_enabled":true,"crash_autosave_enabled":true,"crash_autosave_cooldown_secs":15,"ide_default_save_action":"save_and_open"},"config_path":"/tmp/config.json"}"""
                    "device.list" -> {
                        val devs = currentDevices.get().joinToString(",") { d ->
                            """{"serial":"${d.serial}","state":"${d.state.wireName}"}"""
                        }
                        """{"devices":[$devs]}"""
                    }
                    "save.list_jobs" -> """{"jobs":[]}"""
                    "bundle.list" -> """{"bundles":[]}"""
                    else -> """{}"""
                }
                responseQueue.put("""{"jsonrpc":"2.0","id":$id,"result":$resultJson}""")
            }
            override fun readLine(): String? = responseQueue.take()
            override fun close() = Unit
        }
        val rpcClient = io.github.shinma06.arb.ipc.ArbdRpcClient(stubTransport)
        val fakeService = object : ArbdDaemonService() {
            override fun <T> withClient(block: (io.github.shinma06.arb.ipc.ArbdRpcClient) -> T): T = block(rpcClient)
        }
        val controller = ArbToolWindowController(
            project = null,
            daemonService = fakeService,
            onOpenBundle = { _, _, _ -> },
            pollIntervalMs = 50L,
        )
        try {
            val unauthDevice = AdbDeviceInfo(serial = "39101FDH200042", state = AdbDeviceState.Unauthorized)
            val offlineDevice = AdbDeviceInfo(serial = "192.168.1.50:5555", state = AdbDeviceState.Offline)
            assertTrue(ArbToolWindowController.formatDeviceLabel(unauthDevice).contains("未承認"))
            assertTrue(ArbToolWindowController.formatDeviceLabel(offlineDevice).contains("オフライン"))
            assertFalse(ArbFormatters.containsEmoji(ArbToolWindowController.formatDeviceLabel(unauthDevice)))
            assertFalse(ArbFormatters.containsEmoji(ArbToolWindowController.formatDeviceLabel(offlineDevice)))

            currentDevices.set(listOf(unauthDevice))
            val d1 = System.currentTimeMillis() + 3_000L
            while (System.currentTimeMillis() < d1) {
                javax.swing.SwingUtilities.invokeAndWait {}
                if (controller.selectedDeviceSerial == "39101FDH200042") break
                Thread.sleep(25L)
            }
            assertEquals("39101FDH200042", controller.selectedDeviceSerial)
            assertEquals("未承認", controller.captureState.statusBadgeText)
            assertTrue(controller.startStopEnabled, "always-on capture toggle stays armable while unauthorized")
            assertFalse(controller.screenshotEnabled, "screenshot must stay disabled while unauthorized")

            currentDevices.set(listOf(unauthDevice.copy(state = AdbDeviceState.Device)))
            val d2 = System.currentTimeMillis() + 3_000L
            while (System.currentTimeMillis() < d2) {
                javax.swing.SwingUtilities.invokeAndWait {}
                if (controller.screenshotEnabled) break
                Thread.sleep(25L)
            }
            assertEquals("39101FDH200042", controller.selectedDeviceSerial)
            assertEquals("待機中", controller.captureState.statusBadgeText)
            assertTrue(controller.startStopEnabled, "capture toggle stays enabled on online transition")
            assertTrue(controller.screenshotEnabled, "screenshot must auto-enable on online transition")
        } finally {
            controller.dispose()
            rpcClient.close()
            fakeService.dispose()
        }
    }
}
