package io.github.shinma06.arb.ide

import io.github.shinma06.arb.state.AuxStream
import io.github.shinma06.arb.state.StreamLoadState
import io.github.shinma06.arb.testsupport.Fault
import io.github.shinma06.arb.testsupport.RecordedRpc
import io.github.shinma06.arb.testsupport.StudioDaemon as Daemon
import io.github.shinma06.arb.testsupport.awaitAuxSettled
import io.github.shinma06.arb.testsupport.awaitReady
import io.github.shinma06.arb.testsupport.openStudioEditor
import io.github.shinma06.arb.testsupport.awaitTrue
import io.github.shinma06.arb.testsupport.crashesJson
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Path
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/**
 * Studio silent-failure regression tests (A06 UI / A10 / A11), driven end to end through a real
 * [ArbBundleFileEditor] + [io.github.shinma06.arb.ui.ArbStudioController] against a scripted daemon.
 * The EDT is replaced by direct execution, so every assertion is deterministic.
 */
class ArbStudioSilentFailureTest {
    private fun methodOf(stream: AuxStream) = when (stream) {
        AuxStream.FILMSTRIP -> "media.filmstrip"
        AuxStream.DISCONTINUITIES -> "timeline.discontinuities"
        AuxStream.CRASHES -> "timeline.crashes"
        AuxStream.SCREENS -> "timeline.screens"
    }

    // ---------------- A10 ----------------

    @Test
    fun eachAuxStreamFailureIsIsolatedRetriedAndNeverStopsPlaybackOrOtherStreams(@TempDir tmp: Path) {
        for (fault in listOf(Fault.BUSINESS_ERROR, Fault.TRANSPORT_DOWN)) {
            for (target in AuxStream.entries) {
                val dir = java.nio.file.Files.createDirectory(tmp.resolve("${fault.name}-${target.name}"))
                val daemon = Daemon(dir)
                daemon.crashes.set(crashesJson(3))
                daemon.faults[methodOf(target)] = fault
                val editor = openStudioEditor(daemon)
                try {
                    editor.awaitReady()
                    editor.awaitAuxSettled()
                    val ctrl = editor.studioController
                    val state = assertNotNull(ctrl.viewerState)
                    // Only the failed stream is Failed; it is never presented as a loaded empty list.
                    for (stream in AuxStream.entries) {
                        if (stream == target) {
                            val failed = assertNotNull(state.streamState(stream) as? StreamLoadState.Failed, "$stream must be Failed")
                            assertTrue(failed.reason.isNotBlank())
                        } else {
                            assertEquals(StreamLoadState.Loaded, state.streamState(stream), "$stream must be unaffected by $target")
                        }
                    }
                    assertEquals(listOf(target), ctrl.failedAuxStreams.map { it.stream })
                    // Unaffected stream data is still shown (crashes carry data unless they are the failed one).
                    assertEquals(target != AuxStream.CRASHES, state.crashes.isNotEmpty())
                    // Video playback and seeking keep working while a stream is failed.
                    val playback = assertNotNull(editor.playbackControllerForTest())
                    playback.seekToFrame(4)
                    assertEquals(4L, playback.state.currentFrameIndex)
                    assertTrue(!playback.isClosed)

                    // Retry still fails -> remains Failed and still isolated.
                    ctrl.retryAuxStream(target)
                    awaitTrue({ "retry did not run" }) { daemon.transport.count(methodOf(target)) >= 2 }
                    editor.awaitQueuedWorkForTest()
                    assertTrue(ctrl.viewerState!!.streamState(target) is StreamLoadState.Failed)

                    // Heal the daemon and retry: exactly the failed stream recovers.
                    daemon.faults.remove(methodOf(target))
                    ctrl.retryAuxStream(target)
                    awaitTrue({ "stream never recovered" }) { ctrl.viewerState!!.streamState(target) == StreamLoadState.Loaded }
                    assertTrue(ctrl.failedAuxStreams.isEmpty())
                    assertEquals(1, ctrl.viewerState!!.crashes.size)
                    playback.seekToFrame(6)
                    assertEquals(6L, playback.state.currentFrameIndex)
                } finally {
                    editor.dispose()
                }
            }
        }
    }

    @Test
    fun retryOfAStreamThatIsNotFailedIsIgnored(@TempDir tmp: Path) {
        val daemon = Daemon(tmp)
        val editor = openStudioEditor(daemon)
        try {
            editor.awaitReady()
            editor.awaitAuxSettled()
            val before = daemon.transport.count("timeline.crashes")
            editor.studioController.retryAuxStream(AuxStream.CRASHES)
            editor.awaitQueuedWorkForTest()
            assertEquals(before, daemon.transport.count("timeline.crashes"), "no reload for a stream that loaded fine")
        } finally {
            editor.dispose()
        }
    }

    // ---------------- A11 ----------------

    @Test
    fun editSessionSaveFailureIsVisibleRetriedAndPersistedLatest(@TempDir tmp: Path) {
        for (fault in listOf(Fault.TRANSPORT_DOWN, Fault.BUSINESS_ERROR)) {
            val dir = java.nio.file.Files.createDirectory(tmp.resolve(fault.name))
            val daemon = Daemon(dir)
            val editor = openStudioEditor(daemon)
            try {
                editor.awaitReady()
                editor.awaitAuxSettled()
                val ctrl = editor.studioController
                assertNull(ctrl.editSaveFailure)

                daemon.faults["edit.update_session"] = fault
                ctrl.applyInFrame(2, settled = true)
                awaitTrue({ "failure was never surfaced" }) { ctrl.editSaveFailure != null }
                assertTrue(ctrl.hasUnsavedEditRange)
                assertTrue(daemon.storedRanges.isEmpty(), "nothing was persisted yet")

                // A later change while still failing keeps the failure and only the newest range pending.
                ctrl.applyOutFrame(7, settled = true)
                awaitTrue({ "second attempt not made" }) { daemon.transport.count("edit.update_session") >= 2 }
                editor.awaitQueuedWorkForTest()
                assertNotNull(ctrl.editSaveFailure)

                // Recovery via the explicit "再保存" action persists the newest range and clears the marker.
                daemon.faults.remove("edit.update_session")
                ctrl.retryEditSave()
                awaitTrue({ "retry did not persist" }) { ctrl.editSaveFailure == null && daemon.storedRanges.isNotEmpty() }
                val (inF, outF, _) = daemon.storedRanges.last()
                assertEquals(2L, inF)
                assertEquals(7L, outF)
                // With nothing pending, "再保存" is a no-op.
                val n = daemon.transport.count("edit.update_session")
                ctrl.retryEditSave()
                editor.awaitQueuedWorkForTest()
                assertEquals(n, daemon.transport.count("edit.update_session"))
            } finally {
                editor.dispose()
            }
        }
    }

    @Test
    fun unsavedRangeIsFlushedOnCloseAndSurvivesReopen(@TempDir tmp: Path) {
        val daemon = Daemon(tmp)
        val first = openStudioEditor(daemon)
        first.awaitReady()
        first.awaitAuxSettled()
        daemon.faults["edit.update_session"] = Fault.TRANSPORT_DOWN
        first.studioController.applyInFrame(3, settled = true)
        awaitTrue({ "failure not surfaced" }) { first.studioController.editSaveFailure != null }
        assertNull(daemon.storedSession.get())

        assertFalse(first.flushBeforeClose())
        // The daemon is back by the time the tab closes: close must flush the unsaved range once more.
        daemon.faults.remove("edit.update_session")
        assertTrue(first.flushBeforeClose())
        assertNotNull(daemon.storedSession.get())
        first.dispose()
        assertEquals(3L, daemon.storedRanges.last().first)

        val reopened = openStudioEditor(daemon)
        try {
            reopened.awaitReady()
            assertEquals(3L, reopened.studioController.viewerState!!.inFrameIndex, "reopening restores the persisted IN")
        } finally {
            reopened.dispose()
        }
    }

    @Test
    fun closingOlderTabCannotOverwriteTheNewerTabsSavedRange(@TempDir tmp: Path) {
        val daemon = Daemon(tmp)
        val older = openStudioEditor(daemon)
        val newer = openStudioEditor(daemon)
        try {
            older.awaitReady()
            newer.awaitReady()
            daemon.faults["edit.update_session"] = Fault.BUSINESS_ERROR
            older.studioController.applyInFrame(1, settled = true)
            older.awaitQueuedWorkForTest()
            assertNotNull(older.studioController.editSaveFailure)
            daemon.faults.remove("edit.update_session")
            daemon.loseEditResponse = true
            newer.studioController.applyInFrame(4, settled = true)
            newer.awaitQueuedWorkForTest()
            assertNotNull(newer.studioController.editSaveFailure)
            daemon.loseEditResponse = false
            assertTrue(older.flushBeforeClose())
            assertTrue(older.studioController.timelineWarnings.any { it.contains("別のタブ") })
            assertEquals(listOf(4L), daemon.storedRanges.map { it.first })
        } finally {
            older.dispose()
            newer.dispose()
        }
    }

    @Test
    fun closeWithoutAnyUnsavedRangeSendsNoUpdate(@TempDir tmp: Path) {
        val daemon = Daemon(tmp)
        val editor = openStudioEditor(daemon)
        editor.awaitReady()
        editor.awaitAuxSettled()
        editor.dispose()
        awaitTrue({ "lease never closed" }) { daemon.transport.count("bundle.close") == 1 }
        assertEquals(0, daemon.transport.count("edit.update_session"))
    }

    // ---------------- A06 (UI side): the 5,000-row display limit is stated ----------------

    @Test
    fun logBucketCutAtTheDisplayLimitShowsTheNoticeAndExactlyFullDoesNot(@TempDir tmp: Path) {
        val expected = "ログは表示上限（5,000 行）に達したため一部省略しています。検索対象は各区分の先頭から表示上限までです。"
        // 5,003 rows exist: 5,000 are shown, three are cut -> notice.
        val cut = Daemon(java.nio.file.Files.createDirectory(tmp.resolve("cut")), mainLogCount = 5_003)
        val cutEditor = openStudioEditor(cut)
        try {
            cutEditor.awaitReady()
            awaitTrue({ "logs never loaded" }) { cutEditor.studioController.viewerState?.mainLogs?.size == 5_000 }
            val state = assertNotNull(cutEditor.studioController.viewerState)
            assertTrue(state.mainLogsLimited)
            assertEquals(expected, cutEditor.studioController.logLimitNotice)
        } finally {
            cutEditor.dispose()
        }
        // Exactly 5,000 rows: nothing is hidden, so no notice.
        val exact = Daemon(java.nio.file.Files.createDirectory(tmp.resolve("exact")), mainLogCount = 5_000)
        val exactEditor = openStudioEditor(exact)
        try {
            exactEditor.awaitReady()
            awaitTrue({ "logs never loaded" }) { exactEditor.studioController.viewerState?.mainLogs?.size == 5_000 }
            awaitTrue({ "probe never ran" }) { exact.transport.calls.count { it.isLimitProbe() } >= 1 }
            assertTrue(!exactEditor.studioController.viewerState!!.mainLogsLimited)
            assertNull(exactEditor.studioController.logLimitNotice)
        } finally {
            exactEditor.dispose()
        }
    }

    private fun RecordedRpc.isLimitProbe(): Boolean =
        method == "timeline.query_logs" && params.get("limit")?.asInt == 1
}
