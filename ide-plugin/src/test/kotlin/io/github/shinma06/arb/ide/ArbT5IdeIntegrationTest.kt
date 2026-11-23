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
        assertTrue(xml.contains("io.github.shinma06.arb.LaunchTray"))
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

    @Test
    fun logcatTitleActionsAppendsNonDestructivelyAndIdempotently() {
        val builtInA = object : com.intellij.openapi.actionSystem.AnAction("Built-in A") {
            override fun actionPerformed(e: com.intellij.openapi.actionSystem.AnActionEvent) = Unit
        }
        val builtInB = object : com.intellij.openapi.actionSystem.AnAction("Built-in B") {
            override fun actionPerformed(e: com.intellij.openapi.actionSystem.AnActionEvent) = Unit
        }
        val arbSaveLogs = ArbLogcatSaveLogsAction()

        val mergedOnce = ArbLogcatTitleActions.mergeTitleActions(
            existing = listOf(builtInA, builtInB, com.intellij.openapi.actionSystem.Separator.getInstance()),
            actionToAppend = arbSaveLogs,
            actionIdResolver = { null },
        )
        assertEquals(listOf(builtInA, builtInB, arbSaveLogs), mergedOnce)

        val mergedTwice = ArbLogcatTitleActions.mergeTitleActions(
            existing = mergedOnce,
            actionToAppend = arbSaveLogs,
            actionIdResolver = { null },
        )
        assertEquals(listOf(builtInA, builtInB, arbSaveLogs), mergedTwice, "re-attaching must be idempotent")
    }

    @Test
    fun videoViewportCanvasScalesTapsWithDisplaySizeAndFallsBackForLegacyBundles() {
        val drawRect = java.awt.Rectangle(10, 20, 460, 1024)
        // Downscaled 460x1024 video recorded from a 1080x2400 physical display:
        // physical center tap (540, 1200) must land at exact canvas center (10 + 230, 20 + 512)!
        val (scaledX, scaledY) = io.github.shinma06.arb.ui.VideoViewportCanvas.mapTapToCanvasPoint(
            tapX = 540.0,
            tapY = 1200.0,
            drawRect = drawRect,
            videoW = 460,
            videoH = 1024,
            displayW = 1080,
            displayH = 2400,
        )
        assertEquals(240, scaledX)
        assertEquals(532, scaledY)

        // Legacy bundle (displayW/displayH absent): falls back cleanly to video dimensions (460x1024)
        val (legacyX, legacyY) = io.github.shinma06.arb.ui.VideoViewportCanvas.mapTapToCanvasPoint(
            tapX = 230.0,
            tapY = 512.0,
            drawRect = drawRect,
            videoW = 460,
            videoH = 1024,
            displayW = null,
            displayH = null,
        )
        assertEquals(240, legacyX)
        assertEquals(532, legacyY)
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
    fun captureBarEmptyDevicePlaceholderAndTopRowAlignmentLayout() {
        val bar = io.github.shinma06.arb.ui.CaptureBarPanel(
            onStartCapture = {},
            onStopCapture = {},
            onSaveNow = {},
            onRefresh = {},
        )

        // 1. Initial empty state: disabled placeholder, popup blocked, null serial
        assertFalse(bar.deviceComboEnabled(), "empty device dropdown must be disabled")
        assertEquals(io.github.shinma06.arb.ui.CaptureBarPanel.NO_DEVICES_PLACEHOLDER, bar.deviceComboSelectedText())
        assertEquals(1, bar.deviceComboItemCount())
        assertTrue(bar.isDevicePopupBlockedForTest(), "empty popup must be blocked")
        assertNull(bar.selectedDeviceSerialForTest(), "placeholder text must never be returned as serial")
        assertFalse(ArbFormatters.containsEmoji(bar.deviceComboSelectedText().orEmpty()))

        // 2. Real device connects -> dropdown enables and selects real serial
        val withDevice = io.github.shinma06.arb.state.CaptureBarState(
            daemonConnected = true,
            devices = listOf(
                io.github.shinma06.arb.model.AdbDeviceInfo(
                    serial = "emulator-5554",
                    state = io.github.shinma06.arb.model.AdbDeviceState.Device,
                ),
            ),
            selectedSerial = "emulator-5554",
        )
        bar.render(withDevice)
        assertTrue(bar.deviceComboEnabled())
        assertFalse(bar.isDevicePopupBlockedForTest())
        assertEquals("emulator-5554", bar.selectedDeviceSerialForTest())

        // 3. Device disconnects -> reverts cleanly to disabled placeholder
        bar.render(withDevice.copy(devices = emptyList(), selectedSerial = null))
        assertFalse(bar.deviceComboEnabled())
        assertTrue(bar.isDevicePopupBlockedForTest())
        assertEquals(io.github.shinma06.arb.ui.CaptureBarPanel.NO_DEVICES_PLACEHOLDER, bar.deviceComboSelectedText())
        assertNull(bar.selectedDeviceSerialForTest())

        // 4. Layout pass at typical ToolWindow width (320px): square gear button, single vertical baseline
        val (combo, pill, gear) = bar.headerControlsForLayoutTest()
        val headerRow = requireNotNull(combo.parent)
        headerRow.setSize(320, 28)
        headerRow.doLayout()

        assertEquals(gear.height, gear.width, "gear settings button must be a crisp square icon button")
        assertTrue(gear.width in 24..34, "gear button should be ~28x28px, was ${gear.width}x${gear.height}")
        assertTrue(combo.width > pill.width * 2, "only device dropdown should stretch horizontally (combo=${combo.width}, pill=${pill.width})")

        val comboMidY = combo.y + combo.height / 2
        val pillMidY = pill.y + pill.height / 2
        val gearMidY = gear.y + gear.height / 2
        assertTrue(kotlin.math.abs(comboMidY - pillMidY) <= 1, "combo/pill vertical centers misaligned: $comboMidY vs $pillMidY")
        assertTrue(kotlin.math.abs(comboMidY - gearMidY) <= 1, "combo/gear vertical centers misaligned: $comboMidY vs $gearMidY")
    }

    @Test
    fun settingsPanelFitsDefaultWidthWithWrappedBannerAndCompactSpinners() {
        val panel = ArbSettingsPanel()
        panel.setDaemonStatus(
            online = false,
            detailMessage = "古いバージョンの arbd が起動中です（config.get 非対応）。新しい arbd へ再起動してください。",
            showRestartAction = true,
        )

        assertTrue(panel.bannerWrapsLines(), "warning banner must wrap text cleanly")
        assertTrue(panel.isRestartButtonVisible(), "1-click restart button must be visible in banner")

        // Preferred width must fit comfortably inside Android Studio's default Settings pane (~520-560px)
        val prefW = panel.preferredSize.width
        assertTrue(prefW <= 540, "Settings panel preferred width ($prefW) must fit default 540px pane without horizontal scroll")

        panel.setSize(520, 580)
        fun layoutTree(c: java.awt.Container) {
            c.doLayout()
            for (child in c.components) {
                if (child is java.awt.Container) layoutTree(child)
            }
        }
        layoutTree(panel)

        for (spinner in panel.spinnersForLayoutTest()) {
            assertTrue(
                spinner.preferredSize.width <= 130,
                "spinner preferred width must stay compact, was ${spinner.preferredSize.width}",
            )
            assertTrue(
                spinner.width <= 130,
                "spinner laid-out width must not stretch off-screen, was ${spinner.width}",
            )
        }
    }

    @Test
    fun settingsConfigurableDetectsStaleDaemonMethodNotFoundAndSupportsOneClickRestart() {
        var daemonUpgraded = false
        var restartCalls = 0
        val upgradedConfig = ArbConfig(saveWindowSecs = 120, videoMaxFps = 60)

        val staleError = io.github.shinma06.arb.model.ArbdRpcException(
            code = io.github.shinma06.arb.model.ArbdRpcException.RPC_ERR_METHOD_NOT_FOUND,
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

        configurable.createComponent()
        val panel = requireNotNull(configurable.currentPanelForTest())

        // Before restart: honest stale-daemon banner + disabled daemon controls + visible 1-click restart button
        assertTrue(panel.isOfflineBannerVisible())
        assertTrue(panel.isRestartButtonVisible())
        assertFalse(panel.isDaemonControlsEnabled())
        assertTrue(panel.offlineBannerText().contains("古いバージョンの arbd"))
        assertFalse(panel.offlineBannerText().contains("接続できません"))

        // 1-click restart upgrades daemon and immediately reloads live config
        panel.clickRestartDaemon()
        assertEquals(1, restartCalls)
        assertFalse(panel.isOfflineBannerVisible())
        assertTrue(panel.isDaemonControlsEnabled())
        assertEquals(120, panel.currentDraft().saveWindowSecs)
        assertEquals(60, panel.currentDraft().videoMaxFps)
        configurable.disposeUIResources()
    }

    @Test
    fun backgroundDevicePollingAutoSelectsNewlyBootedEmulatorFromZeroToOne() {
        val currentDevices = java.util.concurrent.atomic.AtomicReference<List<io.github.shinma06.arb.model.AdbDeviceInfo>>(
            emptyList(),
        )
        // Build a lightweight in-memory mock JSON-RPC transport backed by a pipe/queue so
        // ArbToolWindowPanel runs its real background poller end-to-end with zero external process!
        val stubTransport = object : io.github.shinma06.arb.ipc.LocalIpcTransport {
            override val endpoint: String = "mock://device-poll"
            private val responseQueue = java.util.concurrent.LinkedBlockingQueue<String>()
            override fun writeLine(jsonLine: String) {
                val obj = com.google.gson.JsonParser.parseString(jsonLine).asJsonObject
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

        val toolWindow = io.github.shinma06.arb.ui.ArbToolWindowPanel(
            daemonService = fakeService,
            onOpenBundle = { _, _, _ -> },
            pollIntervalMs = 60L,
        )
        try {
            // 1. Initially 0 devices -> disabled placeholder shown, online status = "待機中"
            val deadlineEmpty = System.currentTimeMillis() + 3_000L
            while (System.currentTimeMillis() < deadlineEmpty) {
                javax.swing.SwingUtilities.invokeAndWait {}
                if (toolWindow.captureBarPanel.statusBadgeDisplayText() == "待機中") break
                Thread.sleep(25L)
            }
            assertEquals("待機中", toolWindow.captureBarPanel.statusBadgeDisplayText())
            assertFalse(toolWindow.captureBarPanel.deviceComboEnabled())
            assertNull(toolWindow.captureBarPanel.selectedDeviceSerialForTest())

            // 2. Emulator boots while Tool Window is open -> poller auto-detects & auto-selects it!
            currentDevices.set(
                listOf(
                    io.github.shinma06.arb.model.AdbDeviceInfo(
                        serial = "emulator-5554",
                        state = io.github.shinma06.arb.model.AdbDeviceState.Device,
                    ),
                ),
            )
            val deadlineDetected = System.currentTimeMillis() + 3_000L
            while (System.currentTimeMillis() < deadlineDetected) {
                javax.swing.SwingUtilities.invokeAndWait {}
                if (toolWindow.captureBarPanel.selectedDeviceSerialForTest() == "emulator-5554") break
                Thread.sleep(25L)
            }
            assertTrue(toolWindow.captureBarPanel.deviceComboEnabled(), "newly booted emulator must enable dropdown automatically")
            assertEquals("emulator-5554", toolWindow.captureBarPanel.selectedDeviceSerialForTest())

            // 3. First emulator disconnects while a second USB device is online -> auto-switches to online device!
            currentDevices.set(
                listOf(
                    io.github.shinma06.arb.model.AdbDeviceInfo(
                        serial = "PIXEL9-USB-01",
                        state = io.github.shinma06.arb.model.AdbDeviceState.Device,
                    ),
                ),
            )
            val deadlineSwitched = System.currentTimeMillis() + 3_000L
            while (System.currentTimeMillis() < deadlineSwitched) {
                javax.swing.SwingUtilities.invokeAndWait {}
                if (toolWindow.captureBarPanel.selectedDeviceSerialForTest() == "PIXEL9-USB-01") break
                Thread.sleep(25L)
            }
            assertEquals("PIXEL9-USB-01", toolWindow.captureBarPanel.selectedDeviceSerialForTest())
        } finally {
            toolWindow.dispose()
            rpcClient.close()
            fakeService.dispose()
        }
    }

    @Test
    fun physicalUsbUnauthorizedDeviceTransitionsCleanlyToOnlineAndEnablesControls() {
        val currentDevices = java.util.concurrent.atomic.AtomicReference<List<io.github.shinma06.arb.model.AdbDeviceInfo>>(
            emptyList(),
        )
        val stubTransport = object : io.github.shinma06.arb.ipc.LocalIpcTransport {
            override val endpoint: String = "mock://usb-unauth"
            private val responseQueue = java.util.concurrent.LinkedBlockingQueue<String>()
            override fun writeLine(jsonLine: String) {
                val obj = com.google.gson.JsonParser.parseString(jsonLine).asJsonObject
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
        val toolWindow = io.github.shinma06.arb.ui.ArbToolWindowPanel(
            daemonService = fakeService,
            onOpenBundle = { _, _, _ -> },
            pollIntervalMs = 50L,
        )
        try {
            // Verify clear Japanese labels for unauthorized / offline states (zero emoji!)
            val unauthDevice = io.github.shinma06.arb.model.AdbDeviceInfo(
                serial = "39101FDH200042",
                state = io.github.shinma06.arb.model.AdbDeviceState.Unauthorized,
            )
            val offlineDevice = io.github.shinma06.arb.model.AdbDeviceInfo(
                serial = "192.168.1.50:5555",
                state = io.github.shinma06.arb.model.AdbDeviceState.Offline,
            )
            assertTrue(io.github.shinma06.arb.ui.CaptureBarPanel.formatDeviceLabel(unauthDevice).contains("未承認"))
            assertTrue(io.github.shinma06.arb.ui.CaptureBarPanel.formatDeviceLabel(offlineDevice).contains("オフライン"))
            assertFalse(ArbFormatters.containsEmoji(io.github.shinma06.arb.ui.CaptureBarPanel.formatDeviceLabel(unauthDevice)))
            assertFalse(ArbFormatters.containsEmoji(io.github.shinma06.arb.ui.CaptureBarPanel.formatDeviceLabel(offlineDevice)))

            // 1. Physical USB phone plugged in before tapping "Allow USB debugging" -> unauthorized
            currentDevices.set(listOf(unauthDevice))
            val d1 = System.currentTimeMillis() + 3_000L
            while (System.currentTimeMillis() < d1) {
                javax.swing.SwingUtilities.invokeAndWait {}
                if (toolWindow.captureBarPanel.selectedDeviceSerialForTest() == "39101FDH200042") break
                Thread.sleep(25L)
            }
            assertEquals("39101FDH200042", toolWindow.captureBarPanel.selectedDeviceSerialForTest())
            assertEquals("未承認", toolWindow.captureBarPanel.statusBadgeDisplayText())
            assertFalse(toolWindow.captureBarPanel.isStartStopButtonEnabledForTest(), "capture start must stay disabled while unauthorized")
            assertFalse(toolWindow.captureBarPanel.isScreenshotButtonEnabledForTest(), "screenshot must stay disabled while unauthorized")

            // 2. User approves USB debugging on phone -> transitions unauthorized -> online ("device")!
            currentDevices.set(
                listOf(unauthDevice.copy(state = io.github.shinma06.arb.model.AdbDeviceState.Device)),
            )
            val d2 = System.currentTimeMillis() + 3_000L
            while (System.currentTimeMillis() < d2) {
                javax.swing.SwingUtilities.invokeAndWait {}
                if (toolWindow.captureBarPanel.isStartStopButtonEnabledForTest()) break
                Thread.sleep(25L)
            }
            assertEquals("39101FDH200042", toolWindow.captureBarPanel.selectedDeviceSerialForTest())
            assertEquals("待機中", toolWindow.captureBarPanel.statusBadgeDisplayText())
            assertTrue(toolWindow.captureBarPanel.isStartStopButtonEnabledForTest(), "capture start must auto-enable on online transition")
            assertTrue(toolWindow.captureBarPanel.isScreenshotButtonEnabledForTest(), "screenshot must auto-enable on online transition")
        } finally {
            toolWindow.dispose()
            rpcClient.close()
            fakeService.dispose()
        }
    }
}
