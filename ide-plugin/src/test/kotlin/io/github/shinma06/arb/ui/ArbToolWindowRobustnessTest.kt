package io.github.shinma06.arb.ui

import io.github.shinma06.arb.ide.ArbdDaemonService
import io.github.shinma06.arb.ipc.ArbdRpcClient
import io.github.shinma06.arb.model.BundleGcFailure
import io.github.shinma06.arb.model.BundleGcResponse
import io.github.shinma06.arb.state.BundleFilter
import io.github.shinma06.arb.state.BundleMetaScheduler
import io.github.shinma06.arb.testsupport.CONFIG_JSON
import io.github.shinma06.arb.testsupport.STATUS_JSON
import io.github.shinma06.arb.testsupport.ScriptedTransport
import io.github.shinma06.arb.testsupport.TransportDown
import io.github.shinma06.arb.testsupport.awaitTrue
import io.github.shinma06.arb.testsupport.bundleEntryJson
import io.github.shinma06.arb.testsupport.crashesJson
import io.github.shinma06.arb.testsupport.rpcError
import io.github.shinma06.arb.testsupport.timelineSummaryJson
import org.junit.jupiter.api.Test
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference
import javax.swing.SwingUtilities
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

/**
 * Deterministic, headless state-machine tests for the Library / Tool Window controller:
 * A07 (metadata starvation + crash `false` cache pinning), A12 (single-flight actions and the
 * business-error vs transport-failure split) and the `bundle.gc` outcome contract.
 *
 * Polls are driven explicitly (`pollOnceForTest`), the daemon is a scripted fake, and the EDT is
 * drained with `invokeAndWait`, so no test depends on timing.
 */
class ArbToolWindowRobustnessTest {
    private class Fixture(
        val transport: ScriptedTransport,
        val service: ArbdDaemonService,
        val controller: ArbToolWindowController,
        val infos: CopyOnWriteArrayList<String>,
        val warnings: CopyOnWriteArrayList<String>,
        val failures: CopyOnWriteArrayList<Pair<String, String?>>,
    ) {
        fun drainEdt() = SwingUtilities.invokeAndWait {}
    }

    private fun fixture(handler: (method: String, params: com.google.gson.JsonObject) -> String): Fixture {
        val transport = ScriptedTransport(handler = handler)
        val rpc = ArbdRpcClient(transport)
        val service = object : ArbdDaemonService() {
            override fun <T> withClient(block: (ArbdRpcClient) -> T): T = synchronized(this) { block(rpc) }
        }
        val infos = CopyOnWriteArrayList<String>()
        val warnings = CopyOnWriteArrayList<String>()
        val failures = CopyOnWriteArrayList<Pair<String, String?>>()
        val controller = ArbToolWindowController(
            project = null,
            daemonService = service,
            onOpenBundle = { _, _, _ -> },
            pollIntervalMs = 0L,
            onNotifyInfo = { infos += it },
            onNotifyWarning = { warnings += it },
            onNotifyActionFailed = { op, detail -> failures += (op to detail) },
        )
        return Fixture(transport, service, controller, infos, warnings, failures)
    }

    private fun bundleIds(n: Int) = (0 until n).map { "b%03d".format(it) }

    private fun bundleListJson(ids: List<String>) = """{"bundles":[${ids.joinToString(",") { bundleEntryJson(it) }}]}"""

    // ---------------- A07: starvation + Unknown crash status ----------------

    @Test
    fun failingHeadOf24DoesNotStarveTheNext100AndRecoversWithCrashFilter() {
        val ids = bundleIds(124)
        val failingSummary = AtomicReference(ids.take(24).toSet())
        val crashing = setOf("b003", "b100") // one failing-head bundle and one tail bundle
        val f = fixture { method, params ->
            val id = params.get("bundle_id_or_path")?.asString
            when (method) {
                "system.status" -> STATUS_JSON
                "config.get" -> CONFIG_JSON
                "device.list" -> """{"devices":[]}"""
                "save.list_jobs" -> """{"jobs":[]}"""
                "bundle.list" -> bundleListJson(ids)
                "timeline.summary" ->
                    if (id in failingSummary.get()) rpcError("summary unavailable") else timelineSummaryJson(id!!)
                "timeline.crashes" -> if (id in crashing) crashesJson(1) else crashesJson()
                else -> error("unexpected $method")
            }
        }
        try {
            var summaryCallsPerPoll = 0
            var pollsToCoverTail = -1
            for (poll in 1..8) {
                val before = f.transport.count("timeline.summary")
                f.controller.pollOnceForTest()
                summaryCallsPerPoll = maxOf(summaryCallsPerPoll, f.transport.count("timeline.summary") - before)
                f.drainEdt()
                val tailDone = f.controller.catalogState.items.drop(24).all { it.durationSecs != null }
                if (tailDone && pollsToCoverTail < 0) pollsToCoverTail = poll
            }
            assertTrue(pollsToCoverTail in 1..6, "tail of 100 must converge within a few polls, took $pollsToCoverTail")
            assertTrue(summaryCallsPerPoll <= 24, "per-poll budget must hold, saw $summaryCallsPerPoll")
            val items = f.controller.catalogState.items
            assertTrue(items.drop(24).all { it.durationSecs != null }, "all 100 non-failing bundles fetched")
            // The failing head is "unknown", never silently "no crash".
            assertTrue(items.take(24).all { it.durationSecs == null && it.crashStatusUnknown && !it.hasCrash })
            assertTrue(items.first { it.bundleId == "b100" }.hasCrash)

            // Recovery: the 24 start succeeding; the scheduler's back-off must eventually retry them.
            failingSummary.set(emptySet())
            var converged = false
            for (poll in 1..40) {
                f.controller.pollOnceForTest()
                f.drainEdt()
                if (f.controller.catalogState.items.all { it.durationSecs != null && !it.crashStatusUnknown }) {
                    converged = true
                    break
                }
            }
            assertTrue(converged, "failed bundles must be retried and converge after recovery")
            assertTrue(f.controller.catalogState.items.first { it.bundleId == "b003" }.hasCrash)
            f.controller.selectFilter(BundleFilter.CRASH)
            f.drainEdt()
            assertEquals(setOf("b003", "b100"), f.controller.displayedBundles.map { it.bundleId }.toSet())
        } finally {
            f.controller.dispose()
        }
    }

    @Test
    fun crashQueryFailureIsUnknownNotCachedAsNoCrashAndRecovers() {
        val crashesFail = AtomicReference(true)
        val f = fixture { method, params ->
            when (method) {
                "system.status" -> STATUS_JSON
                "config.get" -> CONFIG_JSON
                "device.list" -> """{"devices":[]}"""
                "save.list_jobs" -> """{"jobs":[]}"""
                "bundle.list" -> bundleListJson(listOf("crashy"))
                "timeline.summary" -> timelineSummaryJson("crashy")
                "timeline.crashes" -> if (crashesFail.get()) rpcError("crash index busy") else crashesJson(7)
                else -> error("unexpected $method ${params}")
            }
        }
        try {
            f.controller.pollOnceForTest()
            f.drainEdt()
            val unknown = f.controller.catalogState.items.single()
            assertTrue(unknown.durationSecs != null, "summary-derived data is kept while crash status is unknown")
            assertTrue(unknown.crashStatusUnknown)
            assertFalse(unknown.hasCrash)
            assertTrue("クラッシュ判定不明" in unknown.semanticTags)

            crashesFail.set(false)
            var recovered = false
            for (poll in 1..40) {
                f.controller.pollOnceForTest()
                f.drainEdt()
                if (f.controller.catalogState.items.single().hasCrash) {
                    recovered = true
                    break
                }
            }
            assertTrue(recovered, "a bundle with crashes must return to the crash filter once the query works")
            val item = f.controller.catalogState.items.single()
            assertFalse(item.crashStatusUnknown)
            assertEquals(1, f.controller.catalogState.countFor(BundleFilter.CRASH))
        } finally {
            f.controller.dispose()
        }
    }

    @Test
    fun metaSchedulerOrdersUntriedFirstAndBacksOffFailures() {
        val scheduler = BundleMetaScheduler(budgetPerTick = 2)
        val ids = listOf("a", "b", "c", "d")
        assertEquals(listOf("a", "b"), scheduler.select(ids, tick = 1))
        scheduler.markFailure("a", 1)
        scheduler.markFailure("b", 1)
        // a/b cool down; untried c/d are served first.
        assertEquals(listOf("c", "d"), scheduler.select(ids, tick = 2))
        scheduler.markSuccess("c")
        scheduler.markSuccess("d")
        assertEquals(emptyList(), scheduler.select(listOf("a", "b"), tick = 2), "still cooling down")
        assertEquals(listOf("a", "b"), scheduler.select(listOf("a", "b"), tick = 3), "retried after the back-off")
        // The back-off grows but is capped.
        repeat(10) { scheduler.markFailure("a", 100L + it) }
        assertEquals(emptyList(), scheduler.select(listOf("a"), tick = 100L + 9 + 15))
        assertEquals(listOf("a"), scheduler.select(listOf("a"), tick = 100L + 9 + BundleMetaScheduler.DEFAULT_MAX_BACKOFF_TICKS))
    }

    // ---------------- A12: single flight + business error vs transport ----------------

    private fun pollingHandler(extra: (String, com.google.gson.JsonObject) -> String?): (String, com.google.gson.JsonObject) -> String =
        { method, params ->
            extra(method, params) ?: when (method) {
                "system.status" -> STATUS_JSON
                "config.get" -> CONFIG_JSON
                "device.list" -> """{"devices":[]}"""
                "save.list_jobs" -> """{"jobs":[]}"""
                "bundle.list" -> """{"bundles":[]}"""
                else -> error("unexpected $method")
            }
        }

    @Test
    fun doubleClickInOneEdtTurnSubmitsOnceAndAlwaysReleasesBusy() {
        val gate = CountDownLatch(1)
        val f = fixture(
            pollingHandler { method, _ ->
                if (method == "save.trigger") {
                    check(gate.await(10, TimeUnit.SECONDS))
                    rpcError("disk full")
                } else {
                    null
                }
            },
        )
        try {
            f.controller.pollOnceForTest()
            f.drainEdt()
            assertTrue(f.controller.captureState.daemonConnected)
            SwingUtilities.invokeAndWait {
                f.controller.triggerPrimarySave()
                f.controller.triggerPrimarySave() // same EDT turn: must be dropped, not queued
                assertTrue(f.controller.saveBusy)
            }
            gate.countDown()
            awaitTrue({ "save never settled" }) { f.failures.isNotEmpty() }
            awaitTrue({ "busy never released" }) { f.drainEdt(); !f.controller.saveBusy }
            assertEquals(1, f.transport.count("save.trigger"), "a double click must send exactly one save")
            assertEquals(1, f.failures.size, "exactly one failure notification")
            // A business error (daemon answered with a JSON-RPC error) keeps the connected state.
            assertTrue(f.controller.captureState.daemonConnected, "business error must not flip to disconnected")
            // After settling, a deliberate second save is accepted again.
            SwingUtilities.invokeAndWait { f.controller.triggerPrimarySave() }
            awaitTrue({ "second save not submitted" }) { f.transport.count("save.trigger") == 2 }
            awaitTrue({ "second save not settled" }) { f.drainEdt(); !f.controller.saveBusy }
        } finally {
            f.controller.dispose()
        }
    }

    @Test
    fun transportFailureReleasesBusyAndDoesShowDisconnected() {
        val f = fixture(
            pollingHandler { method, _ -> if (method == "save.logs") throw TransportDown() else null },
        )
        try {
            f.controller.pollOnceForTest()
            f.drainEdt()
            assertTrue(f.controller.captureState.daemonConnected)
            SwingUtilities.invokeAndWait { f.controller.saveLogsOnly() }
            awaitTrue({ "log save never settled" }) { f.drainEdt(); f.failures.isNotEmpty() && !f.controller.logSaveBusy }
            assertFalse(f.controller.captureState.daemonConnected, "a dead transport is a real disconnect")
        } finally {
            f.controller.dispose()
        }
    }

    @Test
    fun screenshotBusinessErrorKeepsDaemonConnectedAndReleasesGuard() {
        val f = fixture(
            pollingHandler { method, _ -> if (method == "save.screenshot") rpcError("no device") else null },
        )
        try {
            f.controller.pollOnceForTest()
            f.drainEdt()
            SwingUtilities.invokeAndWait {
                f.controller.captureScreenshot()
                f.controller.captureScreenshot()
            }
            awaitTrue({ "screenshot never settled" }) { f.drainEdt(); f.failures.isNotEmpty() && !f.controller.screenshotBusy }
            assertEquals(1, f.transport.count("save.screenshot"))
            assertTrue(f.controller.captureState.daemonConnected)
            assertEquals("スクリーンショットの保存", f.failures.single().first)
        } finally {
            f.controller.dispose()
        }
    }

    private val failedJobJson =
        """{"job_id":"j1","state":"failed_retryable","trigger":"manual","destination_path":"/d","pinned_chunk_ids":[],"last_error":"io","created_at_utc":"2026-11-25T00:00:00Z","updated_at_utc":"2026-11-25T00:00:00Z"}"""

    @Test
    fun retryDoubleSubmitSendsOnceAndBusinessErrorClearsBusyWithoutDisconnect() {
        val gate = CountDownLatch(1)
        val f = fixture(
            pollingHandler { method, _ ->
                when (method) {
                    "save.list_jobs" -> """{"jobs":[$failedJobJson]}"""
                    "save.retry" -> {
                        check(gate.await(10, TimeUnit.SECONDS))
                        rpcError("retry rejected")
                    }
                    else -> null
                }
            },
        )
        try {
            f.controller.pollOnceForTest()
            f.drainEdt()
            assertEquals(1, f.controller.recoveryState.retryableJobs.size)
            SwingUtilities.invokeAndWait {
                f.controller.retryFailedJob("j1")
                f.controller.retryFailedJob("j1")
                assertEquals("j1", f.controller.recoveryState.busyJobId)
            }
            gate.countDown()
            awaitTrue({ "retry never settled" }) { f.drainEdt(); f.failures.isNotEmpty() && f.controller.recoveryState.busyJobId == null }
            assertEquals(1, f.transport.count("save.retry"))
            assertTrue(f.controller.captureState.daemonConnected)
            assertTrue(f.controller.recoveryState.lastMessage.orEmpty().contains("失敗しました"))
            assertNotNull(f.failures.single().second)
        } finally {
            f.controller.dispose()
        }
    }

    @Test
    fun pollFinishingWhileRetryIsQueuedKeepsTheJobBusy() {
        val pollEntered = CountDownLatch(1)
        val finishPoll = CountDownLatch(1)
        val retryEntered = CountDownLatch(1)
        val finishRetry = CountDownLatch(1)
        val f = fixture(pollingHandler { method, _ ->
            when (method) {
                "bundle.list" -> {
                    pollEntered.countDown()
                    check(finishPoll.await(10, TimeUnit.SECONDS))
                    bundleListJson(emptyList())
                }
                "save.retry" -> {
                    retryEntered.countDown()
                    check(finishRetry.await(10, TimeUnit.SECONDS))
                    rpcError("retry rejected")
                }
                else -> null
            }
        })
        try {
            f.controller.refreshNow()
            assertTrue(pollEntered.await(10, TimeUnit.SECONDS))
            SwingUtilities.invokeAndWait { f.controller.retryFailedJob("j1") }
            finishPoll.countDown()
            assertTrue(retryEntered.await(10, TimeUnit.SECONDS))
            SwingUtilities.invokeAndWait {
                assertEquals("j1", f.controller.recoveryState.busyJobId)
                f.controller.retryFailedJob("j1")
            }
            finishRetry.countDown()
            awaitTrue { f.drainEdt(); f.controller.recoveryState.busyJobId == null }
            assertEquals(1, f.transport.count("save.retry"))
        } finally {
            finishPoll.countDown()
            finishRetry.countDown()
            f.controller.dispose()
        }
    }

    @Test
    fun captureModeAndRestartGuardsAreSingleFlight() {
        val gate = CountDownLatch(1)
        val f = fixture(
            pollingHandler { method, _ ->
                if (method == "config.update") {
                    check(gate.await(10, TimeUnit.SECONDS))
                    rpcError("rejected")
                } else {
                    null
                }
            },
        )
        try {
            f.controller.pollOnceForTest()
            f.drainEdt()
            SwingUtilities.invokeAndWait {
                f.controller.setCaptureMode(io.github.shinma06.arb.model.CaptureMode.LOGS_ONLY)
                f.controller.setCaptureMode(io.github.shinma06.arb.model.CaptureMode.LOGS_ONLY)
                assertTrue(f.controller.captureModeBusy)
            }
            gate.countDown()
            awaitTrue({ "capture mode never settled" }) { f.drainEdt(); !f.controller.captureModeBusy }
            assertEquals(1, f.transport.count("config.update"))
            assertTrue(f.controller.captureState.daemonConnected)
        } finally {
            f.controller.dispose()
        }
    }

    private fun gc(
        purgedBundleIds: List<String> = emptyList(),
        skippedLeasedBundleIds: List<String> = emptyList(),
        skippedActiveJobBundleIds: List<String> = emptyList(),
        skippedUnmanagedOrLegacyBundleIds: List<String> = emptyList(),
        skippedInGraceBundleIds: List<String> = emptyList(),
        failed: List<BundleGcFailure> = emptyList(),
    ) = BundleGcResponse(
        purgedBundleIds, skippedLeasedBundleIds, skippedActiveJobBundleIds,
        skippedUnmanagedOrLegacyBundleIds, skippedInGraceBundleIds, failed,
    )

    // ---------------- bundle.gc contract: every bucket reaches the user ----------------

    @Test
    fun mixedGcResponseSurfacesEveryBucketInSummaryAndWarning() {
        val gc = gc(
            purgedBundleIds = listOf("p1", "p2"),
            skippedLeasedBundleIds = listOf("l1"),
            skippedActiveJobBundleIds = listOf("a1", "a2", "a3"),
            skippedUnmanagedOrLegacyBundleIds = listOf("u1"),
            skippedInGraceBundleIds = listOf("g1", "g2"),
            failed = listOf(BundleGcFailure(bundleId = "f1", error = "EACCES")),
        )
        val summary = ArbToolWindowController.formatGcSummary(gc)
        for (part in listOf("削除=2", "使用中(Lease)保護=1", "猶予中=2", "外部保存先/旧形式保護=1", "保存ジョブ保護=3", "失敗=1")) {
            assertTrue(part in summary, "summary must mention $part: $summary")
        }
        val (isWarning, text) = ArbToolWindowController.formatGcNotification(gc)
        assertTrue(isWarning)
        for (hint in listOf("タブを閉じてから", "外部保存先・旧形式", "猶予期間内", "保存処理中", "削除失敗: EACCES")) {
            assertTrue(hint in text, "notification must explain $hint: $text")
        }
        // Each single non-empty bucket alone is already a warning (never reported as plain success).
        for (single in listOf(
            gc(skippedInGraceBundleIds = listOf("g")),
            gc(skippedLeasedBundleIds = listOf("l")),
            gc(skippedActiveJobBundleIds = listOf("a")),
            gc(skippedUnmanagedOrLegacyBundleIds = listOf("u")),
            gc(failed = listOf(BundleGcFailure("f", "e"))),
        )) {
            assertTrue(ArbToolWindowController.formatGcNotification(single).first, "$single")
        }
        assertFalse(ArbToolWindowController.formatGcNotification(gc(purgedBundleIds = listOf("p"))).first)
        assertEquals("ゴミ箱に削除対象はありませんでした", ArbToolWindowController.formatGcSummary(gc()))
    }
}
