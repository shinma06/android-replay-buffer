package io.github.shinma06.arb.state

import io.github.shinma06.arb.model.ArbConfig
import io.github.shinma06.arb.model.IdeDefaultSaveAction
import io.github.shinma06.arb.model.OutputDirPatch
import io.github.shinma06.arb.playback.AudioSink
import io.github.shinma06.arb.playback.AudioState
import io.github.shinma06.arb.playback.AudioSyncPlayer
import io.github.shinma06.arb.playback.FrameSeekResult
import io.github.shinma06.arb.playback.FrameSeeker
import io.github.shinma06.arb.playback.InMemoryPcmSource
import io.github.shinma06.arb.playback.PcmFormat
import io.github.shinma06.arb.playback.PlaybackController
import org.junit.jupiter.api.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

class ArbT5IntegrationModelTest {
    @Test
    fun trimCloseStateDistinguishesUntouchedCustomDirtyExportedAndResetRanges() {
        val tracker = TrimCloseState()
        tracker.initialize(totalFrames = 100L, initialIn = 0L, initialOut = 99L)
        assertFalse(tracker.hasUnexportedCustomTrim(), "full untouched range must close silently")

        // Narrow to custom range -> dirty
        tracker.onRangeAdjusted(10L, 45L)
        assertTrue(tracker.hasUnexportedCustomTrim(), "unexported custom range must prompt before close")
        assertEquals(10L to 45L, tracker.currentRange())

        // Exporting the exact range clears dirty state
        tracker.markExported(10L, 45L)
        assertFalse(tracker.hasUnexportedCustomTrim(), "exported custom range must close silently")

        // Editing to a new range makes it dirty again
        tracker.onRangeAdjusted(12L, 45L)
        assertTrue(tracker.hasUnexportedCustomTrim(), "subsequent edit after export must become dirty again")

        // Resetting back to full range clears dirty state
        tracker.onRangeAdjusted(0L, 99L)
        assertFalse(tracker.hasUnexportedCustomTrim(), "resetting back to full range must close silently")

        // Initial range restored from edit session is clean until modified in this tab
        tracker.initialize(totalFrames = 100L, initialIn = 15L, initialOut = 60L)
        assertFalse(tracker.hasUnexportedCustomTrim(), "loaded baseline session range must not nag immediately")
        tracker.onRangeAdjusted(20L, 60L)
        assertTrue(tracker.hasUnexportedCustomTrim())
    }

    @Test
    fun settingsDraftValidationDiffingAndStorageUnitConversionWork() {
        val base = ArbConfig()
        val draft = ArbSettingsDraft.fromConfig(base, customArbdBinaryPath = "")
        assertFalse(draft.isDaemonModifiedAgainst(base))
        assertNull(draft.validateDaemonFields())

        val absDir = java.nio.file.Paths.get("").toAbsolutePath().resolve("out-test").toString()
        val edited = draft.copy(
            saveWindowSecs = 120,
            ringBudgetBytes = 2L * 1024L * 1024L * 1024L,
            videoMaxSize = 1024,
            videoMaxFps = 15,
            audioEnabled = false,
            crashAutosaveCooldownSecs = 30,
            outputDirOverride = absDir,
            ideDefaultSaveAction = IdeDefaultSaveAction.SAVE_ONLY,
        )
        assertTrue(edited.isDaemonModifiedAgainst(base))
        assertNull(edited.validateDaemonFields())

        val patch = edited.toConfigPatch(base)
        assertEquals(120, patch.saveWindowSecs)
        assertEquals(2L * 1024L * 1024L * 1024L, patch.ringBudgetBytes)
        assertNull(patch.minFreeDiskBytes, "unchanged field must stay omitted")
        assertEquals(1024, patch.videoMaxSize)
        assertEquals(15, patch.videoMaxFps)
        assertEquals(false, patch.audioEnabled)
        assertNull(patch.crashAutosaveEnabled, "unchanged field must stay omitted")
        assertEquals(30, patch.crashAutosaveCooldownSecs)
        assertEquals(OutputDirPatch.Set(absDir), patch.outputDirOverride)
        assertEquals(IdeDefaultSaveAction.SAVE_ONLY, patch.ideDefaultSaveAction)

        // Clearing an existing outputDirOverride yields OutputDirPatch.Clear
        val baseWithDir = base.copy(outputDirOverride = absDir)
        val cleared = ArbSettingsDraft.fromConfig(baseWithDir).copy(outputDirOverride = "")
        assertEquals(OutputDirPatch.Clear, cleared.toConfigPatch(baseWithDir).outputDirOverride)

        // Validation catches out-of-range values and relative paths
        assertNotNull(draft.copy(saveWindowSecs = 4).validateDaemonFields())
        assertNotNull(draft.copy(saveWindowSecs = 601).validateDaemonFields())
        assertNotNull(draft.copy(ringBudgetBytes = 1024L).validateDaemonFields())
        assertNotNull(draft.copy(minFreeDiskBytes = 1024L).validateDaemonFields())
        assertNotNull(draft.copy(crashAutosaveCooldownSecs = 0).validateDaemonFields())
        assertNotNull(draft.copy(outputDirOverride = "relative/path").validateDaemonFields())

        // MiB / GiB conversion round-trip
        val gibVal = StorageSizeValue.fromBytes(ArbConfig.DEFAULT_RING_BUDGET_BYTES)
        assertEquals(StorageSizeUnit.GIB, gibVal.unit)
        assertEquals(1.5, gibVal.amount)
        assertEquals(ArbConfig.DEFAULT_RING_BUDGET_BYTES, gibVal.toBytes())

        val mibVal = StorageSizeValue.fromBytes(256L * 1024L * 1024L)
        assertEquals(StorageSizeUnit.MIB, mibVal.unit)
        assertEquals(256.0, mibVal.amount)
        assertEquals(256L * 1024L * 1024L, mibVal.toBytes())
    }

    @Test
    fun playbackControllerAttachesAsyncAudioPlayerAndClosesSafelyWhenAlreadyClosed() {
        val seeker = object : FrameSeeker {
            override fun seekExact(frameIndex: Long) = FrameSeekResult(frameIndex, frameIndex, false)
            override fun scrubApprox(frameIndex: Long) = FrameSeekResult(frameIndex, frameIndex, true)
        }
        val sink = object : AudioSink {
            override val isAvailable: Boolean = true
            var closed = false
            override fun open(format: PcmFormat): Boolean = true
            override fun start() = Unit
            override fun stop() = Unit
            override fun flush() = Unit
            override fun write(pcm: ByteArray, offset: Int, length: Int): Int = length
            override fun close() { closed = true }
        }

        val controller = PlaybackController(
            totalFrames = 30L,
            frameSeeker = seeker,
            autoRunLoop = false,
        )
        assertEquals(AudioState.UNAVAILABLE, controller.state.audioState)

        val pcm = InMemoryPcmSource(48_000, 2, ByteArray(48_000 * 4))
        val player = AudioSyncPlayer(source = pcm, sink = sink, avOffsetMs = 106L, autoRunPump = false)
        controller.attachAudioPlayer(player, avOffsetMs = 106L)
        assertEquals(106L, controller.state.avOffsetMs)
        assertEquals(AudioState.PAUSED, controller.state.audioState)

        controller.play()
        assertEquals(AudioState.PLAYING, controller.state.audioState)

        controller.close()
        assertTrue(sink.closed, "closing controller must close attached audio player")

        // Attaching after close must immediately close the late player without leaking
        val lateSink = object : AudioSink {
            override val isAvailable: Boolean = true
            var closed = false
            override fun open(format: PcmFormat): Boolean = true
            override fun start() = Unit
            override fun stop() = Unit
            override fun flush() = Unit
            override fun write(pcm: ByteArray, offset: Int, length: Int): Int = length
            override fun close() { closed = true }
        }
        val latePlayer = AudioSyncPlayer(
            source = InMemoryPcmSource(48_000, 2, ByteArray(4_800 * 4)),
            sink = lateSink,
            autoRunPump = false,
        )
        controller.attachAudioPlayer(latePlayer)
        assertTrue(lateSink.closed, "attaching audio player to a closed controller must close it immediately")
    }
}
