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
import io.github.shinma06.arb.model.CaptureStatusView
import io.github.shinma06.arb.model.DeviceListResponse
import io.github.shinma06.arb.model.ProtocolJson
import io.github.shinma06.arb.model.RecoverySummaryView
import io.github.shinma06.arb.model.SaveJobState
import io.github.shinma06.arb.model.SaveOutcomeResponse
import io.github.shinma06.arb.model.SystemStatusResponse
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
