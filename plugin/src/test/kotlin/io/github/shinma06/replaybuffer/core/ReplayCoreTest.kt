package io.github.shinma06.replaybuffer.core

import java.nio.file.Files
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.TimeUnit
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class ReplayCoreTest {
    @Test
    fun queuedEnableRejectsALaterModeChangeInsideTheControlBoundary() {
        val workspace = Files.createTempDirectory("replay-mode-race-")
        val core = ReplayCore(ReplaySettings(), workspace)
        val blocked = java.util.concurrent.CountDownLatch(1)
        val release = java.util.concurrent.CountDownLatch(1)
        val once = java.util.concurrent.atomic.AtomicBoolean()
        val subscription = core.subscribe {
            if (!it.enabled && once.compareAndSet(false, true)) {
                blocked.countDown()
                check(release.await(10, TimeUnit.SECONDS))
            }
        }
        try {
            assertTrue(blocked.await(10, TimeUnit.SECONDS))
            val on = core.setEnabled(true)
            val change = core.applySettings(ReplaySettings(captureMode = CaptureMode.LOG_ONLY, replaySeconds = 60))
            release.countDown()
            assertTrue(on.get(10, TimeUnit.SECONDS).accepted)
            assertFalse(change.get(10, TimeUnit.SECONDS).accepted)
            assertEquals(ReplaySettings(), core.snapshot().settings)
            assertEquals(0L, core.snapshot().settingsRevision)
            assertTrue(core.snapshot().enabled)
            assertTrue(core.setEnabled(false).get(10, TimeUnit.SECONDS).accepted)
            assertTrue(core.applySettings(ReplaySettings(captureMode = CaptureMode.LOG_ONLY)).get().accepted)
            assertEquals(StreamState.NOT_REQUESTED, core.snapshot().video.state)
        } finally {
            release.countDown()
            subscription.close()
            core.closeAsync().get(10, TimeUnit.SECONDS)
            Files.delete(workspace)
        }
    }

    @Test
    fun unresolvedSdkStillEnablesAndCloseStopsCallbacks() {
        val workspace = Files.createTempDirectory("replay-core-test-")
        val core = ReplayCore(ReplaySettings(), workspace)
        val threads = CopyOnWriteArrayList<String>()
        val subscription = core.subscribe { threads += Thread.currentThread().name }
        try {
            val enable = core.setEnabled(true).get(10, TimeUnit.SECONDS)
            assertTrue(enable.accepted, enable.reason)
            val state = core.snapshot()
            assertTrue(state.enabled)
            assertEquals(CaptureState.WAITING, state.captureState)
            assertFalse(state.canSave)
            assertEquals("保存先を設定してください", state.saveDisabledReason)
            assertTrue(threads.isNotEmpty() && threads.all { it == "replay-control" })
            assertTrue(core.setEnabled(false).get(10, TimeUnit.SECONDS).accepted)
            assertEquals(CaptureState.DISABLED, core.snapshot().captureState)
            core.closeAsync().get(10, TimeUnit.SECONDS)
            val count = threads.size
            assertFalse(core.setEnabled(true).get().accepted)
            assertEquals(count, threads.size)
            assertEquals(0, Files.list(workspace).use { it.count() })
        } finally { subscription.close(); runCatching { core.closeAsync().get(10, TimeUnit.SECONDS) };
            Files.walk(workspace).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } } }
    }

    @Test
    fun automaticRunChangesDoNotReplaceManualTargetOrAcceptShellText() {
        val workspace = Files.createTempDirectory("replay-core-test-")
        val core = ReplayCore(ReplaySettings(application = ApplicationTarget("com.manual.app", ApplicationMode.MANUAL)), workspace)
        try {
            assertTrue(core.updateApplication(ApplicationTarget("com.auto.app")).get().accepted)
            assertEquals("com.manual.app", core.snapshot().settings.application.packageName)
            assertFalse(core.updateApplication(ApplicationTarget("com.app;kill")).get().accepted)
            assertTrue(core.applySettings(ReplaySettings(replaySeconds = 900)).get().accepted)
            assertEquals(900, core.snapshot().settings.replaySeconds)
        } finally { core.closeAsync().get(10, TimeUnit.SECONDS); Files.deleteIfExists(workspace) }
    }
}
