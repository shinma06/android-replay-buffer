package io.github.shinma06.arb.ui

import io.github.shinma06.arb.ide.ArbBundleFileEditor
import io.github.shinma06.arb.testsupport.Fault
import io.github.shinma06.arb.testsupport.StudioDaemon
import io.github.shinma06.arb.testsupport.awaitTrue
import io.github.shinma06.arb.testsupport.crashesJson
import io.github.shinma06.arb.testsupport.openStudioEditor
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * Generation / ordering tests for the Studio wiring with a **manual EDT**: worker results are
 * queued and only run when the test drains them, so late, reordered and cross-tab completions
 * are reproduced deterministically.
 */
class ArbStudioControllerOrderingTest {
    /** The EDT stand-in: tasks run only when drained, in submission order. */
    private class ManualEdt {
        private val queue = ConcurrentLinkedQueue<() -> Unit>()
        fun post(task: () -> Unit) {
            queue.add(task)
        }
        val pending: Int get() = queue.size
        fun drain(): Int {
            var n = 0
            while (true) {
                val task = queue.poll() ?: return n
                task()
                n++
            }
        }
    }

    private fun readyEditor(dir: Path, edt: ManualEdt, daemon: StudioDaemon = StudioDaemon(dir)): Pair<ArbBundleFileEditor, StudioDaemon> {
        val editor = openStudioEditor(daemon, publishEdt = edt::post)
        awaitTrue({ "editor never became ready" }) {
            edt.drain()
            editor.studioController.viewerState != null && editor.playbackControllerForTest() != null
        }
        awaitTrue({ "aux never settled" }) {
            edt.drain()
            editor.studioController.viewerState?.let { s ->
                io.github.shinma06.arb.state.AuxStream.entries.all { s.streamState(it) !is io.github.shinma06.arb.state.StreamLoadState.Pending }
            } == true
        }
        return editor to daemon
    }

    // ---------------- double export ----------------

    @Test
    fun exportRequestedTwiceInOneEdtTurnIsSentOnceAndTheGuardAlwaysReleases(@TempDir tmp: Path) {
        val edt = ManualEdt()
        val (editor, daemon) = readyEditor(Files.createDirectory(tmp.resolve("a")), edt)
        try {
            val gate = CountDownLatch(1)
            daemon.exportGate = gate
            val ctrl = editor.studioController
            ctrl.triggerExport()
            ctrl.triggerExport() // same turn: the second request must not reach the daemon
            assertTrue(ctrl.isExporting)
            assertFalse(ctrl.isExportButtonEnabled)
            awaitTrue({ "export never reached the daemon" }) { daemon.transport.count("export.trim") >= 1 }
            gate.countDown()
            awaitTrue({ "export never finished" }) { edt.drain(); !ctrl.isExporting }
            assertEquals(1, daemon.transport.count("export.trim"), "exactly one export")
            assertNotNull(ctrl.viewerState?.exportCompletion)

            // A later, deliberate export is accepted again.
            daemon.exportGate = null
            ctrl.triggerExport()
            awaitTrue({ "second export never ran" }) { edt.drain(); daemon.transport.count("export.trim") == 2 && !ctrl.isExporting }
        } finally {
            editor.dispose()
        }
    }

    @Test
    fun failedExportReleasesTheGuardAndShowsTheError(@TempDir tmp: Path) {
        val edt = ManualEdt()
        val (editor, daemon) = readyEditor(Files.createDirectory(tmp.resolve("a")), edt)
        try {
            daemon.faults["export.trim"] = Fault.BUSINESS_ERROR
            val ctrl = editor.studioController
            ctrl.triggerExport()
            ctrl.triggerExport()
            awaitTrue({ "failed export never settled" }) { edt.drain(); !ctrl.isExporting && ctrl.errorMessage != null }
            assertEquals(1, daemon.transport.count("export.trim"))
            assertTrue(ctrl.errorMessage.orEmpty().contains("書き出しに失敗"))
            daemon.faults.remove("export.trim")
            ctrl.triggerExport()
            awaitTrue({ "retry after failure not sent" }) { edt.drain(); daemon.transport.count("export.trim") == 2 }
        } finally {
            editor.dispose()
        }
    }

    // ---------------- stale completions never overwrite newer state ----------------

    @Test
    fun framesPublishedOutOfStepAreCoalescedToTheNewestAndNeverReapplied(@TempDir tmp: Path) {
        val edt = ManualEdt()
        val (editor, _) = readyEditor(Files.createDirectory(tmp.resolve("a")), edt)
        try {
            val playback = assertNotNull(editor.playbackControllerForTest())
            val ctrl = editor.studioController
            playback.seekToFrame(3)
            playback.seekToFrame(6) // newer generation completes before the EDT ever saw #3
            val queued = edt.pending
            assertTrue(queued >= 1)
            edt.drain()
            assertEquals(6L, ctrl.viewerState!!.currentFrameIndex, "the older frame must not be applied after the newer one")
            // Nothing stale is left to flush later.
            assertEquals(0, edt.pending)
            val bitmapAfter = ctrl.frameBitmap
            edt.drain()
            assertEquals(bitmapAfter, ctrl.frameBitmap)
        } finally {
            editor.dispose()
        }
    }

    @Test
    fun staleSelectionResultNeverOverwritesTheUsersNewerRange(@TempDir tmp: Path) {
        val edt = ManualEdt()
        val daemon = StudioDaemon(Files.createDirectory(tmp.resolve("a")), frameCount = 400)
        daemon.crashes.set(crashesJson(3))
        val (editor, _) = readyEditor(Files.createDirectory(tmp.resolve("b")), edt, daemon)
        try {
            val ctrl = editor.studioController
            val playback = assertNotNull(editor.playbackControllerForTest())
            val before = ctrl.viewerState!!
            assertTrue(ctrl.isSelectAroundCrashVisible)

            ctrl.selectAroundCrash() // worker computes a crash selection; its EDT update is parked
            awaitTrue({ "worker never computed the crash range" }) {
                playback.state.inFrameIndex != before.inFrameIndex || playback.state.outFrameIndex != before.outFrameIndex
            }
            // Before that parked update runs, the user drags IN to #1: the newer intent.
            ctrl.applyInFrame(1, settled = true)
            val userRangeOut = ctrl.viewerState!!.outFrameIndex
            edt.drain() // the stale crash-range update now arrives
            awaitTrue({ "range never converged: studio=${ctrl.viewerState!!.inFrameIndex}..${ctrl.viewerState!!.outFrameIndex} playback=${playback.state.inFrameIndex}..${playback.state.outFrameIndex}" }) {
                edt.drain()
                playback.state.inFrameIndex == ctrl.viewerState!!.inFrameIndex &&
                    playback.state.outFrameIndex == ctrl.viewerState!!.outFrameIndex
            }
            assertEquals(1L, ctrl.viewerState!!.inFrameIndex, "the user's later IN must survive")
            assertEquals(playback.state.inFrameIndex, ctrl.viewerState!!.inFrameIndex)
            assertEquals(playback.state.outFrameIndex, ctrl.viewerState!!.outFrameIndex, "UI and playback agree on OUT")
            assertEquals(userRangeOut, ctrl.viewerState!!.outFrameIndex)
        } finally {
            editor.dispose()
        }
    }

    // ---------------- tab move / dispose ----------------

    @Test
    fun aClosedTabIgnoresLateCompletionsAndTheOtherTabIsUntouched(@TempDir tmp: Path) {
        val edtA = ManualEdt()
        val edtB = ManualEdt()
        val daemonA = StudioDaemon(Files.createDirectory(tmp.resolve("a")))
        val daemonB = StudioDaemon(Files.createDirectory(tmp.resolve("b")))
        val (a, _) = readyEditor(Files.createDirectory(tmp.resolve("a2")), edtA, daemonA)
        val (b, _) = readyEditor(Files.createDirectory(tmp.resolve("b2")), edtB, daemonB)
        try {
            val gate = CountDownLatch(1)
            daemonA.exportGate = gate
            a.studioController.triggerExport()
            awaitTrue({ "A export not sent" }) { daemonA.transport.count("export.trim") == 1 }
            val aStateBefore = a.studioController.viewerState
            a.dispose() // the user closes tab A while its export is in flight
            gate.countDown()
            // A's late completion must publish nothing on the (now dead) controller.
            awaitTrue({ "A lease not closed" }) { daemonA.transport.count("bundle.close") == 1 }
            a.awaitQueuedWorkForTest()
            edtA.drain()
            assertEquals(aStateBefore?.exportCompletion, a.studioController.viewerState?.exportCompletion)
            assertNull(a.studioController.viewerState?.exportCompletion)
            // B is a different tab with its own state and export guard.
            assertFalse(b.studioController.isExporting)
            assertEquals(0, daemonB.transport.count("export.trim"))
            b.studioController.triggerExport()
            awaitTrue({ "B export not completed" }) { edtB.drain(); daemonB.transport.count("export.trim") == 1 && !b.studioController.isExporting }
            assertNotNull(b.studioController.viewerState?.exportCompletion)
            assertEquals(1, daemonA.transport.count("bundle.close"), "the lease of the closed tab is released exactly once")
        } finally {
            b.dispose()
            a.dispose()
        }
    }

    @Test
    fun latestValueMailboxKeepsOnlyTheNewestValue() {
        val box = LatestValueMailbox<Int>()
        assertTrue(box.offer(1), "first offer asks for a drain")
        assertFalse(box.offer(2), "a pending drain already exists")
        assertFalse(box.offer(3))
        assertEquals(3, box.take())
        assertNull(box.take())
        assertTrue(box.offer(4), "after the drain a new one is requested")
        assertEquals(4, box.take())
    }

}
