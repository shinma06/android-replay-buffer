package io.github.shinma06.replaybuffer.core

import java.nio.file.Files
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class CaptureStoreSourceTest {
    @Test
    fun packetOrderSurvivesClockChangesButRejectsMissingPacketsAndStaleOwners() = withStore { store ->
        store.generation(1)
        store.session(VideoPacket.Session(32, 32), 1)
        assertFalse(store.frame(VideoPacket.Frame(0, true, false, byteArrayOf(1)), 1))
        assertFalse(store.frame(VideoPacket.Frame(100, false, false, byteArrayOf(1)), 1)) // IDR wait consumes 1.
        assertTrue(store.frame(VideoPacket.Frame(200, false, true, byteArrayOf(2)), 1))
        val first = capture(store).video.single()
        assertEquals(2, first.packetOrdinal)
        assertTrue(first.decodeRun > 0)
        store.clockStatus(false, 1)
        store.clock.boundary()
        store.clockStatus(true, 1)
        store.status("device_log", StreamState.RECOVERING, "log-only", 1)
        store.frame(VideoPacket.Frame(300, false, false, byteArrayOf(3)), 1)
        val second = capture(store).video.last()
        assertEquals(first.decodeRun, second.decodeRun)
        assertEquals(3, second.packetOrdinal)
        assertFailsWith<IllegalArgumentException> {
            store.frame(VideoPacket.Frame(400, false, false, ByteArray(32 * 1024 * 1024)), 1)
        }
        assertFalse(store.frame(VideoPacket.Frame(450, false, true, byteArrayOf(4)), 99))
        store.status("video", StreamState.RECOVERING, "source interruption", 99)
        store.frame(VideoPacket.Frame(500, false, false, byteArrayOf(5)), 1)
        val afterGuard = capture(store).video.last()
        assertEquals(5, afterGuard.packetOrdinal)
        assertEquals(second.decodeRun, afterGuard.decodeRun) // Ordinal detects loss even before status is reported.
        store.frame(VideoPacket.Frame(550, false, true, byteArrayOf(6)), 1)
        val nextGop = capture(store).video.last()
        assertEquals(afterGuard.decodeRun, nextGop.decodeRun)
        assertTrue(afterGuard.file != nextGop.file)
        assertEquals(6, nextGop.packetOrdinal)
        store.status("video", StreamState.RECOVERING, "source interruption", 1)
        store.frame(VideoPacket.Frame(600, false, true, byteArrayOf(6)), 1)
        val resumed = capture(store).video.last()
        assertTrue(resumed.decodeRun > afterGuard.decodeRun)
        assertEquals(7, resumed.packetOrdinal)
        store.frame(VideoPacket.Frame(0, true, false, byteArrayOf(7)), 1)
        assertFalse(store.frame(VideoPacket.Frame(700, false, false, byteArrayOf(8)), 1))
        store.frame(VideoPacket.Frame(800, false, true, byteArrayOf(9)), 1)
        val reconfigured = capture(store).video.last()
        assertTrue(reconfigured.decodeRun > resumed.decodeRun)
        assertEquals(9, reconfigured.packetOrdinal)
        store.session(VideoPacket.Session(32, 32), 1)
        store.frame(VideoPacket.Frame(0, true, false, byteArrayOf(7)), 1)
        store.frame(VideoPacket.Frame(100, false, true, byteArrayOf(10)), 1) // PTS reset belongs to new session.
        val session = capture(store).video.last()
        assertTrue(session.decodeRun > reconfigured.decodeRun)
        assertEquals(10, session.packetOrdinal)
        store.generation(2)
        store.session(VideoPacket.Session(32, 32), 2)
        store.frame(VideoPacket.Frame(0, true, false, byteArrayOf(7)), 2)
        store.frame(VideoPacket.Frame(100, false, true, byteArrayOf(11)), 2)
        val generation = capture(store).video.last()
        assertTrue(generation.decodeRun > session.decodeRun)
        assertEquals(11, generation.packetOrdinal)
    }

    @Test
    fun capacityLossDoesNotChangeLiveRunOrPinnedCapture() = withStore(2) { store ->
        store.generation(1)
        store.session(VideoPacket.Session(32, 32), 1)
        store.frame(VideoPacket.Frame(0, true, false, byteArrayOf(1)), 1)
        store.frame(VideoPacket.Frame(100, false, true, byteArrayOf(1)), 1)
        val fixed = store.capture(ReplaySettings())!!
        val original = fixed.video.toList()
        store.frame(VideoPacket.Frame(200, false, false, byteArrayOf(2)), 1)
        store.frame(VideoPacket.Frame(300, false, false, byteArrayOf(3)), 1) // Evicts oldest IDR.
        store.release(fixed.id)
        val evicted = capture(store)
        assertEquals(listOf(2L, 3L), evicted.video.map { it.packetOrdinal })
        assertTrue(evicted.video.all { it.decodeRun == original.single().decodeRun })
        assertTrue(evicted.gaps.any { it.stream == "video" })
        assertEquals(original, fixed.video)
    }

    @Test
    fun coalescedNullTimeInterruptionStillStartsANewDecodeRun() = withStore { store ->
        store.generation(1)
        store.session(VideoPacket.Session(32, 32), 1)
        store.frame(VideoPacket.Frame(0, true, false, byteArrayOf(1)), 1)
        store.frame(VideoPacket.Frame(100, false, true, byteArrayOf(1)), 1)
        store.status("video", StreamState.RECOVERING, "same null-time cut", 1)
        store.frame(VideoPacket.Frame(200, false, true, byteArrayOf(2)), 1)
        val interrupted = capture(store)
        assertTrue(interrupted.videoCuts.keys.any { it.reason == "same null-time cut" })
        store.status("video", StreamState.RECOVERING, "same null-time cut", 1)
        store.frame(VideoPacket.Frame(300, false, true, byteArrayOf(3)), 1)
        val coalesced = capture(store)
        assertTrue(coalesced.videoCuts.keys.none { it.reason == "same null-time cut" })
        assertEquals(1, coalesced.gaps.count { it.reason == "same null-time cut" })
        assertTrue(coalesced.video.last().decodeRun > interrupted.video.last().decodeRun)
        assertEquals(3, coalesced.video.last().packetOrdinal)
    }

    private fun capture(store: CaptureStore): FrozenCapture = store.capture(ReplaySettings())!!.also { store.release(it.id) }

    private fun withStore(limit: Long = ReplaySettings.VIDEO_BYTES, check: (CaptureStore) -> Unit) {
        val root = Files.createTempDirectory("replay-source-facts-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0, videoLimit = limit)
        try { check(store) } finally {
            store.close()
            Files.walk(root).use { files -> files.sorted(Comparator.reverseOrder()).forEach(Files::deleteIfExists) }
        }
    }
}
