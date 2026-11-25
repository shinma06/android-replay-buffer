package io.github.shinma06.arb.ui

import com.google.gson.JsonObject
import com.google.gson.JsonParser
import io.github.shinma06.arb.ide.ArbdDaemonService
import io.github.shinma06.arb.ipc.ArbdRpcClient
import io.github.shinma06.arb.ipc.LocalIpcTransport
import io.github.shinma06.arb.model.BundleDiskState
import io.github.shinma06.arb.model.BundleEntryView
import io.github.shinma06.arb.model.BundleGcFailure
import io.github.shinma06.arb.model.BundleGcResponse
import io.github.shinma06.arb.model.BundleLifecycleState
import io.github.shinma06.arb.model.CatalogFormatVersion
import io.github.shinma06.arb.state.BundleCatalogState
import io.github.shinma06.arb.state.BundleFilter
import io.github.shinma06.arb.state.CaptureBarState
import org.junit.jupiter.api.Test
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.LinkedBlockingQueue
import javax.swing.SwingUtilities
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/**
 * Headless regression tests for Item #11:
 * - Manual bundle deletion reconciliation (`disk_state` 3-value visibility, polling suppression,
 *   non-destructive `bundle.forget` single & bulk flows, open guard).
 * - Empty Trash UX (disabled on 0 items, inline confirmation bar, `bundle.gc(ignore_grace=true)`,
 *   full skip/failure breakdown summary, balloon notifications, and error surfacing).
 */
class ArbCatalogReconcileAndEmptyTrashTest {
    private class RecordedCall(val method: String, val params: JsonObject)

    private class RecordingTransport(
        private val respond: (id: Long, method: String, params: JsonObject) -> String,
    ) : LocalIpcTransport {
        override val endpoint: String = "mock://item11"
        val calls = CopyOnWriteArrayList<RecordedCall>()
        private val responses = LinkedBlockingQueue<String>()

        override fun writeLine(jsonLine: String) {
            val obj = JsonParser.parseString(jsonLine).asJsonObject
            val id = obj.get("id").asLong
            val method = obj.get("method").asString
            val params = obj.get("params")?.takeIf { it.isJsonObject }?.asJsonObject ?: JsonObject()
            calls += RecordedCall(method, params)
            responses.put(respond(id, method, params))
        }

        override fun readLine(): String? = responses.take()
        override fun close() = Unit
    }

    private fun ok(id: Long, resultJson: String): String = """{"jsonrpc":"2.0","id":$id,"result":$resultJson}"""

    private fun stubService(transport: RecordingTransport): Pair<ArbdDaemonService, ArbdRpcClient> {
        val rpc = ArbdRpcClient(transport)
        val service = object : ArbdDaemonService() {
            override fun <T> withClient(block: (ArbdRpcClient) -> T): T = synchronized(this) { block(rpc) }
        }
        return service to rpc
    }

    private val statusJson =
        """{"pid":77,"uptime_ms":10,"capture":{"active":false,"connected":false,"storage_pressure_stopped":false,"capture_mode":"video_and_logs","video_frames":0,"audio_frames":0,"log_records":0,"tap_records":0,"discontinuities":0,"static_hold_events":0},"ring_chunk_count":0,"ring_total_bytes":0,"ring_pinned_chunk_count":0,"buffered_secs":0,"active_viewports":0,"active_save_jobs":0,"recovery":{}}"""
    private val configJson =
        """{"config":{"config_version":1,"save_window_secs":300,"ring_budget_bytes":1610612736,"min_free_disk_bytes":3221225472,"video_max_size":1920,"video_max_fps":30,"audio_enabled":true,"capture_mode":"video_and_logs","crash_autosave_enabled":true,"crash_autosave_cooldown_secs":15,"ide_default_save_action":"save_and_open"},"config_path":"/tmp/config.json"}"""

    private fun waitUntil(
        timeoutMs: Long = 3_000L,
        diag: () -> String = { "" },
        predicate: () -> Boolean,
    ) {
        val deadline = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < deadline) {
            SwingUtilities.invokeAndWait {}
            if (predicate()) return
            Thread.sleep(15)
        }
        SwingUtilities.invokeAndWait {}
        assertTrue(predicate(), "condition was not satisfied within ${timeoutMs}ms: ${diag()}")
    }

    @Test
    fun emptyTrashButtonDisabledWhenEmptyAndInlineConfirmationSendsIgnoreGraceWithFullBreakdown() {
        var bundlesJson = """{"bundles":[
            {"bundle_id":"b-managed","path":"/data/bundles/b-managed","format_version":"v2","state":"trashed","trashed_at_utc":"2026-11-25T00:00:00Z","created_at_utc":"2026-11-25T00:00:00Z","active_leases":0,"disk_state":"present"},
            {"bundle_id":"b-leased","path":"/data/bundles/b-leased","format_version":"v2","state":"trashed","trashed_at_utc":"2026-11-25T00:00:00Z","created_at_utc":"2026-11-25T00:00:00Z","active_leases":1,"disk_state":"present"},
            {"bundle_id":"b-ext","path":"/external/b-ext","format_version":"v2","state":"trashed","trashed_at_utc":"2026-11-25T00:00:00Z","created_at_utc":"2026-11-25T00:00:00Z","active_leases":0,"disk_state":"present"}
        ]}"""
        val transport = RecordingTransport { id, method, _ ->
            when (method) {
                "system.status" -> ok(id, statusJson)
                "config.get" -> ok(id, configJson)
                "device.list" -> ok(id, """{"devices":[]}""")
                "save.list_jobs" -> ok(id, """{"jobs":[]}""")
                "bundle.list" -> ok(id, bundlesJson)
                "timeline.summary" -> ok(id, """{"bundle_id":"x","path":"/x","format":"v2","bundle_kind":"full","is_legacy":false,"provenance":{"kind":"original"},"streams":{},"unanchored_summary":{"included_count":0,"truncated_count":0},"epochs":[],"frame_count":0,"keyframe_count":0,"log_main_count":0,"log_unanchored_count":0,"tap_main_count":0,"tap_unanchored_count":0,"discontinuity_count":0}""")
                "timeline.crashes" -> ok(id, """{"crashes":[]}""")
                "bundle.gc" -> {
                    bundlesJson = """{"bundles":[
                        {"bundle_id":"b-leased","path":"/data/bundles/b-leased","format_version":"v2","state":"trashed","trashed_at_utc":"2026-11-25T00:00:00Z","created_at_utc":"2026-11-25T00:00:00Z","active_leases":1,"disk_state":"present"},
                        {"bundle_id":"b-ext","path":"/external/b-ext","format_version":"v2","state":"trashed","trashed_at_utc":"2026-11-25T00:00:00Z","created_at_utc":"2026-11-25T00:00:00Z","active_leases":0,"disk_state":"present"}
                    ]}"""
                    ok(
                        id,
                        """{"purged_bundle_ids":["b-managed"],"skipped_leased_bundle_ids":["b-leased"],"skipped_active_job_bundle_ids":[],"skipped_unmanaged_or_legacy_bundle_ids":["b-ext"],"skipped_in_grace_bundle_ids":[],"failed":[]}""",
                    )
                }
                else -> error("unexpected method: $method")
            }
        }
        val (service, rpc) = stubService(transport)
        val warnings = CopyOnWriteArrayList<String>()
        val infos = CopyOnWriteArrayList<String>()
        val errors = CopyOnWriteArrayList<Pair<String, String?>>()
        val controller = ArbToolWindowController(
            project = null,
            daemonService = service,
            onOpenBundle = { _, _, _ -> },
            pollIntervalMs = 0L,
            onNotifyInfo = { infos += it },
            onNotifyWarning = { warnings += it },
            onNotifyActionFailed = { op, detail -> errors += (op to detail) },
        )
        try {
            // 0 items in trash -> button disabled, requestEmptyTrash is no-op
            controller.applyStateForTest(
                capture = CaptureBarState(daemonConnected = true),
                catalog = BundleCatalogState(activeFilter = BundleFilter.TRASH),
            )
            assertFalse(controller.emptyTrashEnabled, "Empty Trash must be disabled when trashCount == 0")
            controller.requestEmptyTrash()
            SwingUtilities.invokeAndWait {}
            assertFalse(controller.showEmptyTrashConfirmation)

            // Refresh to populate 3 trashed items
            controller.selectFilter(BundleFilter.TRASH)
            controller.refreshNow()
            waitUntil(diag = {
                "trash=${controller.trashCount}, connected=${controller.captureState.daemonConnected}, " +
                    "msg=${controller.captureState.lastStatusMessage}, calls=${transport.calls.map { it.method }}"
            }) { controller.trashCount == 3 && controller.emptyTrashEnabled }

            // Request shows inline confirmation; cancel dismisses it without calling bundle.gc
            controller.requestEmptyTrash()
            SwingUtilities.invokeAndWait {}
            assertTrue(controller.showEmptyTrashConfirmation)
            assertFalse(transport.calls.any { it.method == "bundle.gc" })

            controller.cancelEmptyTrash()
            SwingUtilities.invokeAndWait {}
            assertFalse(controller.showEmptyTrashConfirmation)

            // Re-open and confirm -> calls bundle.gc(ignore_grace = true), updates list, emits warning with breakdown
            controller.requestEmptyTrash()
            SwingUtilities.invokeAndWait {}
            controller.confirmEmptyTrash()
            waitUntil { !controller.gcBusy && controller.trashCount == 2 }

            val gcCall = transport.calls.single { it.method == "bundle.gc" }
            assertEquals(true, gcCall.params.get("ignore_grace")?.asBoolean)
            assertFalse(controller.showEmptyTrashConfirmation)
            val note = controller.catalogNote.orEmpty()
            assertTrue(note.contains("削除=1"), "note must report purged count: $note")
            assertTrue(note.contains("使用中(Lease)保護=1"), "note must report leased skip: $note")
            assertTrue(note.contains("外部保存先/旧形式保護=1"), "note must report unmanaged/legacy skip: $note")
            assertEquals(1, warnings.size, "protected items must trigger a warning notification")
            assertTrue(warnings[0].contains("安全のため自動削除していません"), warnings[0])
            assertTrue(infos.isEmpty())
            assertTrue(errors.isEmpty())

            // Verify pure formatter surfaces grace, active-job, and failed buckets as well
            val fullSummary = ArbToolWindowController.formatGcSummary(
                BundleGcResponse(
                    purgedBundleIds = listOf("a"),
                    skippedLeasedBundleIds = listOf("b"),
                    skippedActiveJobBundleIds = listOf("c"),
                    skippedUnmanagedOrLegacyBundleIds = listOf("d"),
                    skippedInGraceBundleIds = listOf("e"),
                    failed = listOf(BundleGcFailure("f", "sharing violation")),
                ),
            )
            assertTrue(fullSummary.contains("猶予中=1"), fullSummary)
            assertTrue(fullSummary.contains("保存ジョブ保護=1"), fullSummary)
            assertTrue(fullSummary.contains("失敗=1"), fullSummary)
        } finally {
            controller.dispose()
            rpc.close()
            service.dispose()
        }
    }

    @Test
    fun emptyTrashFailureSurfacesErrorNotificationAndClearsBusyFlag() {
        val transport = RecordingTransport { id, method, _ ->
            when (method) {
                "bundle.gc" -> """{"jsonrpc":"2.0","id":$id,"error":{"code":-32003,"message":"catalog locked"}}"""
                else -> ok(id, """{}""")
            }
        }
        val (service, rpc) = stubService(transport)
        val errors = CopyOnWriteArrayList<Pair<String, String?>>()
        val controller = ArbToolWindowController(
            project = null,
            daemonService = service,
            onOpenBundle = { _, _, _ -> },
            pollIntervalMs = 0L,
            onNotifyActionFailed = { op, detail -> errors += (op to detail) },
        )
        try {
            controller.applyStateForTest(
                capture = CaptureBarState(daemonConnected = true),
                catalog = BundleCatalogState.fromEntries(
                    entries = listOf(
                        BundleEntryView("b-1", "/p", CatalogFormatVersion.V2, BundleLifecycleState.TRASHED, "2026-11-25T00:00:00Z", null, "2026-11-25T00:00:00Z", 0),
                    ),
                    activeFilter = BundleFilter.TRASH,
                ),
            )
            controller.confirmEmptyTrash()
            waitUntil { errors.isNotEmpty() && !controller.gcBusy }
            assertEquals("ゴミ箱を空にする", errors[0].first)
            assertTrue(errors[0].second.orEmpty().contains("catalog locked"))
            assertTrue(controller.catalogNote.orEmpty().contains("ゴミ箱を空にできませんでした"))
        } finally {
            controller.dispose()
            rpc.close()
            service.dispose()
        }
    }

    @Test
    fun missingAndUnavailableEntriesSkipTimelineSummaryPollingAndSupportForget() {
        var bundlesJson = """{"bundles":[
            {"bundle_id":"b-present","path":"/data/bundles/b-present","format_version":"v2","state":"active","created_at_utc":"2026-11-25T00:00:00Z","active_leases":0,"disk_state":"present"},
            {"bundle_id":"b-missing","path":"/data/bundles/b-missing","format_version":"v2","state":"active","created_at_utc":"2026-11-25T00:00:01Z","active_leases":0,"disk_state":"missing"},
            {"bundle_id":"b-unavail","path":"/mnt/usb/b-unavail","format_version":"v2","state":"active","created_at_utc":"2026-11-25T00:00:02Z","active_leases":0,"disk_state":"storage_unavailable"}
        ]}"""
        val transport = RecordingTransport { id, method, _ ->
            when (method) {
                "system.status" -> ok(id, statusJson)
                "config.get" -> ok(id, configJson)
                "device.list" -> ok(id, """{"devices":[]}""")
                "save.list_jobs" -> ok(id, """{"jobs":[]}""")
                "bundle.list" -> ok(id, bundlesJson)
                "timeline.summary" -> ok(id, """{"bundle_id":"b-present","path":"/data/bundles/b-present","format":"v2","bundle_kind":"full","is_legacy":false,"provenance":{"kind":"original"},"streams":{},"unanchored_summary":{"included_count":0,"truncated_count":0},"epochs":[],"frame_count":30,"keyframe_count":1,"log_main_count":0,"log_unanchored_count":0,"tap_main_count":0,"tap_unanchored_count":0,"discontinuity_count":0}""")
                "timeline.crashes" -> ok(id, """{"crashes":[]}""")
                "bundle.forget" -> {
                    bundlesJson = """{"bundles":[
                        {"bundle_id":"b-present","path":"/data/bundles/b-present","format_version":"v2","state":"active","created_at_utc":"2026-11-25T00:00:00Z","active_leases":0,"disk_state":"present"},
                        {"bundle_id":"b-unavail","path":"/mnt/usb/b-unavail","format_version":"v2","state":"active","created_at_utc":"2026-11-25T00:00:02Z","active_leases":0,"disk_state":"storage_unavailable"}
                    ]}"""
                    ok(
                        id,
                        """{"forgotten_bundle_ids":["b-missing"],"skipped_present_bundle_ids":[],"skipped_storage_unavailable_bundle_ids":["b-unavail"],"skipped_leased_bundle_ids":[],"skipped_active_job_bundle_ids":[]}""",
                    )
                }
                else -> error("unexpected method: $method")
            }
        }
        val (service, rpc) = stubService(transport)
        val openedIds = CopyOnWriteArrayList<String>()
        val warnings = CopyOnWriteArrayList<String>()
        val controller = ArbToolWindowController(
            project = null,
            daemonService = service,
            onOpenBundle = { id, _, _ -> openedIds += id },
            pollIntervalMs = 0L,
            onNotifyWarning = { warnings += it },
        )
        try {
            controller.refreshNow()
            waitUntil { controller.displayedBundles.size == 3 }

            // `timeline.summary` MUST be called ONLY for `b-present`, never for `missing` or `storage_unavailable`!
            val summaryTargets = transport.calls
                .filter { it.method == "timeline.summary" }
                .map { it.params.get("bundle_id_or_path").asString }
            assertEquals(listOf("b-present"), summaryTargets)

            assertEquals(1, controller.missingBundleCount)
            val missingItem = controller.displayedBundles.single { it.bundleId == "b-missing" }
            val unavailItem = controller.displayedBundles.single { it.bundleId == "b-unavail" }
            assertTrue(missingItem.isMissingOnDisk && missingItem.canForget && !missingItem.canOpen)
            assertTrue(unavailItem.isStorageUnavailable && !unavailItem.canForget && !unavailItem.canOpen)

            // Clicking a missing or unavailable item must not open an editor tab
            controller.openBundle(missingItem)
            controller.openBundle(unavailItem)
            SwingUtilities.invokeAndWait {}
            assertTrue(openedIds.isEmpty())
            assertEquals(2, warnings.size)

            // Bulk forget removes missing item and keeps present + storage_unavailable intact
            controller.forgetAllMissingBundles()
            waitUntil { !controller.forgetBusy && controller.missingBundleCount == 0 && controller.displayedBundles.size == 2 }
            val forgetCall = transport.calls.single { it.method == "bundle.forget" }
            assertEquals(true, forgetCall.params.get("all_missing")?.asBoolean)
        } finally {
            controller.dispose()
            rpc.close()
            service.dispose()
        }
    }
}
