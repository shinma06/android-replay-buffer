package io.github.shinma06.arb.state

import com.google.gson.JsonArray
import com.google.gson.JsonElement
import com.google.gson.JsonNull
import com.google.gson.JsonObject
import com.google.gson.JsonParser
import io.github.shinma06.arb.model.ArbConfig
import io.github.shinma06.arb.model.BundleEntryView
import io.github.shinma06.arb.model.BundleForgetResponse
import io.github.shinma06.arb.model.BundleGcResponse
import io.github.shinma06.arb.model.CaptureMode
import io.github.shinma06.arb.model.CaptureStartResponse
import io.github.shinma06.arb.model.CaptureStatusView
import io.github.shinma06.arb.model.DeviceListResponse
import io.github.shinma06.arb.model.ExportShareMp4Response
import io.github.shinma06.arb.model.ExportTrimResponse
import io.github.shinma06.arb.model.ProtocolJson
import io.github.shinma06.arb.model.RecoverySummaryView
import io.github.shinma06.arb.model.SaveJobState
import io.github.shinma06.arb.model.SaveOutcomeResponse
import io.github.shinma06.arb.model.SystemStatusResponse
import io.github.shinma06.arb.model.TimelineLogsResponse
import org.junit.jupiter.api.Test
import java.io.File
import kotlin.test.assertEquals
import kotlin.test.assertTrue

/**
 * PDD SSOT contract tables (`docs/specs` JSON tables) checked row by row against the client-core
 * production policy, wire parsing and view models. Rust checks the same rows
 * (`arb-core`/`arbd`/`arb-cli` `pdd_contract_tables`).
 */
class PddContractTablesTest {
    private fun table(name: String): JsonObject {
        val root = System.getProperty("arb.repo.root") ?: error("arb.repo.root is not set")
        val doc = JsonParser.parseString(File(root, "docs/specs/$name").readText()).asJsonObject
        assertEquals(1, doc["schema_version"].asInt, "$name schema_version")
        return doc
    }

    private fun JsonObject.rows(key: String, min: Int): List<JsonObject> {
        val rows = getAsJsonArray(key).map { it.asJsonObject }
        assertTrue(rows.size >= min, "`$key` needs at least $min rows, has ${rows.size}")
        val ids = rows.mapNotNull { it["id"]?.asString }
        assertEquals(ids.size, ids.toSet().size, "duplicate ids in `$key`")
        return rows
    }

    private fun JsonObject.str(key: String): String? = get(key)?.takeUnless { it.isJsonNull }?.asString

    @Test
    fun silentSuccessWarningsSurviveTypedRpcDecoding() {
        val rows = table("ui-feedback-contract-table.json").rows("silent_success_rows", 6)
        for (row in rows.filter { it.has("warning") }) {
            val code = row["warning"].asString
            val json = """{"warnings":[{"code":"$code","message":"通知","details":{"burned_tap_count":0}}],"burned_tap_count":0}"""
            val warnings = when (row["tool"].asString) {
                "arb_query_logs" -> ProtocolJson.gson.fromJson(json, TimelineLogsResponse::class.java).warnings
                "arb_export_trim" -> when (row["arguments"].asJsonObject["format"].asString) {
                    "bundle" -> ProtocolJson.gson.fromJson(json, ExportTrimResponse::class.java).run {
                        assertEquals(0, burnedTapCount)
                        warnings
                    }
                    else -> ProtocolJson.gson.fromJson(json, ExportShareMp4Response::class.java).run {
                        assertEquals(0, burnedTapCount)
                        warnings
                    }
                }
                else -> continue
            }
            assertEquals(code, warnings?.single()?.code)
            assertEquals(0, warnings?.single()?.details?.get("burned_tap_count")?.asInt)
        }
    }

    private fun idleStatus(): SystemStatusResponse = SystemStatusResponse(
        pid = 1L,
        uptimeMs = 1L,
        capture = CaptureStatusView(
            active = false,
            connected = false,
            serial = null,
            storagePressureStopped = false,
            captureMode = CaptureMode.VIDEO_AND_LOGS,
            lastVideoError = null,
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

    private fun strings(list: List<String>): JsonArray = JsonArray().apply { list.forEach { add(it) } }

    /** Same JSON shape as Rust `serde_json::to_value(DeviceSelectionOutcome)` plus `auto_selected`. */
    private fun DeviceSelectionOutcome.toWire(): JsonObject = JsonObject().apply {
        when (val o = this@toWire) {
            is DeviceSelectionOutcome.Selected -> {
                addProperty("status", "selected")
                addProperty("serial", o.serial)
                addProperty("auto_selected", o.autoSelected)
            }
            DeviceSelectionOutcome.NoOnlineDevices -> addProperty("status", "no_online_devices")
            is DeviceSelectionOutcome.MultipleDevicesNeedSelection -> {
                addProperty("status", "multiple_devices_need_selection")
                add("online_serials", strings(o.onlineSerials))
            }
            is DeviceSelectionOutcome.WaitingForPreferredDevice -> {
                addProperty("status", "waiting_for_preferred_device")
                addProperty("preferred_serial", o.preferredSerial)
                add("other_online_serials", strings(o.otherOnlineSerials))
            }
        }
    }

    @Test
    fun deviceSelectionRowsMatchKotlinPolicyAndCaptureBarProjection() {
        for (row in table("device-selection-policy-table.json").rows("selection_rows", 27)) {
            val id = row["id"].asString
            val preferred = row.str("preferred_serial")
            val armed = row["capture_enabled"].asBoolean
            val devices = ProtocolJson.gson.fromJson(
                JsonObject().apply { add("devices", row["devices"]) },
                DeviceListResponse::class.java,
            ).devices
            val expected = row.getAsJsonObject("expected")

            val outcome = DeviceSelectionPolicy.evaluate(preferred, devices)
            assertEquals(expected["selection"], outcome.toWire(), "$id: selection")
            val bar = CaptureBarReducer.fromStatus(
                previous = CaptureBarState(),
                status = idleStatus(),
                devices = devices,
                config = ArbConfig(deviceSerial = preferred, captureEnabled = armed),
            )
            val ide = expected.getAsJsonObject("ide")
            assertEquals(ide["has_online_device"].asBoolean, bar.hasOnlineDevice, "$id: valid online device")
            assertEquals(ide.str("selected_serial"), bar.selectedSerial, "$id: selectedSerial")
            assertEquals(ide["selected_device_online"].asBoolean, bar.isSelectedDeviceOnline, "$id: selected device online")
            assertEquals(outcome, bar.deviceSelection, "$id: reducer uses the shared policy")
            assertEquals(armed, bar.config.captureEnabled, "$id: capture_enabled is never dropped")
        }
    }

    @Test
    fun captureIntentRowsDecodePersistedPreferencesAndChanges() {
        for (row in table("device-selection-policy-table.json").rows("capture_intent_rows", 9)) {
            val changes = row.getAsJsonObject("config_changed")
            val outcome = row.str("outcome")
            val failed = outcome == "device_not_found" || outcome == "conflict"
            if (failed) {
                // #583: a rejected start reports no change and keeps the stored pair.
                assertEquals(0, changes.size(), "${row.str("id")}: failure changes nothing")
                assertEquals(row.str("configured"), row.str("persisted"), "${row.str("id")}")
                assertEquals(row["enabled"].asBoolean, row["persisted_enabled"].asBoolean, "${row.str("id")}")
            }
            val wire = JsonObject().apply {
                addProperty("serial", row.str("persisted").orEmpty())
                addProperty("started", outcome == "started" || outcome == "already_active")
                add("serial_source", row["serial_source"])
                add("config_changed", changes)
                if (outcome == "already_active") addProperty("already_active", true)
                row.getAsJsonArray("warning_codes")?.takeIf { it.size() > 0 }?.let { codes ->
                    add("warnings", JsonArray().apply {
                        codes.forEach { code ->
                            add(JsonObject().apply {
                                add("code", code)
                                addProperty("message", "待機中")
                            })
                        }
                    })
                }
            }
            val start = ProtocolJson.gson.fromJson(wire, CaptureStartResponse::class.java)
            assertEquals(outcome == "already_active", start.alreadyActive == true, "${row.str("id")}: already_active")
            assertEquals(
                row.getAsJsonArray("warning_codes")?.map { it.asString }.orEmpty(),
                start.warnings.orEmpty().map { it.code },
                "${row.str("id")}: warnings",
            )
            assertEquals(row.str("serial_source"), start.serialSource)
            assertEquals(changes.getAsJsonArray("capture_enabled")?.map { it.asBoolean }, start.configChanged?.captureEnabled)
            assertEquals(changes.getAsJsonArray("device_serial")?.map { it.takeUnless { value -> value.isJsonNull }?.asString }, start.configChanged?.deviceSerial)
            val status = ProtocolJson.gson.toJsonTree(idleStatus()).asJsonObject.apply {
                add("configured_device_serial", row["persisted"])
                addProperty("serial_mismatch", false)
            }
            val decoded = ProtocolJson.gson.fromJson(status, SystemStatusResponse::class.java)
            assertEquals(row.str("persisted"), decoded.configuredDeviceSerial)
            assertEquals(false, decoded.serialMismatch)
        }
    }

    @Test
    fun serialValidationRowsMatchKotlinValidator() {
        for (row in table("device-selection-policy-table.json").rows("serial_validation_rows", 46)) {
            val serial = row.str("serial") ?: row.getAsJsonObject("serial_repeat").let {
                it["unit"].asString.repeat(it["count"].asInt)
            }
            assertEquals(
                row["valid"].asBoolean,
                DeviceSelectionPolicy.isValidAdbSerial(serial),
                "serial ${serial.take(40)} (${row["why"].asString})",
            )
        }
    }

    @Test
    fun saveRowsDecideSuccessExactlyLikeCliAndMcp() {
        for (row in table("ui-feedback-contract-table.json").rows("save_rows", 7)) {
            val id = row["id"].asString
            val expected = row.getAsJsonObject("expected")
            val res = ProtocolJson.gson.fromJson(row["response"], SaveOutcomeResponse::class.java)
            val success = res.savedBundleId != null
            assertEquals(expected["ide_feedback"].asString == "success", success, "$id: IDE success")
            assertEquals(expected["mcp_is_error"].asBoolean, !success, "$id: IDE agrees with MCP isError")
            assertEquals(expected["cli_exit_code"].asInt == 0, success, "$id: IDE agrees with CLI exit code")
            assertEquals(row["response"].asJsonObject["job"].asJsonObject["job_id"].asString, res.job.jobId, "$id: job_id")
            val recovery = FailedSaveRecoveryReducer.withJobs(FailedSaveRecoveryState(), listOf(res.job))
            assertEquals(expected["ide_recovery_lists_job"].asBoolean, recovery.hasRecoverableFailures, "$id: recovery panel")
            assertEquals(res.job.state == SaveJobState.FAILED_RETRYABLE, recovery.hasRecoverableFailures, id)
        }
    }

    @Test
    fun saveEvidenceDecodesFromSharedRows() {
        val doc = table("ui-feedback-contract-table.json")
        val base = doc.rows("save_rows", 7).first()["response"].asJsonObject
        for (row in doc.rows("capture_evidence_rows", 9)) {
            val wire = base.deepCopy()
            for ((name, value) in row.entrySet()) wire.add(name, value)
            val res = ProtocolJson.gson.fromJson(wire, SaveOutcomeResponse::class.java)
            row["notification_warning"]?.let { expected ->
                assertEquals(expected.asString, res.notificationWarning)
            }
            assertEquals(row["warning_codes"].asJsonArray.map { it.asString }, res.warningCodes)
            row["logcat_completeness"]?.asJsonObject?.let { evidence ->
                assertEquals(evidence["overflow_events"].asLong, res.logcatCompleteness?.overflowEvents)
                assertEquals(evidence["dropped_count"].asLong, res.logcatCompleteness?.droppedCount)
            }
            assertEquals(row["video_shortfall"].asBoolean, res.videoShortfall)
            assertEquals(row["log_shortfall"].asBoolean, res.logShortfall)
            assertEquals(row["window_shortfall"].asBoolean, res.windowShortfall)
            assertEquals(row["actual_log_duration_ms"].asLong, res.actualLogDurationMs)
            assertEquals(row["actual_video_duration_ms"].takeUnless { it.isJsonNull }?.asLong, res.actualVideoDurationMs)
            assertEquals(row["requested_duration_secs"].asLong, res.requestedDurationSecs)
            assertEquals(row["post_roll_ms"]?.asLong ?: 0L, res.postRollMs)
            assertEquals(row["actual_post_roll_ms"]?.asLong ?: 0L, res.actualPostRollMs)
            assertEquals(row["post_roll_truncated"]?.asBoolean ?: false, res.postRollTruncated)
            // #570: warnings and warning_codes are the same-length, same-order projection of warning_details.
            val id = row["id"].asString
            val details = res.warningDetails.orEmpty()
            assertEquals(res.warningCodes.orEmpty().size, res.warnings.orEmpty().size, "$id: warnings 1:1")
            assertEquals(res.warningCodes.orEmpty(), details.map { it.code }, "$id: details codes")
            assertEquals(res.warnings.orEmpty(), details.map { it.message }, "$id: details messages")
            assertTrue(details.none { it.message.isBlank() }, "$id: every code has a message")
            assertEquals(
                res.windowShortfall,
                "window_shortfall" in res.warningCodes.orEmpty(),
                "$id: window_shortfall has its own message",
            )
            assertEquals(row["log_coverage_ms"].asLong, res.logCoverageMs, "$id: log coverage")
            row["carried_in_frame"]?.let { assertEquals(it.asBoolean, res.carriedInFrame, "$id: carried in") }
            row["static_hold_ms"]?.let { assertEquals(it.asLong, res.staticHoldMs, "$id: static hold") }
            row["video_coverage_ms"]?.let { assertEquals(it.asLong, res.videoCoverageMs, "$id: video coverage") }
            row["info_codes"]?.let { codes ->
                assertEquals(codes.asJsonArray.map { it.asString }, res.infoCodes, "$id: info codes")
                // Static holds are information, never a warning.
                assertTrue("video_static_hold" !in res.warningCodes.orEmpty(), "$id: static hold is not a warning")
            }
        }
    }

    @Test
    fun diskStateRowsProjectToCatalogItems() {
        for (row in table("ui-feedback-contract-table.json").rows("disk_state_rows", 4)) {
            val id = row["id"].asString
            val expected = row.getAsJsonObject("expected")
            val entry = ProtocolJson.gson.fromJson(row["entry"], BundleEntryView::class.java)
            assertEquals(expected["effective_disk_state"].asString, entry.diskState.wire, "$id: disk_state")
            val item = BundleCatalogState.fromEntries(listOf(entry)).items.single()
            assertEquals(expected["can_open"].asBoolean, item.canOpen, "$id: canOpen")
            assertEquals(expected["can_forget"].asBoolean, item.canForget, "$id: canForget")
            val badge = expected.str("badge")
            val badges = setOf(
                BundleCatalogItem.MISSING_ON_DISK_BADGE_TEXT,
                BundleCatalogItem.STORAGE_UNAVAILABLE_BADGE_TEXT,
            )
            assertEquals(listOfNotNull(badge), item.semanticTags.filter { it in badges }, "$id: badge")
        }
    }

    private fun assertNoBucketDropped(id: String, wire: JsonObject, labels: JsonObject, parsed: Map<String, Int>) {
        assertEquals(labels.keySet(), parsed.keys, "$id: client models every bucket")
        for ((field, size) in parsed) {
            val raw: JsonElement = wire[field] ?: JsonNull.INSTANCE
            assertEquals(if (raw.isJsonArray) raw.asJsonArray.size() else 0, size, "$id: `$field` parsed")
        }
    }

    @Test
    fun gcAndForgetRowsParseWithoutDroppingBuckets() {
        val doc = table("ui-feedback-contract-table.json")
        for (row in doc.rows("gc_rows", 13)) {
            val gc = ProtocolJson.gson.fromJson(row["response"], BundleGcResponse::class.java)
            assertNoBucketDropped(
                row["id"].asString,
                row.getAsJsonObject("response"),
                doc.getAsJsonObject("gc_bucket_labels"),
                mapOf(
                    "purged_bundle_ids" to gc.purgedBundleIds.size,
                    "skipped_leased_bundle_ids" to gc.skippedLeasedBundleIds.size,
                    "reaped_lease_ids" to gc.reapedLeaseIds.size,
                    "skipped_in_grace_bundle_ids" to gc.skippedInGraceBundleIds.size,
                    "skipped_unmanaged_or_legacy_bundle_ids" to gc.skippedUnmanagedOrLegacyBundleIds.size,
                    "skipped_active_job_bundle_ids" to gc.skippedActiveJobBundleIds.size,
                    "failed" to gc.failed.size,
                ),
            )
        }
        for (row in doc.rows("forget_rows", 9)) {
            val res = ProtocolJson.gson.fromJson(row["response"], BundleForgetResponse::class.java)
            assertNoBucketDropped(
                row["id"].asString,
                row.getAsJsonObject("response"),
                doc.getAsJsonObject("forget_bucket_labels"),
                mapOf(
                    "forgotten_bundle_ids" to res.forgottenBundleIds.size,
                    "skipped_present_bundle_ids" to res.skippedPresentBundleIds.size,
                    "skipped_storage_unavailable_bundle_ids" to res.skippedStorageUnavailableBundleIds.size,
                    "skipped_leased_bundle_ids" to res.skippedLeasedBundleIds.size,
                    "reaped_lease_ids" to res.reapedLeaseIds.size,
                    "skipped_active_job_bundle_ids" to res.skippedActiveJobBundleIds.size,
                ),
            )
        }
    }
}
