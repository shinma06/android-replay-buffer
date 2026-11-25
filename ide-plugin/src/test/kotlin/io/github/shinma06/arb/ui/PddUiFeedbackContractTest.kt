package io.github.shinma06.arb.ui

import com.google.gson.JsonObject
import com.google.gson.JsonParser
import io.github.shinma06.arb.ide.ArbdDaemonService
import io.github.shinma06.arb.ipc.ArbdRpcClient
import io.github.shinma06.arb.ipc.LocalIpcTransport
import io.github.shinma06.arb.model.ArbConfig
import io.github.shinma06.arb.model.BundleEntryView
import io.github.shinma06.arb.model.BundleGcResponse
import io.github.shinma06.arb.model.DeviceListResponse
import io.github.shinma06.arb.model.ProtocolJson
import io.github.shinma06.arb.model.SystemStatusResponse
import io.github.shinma06.arb.state.BundleCatalogState
import io.github.shinma06.arb.state.BundleFilter
import io.github.shinma06.arb.state.CaptureBarReducer
import io.github.shinma06.arb.state.CaptureBarState
import org.junit.jupiter.api.Test
import java.io.File
import java.io.IOException
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.LinkedBlockingQueue
import javax.swing.SwingUtilities
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

/**
 * PDD SSOT `docs/specs/ui-feedback-contract-table.json` (and the IDE part of
 * `device-selection-policy-table.json`) driven through the real headless
 * [ArbToolWindowController]: every row's notification class, editor opening, polling and
 * connection state must match the table that the Rust CLI/MCP/daemon tests also check.
 */
class PddUiFeedbackContractTest {
    private class ScriptTransport(
        private val respond: (id: Long, method: String, params: JsonObject) -> String?,
    ) : LocalIpcTransport {
        override val endpoint: String = "mock://pdd-contract"
        val calls = CopyOnWriteArrayList<Pair<String, JsonObject>>()
        private val responses = LinkedBlockingQueue<String>()

        override fun writeLine(jsonLine: String) {
            val obj = JsonParser.parseString(jsonLine).asJsonObject
            val method = obj["method"].asString
            val params = obj["params"]?.takeIf { it.isJsonObject }?.asJsonObject ?: JsonObject()
            calls += method to params
            when (val reply = respond(obj["id"].asLong, method, params)) {
                null -> responses.put(EOF)
                HANG -> Unit
                else -> responses.put(reply)
            }
        }

        override fun readLine(): String? = responses.take().takeUnless { it == EOF }
        override fun close() {
            responses.put(EOF)
        }

        fun targets(method: String, key: String): List<String> =
            calls.filter { it.first == method }.map { it.second[key].asString }
    }

    private class Harness(transport: ScriptTransport, readTimeoutMs: Long = 5_000L) {
        val rpc = ArbdRpcClient(transport, readTimeoutMs, readTimeoutMs, readTimeoutMs)
        val service = object : ArbdDaemonService() {
            override fun <T> withClient(block: (ArbdRpcClient) -> T): T = synchronized(this) { block(rpc) }
        }
        val infos = CopyOnWriteArrayList<String>()
        val warnings = CopyOnWriteArrayList<String>()
        val errors = CopyOnWriteArrayList<Pair<String, String?>>()
        val opened = CopyOnWriteArrayList<String>()
        val controller = ArbToolWindowController(
            project = null,
            daemonService = service,
            onOpenBundle = { id, _, _ -> opened += id },
            pollIntervalMs = 0L,
            onNotifyInfo = { infos += it },
            onNotifyWarning = { warnings += it },
            onNotifyActionFailed = { op, detail -> errors += (op to detail) },
        )

        fun settle() {
            controller.awaitWorkerForTest()
            SwingUtilities.invokeAndWait {}
        }

        fun close() {
            controller.dispose()
            rpc.close()
            service.dispose()
        }
    }

    private fun table(name: String): JsonObject {
        val root = System.getProperty("arb.repo.root") ?: error("arb.repo.root is not set")
        return JsonParser.parseString(File(root, "docs/specs/$name").readText()).asJsonObject
    }

    private fun JsonObject.rows(key: String): List<JsonObject> = getAsJsonArray(key).map { it.asJsonObject }
    private fun JsonObject.str(key: String): String? = get(key)?.takeUnless { it.isJsonNull }?.asString
    private fun JsonObject.strings(key: String): List<String> = getAsJsonArray(key).map { it.asString }

    private fun ok(id: Long, result: String): String = """{"jsonrpc":"2.0","id":$id,"result":$result}"""

    private fun waitUntil(diag: () -> String = { "" }, predicate: () -> Boolean) {
        val deadline = System.currentTimeMillis() + 5_000L
        while (System.currentTimeMillis() < deadline) {
            SwingUtilities.invokeAndWait {}
            if (predicate()) return
            Thread.sleep(10)
        }
        SwingUtilities.invokeAndWait {}
        assertTrue(predicate(), "condition not met: ${diag()}")
    }

    /** Answers the 2-second poll; [extra] handles the method under test. */
    private fun poll(
        bundlesJson: () -> String = { """{"bundles":[]}""" },
        extra: (Long, String, JsonObject) -> String?,
    ): (Long, String, JsonObject) -> String? = { id, method, params ->
        when (method) {
            "system.status" -> ok(id, STATUS_JSON)
            "config.get" -> ok(id, CONFIG_JSON)
            "device.list" -> ok(id, """{"devices":[]}""")
            "save.list_jobs" -> ok(id, """{"jobs":[]}""")
            "bundle.list" -> ok(id, bundlesJson())
            "timeline.summary" -> ok(id, SUMMARY_JSON)
            "timeline.crashes" -> ok(id, """{"crashes":[]}""")
            else -> extra(id, method, params)
        }
    }

    private fun assertSeverity(id: String, expected: String, h: Harness, text: String) {
        when (expected) {
            "info" -> assertEquals(listOf(text), h.infos.toList(), "$id: info notification")
            "warning" -> assertEquals(listOf(text), h.warnings.toList(), "$id: warning notification")
            else -> error("$id: unknown severity $expected")
        }
        assertEquals(1, h.infos.size + h.warnings.size, "$id: exactly one notification")
        assertTrue(h.errors.isEmpty(), "$id: a structured response is not an action failure: ${h.errors}")
    }

    private fun assertSummary(id: String, expected: JsonObject, labels: JsonObject, response: JsonObject, summary: String) {
        expected.str("summary_equals")?.let { assertEquals(it, summary, "$id: summary") }
        for (token in expected.strings("summary_tokens")) assertTrue(token in summary, "$id: `$token` missing in $summary")
        for ((field, label) in labels.entrySet()) {
            val n = response[field]?.takeIf { it.isJsonArray }?.asJsonArray?.size() ?: 0
            if (n > 0) {
                assertTrue(Regex(Regex.escape("${label.asString}$n") + """(?!\d)""").containsMatchIn(summary), "$id: non-zero `$field` silently omitted: $summary")
            } else {
                assertFalse(label.asString in summary, "$id: empty `$field` reported: $summary")
            }
        }
    }

    @Test
    fun gcRowsDriveEmptyTrashNotifications() {
        val doc = table("ui-feedback-contract-table.json")
        for (row in doc.rows("gc_rows")) {
            val id = row["id"].asString
            val response = row.getAsJsonObject("response")
            val trashed = """{"bundle_id":"b-t","path":"/data/bundles/b-t","format_version":"v2","state":"trashed","trashed_at_utc":"2026-11-25T00:00:00Z","created_at_utc":"2026-11-25T00:00:00Z","active_leases":0,"disk_state":"present"}"""
            val transport = ScriptTransport(
                poll({ """{"bundles":[$trashed]}""" }) { rid, method, _ ->
                    check(method == "bundle.gc") { "unexpected $method" }
                    ok(rid, response.toString())
                },
            )
            val h = Harness(transport)
            try {
                h.controller.applyStateForTest(
                    capture = CaptureBarState(daemonConnected = true),
                    catalog = BundleCatalogState.fromEntries(
                        listOf(ProtocolJson.gson.fromJson(trashed, BundleEntryView::class.java)),
                        activeFilter = BundleFilter.TRASH,
                    ),
                )
                SwingUtilities.invokeAndWait {
                    h.controller.requestEmptyTrash()
                }
                SwingUtilities.invokeAndWait { h.controller.confirmEmptyTrash() }
                waitUntil({ "$id: calls=${transport.calls.map { it.first }}" }) {
                    !h.controller.gcBusy && h.infos.size + h.warnings.size == 1
                }
                h.settle()
                val gc = ProtocolJson.gson.fromJson(response, BundleGcResponse::class.java)
                val (_, text) = ArbToolWindowController.formatGcNotification(gc)
                val expected = row.getAsJsonObject("expected")
                assertSeverity(id, expected["severity"].asString, h, text)
                val summary = h.controller.catalogNote.orEmpty()
                assertEquals(ArbToolWindowController.formatGcSummary(gc), summary, "$id: inline note")
                assertSummary(id, expected, doc.getAsJsonObject("gc_bucket_labels"), response, summary)
                for (hint in expected.strings("hint_tokens")) assertTrue(hint in text, "$id: hint `$hint` missing in $text")
                assertTrue(h.controller.captureState.daemonConnected, "$id")
            } finally {
                h.close()
            }
        }
    }

    @Test
    fun forgetRowsDriveForgetNotifications() {
        val doc = table("ui-feedback-contract-table.json")
        for (row in doc.rows("forget_rows")) {
            val id = row["id"].asString
            val response = row.getAsJsonObject("response")
            val transport = ScriptTransport(
                poll { rid, method, _ ->
                    check(method == "bundle.forget") { "unexpected $method" }
                    ok(rid, response.toString())
                },
            )
            val h = Harness(transport)
            try {
                h.controller.applyStateForTest(capture = CaptureBarState(daemonConnected = true))
                when (row["mode"].asString) {
                    "single" -> h.controller.forgetBundle("b-1")
                    "all_missing" -> h.controller.forgetAllMissingBundles()
                    else -> error("$id: unknown mode")
                }
                waitUntil({ "$id: calls=${transport.calls.map { it.first }}" }) {
                    !h.controller.forgetBusy && h.infos.size + h.warnings.size == 1
                }
                h.settle()
                val call = transport.calls.single { it.first == "bundle.forget" }.second
                if (row["mode"].asString == "single") {
                    assertEquals("b-1", call["bundle_id"].asString, id)
                } else {
                    assertEquals(true, call["all_missing"].asBoolean, id)
                }
                val expected = row.getAsJsonObject("expected")
                val summary = h.controller.catalogNote.orEmpty()
                assertSeverity(id, expected["severity"].asString, h, summary)
                assertSummary(id, expected, doc.getAsJsonObject("forget_bucket_labels"), response, summary)
            } finally {
                h.close()
            }
        }
    }

    @Test
    fun saveRowsOpenTheEditorOnlyForACompletedSave() {
        for (row in table("ui-feedback-contract-table.json").rows("save_rows")) {
            val id = row["id"].asString
            val response = row.getAsJsonObject("response")
            val transport = ScriptTransport(
                poll { rid, method, _ ->
                    check(method == "save.trigger") { "unexpected $method" }
                    ok(rid, response.toString())
                },
            )
            val h = Harness(transport)
            try {
                h.controller.applyStateForTest(capture = CaptureBarState(daemonConnected = true))
                SwingUtilities.invokeAndWait { h.controller.triggerPrimarySave() }
                waitUntil({ "$id: calls=${transport.calls.map { it.first }}" }) {
                    transport.calls.any { it.first == "save.trigger" } && !h.controller.saveBusy
                }
                h.settle()
                val expected = row.getAsJsonObject("expected")
                val opened = if (expected["ide_feedback"].asString == "success") listOf(response["bundle_id"].asString) else emptyList()
                assertEquals(opened, h.opened.toList(), "$id: editor opened only for a completed save")
                assertTrue(h.errors.isEmpty(), "$id: a save outcome is not an action failure: ${h.errors}")
                assertEquals(
                    expected["ide_daemon_connected"].asBoolean,
                    h.controller.captureState.daemonConnected,
                    "$id: a failed save never marks the daemon disconnected",
                )
            } finally {
                h.close()
            }
        }
    }

    @Test
    fun diskStateRowsGateTimelinePollingAndOpening() {
        val rows = table("ui-feedback-contract-table.json").rows("disk_state_rows")
        val entries = rows.map { row ->
            row.getAsJsonObject("entry").deepCopy().apply {
                val bid = row["id"].asString.lowercase()
                addProperty("bundle_id", bid)
                addProperty("path", "/data/bundles/$bid")
            }
        }
        val transport = ScriptTransport(
            poll({ """{"bundles":[${entries.joinToString(",")}]}""" }) { _, method, _ -> error("unexpected $method") },
        )
        val h = Harness(transport)
        try {
            h.controller.refreshNow()
            waitUntil { h.controller.displayedBundles.size == rows.size }
            h.settle()
            val polled = rows.filter { it.getAsJsonObject("expected")["timeline_polling"].asBoolean }
                .map { it["id"].asString.lowercase() }
            for (method in listOf("timeline.summary", "timeline.crashes")) {
                assertEquals(polled.sorted(), transport.targets(method, "bundle_id_or_path").sorted(), method)
            }
            for (row in rows) {
                val bid = row["id"].asString.lowercase()
                val expected = row.getAsJsonObject("expected")
                val item = h.controller.displayedBundles.single { it.bundleId == bid }
                h.opened.clear()
                h.warnings.clear()
                h.controller.openBundle(item)
                SwingUtilities.invokeAndWait {}
                val canOpen = expected["can_open"].asBoolean
                assertEquals(if (canOpen) listOf(bid) else emptyList(), h.opened.toList(), "${row["id"]}: open")
                assertEquals(if (canOpen) 0 else 1, h.warnings.size, "${row["id"]}: unavailable entry explains why")
                assertEquals(expected["can_forget"].asBoolean, item.canForget, "${row["id"]}: forget")
            }
            assertFalse(transport.calls.any { it.first == "bundle.forget" }, "never auto-forget")
        } finally {
            h.close()
        }
    }

    @Test
    fun errorRowsSplitBusinessErrorsFromTransportDisconnects() {
        for (row in table("ui-feedback-contract-table.json").rows("error_rows")) {
            val id = row["id"].asString
            val failure = row.getAsJsonObject("failure")
            val kind = failure["kind"].asString
            val transport = ScriptTransport { rid, method, _ ->
                check(method == "bundle.trash") { "unexpected $method" }
                when (kind) {
                    "rpc_error" -> """{"jsonrpc":"2.0","id":$rid,"error":{"code":${failure["code"].asInt},"message":"contract $id"}}"""
                    "eof" -> null
                    "io" -> throw IOException("Broken pipe")
                    "timeout" -> HANG
                    else -> error("$id: unknown failure kind")
                }
            }
            val h = Harness(transport, readTimeoutMs = if (kind == "timeout") 200L else 5_000L)
            try {
                h.controller.applyStateForTest(capture = CaptureBarState(daemonConnected = true))
                h.controller.trashBundle("b-1")
                waitUntil({ "$id: errors=${h.errors}" }) { h.errors.isNotEmpty() }
                h.settle()
                val expected = row.getAsJsonObject("expected")
                val operations = if (expected["ide_action_error_notified"].asBoolean) listOf("ゴミ箱へ移動") else emptyList()
                assertEquals(operations, h.errors.map { it.first }, "$id: action error surfaced")
                assertEquals(
                    expected["ide_daemon_connected"].asBoolean,
                    h.controller.captureState.daemonConnected,
                    "$id: daemon connection state",
                )
                assertEquals(
                    expected["ide_stale_daemon_banner"].asBoolean,
                    h.controller.staleDaemonMessage != null,
                    "$id: stale daemon banner",
                )
            } finally {
                h.close()
            }
        }
    }

    @Test
    fun deviceSelectionRowsProjectToTheSharedToggleAndToolbar() {
        val status = ProtocolJson.gson.fromJson(STATUS_JSON, SystemStatusResponse::class.java)
        for (row in table("device-selection-policy-table.json").rows("selection_rows")) {
            val id = row["id"].asString
            val armed = row["capture_enabled"].asBoolean
            val devices = ProtocolJson.gson.fromJson(
                JsonObject().apply { add("devices", row["devices"]) },
                DeviceListResponse::class.java,
            ).devices
            val bar = CaptureBarReducer.fromStatus(
                previous = CaptureBarState(),
                status = status,
                devices = devices,
                config = ArbConfig(deviceSerial = row.str("preferred_serial"), captureEnabled = armed),
            )
            val ide = row.getAsJsonObject("expected").getAsJsonObject("ide")
            val snapshot = bar.toToolbarSnapshot(captureEnabled = armed)
            assertEquals(armed, snapshot.captureEnabled, "$id: capture_enabled kept")
            assertEquals(ide["selected_device_online"].asBoolean, snapshot.hasOnlineDevice, "$id: can start now")
            assertEquals(ide.str("selected_serial"), bar.selectedSerial, "$id: selected serial")
            ide.str("hint_contains")?.let { token ->
                assertTrue(token in snapshot.disabledDeviceHint.orEmpty(), "$id: hint must identify $token")
                if (bar.selectedSerial == null) {
                    val first = ArbToolWindowController.buildDeviceDropdownEntries(bar).first()
                    assertEquals(null, first.serial, "$id: no device selected")
                    assertTrue(token in first.label, "$id: dropdown explains the empty selection")
                }
            }
            if (!snapshot.hasOnlineDevice) assertNotNull(snapshot.disabledDeviceHint, "$id: disabled start explains why")
            assertEquals(
                ide["toggle_state"].asString,
                CaptureToggleState.of(active = false, armed = armed, connected = false).name.lowercase(),
                "$id: toggle state",
            )
        }
    }

    private companion object {
        const val EOF = "\u0000eof"
        const val HANG = "\u0000hang"
        const val STATUS_JSON =
            """{"pid":77,"uptime_ms":10,"capture":{"active":false,"connected":false,"storage_pressure_stopped":false,"capture_mode":"video_and_logs","video_frames":0,"audio_frames":0,"log_records":0,"tap_records":0,"discontinuities":0,"static_hold_events":0},"ring_chunk_count":0,"ring_total_bytes":0,"ring_pinned_chunk_count":0,"buffered_secs":0,"active_viewports":0,"active_save_jobs":0,"recovery":{}}"""
        const val CONFIG_JSON =
            """{"config":{"config_version":1,"save_window_secs":300,"ring_budget_bytes":1610612736,"min_free_disk_bytes":3221225472,"video_max_size":1920,"video_max_fps":30,"audio_enabled":true,"capture_mode":"video_and_logs","crash_autosave_enabled":true,"crash_autosave_cooldown_secs":15,"ide_default_save_action":"save_and_open"},"config_path":"/tmp/config.json"}"""
        const val SUMMARY_JSON =
            """{"bundle_id":"x","path":"/x","format":"v2","bundle_kind":"full","is_legacy":false,"provenance":{"kind":"original"},"streams":{},"unanchored_summary":{"included_count":0,"truncated_count":0},"epochs":[],"frame_count":30,"keyframe_count":1,"log_main_count":0,"log_unanchored_count":0,"tap_main_count":0,"tap_unanchored_count":0,"discontinuity_count":0}"""
    }
}
