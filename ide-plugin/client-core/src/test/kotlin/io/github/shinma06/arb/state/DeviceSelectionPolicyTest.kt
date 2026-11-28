package io.github.shinma06.arb.state

import com.google.gson.JsonParser
import io.github.shinma06.arb.ipc.ArbdProcessManager
import io.github.shinma06.arb.model.AdbDeviceInfo
import io.github.shinma06.arb.model.AdbDeviceState
import io.github.shinma06.arb.model.AppLogSummary
import io.github.shinma06.arb.model.ArbConfig
import io.github.shinma06.arb.model.ArbConfigPatch
import io.github.shinma06.arb.model.CaptureMode
import io.github.shinma06.arb.model.CaptureStartParams
import io.github.shinma06.arb.model.CaptureStatusView
import io.github.shinma06.arb.model.GestureKind
import io.github.shinma06.arb.model.OutputDirPatch
import io.github.shinma06.arb.model.ProtocolJson
import io.github.shinma06.arb.model.RecordBucket
import io.github.shinma06.arb.model.RecoverySummaryView
import io.github.shinma06.arb.model.SaveScreenshotParams
import io.github.shinma06.arb.model.StringPatch
import io.github.shinma06.arb.model.SystemHelloResponse
import io.github.shinma06.arb.model.SystemStatusResponse
import io.github.shinma06.arb.model.TapRecord
import io.github.shinma06.arb.model.TimeAlignment
import io.github.shinma06.arb.model.TimestampSemantics
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.io.File
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.attribute.FileTime
import java.time.Instant
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
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

    /**
     * #583 two-phase explicit start, judged with the Kotlin policy over the shared rows (Rust
     * `capture_intent_rows_follow_two_phase_explicit_start` checks the same rows). An explicit
     * serial is pinned only when the policy selects it; a failed start changes nothing; while
     * recording, only the active serial (or none) succeeds.
     */
    @Test
    fun captureIntentRowsFollowTwoPhaseExplicitStart() {
        val root = System.getProperty("arb.repo.root") ?: error("arb.repo.root is not set")
        val doc = JsonParser.parseString(File(root, "docs/specs/device-selection-policy-table.json").readText()).asJsonObject
        val rows = doc.getAsJsonArray("capture_intent_rows").map { it.asJsonObject }
        assertTrue(rows.size >= 9)
        assertTrue(rows.map { it["id"].asString }.containsAll(listOf("CI-06", "CI-07", "CI-08", "CI-09")))
        fun com.google.gson.JsonObject.str(key: String): String? = get(key)?.takeUnless { it.isJsonNull }?.asString
        for (row in rows) {
            val id = row["id"].asString
            val online = row.getAsJsonArray("online").map { device(it.asString) }
            val argument = row.str("argument")
            val active = row.str("active")
            val expected = when {
                active != null && argument != null && argument != active -> "conflict"
                active != null -> "already_active"
                argument != null -> when (DeviceSelectionPolicy.evaluate(argument, online)) {
                    is DeviceSelectionOutcome.Selected -> "started"
                    else -> "device_not_found"
                }
                else -> when (DeviceSelectionPolicy.evaluate(row.str("configured"), online)) {
                    is DeviceSelectionOutcome.Selected -> "started"
                    else -> "waiting"
                }
            }
            assertEquals(expected, row.str("outcome"), "$id: outcome")
            val failed = expected == "conflict" || expected == "device_not_found"
            if (failed) {
                assertEquals(row.str("configured"), row.str("persisted"), "$id: serial unchanged")
                assertEquals(row["enabled"].asBoolean, row["persisted_enabled"].asBoolean, "$id: intent unchanged")
                assertEquals(0, row.getAsJsonObject("config_changed").size(), "$id: no config_changed")
            } else {
                assertTrue(row["persisted_enabled"].asBoolean, "$id: a start arms capture")
                val pinned = argument ?: row.str("configured")
                assertEquals(pinned, row.str("persisted"), "$id: persisted serial")
                if (argument != null) {
                    assertTrue(online.any { it.serial == argument }, "$id: pinned only after Selected")
                }
            }
        }
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

    @Test
    fun t6hTapRecordGestureKindJsonRoundtripsAndDegradesLegacyBundlesToTap() {
        // A legacy bundle's `taps` row (saved before T-6H item 2) never had gesture_kind/end_x/
        // end_y/end_t_offset_ns/path on the wire at all, and `arbd` omits an empty `path` even on
        // current bundles (`skip_serializing_if = "Vec::is_empty"`). Both must degrade cleanly.
        val legacyTapJson = """
            {
              "seq": 1,
              "boot_id": "boot-legacy",
              "event_mono_ns": 1000,
              "pc_ingest_cont_ns": 2000,
              "alignment": {
                "certainty": "anchored",
                "boot_id": "boot-legacy",
                "boottime_ns": 1000,
                "half_width_ns": 5,
                "semantics": "input_event_time"
              },
              "bucket": "main",
              "action": "DOWN",
              "x": 10.0,
              "y": 20.0,
              "pointer_id": 0
            }
        """.trimIndent()
        val legacyTap = ProtocolJson.gson.fromJson(legacyTapJson, TapRecord::class.java)
        assertEquals(GestureKind.TAP, legacyTap.gestureKind, "missing gesture_kind must degrade to Tap")
        assertNull(legacyTap.endX)
        assertNull(legacyTap.endY)
        assertNull(legacyTap.endTOffsetNs)
        assertEquals(emptyList(), legacyTap.path, "missing path must degrade to an empty list, never null")

        // A current Swipe row carries a decimated path and a non-null end point.
        val swipeJson = """
            {
              "seq": 2,
              "boot_id": "boot-current",
              "event_mono_ns": 5000,
              "pc_ingest_cont_ns": 6000,
              "alignment": {
                "certainty": "anchored",
                "boot_id": "boot-current",
                "boottime_ns": 5000,
                "half_width_ns": 5,
                "semantics": "input_event_time"
              },
              "bucket": "main",
              "action": "SWIPE",
              "x": 50.0,
              "y": 900.0,
              "pointer_id": 1,
              "gesture_kind": "swipe",
              "end_x": 50.0,
              "end_y": 200.0,
              "end_t_offset_ns": 180000000,
              "path": [
                {"x": 50.0, "y": 900.0, "t_offset_ns": 0},
                {"x": 50.0, "y": 200.0, "t_offset_ns": 180000000}
              ]
            }
        """.trimIndent()
        val swipeTap = ProtocolJson.gson.fromJson(swipeJson, TapRecord::class.java)
        assertEquals(GestureKind.SWIPE, swipeTap.gestureKind)
        assertEquals(RecordBucket.MAIN, swipeTap.bucket)
        assertEquals(50.0, swipeTap.endX)
        assertEquals(200.0, swipeTap.endY)
        assertEquals(180_000_000L, swipeTap.endTOffsetNs)
        assertEquals(2, swipeTap.path.size)
        assertEquals(900.0, swipeTap.path[0].y)
        assertEquals(180_000_000L, swipeTap.path[1].tOffsetNs)

        // True serialization -> deserialization round-trip preserves all Swipe fields.
        val roundTripped = ProtocolJson.gson.fromJson(
            ProtocolJson.gson.toJson(swipeTap),
            TapRecord::class.java,
        )
        assertEquals(swipeTap, roundTripped)

        // Explicit JSON nulls for optional T-6H fields must also degrade cleanly without NPE.
        val explicitNullJson = """
            {
              "seq": 4,
              "pc_ingest_cont_ns": 8000,
              "alignment": {
                "certainty": "anchored",
                "boot_id": "boot-null",
                "boottime_ns": 8000,
                "half_width_ns": 5,
                "semantics": "input_event_time"
              },
              "bucket": "main",
              "action": "TAP",
              "gesture_kind": null,
              "end_x": null,
              "end_y": null,
              "end_t_offset_ns": null,
              "path": null
            }
        """.trimIndent()
        val explicitNullTap = ProtocolJson.gson.fromJson(explicitNullJson, TapRecord::class.java)
        assertEquals(GestureKind.TAP, explicitNullTap.gestureKind)
        assertEquals(emptyList(), explicitNullTap.path)

        // Missing required alignment must throw JsonParseException rather than NullPointerException.
        val missingAlignmentJson = """{"seq": 5, "pc_ingest_cont_ns": 9000, "bucket": "main", "action": "TAP"}"""
        assertFailsWith<com.google.gson.JsonParseException> {
            ProtocolJson.gson.fromJson(missingAlignmentJson, TapRecord::class.java)
        }

        // An ordinary Kotlin construction (not via JSON) still honours the constructor defaults.
        val constructed = TapRecord(
            seq = 3,
            pcIngestContNs = 7_000L,
            alignment = TimeAlignment.Anchored("boot-x", 7_000L, 5L, TimestampSemantics.INPUT_EVENT_TIME),
            bucket = RecordBucket.MAIN,
            action = "TAP",
        )
        assertEquals(GestureKind.TAP, constructed.gestureKind)
        assertEquals(emptyList(), constructed.path)
    }
}
