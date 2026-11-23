package io.github.shinma06.arb.state

import io.github.shinma06.arb.ipc.ArbdProcessManager
import io.github.shinma06.arb.model.AdbDeviceInfo
import io.github.shinma06.arb.model.AdbDeviceState
import io.github.shinma06.arb.model.AppLogSummary
import io.github.shinma06.arb.model.ArbConfig
import io.github.shinma06.arb.model.ArbConfigPatch
import io.github.shinma06.arb.model.CaptureMode
import io.github.shinma06.arb.model.CaptureStartParams
import io.github.shinma06.arb.model.CaptureStatusView
import io.github.shinma06.arb.model.OutputDirPatch
import io.github.shinma06.arb.model.ProtocolJson
import io.github.shinma06.arb.model.RecoverySummaryView
import io.github.shinma06.arb.model.SaveScreenshotParams
import io.github.shinma06.arb.model.StringPatch
import io.github.shinma06.arb.model.SystemHelloResponse
import io.github.shinma06.arb.model.SystemStatusResponse
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.attribute.FileTime
import java.time.Instant
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

class DeviceSelectionPolicyTest {
    private fun device(serial: String, state: AdbDeviceState = AdbDeviceState.Device): AdbDeviceInfo =
        AdbDeviceInfo(serial = serial, state = state, model = "Pixel 9")

    private fun idleStatus(
        captureMode: CaptureMode = CaptureMode.VIDEO_AND_LOGS,
        active: Boolean = false,
        connected: Boolean = false,
        serial: String? = null,
        lastVideoError: String? = null,
    ): SystemStatusResponse = SystemStatusResponse(
        pid = 4242L,
        uptimeMs = 1_000L,
        capture = CaptureStatusView(
            active = active,
            connected = connected,
            serial = serial,
            storagePressureStopped = false,
            captureMode = captureMode,
            lastVideoError = lastVideoError,
            videoFrames = 0L,
            audioFrames = 0L,
            logRecords = 10L,
            tapRecords = 0L,
            discontinuities = 0L,
            staticHoldEvents = 0L,
        ),
        ringChunkCount = 1L,
        ringTotalBytes = 1024L,
        ringPinnedChunkCount = 0L,
        activeViewports = 0L,
        activeSaveJobs = 0L,
        recovery = RecoverySummaryView(),
    )

    @Test
    fun enforcesStrictNoAutoSwitchAndMultiDeviceGuard() {
        // 0 online -> NoOnlineDevices
        assertEquals(
            DeviceSelectionOutcome.NoOnlineDevices,
            DeviceSelectionPolicy.evaluate(null, listOf(device("R3CN1", AdbDeviceState.Offline))),
        )

        // Exactly 1 online with no preference -> auto-selected
        assertEquals(
            DeviceSelectionOutcome.Selected(serial = "192.168.1.50:5555", autoSelected = true),
            DeviceSelectionPolicy.evaluate(
                null,
                listOf(device("192.168.1.50:5555"), device("offline-1", AdbDeviceState.Offline)),
            ),
        )

        // 2+ online with no preference -> MultipleDevicesNeedSelection (NEVER auto-select!)
        val multi = DeviceSelectionPolicy.evaluate(
            null,
            listOf(device("emulator-5554"), device("192.168.1.50:5555")),
        )
        assertIs<DeviceSelectionOutcome.MultipleDevicesNeedSelection>(multi)
        assertEquals(listOf("emulator-5554", "192.168.1.50:5555"), multi.onlineSerials)

        // Bound preferred serial disconnects while another device is online -> stay bound, NEVER switch!
        val waiting = DeviceSelectionPolicy.evaluate(
            "192.168.1.50:5555",
            listOf(device("emulator-5554"), device("192.168.1.50:5555", AdbDeviceState.Offline)),
        )
        assertIs<DeviceSelectionOutcome.WaitingForPreferredDevice>(waiting)
        assertEquals("192.168.1.50:5555", waiting.preferredSerial)
        assertEquals(listOf("emulator-5554"), waiting.otherOnlineSerials)
    }

    @Test
    fun captureBarReducerEnforcesNoAutoSwitchAndSurfacesLogsOnlyAndEncoderBanner() {
        val twoDevices = listOf(device("emulator-5554"), device("192.168.1.50:5555"))

        // 1. Two devices online, no selection -> selectedSerial stays null, banner surfaced.
        val multiState = CaptureBarReducer.fromStatus(
            previous = CaptureBarState(),
            status = idleStatus(),
            devices = twoDevices,
        )
        assertNull(multiState.selectedSerial)
        assertNull(multiState.selectedDevice)
        assertTrue(multiState.multipleDevicesNeedSelection)
        assertEquals("端末を選択", multiState.statusBadgeText)
        assertTrue(multiState.banners.any { it.kind == CaptureBannerKind.CONFIG_WARNING && it.title.contains("複数端末") })

        // 2. User selects Wi-Fi device; later Wi-Fi device goes offline -> stays bound, NEVER switches to emulator-5554!
        val bound = multiState.copy(selectedSerial = "192.168.1.50:5555")
        val afterDisconnect = CaptureBarReducer.fromStatus(
            previous = bound,
            status = idleStatus(),
            devices = listOf(device("emulator-5554"), device("192.168.1.50:5555", AdbDeviceState.Offline)),
        )
        assertEquals("192.168.1.50:5555", afterDisconnect.selectedSerial)
        assertEquals("再接続待機中", afterDisconnect.statusBadgeText)
        assertFalse(afterDisconnect.isSelectedDeviceOnline)

        // 3. Sole idle unauthorized device stays visible with disabled online status, and idle
        //    auto-selected sole device transitions cleanly when swapped to another sole device.
        val unauthOnly = CaptureBarReducer.fromStatus(
            previous = CaptureBarState(),
            status = idleStatus(),
            devices = listOf(device("39101FDH200042", AdbDeviceState.Unauthorized)),
        )
        assertEquals("39101FDH200042", unauthOnly.selectedSerial)
        assertFalse(unauthOnly.isSelectedDeviceOnline)
        assertEquals("未承認", unauthOnly.statusBadgeText)

        val autoSingle = CaptureBarReducer.fromStatus(
            previous = CaptureBarState(),
            status = idleStatus(),
            devices = listOf(device("emulator-5554")),
        )
        assertEquals("emulator-5554", autoSingle.selectedSerial)
        val swappedSingle = CaptureBarReducer.fromStatus(
            previous = autoSingle,
            status = idleStatus(),
            devices = listOf(device("PIXEL9-USB-01")),
        )
        assertEquals("PIXEL9-USB-01", swappedSingle.selectedSerial)

        // 4. Active logs_only capture displays REC (ログのみ) and encoder warning banner when present.
        val logsOnlyRec = CaptureBarReducer.fromStatus(
            previous = CaptureBarState(),
            status = idleStatus(
                captureMode = CaptureMode.LOGS_ONLY,
                active = true,
                connected = true,
                serial = "192.168.1.50:5555",
                lastVideoError = "Possible H.264 hardware encoder contention (Running Devices mirroring)",
            ),
            devices = listOf(device("192.168.1.50:5555")),
        )
        assertTrue(logsOnlyRec.isLogsOnlyMode)
        assertEquals("REC (ログのみ)", logsOnlyRec.statusBadgeText)
        assertTrue(logsOnlyRec.banners.any { it.kind == CaptureBannerKind.STREAM_GAP && it.title.contains("映像エンコーダ") })
    }

    @Test
    fun validatesUsbWifiIpv6AndMdnsTlsSerialsAndRejectsUnsafeInputs() {
        val valid = listOf(
            "R3CN1234567",
            "emulator-5554",
            "192.168.1.50:5555",
            "[fe80::1]:5555",
            "[fe80::1%eth0]:5555",
            "adb-R3CN1234-AbCdEf._adb-tls-connect._tcp",
        )
        for (serial in valid) {
            assertTrue(DeviceSelectionPolicy.isValidAdbSerial(serial), "Expected valid serial: $serial")
        }

        val invalid = listOf(
            "",
            "192.168.1.50:5555 --extra",
            "../etc/passwd",
            "-s",
            "emu;rm -rf /",
            "emu\n5554",
        )
        for (serial in invalid) {
            assertFalse(DeviceSelectionPolicy.isValidAdbSerial(serial), "Expected invalid serial: $serial")
        }
    }

    @Test
    fun t6cProtocolJsonRoundtripsCaptureModeAppLogsConfigPatchAndHelloBuildId(@TempDir tmp: Path) {
        val helloJson = """
            {
              "protocol_version": 1,
              "schema_version": 2,
              "pid": 1234,
              "data_dir": "/tmp/d",
              "cache_dir": "/tmp/c",
              "runtime_dir": "/tmp/r",
              "daemon_version": "0.1.0",
              "build_id": "0.1.0-1048576-1700000000",
              "binary_mtime_secs": 1700000000
            }
        """.trimIndent()
        val hello = ProtocolJson.gson.fromJson(helloJson, SystemHelloResponse::class.java)
        assertEquals("0.1.0-1048576-1700000000", hello.buildId)
        assertEquals(1_700_000_000L, hello.binaryMtimeSecs)

        val patch = ArbConfigPatch(
            captureMode = CaptureMode.LOGS_ONLY,
            deviceSerial = StringPatch.Set("192.168.1.50:5555"),
            screenshotDirOverride = OutputDirPatch.Set("/tmp/logcat-shots"),
        )
        val patchObj = ProtocolJson.toConfigPatchJson(patch)
        assertEquals("logs_only", patchObj.get("capture_mode").asString)
        assertEquals("192.168.1.50:5555", patchObj.get("device_serial").asString)
        assertEquals("/tmp/logcat-shots", patchObj.get("screenshot_dir_override").asString)

        val clearPatchObj = ProtocolJson.toConfigPatchJson(
            ArbConfigPatch(
                deviceSerial = StringPatch.Clear,
                screenshotDirOverride = OutputDirPatch.Clear,
            ),
        )
        assertTrue(clearPatchObj.get("device_serial").isJsonNull)
        assertTrue(clearPatchObj.get("screenshot_dir_override").isJsonNull)

        val startJson = ProtocolJson.gson.toJsonTree(
            CaptureStartParams(serial = "192.168.1.50:5555", captureMode = CaptureMode.LOGS_ONLY),
        ).asJsonObject
        assertEquals("logs_only", startJson.get("capture_mode").asString)

        val shotJson = ProtocolJson.gson.toJsonTree(
            SaveScreenshotParams(serial = "192.168.1.50:5555", outputDir = "/tmp/logcat-shots"),
        ).asJsonObject
        assertEquals("/tmp/logcat-shots", shotJson.get("output_dir").asString)

        val appLogJson = """
            {
              "package_name": "com.example.shop",
              "relative_path": "app-logs/com.example.shop.jsonl",
              "line_count": 42,
              "crash_count": 1,
              "sha256": "${"a".repeat(64)}"
            }
        """.trimIndent()
        val appLog = ProtocolJson.gson.fromJson(appLogJson, AppLogSummary::class.java)
        assertEquals("com.example.shop", appLog.packageName)
        assertEquals("app-logs/com.example.shop.jsonl", appLog.relativePath)
        assertEquals(42L, appLog.lineCount)
        assertEquals(1L, appLog.crashCount)

        // Settings draft validates & diffs T-6C fields cleanly (using a native absolute path across all OSes).
        val nativeShotsDir = tmp.resolve("shots").toAbsolutePath().toString()
        val draft = ArbSettingsDraft.fromConfig(ArbConfig()).copy(
            captureMode = CaptureMode.LOGS_ONLY,
            deviceSerial = "adb-R3CN1234-AbCdEf._adb-tls-connect._tcp",
            screenshotDirOverride = nativeShotsDir,
        )
        assertNull(draft.validateDaemonFields())
        assertTrue(draft.isDaemonModifiedAgainst(ArbConfig()))
        val badDraft = draft.copy(deviceSerial = "bad;serial")
        assertNotNull(badDraft.validateDaemonFields())
    }

    @Test
    fun autoUpgradeDetectsSamePathInPlaceBinaryReplacementUsingHelloMtime(@TempDir tmp: Path) {
        val bin = Files.writeString(tmp.resolve("arbd"), "v2")
        Files.setLastModifiedTime(bin, FileTime.from(Instant.ofEpochSecond(1_700_001_000L)))

        assertFalse(
            ArbdProcessManager.shouldAutoUpgradeBinary(bin, bin, runningBinaryMtimeSecs = null),
            "without hello mtime, same file must not restart",
        )
        assertFalse(
            ArbdProcessManager.shouldAutoUpgradeBinary(bin, bin, runningBinaryMtimeSecs = 1_700_001_000L),
            "equal mtime must not restart",
        )
        assertTrue(
            ArbdProcessManager.shouldAutoUpgradeBinary(bin, bin, runningBinaryMtimeSecs = 1_700_000_000L),
            "newer on-disk mtime at the same path must trigger seamless auto-upgrade",
        )
    }
    @Test
    fun `serial validation mirrors Rust validate_adb_serial exactly`() {
        // Same accept / reject vectors as `arb_core::device` tests (GUI / CLI / daemon parity).
        val valid = listOf(
            "emulator-5554",
            "R3CN1234567",
            "192.168.1.50:5555",
            "[fe80::1]:5555",
            "[2001:db8::42]:37891",
            "[fe80::1]",
            "[fe80::1%wlan0]:5555",
            "adb-R3CN1234-AbCdEf._adb-tls-connect._tcp",
            "adb-2A121FDH200ABC-xYz123._adb-tls-connect._tcp.",
            "a".repeat(255),
        )
        for (serial in valid) {
            assertTrue(DeviceSelectionPolicy.isValidAdbSerial(serial), "expected valid: $serial")
        }
        val invalid = listOf(
            "", " ", "emulator-5554;rm -rf /", "192.168.1.50:5555 && id", "dev|cat",
            "dev\$(whoami)", "dev`id`", "dev\nline2", "dev\u0000null", "../escape", "foo..bar",
            "-flag-injection", "-e", "--help", "foo/bar", "foo\\bar", "[fe80::1", "fe80::1]:5555",
            "[]:5555", "[fe80::1]:abc", "[fe80::1]:", "[fe80::1]x", "x[fe80::1]", "[[a]]", "a b",
            // ASCII only; '%' only as a single IPv6 zone-id separator inside brackets.
            "émulator-5554", "端末1", "١٢٣", "dev%41", "[%eth0]:5555", "[fe80::1%]:5555",
            "[fe80::1%a%b]:5555", "[fe80::1]%eth0",
            "a".repeat(256),
        )
        for (serial in invalid) {
            assertFalse(DeviceSelectionPolicy.isValidAdbSerial(serial), "expected invalid: ${serial.take(40)}")
        }
    }

    @Test
    fun `online serials are trimmed and blank entries ignored like Rust`() {
        assertEquals(
            DeviceSelectionOutcome.Selected(serial = "emulator-5554", autoSelected = true),
            DeviceSelectionPolicy.evaluateOnlineSerials(null, listOf(" emulator-5554 ", "  ")),
        )
        assertEquals(
            DeviceSelectionOutcome.Selected(serial = "emulator-5554", autoSelected = false),
            DeviceSelectionPolicy.evaluateOnlineSerials(" emulator-5554", listOf("emulator-5554 ", "R3CN")),
        )
    }
}
