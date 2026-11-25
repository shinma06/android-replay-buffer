package io.github.shinma06.arb.ui

import com.google.gson.JsonObject
import com.google.gson.JsonParser
import io.github.shinma06.arb.ide.ArbAboutInfo
import io.github.shinma06.arb.ide.ArbLocalSettingsState
import io.github.shinma06.arb.ide.ArbScreenshotDestinationResolver
import io.github.shinma06.arb.ide.ArbSettingsConfigurable
import io.github.shinma06.arb.ide.ArbdDaemonService
import io.github.shinma06.arb.ipc.ArbdRpcClient
import io.github.shinma06.arb.ipc.LocalIpcTransport
import io.github.shinma06.arb.model.AdbDeviceInfo
import io.github.shinma06.arb.model.AdbDeviceState
import io.github.shinma06.arb.model.AppLogSummary
import io.github.shinma06.arb.model.ArbConfig
import io.github.shinma06.arb.model.BundleFormat
import io.github.shinma06.arb.model.CaptureMode
import io.github.shinma06.arb.model.CaptureStatusView
import io.github.shinma06.arb.model.LogRecord
import io.github.shinma06.arb.model.OutputDirPatch
import io.github.shinma06.arb.model.Provenance
import io.github.shinma06.arb.model.RecordBucket
import io.github.shinma06.arb.model.RecoverySummaryView
import io.github.shinma06.arb.model.StreamsSummary
import io.github.shinma06.arb.model.StringPatch
import io.github.shinma06.arb.model.SystemHelloResponse
import io.github.shinma06.arb.model.SystemStatusResponse
import io.github.shinma06.arb.model.TimeAlignment
import io.github.shinma06.arb.model.TimelineSummaryResponse
import io.github.shinma06.arb.model.TimestampSemantics
import io.github.shinma06.arb.model.UnanchoredSummary
import io.github.shinma06.arb.state.ArbFormatters
import io.github.shinma06.arb.state.ArbSettingsDraft
import io.github.shinma06.arb.state.CaptureBarReducer
import io.github.shinma06.arb.state.CaptureBarState
import io.github.shinma06.arb.state.DeviceSelectionOutcome
import io.github.shinma06.arb.state.SyncedEventKind
import io.github.shinma06.arb.state.SyncedEventRow
import io.github.shinma06.arb.state.TimelineViewerState
import androidx.compose.ui.graphics.toArgb
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.io.IOException
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import javax.swing.SwingUtilities
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * T-6D: T-6C backend capabilities wired into the T-6B Compose/Jewel UI, plus regression tests for
 * every Part 2 Compose/Jewel lifecycle audit fix. Fully headless (no Skiko, no live IDE frame).
 */
class ArbT6dUiGlueAuditTest {
    // ---------------- Fixtures ----------------

    private class RecordedCall(val method: String, val params: JsonObject)

    /** Stub NDJSON transport answering JSON-RPC by method and recording every request. */
    private class RecordingTransport(
        private val respond: (method: String, params: JsonObject) -> String,
    ) : LocalIpcTransport {
        override val endpoint: String = "mock://t6d"
        val calls = CopyOnWriteArrayList<RecordedCall>()
        private val responses = LinkedBlockingQueue<String>()

        override fun writeLine(jsonLine: String) {
            val obj = JsonParser.parseString(jsonLine).asJsonObject
            val id = obj.get("id").asLong
            val method = obj.get("method").asString
            val params = obj.get("params")?.takeIf { it.isJsonObject }?.asJsonObject ?: JsonObject()
            calls += RecordedCall(method, params)
            responses.put("""{"jsonrpc":"2.0","id":$id,"result":${respond(method, params)}}""")
        }

        override fun readLine(): String? = responses.take()

        override fun close() = Unit

        fun methods(): List<String> = calls.map { it.method }
    }

    private fun captureJson(active: Boolean, serial: String?, mode: CaptureMode): String {
        val serialPart = serial?.let { "\"serial\":\"$it\"," }.orEmpty()
        return """{"active":$active,"connected":$active,$serialPart"storage_pressure_stopped":false,"capture_mode":"${mode.wire}","video_frames":0,"audio_frames":0,"log_records":0,"tap_records":0,"discontinuities":0,"static_hold_events":0}"""
    }

    private fun configJson(mode: CaptureMode, deviceSerial: String? = null): String {
        val serialPart = deviceSerial?.let { ",\"device_serial\":\"$it\"" }.orEmpty()
        return """{"config":{"config_version":1,"save_window_secs":300,"ring_budget_bytes":1610612736,"min_free_disk_bytes":3221225472,"video_max_size":1920,"video_max_fps":30,"audio_enabled":true,"capture_mode":"${mode.wire}"$serialPart,"crash_autosave_enabled":true,"crash_autosave_cooldown_secs":15,"ide_default_save_action":"save_and_open"},"config_path":"/tmp/config.json"}"""
    }

    private fun stubDaemon(
        transport: RecordingTransport,
    ): Pair<ArbdDaemonService, ArbdRpcClient> {
        val rpc = ArbdRpcClient(transport)
        val service = object : ArbdDaemonService() {
            override fun <T> withClient(block: (ArbdRpcClient) -> T): T = synchronized(this) { block(rpc) }
        }
        return service to rpc
    }

    private fun defaultResponder(
        devices: List<AdbDeviceInfo> = emptyList(),
        captureActive: Boolean = false,
        captureSerial: String? = null,
        mode: CaptureMode = CaptureMode.VIDEO_AND_LOGS,
    ): (String, JsonObject) -> String = { method, params ->
        when (method) {
            "system.status" -> """{"pid":42,"uptime_ms":1,"capture":${captureJson(captureActive, captureSerial, mode)},"ring_chunk_count":0,"ring_total_bytes":0,"ring_pinned_chunk_count":0,"buffered_secs":0,"active_viewports":0,"active_save_jobs":0,"recovery":{}}"""
            "config.get" -> configJson(mode)
            "config.update" -> configJson(
                mode = params.get("capture_mode")?.asString?.let(CaptureMode::fromWire) ?: mode,
                deviceSerial = params.get("device_serial")?.takeIf { !it.isJsonNull }?.asString,
            )
            "device.list" -> """{"devices":[${devices.joinToString(",") { """{"serial":"${it.serial}","state":"${it.state.wireName}"}""" }}]}"""
            "save.list_jobs" -> """{"jobs":[]}"""
            "bundle.list" -> """{"bundles":[]}"""
            "capture.stop" -> """{"stopped":true,"capture":${captureJson(false, captureSerial, mode)}}"""
            "capture.start" -> """{"serial":"${params.get("serial")?.asString ?: "auto"}","started":true,"capture_mode":"${params.get("capture_mode")?.asString ?: mode.wire}"}"""
            "save.screenshot" -> """{"path":"/tmp/shot.png","width":2,"height":4,"sha256":"00","captured_at_unix_ms":1,"status":"present"}"""
            else -> "{}"
        }
    }

    private fun awaitTrue(timeoutMs: Long = 3_000L, condition: () -> Boolean) {
        val deadline = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < deadline) {
            SwingUtilities.invokeAndWait {}
            if (condition()) return
            Thread.sleep(15L)
        }
        SwingUtilities.invokeAndWait {}
        assertTrue(condition(), "condition not met within ${timeoutMs}ms")
    }

    private fun status(active: Boolean = false, serial: String? = null, mode: CaptureMode = CaptureMode.VIDEO_AND_LOGS) =
        SystemStatusResponse(
            pid = 7L,
            uptimeMs = 1L,
            capture = CaptureStatusView(
                active = active,
                connected = active,
                serial = serial,
                storagePressureStopped = false,
                captureMode = mode,
                videoFrames = 0L,
                audioFrames = 0L,
                logRecords = 0L,
                tapRecords = 0L,
                discontinuities = 0L,
                staticHoldEvents = 0L,
            ),
            ringChunkCount = 0L,
            ringTotalBytes = 0L,
            ringPinnedChunkCount = 0L,
            activeViewports = 0L,
            activeSaveJobs = 0L,
            recovery = RecoverySummaryView(),
        )

    @Test
    fun standaloneToolbarRefreshesDevicesWithoutOpeningThePanel() {
        val transport = RecordingTransport(defaultResponder(devices = listOf(online("emu-1"))))
        val (service, rpc) = stubDaemon(transport)
        try {
            service.refreshToolbarSnapshot()
            assertEquals("emu-1", service.cachedToolbarSnapshot.selectedDeviceSerial)
        } finally {
            rpc.close()
        }
    }

    private fun online(serial: String) = AdbDeviceInfo(serial = serial, state = AdbDeviceState.Device)

    private fun offlineController(): Pair<ArbToolWindowController, ArbdDaemonService> {
        val service = ArbdDaemonService()
        val controller = ArbToolWindowController(
            project = null,
            daemonService = service,
            onOpenBundle = { _, _, _ -> },
            pollIntervalMs = 0L,
            screenshotDirResolver = { null },
        )
        return controller to service
    }

    // ---------------- Part 1.3: strict multi-device DeviceSelectionPolicy wiring ----------------

    @Test
    fun deviceDropdownFollowsStrictSelectionPolicyForZeroOneAndMultipleDevices() {
        val (controller, service) = offlineController()
        try {
            // 0 online -> clear placeholder, disabled dropdown, no serial; capture toggle stays armable.
            val none = CaptureBarReducer.fromStatus(CaptureBarState(), status(), devices = emptyList())
            controller.applyStateForTest(capture = none)
            assertEquals(listOf(ArbToolWindowController.NO_DEVICES_PLACEHOLDER), controller.deviceDropdownLabels)
            assertFalse(controller.deviceDropdownEnabled)
            assertNull(controller.selectedDeviceSerial)
            assertTrue(controller.startStopEnabled)
            assertFalse(controller.screenshotEnabled)

            // 1 online + no preferred -> auto-select and enable capture controls.
            val one = CaptureBarReducer.fromStatus(none, status(), devices = listOf(online("emulator-5554")))
            controller.applyStateForTest(capture = one)
            assertEquals(listOf("emulator-5554"), controller.deviceDropdownLabels)
            assertEquals("emulator-5554", controller.selectedDeviceSerial)
            assertTrue(controller.startStopEnabled)
            assertNull(controller.deviceSelectionNotice)

            // 2+ online + no preferred -> NEVER auto-select; leading prompt + inline notice.
            val two = CaptureBarReducer.fromStatus(
                one,
                status(),
                devices = listOf(online("emulator-5554"), online("R3CN1234567")),
            )
            assertTrue(two.deviceSelection is DeviceSelectionOutcome.MultipleDevicesNeedSelection)
            controller.applyStateForTest(capture = two)
            assertEquals(
                listOf(ArbToolWindowController.MULTIPLE_DEVICES_PROMPT, "emulator-5554", "R3CN1234567"),
                controller.deviceDropdownLabels,
            )
            assertEquals(0, controller.selectedDeviceIndex, "prompt must be shown, not the first device")
            assertNull(controller.selectedDeviceSerial)
            assertTrue(controller.startStopEnabled)
            assertFalse(controller.screenshotEnabled)
            assertEquals(ArbToolWindowController.MULTIPLE_DEVICES_PROMPT, controller.deviceSelectionNotice)
            assertTrue(controller.startStopTooltip.contains(ArbToolWindowController.MULTIPLE_DEVICES_PROMPT))
            assertFalse(ArbFormatters.containsEmoji(ArbToolWindowController.MULTIPLE_DEVICES_PROMPT))
        } finally {
            controller.dispose()
            service.dispose()
        }
    }

    @Test
    fun pinnedDeviceDisconnectNeverSilentlySwitchesToAnotherOnlineDevice() {
        val (controller, service) = offlineController()
        try {
            val pinnedConfig = ArbConfig(deviceSerial = "PIXEL-A")
            val both = CaptureBarReducer.fromStatus(
                CaptureBarState(),
                status(),
                devices = listOf(online("PIXEL-A"), online("PIXEL-B")),
                config = pinnedConfig,
            )
            controller.applyStateForTest(capture = both)
            assertEquals("PIXEL-A", controller.selectedDeviceSerial)
            assertEquals(0, controller.selectedDeviceIndex)

            // PIXEL-A unplugged while PIXEL-B stays online.
            val gone = CaptureBarReducer.fromStatus(both, status(), devices = listOf(online("PIXEL-B")), config = pinnedConfig)
            assertTrue(gone.deviceSelection is DeviceSelectionOutcome.WaitingForPreferredDevice)
            controller.applyStateForTest(capture = gone)
            assertEquals(
                listOf("PIXEL-A — ${ArbToolWindowController.WAITING_FOR_PINNED_SUFFIX}", "PIXEL-B"),
                controller.deviceDropdownLabels,
            )
            assertEquals(0, controller.selectedDeviceIndex, "pinned PIXEL-A must remain the shown selection")
            assertNull(controller.selectedDeviceSerial, "PIXEL-B must never be auto-selected")
            assertTrue(controller.startStopEnabled)
            assertFalse(controller.screenshotEnabled)
            val notice = assertNotNull(controller.deviceSelectionNotice)
            assertTrue(notice.contains("PIXEL-A") && notice.contains("切断中（再接続待ち）"), notice)

            // Reconnect -> resumes on the pinned device.
            val back = CaptureBarReducer.fromStatus(gone, status(), devices = listOf(online("PIXEL-B"), online("PIXEL-A")), config = pinnedConfig)
            controller.applyStateForTest(capture = back)
            assertEquals("PIXEL-A", controller.selectedDeviceSerial)
            assertNull(controller.deviceSelectionNotice)
        } finally {
            controller.dispose()
            service.dispose()
        }
    }

    @Test
    fun explicitDeviceChoicePinsAndPersistsDeviceSerialViaConfigUpdate() {
        val devices = listOf(online("emulator-5554"), online("R3CN1234567"))
        val transport = RecordingTransport(defaultResponder(devices = devices))
        val (service, rpc) = stubDaemon(transport)
        val controller = ArbToolWindowController(
            project = null,
            daemonService = service,
            onOpenBundle = { _, _, _ -> },
            pollIntervalMs = 0L,
            screenshotDirResolver = { null },
        )
        try {
            val multi = CaptureBarReducer.fromStatus(CaptureBarState(), status(), devices = devices)
            controller.applyStateForTest(capture = multi)
            controller.selectDeviceByIndex(0) // the prompt row is not selectable
            assertTrue(transport.calls.isEmpty())

            controller.selectDeviceByIndex(2)
            awaitTrue { controller.selectedDeviceSerial == "R3CN1234567" }
            assertEquals("R3CN1234567", controller.selectedDeviceSerial)
            assertNull(controller.captureState.autoSelectedSerial, "explicit choice must not be treated as auto-selected")
            val update = transport.calls.first { it.method == "config.update" }
            assertEquals("R3CN1234567", update.params.get("device_serial").asString)

        } finally {
            controller.dispose()
            rpc.close()
            service.dispose()
        }
    }

    @Test
    fun reselectingSoleAutoSelectedDeviceNeverCreatesPin() {
        val devices = listOf(online("emulator-5554"))
        // The stub serves the same single device, so any background refresh recomputes the same state.
        val transport = RecordingTransport(defaultResponder(devices = devices))
        val (service, rpc) = stubDaemon(transport)
        val controller = ArbToolWindowController(
            project = null,
            daemonService = service,
            onOpenBundle = { _, _, _ -> },
            pollIntervalMs = 0L,
            screenshotDirResolver = { null },
        )
        try {
            val single = CaptureBarReducer.fromStatus(CaptureBarState(), status(), devices = devices)
            assertEquals("emulator-5554", single.autoSelectedSerial)
            controller.applyStateForTest(capture = single)
            controller.selectDeviceByIndex(0)
            Thread.sleep(150L)
            SwingUtilities.invokeAndWait {}
            assertFalse(transport.methods().contains("config.update"), "auto-selected device must not be pinned")
            assertEquals("emulator-5554", controller.captureState.autoSelectedSerial)
            assertEquals("emulator-5554", controller.selectedDeviceSerial)
        } finally {
            controller.dispose()
            rpc.close()
            service.dispose()
        }
    }

    // ---------------- Part 1.1: CaptureMode::LogsOnly toggle ----------------

    @Test
    fun captureModeToggleUpdatesConfigAndRestartsActiveCaptureOnSameDevice() {
        val transport = RecordingTransport(
            defaultResponder(devices = listOf(online("PIXEL-A")), captureActive = true, captureSerial = "PIXEL-A"),
        )
        val (service, rpc) = stubDaemon(transport)
        val controller = ArbToolWindowController(
            project = null,
            daemonService = service,
            onOpenBundle = { _, _, _ -> },
            pollIntervalMs = 0L,
            screenshotDirResolver = { null },
        )
        try {
            val recording = CaptureBarReducer.fromStatus(
                CaptureBarState(),
                status(active = true, serial = "PIXEL-A"),
                devices = listOf(online("PIXEL-A")),
            )
            controller.applyStateForTest(capture = recording)
            assertEquals(CaptureMode.VIDEO_AND_LOGS, controller.captureMode)
            assertNull(controller.captureModeChipText)
            assertTrue(controller.captureModeToggleEnabled)

            controller.setCaptureMode(CaptureMode.VIDEO_AND_LOGS) // no-op
            controller.setCaptureMode(CaptureMode.LOGS_ONLY)
            awaitTrue { transport.methods().contains("capture.start") }
            val methods = transport.methods()
            val updateIdx = methods.indexOf("config.update")
            val stopIdx = methods.indexOf("capture.stop")
            val startIdx = methods.indexOf("capture.start")
            assertTrue(updateIdx in 0 until stopIdx && stopIdx < startIdx, "order: $methods")
            assertEquals("logs_only", transport.calls[updateIdx].params.get("capture_mode").asString)
            val start = transport.calls[startIdx].params
            assertEquals("logs_only", start.get("capture_mode").asString)
            assertEquals("PIXEL-A", start.get("serial").asString, "restart must stay on the same device")
            awaitTrue { !controller.captureModeBusy }
        } finally {
            controller.dispose()
            rpc.close()
            service.dispose()
        }
    }

    @Test
    fun idleCaptureModeToggleOnlyUpdatesConfigAndLogsOnlyShowsQuietChip() {
        val transport = RecordingTransport(defaultResponder(devices = listOf(online("PIXEL-A"))))
        val (service, rpc) = stubDaemon(transport)
        val controller = ArbToolWindowController(
            project = null,
            daemonService = service,
            onOpenBundle = { _, _, _ -> },
            pollIntervalMs = 0L,
            screenshotDirResolver = { null },
        )
        try {
            controller.applyStateForTest(
                capture = CaptureBarReducer.fromStatus(CaptureBarState(), status(), devices = listOf(online("PIXEL-A"))),
            )
            controller.setCaptureMode(CaptureMode.LOGS_ONLY)
            awaitTrue { transport.methods().contains("config.update") }
            awaitTrue { !controller.captureModeBusy }
            assertFalse(transport.methods().contains("capture.stop"))
            assertFalse(transport.methods().contains("capture.start"))
        } finally {
            controller.dispose()
            rpc.close()
            service.dispose()
        }

        // Chip assertions on a controller with no background work queued (no refresh races).
        val (quiet, quietService) = offlineController()
        try {
            val logsOnly = CaptureBarReducer.fromStatus(
                CaptureBarState(),
                status(active = true, serial = "PIXEL-A", mode = CaptureMode.LOGS_ONLY),
                devices = listOf(online("PIXEL-A")),
                config = ArbConfig(captureMode = CaptureMode.LOGS_ONLY),
            )
            quiet.applyStateForTest(capture = logsOnly)
            assertTrue(quiet.isLogsOnlyMode)
            assertEquals(ArbToolWindowController.LOGS_ONLY_CHIP_TEXT, quiet.captureModeChipText)
            assertEquals("ログのみ常時", quiet.captureModeChipText)
            assertEquals("REC (ログのみ)", quiet.captureState.statusBadgeText)
            assertEquals("映像＋ログ", ArbToolWindowController.captureModeLabel(CaptureMode.VIDEO_AND_LOGS))
            assertEquals("ログのみ", ArbToolWindowController.captureModeLabel(CaptureMode.LOGS_ONLY))
        } finally {
            quiet.dispose()
            quietService.dispose()
        }
    }

    // ---------------- Part 1.4: Logcat screenshot destination -> save.screenshot(output_dir) ----------------

    @Test
    fun toolWindowScreenshotPassesInheritedDirectoryAsOutputDir() {
        val transport = RecordingTransport(defaultResponder(devices = listOf(online("PIXEL-A"))))
        val (service, rpc) = stubDaemon(transport)
        val shotDir = Files.createTempDirectory("arb-t6d-shots").toAbsolutePath().toString()
        val controller = ArbToolWindowController(
            project = null,
            daemonService = service,
            onOpenBundle = { _, _, _ -> },
            pollIntervalMs = 0L,
            screenshotDirResolver = { shotDir },
        )
        try {
            controller.applyStateForTest(
                capture = CaptureBarReducer.fromStatus(CaptureBarState(), status(), devices = listOf(online("PIXEL-A"))),
            )
            controller.captureScreenshot()
            awaitTrue { transport.methods().contains("save.screenshot") }
            val params = transport.calls.first { it.method == "save.screenshot" }.params
            assertEquals(shotDir, params.get("output_dir").asString)
            assertFalse(params.has("output_path"), "daemon owns file naming (CLI/MCP parity)")
            assertEquals("PIXEL-A", params.get("serial").asString)
        } finally {
            controller.dispose()
            rpc.close()
            service.dispose()
        }
    }

    @Test
    fun screenshotOutputDirResolutionHonorsLogcatInheritanceAndCustomOverride() {
        val logcatDir = Files.createTempDirectory("arb-t6d-logcat")
        val inherit = ArbLocalSettingsState().apply { screenshotInheritLogcat = true }
        assertEquals(
            logcatDir.toAbsolutePath().normalize().toString(),
            ArbScreenshotDestinationResolver.resolveOutputDir(
                project = null,
                localSettings = inherit,
                logcatLocationProvider = { "\$DESKTOP_DIR\$" },
                logcatDirectoryResolver = { _, _ -> logcatDir },
            ),
        )
        assertNull(
            ArbScreenshotDestinationResolver.resolveOutputDir(
                project = null,
                localSettings = inherit,
                logcatLocationProvider = { null },
            ),
            "unavailable Logcat settings fall back to arbd's own screenshot_dir_override chain",
        )
        val custom = Files.createTempDirectory("arb-t6d-custom")
        val override = ArbLocalSettingsState().apply {
            screenshotInheritLogcat = false
            customScreenshotDir = custom.toString()
        }
        assertEquals(
            custom.toAbsolutePath().normalize().toString(),
            ArbScreenshotDestinationResolver.resolveOutputDir(project = null, localSettings = override),
        )
        val relative = ArbLocalSettingsState().apply {
            screenshotInheritLogcat = false
            customScreenshotDir = "relative/dir"
        }
        assertNull(ArbScreenshotDestinationResolver.resolveOutputDir(project = null, localSettings = relative))
    }

    // ---------------- Part 1.2: per-app extracted log filter in Studio ----------------

    private fun studioState(appLogs: List<AppLogSummary>, legacy: Boolean = false): TimelineViewerState {
        val log = { seq: Long, pkg: String? ->
            LogRecord(
                seq = seq,
                pcIngestContNs = seq,
                alignment = TimeAlignment.Anchored("boot", 1_000L + seq, 1L, TimestampSemantics.LOGCAT_HEADER_WALL),
                bucket = RecordBucket.MAIN,
                packageName = pkg,
                tag = "T",
                message = "m$seq",
            )
        }
        return TimelineViewerState(
            bundleId = "b-apps",
            path = "/tmp/b-apps",
            format = if (legacy) BundleFormat.LEGACY_SCHEMA4 else BundleFormat.V2,
            isLegacy = legacy,
            leaseId = "l",
            summary = TimelineSummaryResponse(
                bundleId = "b-apps",
                path = "/tmp/b-apps",
                format = BundleFormat.V2,
                isLegacy = legacy,
                provenance = Provenance.Original,
                streams = StreamsSummary(),
                unanchoredSummary = UnanchoredSummary(0L, 0L),
                epochs = emptyList(),
                frameCount = 0,
                keyframeCount = 0,
                logMainCount = 3,
                logUnanchoredCount = 0,
                tapMainCount = 0,
                tapUnanchoredCount = 0,
                discontinuityCount = 0,
                appLogs = appLogs,
            ),
            frameCount = 0L,
            mainLogs = listOf(log(1L, "com.example.app"), log(2L, "com.other"), log(3L, "com.example.app")),
        )
    }

    @Test
    fun studioPerAppPackageFilterQueriesSelectedPackageAndIgnoresStaleResults() {
        val requested = mutableListOf<String?>()
        val studio = ArbStudioController(
            onScrub = {},
            onSeekExact = {},
            onTrimExport = { _, _ -> },
            onPngExport = { _, _ -> },
            onAppPackageFilterChanged = { requested += it },
        )
        val apps = listOf(
            AppLogSummary("com.example.app", "app-logs/com.example.app.jsonl", 1_234L, 1L, "aa"),
            AppLogSummary("com.other", "app-logs/com.other.jsonl", 10L, 0L, "bb"),
        )
        studio.renderState(studioState(apps), image = null)
        assertTrue(studio.isAppPackageFilterVisible)
        assertEquals(
            listOf("全アプリ", "com.example.app · 1234 行 · クラッシュ 1", "com.other · 10 行"),
            studio.appPackageFilterOptions.map { it.label },
        )
        assertEquals(0, studio.selectedAppPackageIndex)
        assertEquals(3, studio.mainLogRowCount)

        studio.selectAppPackageByIndex(1)
        assertEquals(listOf<String?>("com.example.app"), requested)
        assertEquals("com.example.app", studio.selectedAppPackage)
        assertTrue(studio.appPackageFilterLoading)
        assertEquals(1, studio.selectedAppPackageIndex)

        // Re-selecting the same package does not re-query; unknown packages are rejected.
        studio.selectAppPackage("com.example.app")
        studio.selectAppPackage("com.not.listed")
        assertEquals(1, requested.size)

        val filtered = studioState(apps).mainLogs.filter { it.packageName == "com.example.app" }
        studio.applyAppPackageLogs("com.other", filtered, emptyList()) // stale -> ignored
        assertEquals(3, studio.mainLogRowCount)
        assertTrue(studio.appPackageFilterLoading)

        studio.applyAppPackageLogs("com.example.app", filtered, emptyList())
        assertFalse(studio.appPackageFilterLoading)
        assertEquals(2, studio.mainLogRowCount)
        assertTrue(studio.syncedRows.filter { it.kind == SyncedEventKind.LOG }.all { it.detail in setOf("m1", "m3") })

        studio.selectAppPackageByIndex(0)
        assertEquals(listOf<String?>("com.example.app", null), requested)
        assertNull(studio.selectedAppPackage)
    }

    @Test
    fun studioPerAppFilterHiddenForLegacyAndBundlesWithoutAppLogs() {
        val studio = ArbStudioController(onScrub = {}, onSeekExact = {}, onTrimExport = { _, _ -> }, onPngExport = { _, _ -> })
        studio.renderState(studioState(emptyList()), image = null)
        assertFalse(studio.isAppPackageFilterVisible)
        assertTrue(studio.appPackageFilterOptions.isEmpty())

        val legacy = ArbStudioController(onScrub = {}, onSeekExact = {}, onTrimExport = { _, _ -> }, onPngExport = { _, _ -> })
        legacy.renderState(
            studioState(listOf(AppLogSummary("com.example.app", "app-logs/x.jsonl", 1L, 0L, "aa")), legacy = true),
            image = null,
        )
        assertFalse(legacy.isAppPackageFilterVisible, "legacy (RO) bundles have no app-logs sidecars")
    }

    // ---------------- Part 2 audit regressions ----------------

    @Test
    fun settingsPreservesCaptureModeDeviceSerialAndScreenshotOverrideAndEditsCaptureMode(@TempDir shots: Path) {
        val baseline = ArbConfig(
            captureMode = CaptureMode.LOGS_ONLY,
            deviceSerial = "R3CN1234567",
            screenshotDirOverride = shots.toString(),
        )
        var written: Pair<ArbConfig, ArbSettingsDraft>? = null
        val configurable = ArbSettingsConfigurable(
            project = null,
            daemonConfigReader = { baseline },
            daemonConfigWriter = { base, draft ->
                written = base to draft
                base.copy(captureMode = draft.captureMode)
            },
            localBinaryReader = { "" },
            localBinaryWriter = {},
        )
        val form = configurable.createFormModel()
        assertEquals(CaptureMode.LOGS_ONLY, form.captureMode)
        assertFalse(
            configurable.isModified,
            "a daemon config with logs_only / pinned serial / CLI screenshot dir must not look modified",
        )
        val untouchedPatch = form.currentDraft().toConfigPatch(baseline)
        assertNull(untouchedPatch.captureMode)
        assertEquals(StringPatch.Unchanged, untouchedPatch.deviceSerial)
        assertEquals(OutputDirPatch.Unchanged, untouchedPatch.screenshotDirOverride)

        form.captureMode = CaptureMode.VIDEO_AND_LOGS
        assertTrue(configurable.isModified)
        configurable.apply()
        val (base, draft) = assertNotNull(written)
        val patch = draft.toConfigPatch(base)
        assertEquals(CaptureMode.VIDEO_AND_LOGS, patch.captureMode)
        assertEquals(StringPatch.Unchanged, patch.deviceSerial, "Apply must never clear the pinned device")
        assertEquals(OutputDirPatch.Unchanged, patch.screenshotDirOverride, "Apply must never clear screenshot_dir_override")
        assertFalse(configurable.isModified)
        configurable.disposeUIResources()
    }

    @Test
    fun toolWindowDisposeNeverInterruptsInFlightSharedIpcCall() {
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val interrupted = AtomicBoolean(false)
        val service = object : ArbdDaemonService() {
            override fun <T> withClient(block: (ArbdRpcClient) -> T): T {
                entered.countDown()
                try {
                    release.await(5, TimeUnit.SECONDS)
                } catch (e: InterruptedException) {
                    interrupted.set(true)
                    Thread.currentThread().interrupt()
                }
                throw IOException("offline")
            }
        }
        val controller = ArbToolWindowController(
            project = null,
            daemonService = service,
            onOpenBundle = { _, _, _ -> },
            pollIntervalMs = 0L,
            screenshotDirResolver = { null },
        )
        try {
            controller.refreshNow()
            assertTrue(entered.await(3, TimeUnit.SECONDS))
            controller.dispose()
            assertTrue(controller.isWorkerShutdown)
            Thread.sleep(150L)
            assertFalse(interrupted.get(), "dispose must not interrupt a worker blocked in shared IPC")
            // Post-dispose commands are dropped silently (no RejectedExecutionException to the EDT).
            controller.refreshNow()
            controller.captureScreenshot()
        } finally {
            release.countDown()
            controller.dispose()
            service.dispose()
        }
    }

    @Test
    fun latestValueMailboxCoalescesFramesToSingleDrain() {
        val box = LatestValueMailbox<Int>()
        assertTrue(box.offer(1), "first frame schedules a drain")
        assertFalse(box.offer(2), "pending drain already scheduled")
        assertFalse(box.offer(3))
        assertEquals(3, box.take(), "only the newest frame is rendered")
        assertNull(box.take())
        assertTrue(box.offer(4), "after a drain the next frame schedules again")
        assertEquals(4, box.take())
    }

    @Test
    fun uniqueKeysPreventDuplicateLazyColumnKeyCrash() {
        val crash = SyncedEventRow(SyncedEventKind.CRASH, seq = 9L, boottimeNs = 5L, relativeMs = null, frameIndex = null, title = "E", detail = "d")
        val log = crash.copy(kind = SyncedEventKind.LOG)
        val keys = ArbStudioController.uniqueKeys(listOf(crash, crash, log, crash), ArbStudioController::syncedRowKey)
        assertEquals(listOf("CRASH-9-5", "CRASH-9-5#2", "LOG-9-5", "CRASH-9-5#3"), keys)
        assertEquals(keys.size, keys.toSet().size)
    }

    @Test
    fun semanticTonesResolveOpaqueLightAndDarkVariantsWithoutJbColor() {
        val red = ArbJewelPalette.REC_RED
        assertEquals(0xD93848, red.rgb(isDark = false))
        assertEquals(0xE85A67, red.rgb(isDark = true))
        val color = ArbJewelPalette.opaque(red.rgb(isDark = true))
        assertEquals(1f, color.alpha)
        assertEquals((0xFF shl 24) or 0xE85A67, color.toArgb())
        assertEquals(0xD49B1A, ArbJewelPalette.TRIM_GOLD.rgb(isDark = false))
    }

    @Test
    fun aboutInfoShowsDaemonBuildIdForSamePathUpgradeDiagnostics() {
        val hello = SystemHelloResponse(
            protocolVersion = 1,
            schemaVersion = 1,
            pid = 99L,
            dataDir = "/d",
            cacheDir = "/c",
            runtimeDir = "/r",
            daemonVersion = "0.1.0",
            buildId = "0.1.0-123456-1700000000",
            binaryMtimeSecs = 1_700_000_000L,
        )
        val info = ArbAboutInfo.fromDaemonService("2.0.0", hello, connected = true)
        assertEquals("0.1.0-123456-1700000000", info.daemonBuildId)
        assertNull(ArbAboutInfo.fromDaemonService("2.0.0", null, connected = false).daemonBuildId)
    }

    @Test
    fun noLegacyBorderHexOrEmojiChromeInComposeSources() {
        val repoRoot = Path.of(System.getProperty("arb.repo.root"))
        val uiDir = repoRoot.resolve("ide-plugin/src/main/kotlin/io/github/shinma06/arb")
        val sources = Files.walk(uiDir).use { stream ->
            stream.filter { it.toString().endsWith(".kt") }.toList()
        }
        assertTrue(sources.isNotEmpty())
        val forbidden = listOf("LineBorder", "0xFF1E1F22", "0xFF2B2D30", "0xFF1A1A1A", "JBColor(")
        for (src in sources) {
            val text = Files.readString(src)
            for (token in forbidden) {
                assertFalse(text.contains(token), "$token found in ${src.fileName}")
            }
            assertFalse(text.contains(".shutdownNow()"), "shutdownNow() interrupts shared IPC in ${src.fileName}")
        }
    }
}
