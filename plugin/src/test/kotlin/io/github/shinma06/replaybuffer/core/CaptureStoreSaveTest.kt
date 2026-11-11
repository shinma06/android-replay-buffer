package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonObject
import com.google.gson.JsonParser
import org.jcodec.codecs.h264.H264Encoder
import org.jcodec.codecs.h264.H264Utils
import org.jcodec.common.io.NIOUtils
import org.jcodec.common.model.ColorSpace
import org.jcodec.common.model.Picture
import org.jcodec.containers.mp4.demuxer.MP4Demuxer
import java.nio.ByteBuffer
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.CancellationException
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertNotNull
import kotlin.test.assertNotEquals
import kotlin.test.assertTrue

class CaptureStoreSaveTest {
    @Test
    fun frameSelectionKeepsCutPredecessorAndUnknownRowsWithoutCrossingGaps() {
        for (gap in listOf(false, true)) {
            val root = Files.createTempDirectory("replay-frame-selection-")
            val store = CaptureStore(root.resolve("ring"), minFree = 0)
            val host = System.nanoTime() + 60_000_000_000
            val bytes = sample()
            try {
                store.generation(1); store.session(VideoPacket.Session(32, 32), 1)
                store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
                for (second in 1L..7L) {
                    val received = host + (second - 1) * 1_000_000_000
                    anchor(store.clock, second * 1_000_000_000, received)
                    if (second == 1L) assertTrue(store.frame(VideoPacket.Frame(0, false, true, bytes), 1, received))
                    if (second in listOf(1L, 2L, 4L, 5L, 6L))
                        assertTrue(store.frame(VideoPacket.Frame(second * 1_000_000, false, true, bytes), 1, received))
                    if (gap && second == 4L) store.status("video", StreamState.RECOVERING, "cut gap", 1)
                }
                store.freeze()
                assertTrue(store.frame(VideoPacket.Frame(8_000_000, false, true, bytes), 1, host + 7_000_000_000))
                val selected = store.capture(ReplaySettings(replaySeconds = 2))!!
                assertTrue(selected.windowKnown)
                assertEquals(4_000_000_000, selected.start)
                assertEquals(6_000_000_000, selected.end)
                // The unknown-time frame at pts 0 arrived before the 2 second clock read, so it is outside the window.
                assertEquals(if (gap) listOf(5_000_000L, 6_000_000L) else listOf(4_000_000L, 5_000_000L, 6_000_000L),
                    selected.video.map { it.pts })
                assertEquals(gap, selected.gaps.any { it.reason == "cut gap" })
                val pinned = selected.video.toList()
                store.prune(1)
                assertEquals(pinned, selected.video)
                assertTrue(pinned.all { Files.exists(it.file) })
                assertFailsWith<IllegalStateException> { store.capture(ReplaySettings()) }
                store.release(selected.id)
                store.resume()
                store.clock.boundary()
                val unknown = store.capture(ReplaySettings(replaySeconds = 2))!!
                assertFalse(unknown.windowKnown)
                assertEquals(listOf(0L, 1_000_000L, 2_000_000L, 4_000_000L, 5_000_000L, 6_000_000L, 8_000_000L),
                    unknown.video.map { it.pts })
                assertEquals(selected.gaps, unknown.gaps)
                store.release(unknown.id)
            } finally { store.close(); root.toFile().deleteRecursively() }
        }
    }

    @Test
    fun manyLogsUseMaximumClockHistoryWithoutChangingOrderIdsOrPinnedRows() {
        val root = Files.createTempDirectory("replay-batch-clock-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime() + 1_000_000_000
        try {
            store.generation(1)
            store.status("device_log", StreamState.CAPTURING, null, 1)
            for (i in 0..4096) anchor(store.clock, 1_000_000_000 + i * 1_000_000L, host + i * 1_000_000L)
            assertEquals(4096, store.clock.snapshot().size)
            store.app("com.example.target", 10001, setOf(12), 1, uidExclusive = true)
            val ids = List(10_000) { i ->
                store.log(DeviceLog(1_700_000_005_096_000_000, 12, 12, 10001, i % 2, 4,
                    "Fixture", "row-$i", byteArrayOf(1)), 1, host + 4_096_000_000)!!
            }
            store.freeze()
            val capture = store.capture(ReplaySettings())!!
            assertEquals(ids, capture.logs.map { it.id })
            assertEquals((0 until 10_000).map { "row-$it" }, capture.logs.map { it.source.message })
            assertTrue(capture.logs.all { it.time.sequence == 4_096_000_000L && it.app == true })
            val rows = capture.logs.toList()
            val clocks = capture.clocks.toList()
            anchor(store.clock, 10_000_000_000, host + 9_000_000_000)
            store.clock.boundary(host + 9_000_000_000)
            store.prune(1)
            assertEquals(rows, capture.logs)
            assertEquals(clocks, capture.clocks)
            assertFailsWith<IllegalStateException> { store.capture(ReplaySettings(replaySeconds = 1)) }
            store.release(capture.id)
        } finally { store.close(); root.toFile().deleteRecursively() }
    }

    @Test
    fun partialReaderFailureKeepsItsGapAndOutOfOrderPruneCannotEvictFreshRows() {
        for (oldAfterPrune in listOf(false, true)) {
            val root = Files.createTempDirectory("replay-log-partition-store-")
            val store = CaptureStore(root.resolve("ring"), logLimit = 514, minFree = 0)
            val host = System.nanoTime() + 1_000_000_000
            fun row(second: Long, lid: Int) = DeviceLog(1_700_000_000_000_000_000 + second * 1_000_000_000,
                12, 12, 10001, lid, 4, "Fixture", "same", byteArrayOf(1))
            try {
                store.generation(1); store.status("device_log", StreamState.CAPTURING, null, 1)
                for (second in 1L..5L) anchor(store.clock, second * 1_000_000_000, host + (second - 1) * 1_000_000_000)
                store.app("com.fixture.app", 10001, setOf(12), 1, uidExclusive = true)
                val first = store.log(row(5, 0), 1, host + 4_000_000_000)
                var second: String? = null
                if (oldAfterPrune) {
                    second = store.log(row(5, 1), 1, host + 4_010_000_000)
                    store.prune(2) // The ring is full of fresh rows when the normal poll completes.
                }
                store.status("device_log", StreamState.RECOVERING, "all reader interrupted", 1)
                store.log(row(2, 1), 1, host + 4_100_000_000)
                val partial = store.capture(ReplaySettings(replaySeconds = 30))!!
                assertEquals(StreamState.RECOVERING, partial.states.getValue("device_log").state)
                assertTrue(partial.gaps.any { it.stream == "device_log" && it.toNs == null })
                if (oldAfterPrune) {
                    assertEquals(setOf(first, second), partial.logs.map { it.id }.toSet())
                    assertTrue(partial.gaps.none { it.reason.contains("byte上限") })
                }
                store.release(partial.id)
                store.prune(2)
                if (!oldAfterPrune) second = store.log(row(5, 1), 1, host + 4_200_000_000)
                store.status("device_log", StreamState.CAPTURING, null, 1)
                val recovered = store.capture(ReplaySettings(replaySeconds = 30))!!
                assertEquals(setOf(first, second), recovered.logs.map { it.id }.toSet())
                assertEquals(2, recovered.logs.size) // Equal contents across buffers are distinct records.
                assertTrue(recovered.logs.all { it.app == true })
                assertTrue(recovered.gaps.none { it.reason.contains("byte上限") })
                assertTrue(recovered.gaps.any { it.stream == "device_log" && it.toNs != null })
            } finally { store.close(); root.toFile().deleteRecursively() }
        }
        for (protection in listOf("unknown-clock", "unknown-row", "frozen-pin")) {
            val root = Files.createTempDirectory("replay-log-expiry-proof-")
            val store = CaptureStore(root.resolve("ring"), logLimit = 514, minFree = 0)
            val host = System.nanoTime() + 1_000_000_000
            fun row(second: Long, lid: Int) = DeviceLog(1_700_000_000_000_000_000 + second * 1_000_000_000,
                12, 12, 10001, lid, 4, "Fixture", "same", byteArrayOf(1))
            try {
                store.generation(1); store.status("device_log", StreamState.CAPTURING, null, 1)
                for (second in 1L..5L) anchor(store.clock, second * 1_000_000_000, host + (second - 1) * 1_000_000_000)
                val first = store.log(row(5, 0), 1, host + 4_000_000_000)
                val second = store.log(row(5, 1), 1, host + 4_010_000_000)
                store.prune(2)
                val pinned = if (protection == "frozen-pin") store.capture(ReplaySettings(replaySeconds = 2)) else null
                if (protection == "frozen-pin") store.freeze()
                if (protection == "unknown-clock") {
                    store.clock.boundary()
                    assertEquals(Long.MAX_VALUE, store.clock.endUncertainty())
                }
                val delayed = row(if (protection == "unknown-row") 40 else 2, 1)
                val accepted = store.log(delayed, 1, host + 4_100_000_000)
                pinned?.let {
                    assertEquals(setOf(first, second), it.logs.map { row -> row.id }.toSet())
                    store.release(it.id)
                }
                val capture = store.capture(ReplaySettings(replaySeconds = 30))!!
                if (protection == "frozen-pin") {
                    assertTrue(pinned!!.logs.any { it.id == first })
                    assertTrue(capture.logs.none { it.id == first }) // Frozen time pruning is disabled; byte loss stays explicit.
                } else assertTrue(capture.logs.any { it.id == accepted }, protection)
                if (protection == "unknown-row") {
                    val unknown = capture.logs.single { it.id == accepted }
                    assertNull(unknown.time.sequence)
                    assertEquals(Long.MAX_VALUE, unknown.time.uncertainty)
                }
                assertTrue(capture.gaps.any { it.reason.contains("byte上限") }, protection)
            } finally { store.close(); root.toFile().deleteRecursively() }
        }
    }

    @Test
    fun delayedValidClockKeepsNewTargetLogMembershipWithoutReclassifyingTrueUnknown() {
        val root = Files.createTempDirectory("replay-clock-receipt-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime()
        fun record(elapsed: Long, message: String) = DeviceLog(1_700_000_000_000_000_000 + elapsed,
            12, 12, 10001, 0, 4, "Fixture", message, byteArrayOf(1))
        try {
            store.generation(1); store.status("device_log", StreamState.CAPTURING, null, 1)
            assertTrue(store.clock.add("1", listOf(1_000_000_000L, 1_000_000_000L,
                1_700_000_001_000_000_000, 1_000_000_000), host, host + 200_000))
            store.app("com.example.target", 10001, setOf(12), 1, uidExclusive = true)
            assertNull(store.log(record(1_100_000_000, "wrong generation"), 99, host + 100_100_000))
            val firstAccepted = store.log(record(1_100_000_000, "known window start"), 1, host + 100_100_000)
            assertTrue(store.clock.add("1", listOf(2_000_000_000L, 2_000_000_000L,
                1_700_000_002_000_000_000, 2_000_000_000), host + 1_000_000_000, host + 1_031_842_708))
            val normal = record(2_034_000_000, "causally arrived target")
            val unknown = record(40_000_000_000, "unsupported source")
            store.log(normal, 1, host + 1_035_100_000)
            store.log(unknown, 1, host + 1_060_000_000)
            anchor(store.clock, 3_000_000_000, host + 2_000_000_000)
            val capture = store.capture(ReplaySettings(replaySeconds = 180))!!
            assertEquals(firstAccepted, capture.logs.single { it.source.message == "known window start" }.id)
            val target = capture.logs.single { it.source === normal }
            val unsupported = capture.logs.single { it.source === unknown }
            assertEquals(true, target.app)
            assertEquals(1_034_000_000, target.retainedAt)
            assertEquals(1_034_000_000, target.time.sequence)
            assertEquals(null, unsupported.app)
            assertEquals(null, unsupported.time.sequence)
            assertEquals(Long.MAX_VALUE, unsupported.time.uncertainty)
            val output = SaveWriter().write(capture, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
            val device = Files.readAllLines(output.directory.resolve("logcat-device.jsonl")).map { JsonParser.parseString(it).asJsonObject }
            val app = Files.readAllLines(output.directory.resolve("logcat-app.jsonl")).map { JsonParser.parseString(it).asJsonObject }
            assertEquals(3, device.size)
            assertEquals(2, app.size)
            assertEquals(target.id, app.single { it["message"].asString == normal.message }["record_id"].asString)
            assertTrue(device.single { it["record_id"].asString == unsupported.id }["app_membership"].isJsonNull)
            assertEquals(null, unsupported.app)
            store.release(capture.id)
        } finally {
            store.close()
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }

    @Test
    fun remappedOldUnknownLogDoesNotPolluteAnEmptyApplicationWindowOrFrozenRetry() {
        val root = Files.createTempDirectory("replay-log-remap-window-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime() + 1_000_000_000
        fun record(elapsed: Long, message: String, uid: Long = 10002) =
            DeviceLog(1_700_000_000_000_000_000 + elapsed, 99, 99, uid, 0, 4, "Fixture", message, byteArrayOf(1))
        try {
            store.generation(1); store.status("device_log", StreamState.CAPTURING, null, 1)
            val old = record(1_500_000_000, "old initially unknown")
            store.log(old, 1, host + 500_000_000) // No clock sample exists yet.
            anchor(store.clock, 1_000_000_000, host)
            store.log(record(1_100_000_000, "sequence start"), 1, host + 100_000_000)
            anchor(store.clock, 5_000_000_000, host + 4_000_000_000)
            store.app("com.example.target", 10001, setOf(12), 1, uidExclusive = true)
            val current = record(5_500_000_000, "current non-target")
            store.log(current, 1, host + 4_500_000_000)
            anchor(store.clock, 6_000_000_000, host + 5_000_000_000)
            store.prune(2) // The first unknown record remains in the ring.
            val capture = store.capture(ReplaySettings(replaySeconds = 2))!!
            assertEquals(3_000_000_000, capture.start)
            assertEquals(5_000_000_000, capture.end)
            assertEquals(listOf(current), capture.logs.map { it.source })
            assertTrue(capture.logs.all { it.app == false })
            assertTrue(capture.gaps.isEmpty())
            assertEquals(StreamState.CAPTURING, capture.states["app_log"]?.state)
            assertEquals(null, capture.states["app_log"]?.reason)
            assertEquals(0.0, capture.states["app_log"]?.availableSeconds)
            assertFailsWith<CancellationException> {
                SaveWriter().write(capture, root, { true }) { publication -> Files.move(publication.partial, publication.complete) }
            }
            assertFailsWith<IllegalStateException> { store.capture(ReplaySettings(replaySeconds = 8)) }
            anchor(store.clock, 8_000_000_000, host + 7_000_000_000)
            store.app("com.example.other", 10003, setOf(99), 1, uidExclusive = true)
            store.log(record(8_000_000_000, "later target", 10003), 1, host + 7_000_000_000)
            val output = SaveWriter().write(capture, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
            val device = Files.readAllLines(output.directory.resolve("logcat-device.jsonl"))
            assertEquals(1, device.size)
            assertTrue(Files.readAllLines(output.directory.resolve("logcat-app.jsonl")).isEmpty())
            assertEquals(capture.logs.single().id, JsonParser.parseString(device.single()).asJsonObject["record_id"].asString)
            assertEquals(2, capture.seconds)
            assertEquals(5_000_000_000, capture.end)
            assertEquals(listOf(current), capture.logs.map { it.source })
            store.release(capture.id)
            val wider = store.capture(ReplaySettings(replaySeconds = 8))!!
            val retained = wider.logs.single { it.source === old }
            assertEquals(500_000_000, retained.time.sequence)
            assertEquals(0, retained.retainedAt)
            assertEquals(null, retained.app) // Arrival-time membership was never rewritten.
            assertTrue(wider.logs.any { it.app == true })
            store.release(wider.id)
        } finally {
            store.close()
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }

    @Test
    fun remappedLogsKeepBothBoundaryErrorsAndRealUnknownInformation() {
        val root = Files.createTempDirectory("replay-log-window-errors-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime() + 1_000_000_000
        fun record(elapsed: Long, message: String, uid: Long = 10002) =
            DeviceLog(1_700_000_000_000_000_000 + elapsed, 99, 99, uid, 0, 4, "Fixture", message, byteArrayOf(1))
        try {
            store.generation(1); store.status("device_log", StreamState.CAPTURING, null, 1)
            anchor(store.clock, 1_000_000_000, host)
            store.app("com.example.target", 10001, setOf(12), 1)
            store.log(record(1_100_000_000, "sequence start"), 1, host + 100_000_000)
            anchor(store.clock, 6_000_000_000, host + 5_000_000_000)
            for ((elapsed, message) in listOf(3_997_000_000L to "before outside", 3_998_000_000L to "before edge",
                    4_000_000_000L to "start", 6_002_000_000L to "after edge", 6_003_000_000L to "after outside",
                    999_000_000_000L to "unknown clock")) {
                store.log(record(elapsed, message), 1, host + 5_000_000_000)
            }
            store.log(record(5_500_000_000, "unknown shared UID", 10001), 1, host + 5_000_000_000)
            store.log(record(5_600_000_000, "normal non-target"), 1, host + 5_000_000_000)
            store.status("device_log", StreamState.RECOVERING, "actual interruption", 1)
            store.status("device_log", StreamState.CAPTURING, null, 1)
            val capture = store.capture(ReplaySettings(replaySeconds = 2))!!
            assertEquals(1_000_000, capture.endUncertainty)
            assertEquals(setOf("before edge", "start", "after edge", "unknown clock", "unknown shared UID", "normal non-target"),
                capture.logs.map { it.source.message }.toSet())
            assertTrue(capture.logs.filter { it.source.message?.endsWith("edge") == true }.all { it.time.uncertainty == 1_000_000L })
            assertEquals(null, capture.logs.single { it.source.message == "unknown clock" }.time.sequence)
            assertEquals(null, capture.logs.single { it.source.message == "unknown shared UID" }.app)
            assertTrue(capture.gaps.single().intersects(capture.start, capture.end, capture.endUncertainty))
            store.release(capture.id)
            store.clock.boundary()
            val unknownWindow = store.capture(ReplaySettings(replaySeconds = 2))!!
            assertFalse(unknownWindow.windowKnown)
            assertEquals(9, unknownWindow.logs.size)
            assertEquals(capture.gaps, unknownWindow.gaps)
            store.release(unknownWindow.id)
        } finally {
            store.close()
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }

    @Test
    fun oneInvalidClockReadDoesNotFragmentKnownVideoBeforeAndAfterRecovery() {
        verifyVideoAroundClockFailure(false)
    }

    @Test
    fun clockProcessEpochBoundaryDoesNotFragmentItsIndependentKnownVideoRegions() {
        verifyVideoAroundClockFailure(true)
    }

    @Test
    fun recoverySampleKeepsKnownVideoTogetherBeforeNextClockSample() {
        verifyVideoAroundClockFailure(false, true)
    }

    @Test
    fun processRecoverySampleKeepsKnownVideoTogetherBeforeNextClockSample() {
        verifyVideoAroundClockFailure(true, true)
    }

    @Test
    fun recoveryScopeKeepsUnknownAndOverflowBoundariesUnbounded() {
        val before = ClockSample(0, "1", 2_000_000_000, 2_000_000_000, 0, 2_000_000_000, 0, 0, -1_000_000_000)
        val recovery = before.copy(before = 4_000_000_000, after = 4_000_000_000, mono = 4_000_000_000)
        val gap = CaptureGap("clock", 1_000_000_000, 3_000_009_083, "failure", Long.MAX_VALUE)
        val clocks = listOf(before, recovery)
        for (unproven in listOf(recovery.copy(segment = 1), recovery.copy(sequenceOffset = Long.MAX_VALUE),
                recovery.copy(received = 100_000_000))) {
            assertEquals(null, listOf(gap).gapScopes(clocks, mapOf(gap to unproven)).single().toNs)
        }
        val proven = listOf(gap).gapScopes(clocks, mapOf(gap to recovery)).single()
        assertEquals(gap.toNs, proven.toNs)
        assertEquals(1_000_000, proven.boundaryUncertaintyNs)
        val unknownStart = gap.copy(fromNs = null)
        assertEquals(null, listOf(unknownStart).gapScopes(clocks, mapOf(unknownStart to recovery)).single().fromNs)
        val open = gap.copy(toNs = null)
        assertEquals(null, listOf(open).gapScopes(clocks, mapOf(open to recovery)).single().toNs)
        assertEquals(Long.MAX_VALUE, gap.boundaryUncertaintyNs)
    }

    private fun verifyVideoAroundClockFailure(processFailure: Boolean, immediateRecovery: Boolean = false) {
        val root = Files.createTempDirectory("replay-clock-video-regions-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val encoder = H264Encoder.createH264Encoder()
        val encoded = if (immediateRecovery) (0..5).map { n ->
            val data = encoder.encodeFrame(Picture.create(32, 32, ColorSpace.YUV420J).apply { fill(15 + n) }, ByteBuffer.allocate(65536)).data
            ByteArray(data.remaining()).also { data.get(it) }
        } else emptyList()
        val bytes = encoded.firstOrNull() ?: sample()
        assertTrue(encoded.drop(1).all { frame -> H264Utils.splitFrame(ByteBuffer.wrap(frame)).any { it.get(0).toInt() and 31 == 1 } })
        val host = System.nanoTime() - if (immediateRecovery) 3_000_000_000 else 0
        try {
            store.generation(1); store.status("device_log", StreamState.CAPTURING, null, 1); store.session(VideoPacket.Session(32, 32), 1)
            anchor(store.clock, 1_000_000_000, host)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            for ((i, pts) in listOf(1_100_000L, 1_300_000L, 1_500_000L).withIndex()) {
                assertTrue(store.frame(VideoPacket.Frame(pts, false, !immediateRecovery || i == 0, encoded.getOrElse(i) { bytes }),
                    1, host + (pts - 1_000_000) * 1000))
            }
            anchor(store.clock, 2_000_000_000, host + 1_000_000_000)
            if (processFailure) {
                store.clock.boundary(host + 2_000_000_000)
                store.clockStatus(false, 1)
            } else {
                val healthy = store.clock.add("1", listOf(3_000_000_000, 3_000_000_000,
                    1_700_000_003_000_000_000, 3_002_000_001), host + 2_000_000_000, host + 2_001_000_000)
                assertFalse(healthy)
                store.clockStatus(healthy, 1)
            }
            val recoveredAt = if (immediateRecovery) System.nanoTime() - 2_000_000 else host + 3_000_000_000
            anchor(store.clock, 4_000_000_000, recoveredAt)
            store.clockStatus(true, 1) // Same add -> status order as DeviceCapture, with the received sample in the past.
            val firstPts = if (immediateRecovery) {
                val atRecovery = store.capture(ReplaySettings())!!
                val endOfGap = atRecovery.gaps.single().toNs!!
                assertTrue(endOfGap > 3_000_000_000) // clock.now() has moved beyond the recovery sample.
                store.release(atRecovery.id)
                (endOfGap + 1_000_000_000) / 1000 + 100_000
            } else {
                anchor(store.clock, 5_000_000_000, host + 4_000_000_000)
                5_100_000L
            }
            for ((i, pts) in listOf(firstPts, firstPts + 200_000, firstPts + 400_000).withIndex()) {
                val frameHost = if (immediateRecovery) recoveredAt + (pts - 4_000_000) * 1000 else host + (pts - 1_000_000) * 1000
                assertTrue(store.frame(VideoPacket.Frame(pts, false, !immediateRecovery, encoded.getOrElse(i + 3) { bytes }), 1, frameHost))
            }
            if (immediateRecovery) {
                val lastHost = recoveredAt + (firstPts + 400_000 - 4_000_000) * 1000
                while (System.nanoTime() <= lastHost + 10_000_000) Thread.sleep(1)
            } else anchor(store.clock, 6_000_000_000, host + 5_000_000_000)
            val capture = store.capture(ReplaySettings())!!
            assertEquals(6, capture.video.size)
            assertTrue(capture.video.all { it.time.sequence != null && it.time.uncertainty != Long.MAX_VALUE })
            val gap = capture.gaps.single()
            assertEquals(Long.MAX_VALUE, gap.boundaryUncertaintyNs)
            assertTrue(gap.fromNs != null && gap.toNs != null)
            val output = SaveWriter().write(capture, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
            val manifest = JsonParser.parseString(Files.readString(output.directory.resolve("replay.json"))).asJsonObject
            val parts = manifest["parts"].asJsonArray
            assertEquals(1, parts.size())
            assertEquals("video.mp4", parts.single().asJsonObject["file"].asString)
            assertTrue(parts.single().asJsonObject["duration_us"].asLong > 400001)
            // A clock step stays visible; an outage the recovery read proved continuous is no loss.
            assertEquals(if (processFailure) listOf("video") else emptyList(), output.missingKinds)
            fun steps(m: JsonObject, kind: String) = m["timebase"].asJsonObject["clock_steps"].asJsonArray.map { it.asJsonObject }.filter { it["kind"].asString == kind }
            // Clock outages are not stream gaps; only an outage no recovery proved continuous stays in the timebase.
            assertTrue(manifest["gaps"].asJsonArray.isEmpty)
            assertEquals(!processFailure, gap.resolvedClock(capture.clockRecoveries))
            assertEquals(if (processFailure) 1 else 0, steps(manifest, "clock_step").size)
            val unmeasured = steps(manifest, "unmeasured")
            assertEquals(if (processFailure) 1 else 0, unmeasured.size)
            val scope = unmeasured.singleOrNull()
            if (scope != null) {
                assertTrue(scope["derived_from_valid_samples"].asBoolean)
                assertTrue(scope["from_t_ns"].asString.toLong() + capture.start <= gap.fromNs!!)
                assertTrue(scope["to_t_ns"].asString.toLong() + capture.start >= gap.toNs!!)
                assertTrue(scope["boundary_uncertainty_ns"].asString.toLong() > 0)
            }
            assertEquals(!processFailure, manifest["video_missing_ranges"].asJsonArray.isEmpty)
            val frames = Files.readAllLines(output.directory.resolve("video-frames.jsonl")).map { JsonParser.parseString(it).asJsonObject }
            assertEquals(capture.video.map { it.pts.toString() }, frames.filter { it["presented"].asBoolean }.map { it["source_pts_us"].asString })
            assertEquals(6, frames.count { it["presented"].asBoolean })
            assertEquals(0, frames.count { it["preroll"].asBoolean })
            assertEquals(capture.video.zipWithNext().map { (a, b) -> b.pts - a.pts },
                frames.dropLast(1).map { it["source_duration_us"].asLong })
            if (immediateRecovery) {
                assertEquals(if (processFailure) 3 else 4, capture.clocks.size) // No next periodic sample has been added.
                assertEquals(capture.clocks.last(), capture.clockRecoveries[gap])
                assertTrue(capture.clockRecoveries.getValue(gap).valid)
                scope?.let { assertTrue(it["recovery_sample"].asJsonObject["valid"].asBoolean) }
                val noRecoveryProof = capture.copy(clockRecoveries = emptyMap())
                val unproven = SaveWriter().write(noRecoveryProof, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
                val unprovenManifest = JsonParser.parseString(Files.readString(unproven.directory.resolve("replay.json"))).asJsonObject
                assertEquals(1, unprovenManifest["parts"].asJsonArray.size())
                anchor(store.clock, 5_000_000_000, recoveredAt + 1_000_000_000)
                val retry = SaveWriter().write(capture, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
                val retryManifest = JsonParser.parseString(Files.readString(retry.directory.resolve("replay.json"))).asJsonObject
                assertEquals(parts, retryManifest["parts"])
                assertEquals(gap, capture.gaps.single())
                store.release(capture.id)
                val next = store.capture(ReplaySettings())!!
                assertEquals(if (processFailure) 4 else 5, next.clocks.size)
                val nextOutput = SaveWriter().write(next, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
                val nextManifest = JsonParser.parseString(Files.readString(nextOutput.directory.resolve("replay.json"))).asJsonObject
                assertEquals(1, nextManifest["parts"].asJsonArray.size())
                assertEquals(unmeasured.map { it["from_t_ns"].asString.toLong() + capture.start to it["to_t_ns"].asString.toLong() + capture.start },
                    steps(nextManifest, "unmeasured").map { it["from_t_ns"].asString.toLong() + next.start to it["to_t_ns"].asString.toLong() + next.start })
                assertEquals(capture.video.map { it.pts }, next.video.map { it.pts })
                assertEquals(gap, next.gaps.single())
                store.release(next.id)
                return
            }
            val unknown = capture.copy(gaps = listOf(gap.copy(fromNs = null, toNs = null)))
            val unknownOutput = SaveWriter().write(unknown, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
            val unknownManifest = JsonParser.parseString(Files.readString(unknownOutput.directory.resolve("replay.json"))).asJsonObject
            assertEquals(1, unknownManifest["parts"].asJsonArray.size())
            assertEquals(listOf("video"), unknownOutput.missingKinds)
            assertFalse(steps(unknownManifest, "unmeasured").single()["derived_from_valid_samples"].asBoolean)
            val open = capture.copy(gaps = listOf(gap.copy(toNs = null)))
            val openOutput = SaveWriter().write(open, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
            val openManifest = JsonParser.parseString(Files.readString(openOutput.directory.resolve("replay.json"))).asJsonObject
            assertEquals(1, openManifest["parts"].asJsonArray.size())
            val noStart = capture.copy(gaps = listOf(gap.copy(fromNs = null)))
            val noStartOutput = SaveWriter().write(noStart, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
            val noStartManifest = JsonParser.parseString(Files.readString(noStartOutput.directory.resolve("replay.json"))).asJsonObject
            assertEquals(1, noStartManifest["parts"].asJsonArray.size())
            val unknownClock = capture.copy(clocks = emptyList(), clockRecoveries = emptyMap())
            val noProofOutput = SaveWriter().write(unknownClock, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
            val noProof = JsonParser.parseString(Files.readString(noProofOutput.directory.resolve("replay.json"))).asJsonObject
            assertEquals(1, noProof["parts"].asJsonArray.size())
            anchor(store.clock, 8_000_000_000, host + 7_000_000_000)
            for (pts in listOf(7_100_000L, 7_300_000L, 7_500_000L)) {
                store.frame(VideoPacket.Frame(pts, false, true, bytes), 1, host + (pts - 1_000_000) * 1000)
            }
            val retry = SaveWriter().write(capture, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
            val retryManifest = JsonParser.parseString(Files.readString(retry.directory.resolve("replay.json"))).asJsonObject
            assertEquals(parts, retryManifest["parts"])
            assertEquals(gap, capture.gaps.single())
            store.release(capture.id)
            val later = store.capture(ReplaySettings(replaySeconds = 1))!!
            val laterOutput = SaveWriter().write(later, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
            val laterManifest = JsonParser.parseString(Files.readString(laterOutput.directory.resolve("replay.json"))).asJsonObject
            assertEquals(1, laterManifest["parts"].asJsonArray.size())
            assertEquals("1000000", laterManifest["parts"].asJsonArray.single().asJsonObject["duration_us"].asString)
            assertTrue(laterOutput.missingKinds.isEmpty())
            assertEquals(gap, later.gaps.single())
            store.release(later.id)
            if (!immediateRecovery) {
                val afterGap = store.capture(ReplaySettings(replaySeconds = 1), host = host + 7_200_000_000)!!
                assertEquals(6_200_000_000, afterGap.start)
                assertEquals(7_200_000_000, afterGap.end)
                assertEquals(gap, afterGap.gaps.single()) // Preserve the original uncertain history.
                val clean = SaveWriter().write(afterGap, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
                assertTrue(clean.missingKinds.isEmpty(), Files.readString(clean.directory.resolve("replay.json")))
                assertTrue(afterGap.states.values.all { state -> state.gaps.isEmpty() })
                assertTrue(store.streams(1, afterGap.end).values.all { state -> state.gaps.isEmpty() })
                val cleanManifest = JsonParser.parseString(Files.readString(clean.directory.resolve("replay.json"))).asJsonObject
                assertTrue(cleanManifest["video_missing_ranges"].asJsonArray.isEmpty)
                assertTrue(cleanManifest["gaps"].asJsonArray.isEmpty)
                assertEquals(if (processFailure) 1 else 0, steps(cleanManifest, "unmeasured").size)
                store.release(afterGap.id)
            }
        } finally {
            store.close()
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }

    @Test
    fun clockEpochChangeKeepsAnUnheldTailMissingEvenWithAnEarlierPlayablePart() {
        val root = Files.createTempDirectory("replay-unheld-epoch-tail-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime()
        val bytes = sample()
        try {
            store.generation(1); store.status("device_log", StreamState.CAPTURING, null, 1); store.session(VideoPacket.Session(32, 32), 1)
            anchor(store.clock, 1_000_000_000, host)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            store.frame(VideoPacket.Frame(1_000_000, false, true, bytes), 1, host)
            anchor(store.clock, 1_100_000_000, host + 100_000_000)
            store.frame(VideoPacket.Frame(1_100_000, false, true, bytes), 1, host + 100_000_000)
            assertTrue(store.clock.add("1", listOf(1_500_000_000, 1_500_000_000,
                1_700_000_010_500_000_000, 1_500_000_000), host + 500_000_000, host + 500_000_000))
            val capture = store.capture(ReplaySettings())!!
            val tail = capture.videoTail()!!
            assertFalse(tail.displayHeld)
            assertTrue(tail.fromNs != null && tail.toNs != null)
            assertTrue(tail.clockEpoch != capture.windowClockEpoch)
            assertTrue(capture.gaps.isEmpty()) // The clock change itself, without an explicit stream gap.
            val output = SaveWriter().write(capture, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
            val manifest = JsonParser.parseString(Files.readString(output.directory.resolve("replay.json"))).asJsonObject
            assertEquals(1, manifest["parts"].asJsonArray.size())
            assertTrue(manifest["complete"].asBoolean)
            assertFalse(manifest["video_tail"].asJsonObject["display_held"].asBoolean)
            assertFalse(manifest["video_missing_ranges"].asJsonArray.isEmpty)
            assertEquals(listOf("video"), output.missingKinds)
            val missing = manifest["video_missing_ranges"].asJsonArray.last().asJsonObject
            assertEquals((capture.end - capture.start).toString(), missing["to_t_ns"].asString)
            anchor(store.clock, 2_000_000_000, host + 1_000_000_000)
            store.frame(VideoPacket.Frame(2_000_000, false, true, bytes), 1, host + 1_000_000_000)
            assertEquals(tail, capture.videoTail())
            val retry = SaveWriter().write(capture, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
            assertEquals(output.missingKinds, retry.missingKinds)
            store.release(capture.id)
        } finally {
            store.close()
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }

    @Test
    fun evictedIdrCannotClaimThatAnUndecodableTailWasDisplayed() {
        val root = Files.createTempDirectory("replay-tail-without-idr-")
        val encoder = H264Encoder.createH264Encoder()
        val picture = Picture.create(32, 32, ColorSpace.YUV420J).apply { fill(15) }
        val encodedIdr = encoder.encodeIDRFrame(picture, ByteBuffer.allocate(65536))
        val bytes = ByteArray(encodedIdr.remaining()).also { encodedIdr.get(it) }
        picture.fill(25)
        val encodedP = encoder.encodePFrame(picture, ByteBuffer.allocate(65536))
        val pBytes = ByteArray(encodedP.remaining()).also { encodedP.get(it) }
        assertTrue(H264Utils.splitFrame(ByteBuffer.wrap(pBytes)).any { it.get(0).toInt() and 31 == 1 })
        val store = CaptureStore(root.resolve("ring"), videoLimit = maxOf(bytes.size, pBytes.size).toLong(), minFree = 0)
        val host = System.nanoTime()
        try {
            store.generation(1); store.status("device_log", StreamState.CAPTURING, null, 1); store.session(VideoPacket.Session(32, 32), 1)
            anchor(store.clock, 1_000_000_000, host)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            assertTrue(store.frame(VideoPacket.Frame(1_000_000, false, true, bytes), 1, host))
            anchor(store.clock, 1_100_000_000, host + 100_000_000)
            assertFalse(store.frame(VideoPacket.Frame(1_100_000, false, false, pBytes), 1, host + 100_000_000))
            anchor(store.clock, 1_500_000_000, host + 500_000_000)
            val capture = store.capture(ReplaySettings())!!
            assertFalse(capture.video.single().key)
            val tail = capture.videoTail()!!
            assertEquals(100_000_000, tail.fromNs)
            assertEquals(500_000_000, tail.toNs)
            assertEquals(1_100_000, tail.sourcePtsUs)
            assertFalse(tail.displayHeld)
            assertFailsWith<SaveFailure> { SaveWriter().write(capture, root, { false }) { publication -> Files.move(publication.partial, publication.complete) } }
            assertFailsWith<IllegalStateException> { store.capture(ReplaySettings()) }
            assertTrue(capture.video.all { Files.exists(it.file) })
            anchor(store.clock, 2_000_000_000, host + 1_000_000_000)
            store.frame(VideoPacket.Frame(2_000_000, false, true, bytes), 1, host + 1_000_000_000)
            assertEquals(tail, capture.videoTail())
            store.release(capture.id)
        } finally {
            store.close()
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }

    @Test
    fun longVfrIntervalsKeepOnePartAndWindowCutRetainsItsDisplayedFrame() {
        val root = Files.createTempDirectory("replay-long-vfr-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime()
        val bytes = sample()
        try {
            store.generation(1); store.status("device_log", StreamState.CAPTURING, null, 1); store.session(VideoPacket.Session(32, 32), 1)
            anchor(store.clock, 1_000_000_000, host)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            store.frame(VideoPacket.Frame(1_000_000, false, true, bytes), 1, host)
            // No new image for five seconds; clocks remain valid without claiming video arrivals.
            for (second in 2L..6L) anchor(store.clock, second * 1_000_000_000, host + (second - 1) * 1_000_000_000)
            store.frame(VideoPacket.Frame(6_000_000, false, true, bytes), 1, host + 5_000_000_000)
            store.prune(2)
            val capture = store.capture(ReplaySettings(replaySeconds = 2))!!
            assertEquals(3_000_000_000, capture.start)
            assertEquals(listOf(1_000_000L, 6_000_000L), capture.video.map { it.pts })
            val output = SaveWriter().write(capture, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
            val manifest = JsonParser.parseString(Files.readString(output.directory.resolve("replay.json"))).asJsonObject
            assertEquals(1, manifest["parts"].asJsonArray.size())
            assertTrue(manifest["video_missing_ranges"].asJsonArray.isEmpty)
            val part = manifest["parts"].asJsonArray.single().asJsonObject
            assertEquals("0", part["t_start_ns"].asString)
            assertEquals("3000000", part["edit_start_us"].asString)
            NIOUtils.readableChannel(output.directory.resolve("video.mp4").toFile()).use { channel ->
                val frame = MP4Demuxer.createRawMP4Demuxer(channel).videoTrack.nextFrame()
                assertEquals(5_000_000, frame.duration)
            }
            for (gap in listOf(
                CaptureGap("video", 2_000_000_000, 2_100_000_000, "lost", 0),
                CaptureGap("clock", null, null, "unknown clock"),
            )) {
                val broken = SaveWriter().write(capture.copy(gaps = listOf(gap)), root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
                val brokenManifest = JsonParser.parseString(Files.readString(broken.directory.resolve("replay.json"))).asJsonObject
                // Real video loss cuts the run; a clock measurement outage keeps the frames but stays visible as missing.
                assertEquals(gap.stream == "clock", brokenManifest["video_missing_ranges"].asJsonArray.isEmpty)
                assertTrue("video" in broken.missingKinds)
            }
            store.release(capture.id)
        } finally {
            store.close()
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }

    @Test
    fun videoGapScopeIndexesTheEmittedGapsWhenAnEarlierClockGapIsOmitted() {
        val root = Files.createTempDirectory("replay-gap-index-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime()
        val bytes = sample()
        try {
            store.generation(1); store.status("device_log", StreamState.CAPTURING, null, 1); store.session(VideoPacket.Session(32, 32), 1)
            anchor(store.clock, 1_000_000_000, host)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            store.frame(VideoPacket.Frame(1_000_000, false, true, bytes), 1, host)
            for (second in 2L..6L) anchor(store.clock, second * 1_000_000_000, host + (second - 1) * 1_000_000_000)
            store.frame(VideoPacket.Frame(6_000_000, false, true, bytes), 1, host + 5_000_000_000)
            val capture = store.capture(ReplaySettings(replaySeconds = 4))!!
            val clockGap = CaptureGap("clock", 2_500_000_000, 2_600_000_000, "clock outage", 0)
            val videoGap = CaptureGap("video", 4_000_000_000, 4_100_000_000, "lost", 0)
            val output = SaveWriter().write(capture.copy(gaps = listOf(clockGap, videoGap)), root, { false }) { publication ->
                Files.move(publication.partial, publication.complete)
            }
            val manifest = JsonParser.parseString(Files.readString(output.directory.resolve("replay.json"))).asJsonObject
            val gaps = manifest["gaps"].asJsonArray.map { it.asJsonObject }
            assertEquals(listOf("video"), gaps.map { it["kind"].asString })
            val scope = manifest["video_gap_scopes"].asJsonArray.single().asJsonObject
            val indexed = gaps[scope["gap_index"].asInt]
            assertEquals("video", indexed["kind"].asString)
            assertEquals(indexed["from_t_ns"], scope["from_t_ns"])
            assertEquals(indexed["to_t_ns"], scope["to_t_ns"])
            store.release(capture.id)
        } finally {
            store.close()
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }

    @Test
    fun noNewFrameWithinTheWindowRetainsTheLastFrameAndItsDecodeGopWithinByteLimits() {
        val root = Files.createTempDirectory("replay-static-cut-")
        val bytes = sample()
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime()
        try {
            store.generation(1); store.status("device_log", StreamState.CAPTURING, null, 1); store.session(VideoPacket.Session(32, 32), 1)
            anchor(store.clock, 1_000_000_000, host)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            store.frame(VideoPacket.Frame(1_000_000, false, true, bytes), 1, host)
            store.frame(VideoPacket.Frame(1_100_000, false, false, bytes), 1, host + 100_000_000)
            for (second in 2L..15L) anchor(store.clock, second * 1_000_000_000, host + (second - 1) * 1_000_000_000)
            store.prune(2)
            val capture = store.capture(ReplaySettings(replaySeconds = 2))!!
            assertEquals(12_000_000_000, capture.start)
            assertEquals(listOf(1_000_000L, 1_100_000L), capture.video.map { it.pts })
            assertTrue(capture.video.all { Files.exists(it.file) })
            assertTrue(capture.gaps.isEmpty())
            val fixedTail = capture.videoTail()!!
            assertEquals(12_000_000_000, fixedTail.fromNs)
            assertEquals(14_000_000_000, fixedTail.toNs)
            assertTrue(fixedTail.displayHeld)
            val output = SaveWriter().write(capture, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
            assertTrue(output.missingKinds.isEmpty())
            val manifest = JsonParser.parseString(Files.readString(output.directory.resolve("replay.json"))).asJsonObject
            assertTrue(manifest["complete"].asBoolean)
            assertFalse(manifest["video_tail"].asJsonObject["new_frame_confirmed"].asBoolean)
            assertTrue(manifest["video_tail"].asJsonObject["display_held"].asBoolean)
            assertEquals("2000000", manifest["parts"].asJsonArray.single().asJsonObject["duration_us"].asString)
            assertTrue(manifest["parts"].asJsonArray.single().asJsonObject["confirmed_t_end_ns"].isJsonNull)
            assertEquals(2, Files.readAllLines(output.directory.resolve("video-frames.jsonl")).size)
            val unknown = capture.copy(endUncertainty = Long.MAX_VALUE)
            assertFalse(unknown.videoTail()!!.displayHeld)
            assertEquals(null, unknown.videoTail()!!.fromNs)
            val broken = capture.copy(start = 0, gaps = listOf(CaptureGap("video", 5_000_000_000, 6_000_000_000, "known loss", 0)))
            assertEquals(5_000_000_000, broken.videoTail()!!.toNs)
            val brokenOutput = SaveWriter().write(broken, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
            assertEquals(listOf("video"), brokenOutput.missingKinds)
            val longEnd = Int.MAX_VALUE.toLong() * 1000 + 20_000_000_000
            val longWindow = capture.copy(start = longEnd - 2_000_000_000, end = longEnd)
            val longOutput = SaveWriter().write(longWindow, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
            assertTrue(longOutput.missingKinds.isEmpty())
            val longManifest = JsonParser.parseString(Files.readString(longOutput.directory.resolve("replay.json"))).asJsonObject
            val longPart = longManifest["parts"].asJsonArray.single().asJsonObject
            assertTrue(longPart["media_timeline_clipped"].asBoolean)
            assertEquals("2000000", longPart["duration_us"].asString)
            assertEquals(capture.video.map { it.pts.toString() }, Files.readAllLines(longOutput.directory.resolve("video-frames.jsonl")).map {
                JsonParser.parseString(it).asJsonObject["source_pts_us"].asString
            })
            val following = capture.video.last().let { it.copy(pts = 1_000_000 + longEnd / 1000,
                time = it.time.copy(elapsed = 1_000_000_000 + longEnd, sequence = longEnd)) }
            val confirmedLong = longWindow.copy(video = listOf(capture.video.first(), following))
            val confirmedOutput = SaveWriter().write(confirmedLong, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
            assertTrue(confirmedOutput.missingKinds.isEmpty())
            val confirmed = JsonParser.parseString(Files.readString(confirmedOutput.directory.resolve("replay.json"))).asJsonObject
            assertTrue(confirmed["video_tail"].isJsonNull)
            assertEquals(1, confirmed["parts"].asJsonArray.size())
            assertEquals("2000000000", confirmed["parts"].asJsonArray.single().asJsonObject["confirmed_t_end_ns"].asString)
            NIOUtils.readableChannel(confirmedOutput.directory.resolve("video.mp4").toFile()).use { channel ->
                assertEquals(2_000_000, MP4Demuxer.createRawMP4Demuxer(channel).videoTrack.nextFrame().duration)
            }
            anchor(store.clock, 16_000_000_000, host + 15_000_000_000)
            store.frame(VideoPacket.Frame(16_000_000, false, true, bytes), 1, host + 15_000_000_000)
            assertEquals(fixedTail, capture.videoTail())
            store.release(capture.id)
            store.session(VideoPacket.Session(32, 32), 1)
            anchor(store.clock, 20_000_000_000, host + 19_000_000_000)
            store.prune(2)
            assertFalse(store.hasData()) // A new session cannot use the old static image as its own.
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            store.frame(VideoPacket.Frame(20_000_000, false, true, bytes), 1, host + 19_000_000_000)
            for (second in 21L..5020L) anchor(store.clock, second * 1_000_000_000, host + (second - 1) * 1_000_000_000)
            store.prune(2)
            val aged = store.capture(ReplaySettings(replaySeconds = 2))!!
            assertEquals(4096, aged.clocks.size)
            assertTrue(aged.video.single().time.sequence != null)
            assertTrue(aged.videoTail()!!.displayHeld)
            val agedOutput = SaveWriter().write(aged, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
            assertTrue(agedOutput.missingKinds.isEmpty())
            store.release(aged.id)
        } finally {
            store.close()
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }

    @Test
    fun unknownWindowStillReclaimsByteEvictedGopsWhilePreservingPinnedBytes() {
        val root = Files.createTempDirectory("replay-unknown-cap-fixture-")
        val bytes = sample()
        val store = CaptureStore(root.resolve("ring"), videoLimit = bytes.size.toLong(), minFree = 0)
        val host = System.nanoTime()
        try {
            store.generation(1); store.status("device_log", StreamState.CAPTURING, null, 1); store.session(VideoPacket.Session(32, 32), 1)
            anchor(store.clock, 1_000_000_000, host)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            store.frame(VideoPacket.Frame(1_000_000, false, true, bytes), 1, host)
            val pin = store.capture(ReplaySettings())!!
            val pinnedFile = pin.video.single().file
            val hash = sha256(pinnedFile)
            store.clock.add("2", listOf(100_000_000, 100_000_000, 1_700_000_020_000_000_000, 100_000_000),
                host + 20_000_000_000, host + 20_000_000_000)
            store.generation(2); store.session(VideoPacket.Session(32, 32), 2)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 2)
            for (n in 1..20) {
                store.frame(VideoPacket.Frame(n * 100_000L, false, true, bytes), 2, host + 20_000_000_000)
                store.prune(1)
                assertEquals(2, Files.list(store.directory).use { it.count() })
                assertEquals(hash, sha256(pinnedFile))
            }
            store.release(pin.id)
            store.prune(1)
            assertFalse(Files.exists(pinnedFile))
            assertEquals(1, Files.list(store.directory).use { it.count() })
            assertEquals(bytes.size.toLong(), Files.list(store.directory).use { paths -> paths.mapToLong { Files.size(it) }.sum() })
        } finally { store.close(); Files.delete(root) }
    }

    @Test
    fun rebootKeepsTheNewBootKnownAndExportsOldBootRowsWithoutWindowCoordinates() {
        val root = Files.createTempDirectory("replay-unknown-boot-fixture-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime()
        val bytes = sample()
        try {
            store.generation(1); store.status("device_log", StreamState.CAPTURING, null, 1); store.session(VideoPacket.Session(32, 32), 1)
            anchor(store.clock, 1_000_000_000, host)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            store.frame(VideoPacket.Frame(1_000_000, false, true, bytes), 1, host)
            store.log(DeviceLog(1_700_000_001_000_000_000, 12, 12, 10001, 0, 4, "Fixture", "old boot", byteArrayOf(1)), 1, host)
            store.status("video", StreamState.RECOVERING, "prior known gap", 1)
            store.status("video", StreamState.CAPTURING, null, 1)
            store.clock.add("2", listOf(100_000_000, 100_000_000, 1_700_000_020_000_000_000, 100_000_000),
                host + 20_000_000_000, host + 20_000_000_000)
            // Host nanoTime only gives a lower bound on the reboot distance: 30 s still reaches the old boot.
            val capture = store.capture(ReplaySettings(replaySeconds = 30))!!
            assertTrue(capture.windowKnown)
            assertTrue(capture.gaps.any { it.reason == "prior known gap" })
            assertTrue(capture.states.getValue("video").gaps.any { it.reason == "prior known gap" })
            assertTrue(store.streams(30).getValue("video").gaps.any { it.reason == "prior known gap" })
            assertTrue(store.streams(1).getValue("video").gaps.none { it.reason == "prior known gap" })
            assertEquals(1, capture.video.size)
            assertEquals(1, capture.logs.size)
            val output = SaveWriter().write(capture, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
            val manifest = JsonParser.parseString(Files.readString(output.directory.resolve("replay.json"))).asJsonObject
            assertTrue(manifest["timebase"].asJsonObject["known"].asBoolean)
            assertEquals(store.clock.segment(), manifest["timebase"].asJsonObject["boot_segment"].asInt)
            assertTrue(manifest["parts"].asJsonArray.single().asJsonObject["t_start_ns"].isJsonNull)
            for (name in listOf("video-frames.jsonl", "logcat-device.jsonl")) {
                assertTrue(JsonParser.parseString(Files.readAllLines(output.directory.resolve(name)).single()).asJsonObject["t_ns"].isJsonNull)
            }
            store.release(capture.id)
            store.prune(1, host + 20_000_000_000)
            // The lower bound already proves the old boot is outside a 1 s window on the new boot.
            assertTrue(store.streams(1).getValue("video").gaps.none { it.reason == "prior known gap" })
        } finally {
            store.close()
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }

    @Test
    fun videoAfterARebootGapStaysAlignedOnTheNewBootAndOnlyTheGapIsMissing() {
        val root = Files.createTempDirectory("replay-reboot-video-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime()
        val bytes = sample()
        try {
            store.generation(1); store.status("device_log", StreamState.CAPTURING, null, 1); store.session(VideoPacket.Session(32, 32), 1)
            anchor(store.clock, 1_000_000_000, host)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            store.frame(VideoPacket.Frame(1_000_000, false, true, bytes), 1, host)
            store.status("video", StreamState.RECOVERING, "reboot", 1)
            for (elapsed in listOf(100_000_000L, 400_000_000L)) store.clock.add("2",
                listOf(elapsed, elapsed, 1_700_000_020_000_000_000 + elapsed, elapsed), host + 19_900_000_000 + elapsed, host + 19_900_000_000 + elapsed)
            store.generation(2); store.status("device_log", StreamState.CAPTURING, null, 2); store.session(VideoPacket.Session(32, 32), 2)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 2)
            store.frame(VideoPacket.Frame(200_000, false, true, bytes), 2, host + 20_300_000_000)
            store.frame(VideoPacket.Frame(300_000, false, true, bytes), 2, host + 20_300_000_000)
            val reboot = store.streams(30).getValue("video").gaps.single { it.reason == "reboot" }
            assertEquals(Long.MAX_VALUE, reboot.boundaryUncertaintyNs)
            val capture = store.capture(ReplaySettings(replaySeconds = 30))!!
            assertTrue(capture.windowKnown)
            val scope = capture.gaps.gapScopes(capture.clocks, capture.clockRecoveries, reference = capture.windowSegment)[capture.gaps.indexOf(reboot)]
            assertNull(scope.fromNs)
            // Widened only to the next normal read of the new boot, like a gap on one boot.
            assertEquals(capture.clocks.last().elapsed + capture.clocks.last().sequenceOffset, scope.toNs)
            assertTrue(scope.toNs!! >= reboot.toNs!!)
            val margin = scope.boundaryUncertaintyNs!!
            assertTrue(margin < 20_000_000)
            assertFalse(scope.intersects(scope.toNs + margin + 1, scope.toNs + margin + 2))
            val output = SaveWriter().write(capture, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
            val manifest = JsonParser.parseString(Files.readString(output.directory.resolve("replay.json"))).asJsonObject
            assertTrue(manifest["gaps"].asJsonArray.map { it.asJsonObject }.single { it["reason"].asString == "reboot" }["duration_uncertain"].asBoolean)
            // Only the gap's end has an exact place on the new boot; it must not swallow the new boot's frames.
            val part = manifest["parts"].asJsonArray.map { it.asJsonObject }.single { it["generation"].asLong == 2L }
            assertTrue(part["clock_alignment_known"].asBoolean)
            val firstNew = part["t_start_ns"].asLong
            val holes = manifest["video_missing_ranges"].asJsonArray.map { it.asJsonObject }
            assertTrue(holes.isNotEmpty())
            assertTrue(holes.all { it["to_t_ns"].asLong <= firstNew })
            val frames = Files.readAllLines(output.directory.resolve("video-frames.jsonl")).map { JsonParser.parseString(it).asJsonObject }
            assertTrue(frames.filter { it["generation"].asLong == 2L }.let { rows -> rows.size == 2 && rows.none { it["t_ns"].isJsonNull } })
            assertTrue(frames.filter { it["generation"].asLong == 1L }.all { it["t_ns"].isJsonNull })
            assertTrue("video" in output.missingKinds)
            store.release(capture.id)
        } finally {
            store.close()
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }

    @Test
    fun recoveredAppGapClosesOnlyAfterTargetAndRequiredReadersAndKeepsFrozenSave() {
        for (appFirst in listOf(false, true)) {
            val root = Files.createTempDirectory("replay-app-recovery-")
            val store = CaptureStore(root.resolve("ring"), minFree = 0)
            val host = System.nanoTime() + 1_000_000_000
            fun target(owner: Long) = store.app("com.example.app", 10001, setOf(12), owner, true)
            fun row(second: Long, owner: Long) = store.log(DeviceLog(
                1_700_000_000_000_000_000 + second * 1_000_000_000, 12, 12, 10001, 0, 4,
                "Fixture", "row-$second", byteArrayOf(1)), owner, host + (second - 1) * 1_000_000_000)
            fun appGap() = store.streams().getValue("app_log").gaps.single { it.stream == "app_log" }
            try {
                store.generation(1)
                anchor(store.clock, 1_000_000_000, host)
                store.status("device_log", StreamState.CAPTURING, null, 1)
                target(1)
                val first = row(1, 1)
                anchor(store.clock, 2_000_000_000, host + 1_000_000_000)
                store.freeze()
                listOf("video", "device_log", "app_log").forEach {
                    store.status(it, StreamState.RECOVERING, "connection interrupted", 1)
                }
                val interrupted = store.capture(ReplaySettings())!!
                val originalGap = interrupted.gaps.single { it.stream == "app_log" }
                assertNull(originalGap.toNs)
                store.generation(2)
                anchor(store.clock, 3_000_000_000, host + 2_000_000_000)
                store.resume()
                target(1)
                store.status("device_log", StreamState.CAPTURING, null, 1)
                assertNull(row(3, 1))
                assertEquals(originalGap, appGap()) // Stale callbacks cannot recover the new generation.
                if (appFirst) {
                    target(2)
                    assertNull(appGap().toNs)
                    row(3, 2) // One reader's accepted row cannot recover the aggregate required readers.
                    assertNull(appGap().toNs)
                    store.status("device_log", StreamState.RECOVERING, "required reader failed", 2)
                    assertNull(appGap().toNs)
                    store.status("device_log", StreamState.CAPTURING, null, 2)
                } else {
                    store.app("com.example.app", null, emptySet(), 2)
                    store.status("device_log", StreamState.CAPTURING, null, 2)
                    assertEquals(StreamState.UNAVAILABLE, store.streams().getValue("app_log").state)
                    assertNull(appGap().toNs)
                    target(2)
                }
                val recoveredGap = appGap()
                assertEquals(originalGap.copy(toNs = 2_000_000_000), recoveredGap)
                target(2) // Identical target refresh must not move the recovery boundary.
                assertEquals(recoveredGap, appGap())
                anchor(store.clock, 5_000_000_000, host + 4_000_000_000)
                val last = row(5, 2)
                store.release(interrupted.id)
                val capture = store.capture(ReplaySettings())!!
                assertTrue(capture.logs.all { it.app == true })
                assertEquals(first, capture.logs.first().id)
                assertEquals(last, capture.logs.last().id)
                assertEquals(3.0, capture.states.getValue("app_log").availableSeconds)
                assertEquals(StreamState.CAPTURING, capture.states.getValue("app_log").state)
                val output = SaveWriter().write(capture, root, { false }) { publication ->
                    Files.move(publication.partial, publication.complete)
                }
                val manifest = JsonParser.parseString(Files.readString(output.directory.resolve("replay.json"))).asJsonObject
                val gap = manifest["gaps"].asJsonArray.single { it.asJsonObject["kind"].asString == "app_log" }.asJsonObject
                assertEquals((2_000_000_000 - capture.start).toString(), gap["to_t_ns"].asString)
                assertFalse(gap["duration_uncertain"].asBoolean)
                assertEquals(3.0, manifest["coverage"].asJsonObject["app_log"].asJsonObject["availableSeconds"].asDouble)
                assertEquals(capture.logs.map { it.id }, Files.readAllLines(output.directory.resolve("logcat-app.jsonl"))
                    .map { JsonParser.parseString(it).asJsonObject["record_id"].asString })
                assertEquals(originalGap, interrupted.gaps.single { it.stream == "app_log" })
                assertEquals(listOf(first), interrupted.logs.map { it.id })
                store.clock.boundary(host + 4_000_000_000)
                target(2)
                assertTrue(store.streams().getValue("app_log").reason!!.contains("時計"))
                assertEquals(recoveredGap, appGap())
                assertEquals(3.0, capture.states.getValue("app_log").availableSeconds)
            } finally { store.close(); root.toFile().deleteRecursively() }
        }
    }

    @Test
    fun appRecoveryNeedsCurrentCertainClockAndRebootGapIsApproximate() {
        for (unknownBoot in listOf(false, true)) {
            val root = Files.createTempDirectory("replay-app-clock-recovery-")
            val store = CaptureStore(root.resolve("ring"), minFree = 0)
            val host = System.nanoTime() + 1_000_000_000
            try {
                store.generation(1)
                anchor(store.clock, 1_000_000_000, host)
                store.status("device_log", StreamState.CAPTURING, null, 1)
                store.app("com.example.app", 10001, setOf(12), 1, true)
                store.status("app_log", StreamState.RECOVERING, "connection interrupted", 1)
                store.generation(2)
                store.clock.boundary(host)
                store.clockStatus(false, 2)
                store.app("com.example.app", 10001, setOf(12), 2, true)
                store.status("device_log", StreamState.CAPTURING, null, 2)
                fun gap() = store.streams().getValue("app_log").gaps.single { it.stream == "app_log" }
                assertNull(gap().toNs)
                store.clockStatus(true, 1)
                store.clockStatus(true, 2) // No new sample: healthy callback alone cannot prove current clock.
                assertNull(gap().toNs)
                if (unknownBoot) {
                    store.clock.add("2", listOf(100_000_000, 100_000_000, 1_700_000_020_000_000_000, 100_000_000),
                        host + 2_000_000_000, host + 2_000_000_000)
                } else anchor(store.clock, 3_000_000_000, host + 2_000_000_000)
                store.clockStatus(true, 2)
                assertNull(gap().toNs) // The resolved target still belongs to the previous clock epoch.
                store.app("com.example.app", 10001, setOf(12), 2, true)
                if (unknownBoot) {
                    // The new boot recovers on its own segment; the gap only has an approximate host-time duration.
                    assertTrue(gap().toNs != null)
                    assertEquals(Long.MAX_VALUE, gap().boundaryUncertaintyNs)
                    assertEquals(StreamState.CAPTURING, store.streams().getValue("app_log").state)
                } else {
                    assertEquals(2_000_000_000, gap().toNs)
                    assertTrue(gap().boundaryUncertaintyNs!! < Long.MAX_VALUE)
                }
            } finally { store.close(); root.toFile().deleteRecursively() }
        }
    }

    @Test
    fun nullTargetAndRecoveredClockAndLogGapsRemainImmutableOnlyWhileInWindow() {
        val root = Files.createTempDirectory("replay-gap-fixture-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime()
        try {
            store.generation(1); store.status("device_log", StreamState.CAPTURING, null, 1)
            store.app(null, null, emptySet(), 1)
            assertEquals(StreamState.UNAVAILABLE, store.streams()["app_log"]?.state)
            anchor(store.clock, 1_000_000_000, host)
            store.app("com.example.app", 10001, setOf(12), 1, true)
            store.log(DeviceLog(1_700_000_001_000_000_000, 12, 12, 10001, 0, 4, "a", "a", byteArrayOf(1)), 1, host)
            store.status("device_log", StreamState.RECOVERING, "isolated log interruption", 1)
            store.clockStatus(false, 1)
            anchor(store.clock, 1_100_000_000, host + 100_000_000)
            store.status("device_log", StreamState.CAPTURING, null, 1)
            store.clockStatus(true, 1)
            val recovered = store.streams(1, 100_000_000)
            val appGaps = recovered.getValue("app_log").gaps
            assertEquals(setOf("device_log", "clock"), appGaps.map { it.stream }.toSet())
            assertEquals(listOf("clock"), recovered.getValue("video").gaps.map { it.stream })
            assertFailsWith<UnsupportedOperationException> { (appGaps as MutableList).clear() }
            anchor(store.clock, 6_000_000_000, host + 5_000_000_000)
            assertTrue(store.streams(1, 5_000_000_000).values.all { it.gaps.isEmpty() })
            store.freeze()
            val fixedEnd = store.end()
            store.clock.add("2", listOf(100_000_000, 100_000_000, 1_700_000_020_000_000_000, 100_000_000),
                host + 20_000_000_000, host + 20_000_000_000)
            assertTrue(store.clock.endUncertainty(host + 20_000_000_000) < 20_000_000) // The new boot is its own exact segment.
            assertEquals(fixedEnd, store.end())
            assertTrue(store.streams(1).values.all { it.gaps.isEmpty() }) // Frozen known window is not replaced by the new boot.
            store.resume()
            // Host nanoTime places boot 1 at least 19 s before boot 2, so its gaps leave the 1 s window.
            assertTrue(store.streams(1).values.all { it.gaps.isEmpty() })
            assertEquals(2, appGaps.size)
        } finally { store.close(); Files.delete(root) }
    }

    @Test
    fun framesReceivedBeforeTheNewBootsFirstClockReadAreHiddenPrerollWhenALaterFrameIsBeforeTheWindow() {
        val root = Files.createTempDirectory("replay-reboot-preroll-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime()
        val bytes = sample()
        try {
            store.generation(1); store.status("device_log", StreamState.CAPTURING, null, 1)
            anchor(store.clock, 1_000_000_000, host)
            store.generation(2); store.status("device_log", StreamState.CAPTURING, null, 2); store.session(VideoPacket.Session(32, 32), 2)
            // Received after the reboot but before the new boot's first clock read: no time, and no host bound either.
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 2)
            store.frame(VideoPacket.Frame(100_000, false, true, bytes), 2, host + 20_000_000_000)
            store.frame(VideoPacket.Frame(150_000, false, false, bytes), 2, host + 20_050_000_000)
            for (elapsed in listOf(200_000_000L, 300_000_000L, 400_000_000L, 500_000_000L))
                store.clock.add("2", listOf(elapsed, elapsed, 1_700_000_020_000_000_000 + elapsed, elapsed),
                    host + 19_900_000_000 + elapsed, host + 19_900_000_000 + elapsed)
            for ((pts, received) in listOf(250_000L to 20_300_000_000L, 350_000L to 20_400_000_000L, 450_000L to 20_450_000_000L))
                store.frame(VideoPacket.Frame(pts, false, false, bytes), 2, host + received)
            val frozen = store.capture(ReplaySettings(replaySeconds = 30), host = host + 20_450_000_000)!!
            val timed = frozen.video.filter { it.time.sequence != null }
            assertEquals(listOf(250_000L, 350_000L, 450_000L), timed.map { it.pts })
            assertTrue(frozen.video.filter { it.pts < 200_000 }.all { it.time.sequence == null })
            // The window starts after the first timed frame, so the earlier untimed frames end before it.
            val capture = frozen.copy(start = timed.first().time.sequence!! + 50_000_000)
            val output = SaveWriter().write(capture, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
            val part = JsonParser.parseString(Files.readString(output.directory.resolve("replay.json"))).asJsonObject["parts"].asJsonArray.single().asJsonObject
            assertTrue(part["clock_alignment_known"].asBoolean)
            assertEquals(2, part["preroll_samples"].asInt)
            assertEquals("0", part["t_start_ns"].asString)
            val rows = Files.readAllLines(output.directory.resolve("video-frames.jsonl")).map { JsonParser.parseString(it).asJsonObject }
            assertEquals(listOf("100000", "150000"), rows.filter { it["t_ns"].isJsonNull }.map { it["source_pts_us"].asString })
            assertTrue(rows.filter { it["t_ns"].isJsonNull }.all { !it["presented"].asBoolean && it["preroll"].asBoolean })
            store.release(frozen.id)
        } finally {
            store.close()
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }

    private fun sample(): ByteArray {
        val picture = Picture.create(32, 32, ColorSpace.YUV420J).apply { fill(15) }
        val data = H264Encoder.createH264Encoder().encodeIDRFrame(picture, ByteBuffer.allocate(65536))
        return ByteArray(data.remaining()).also { data.get(it) }
    }

    private fun config(bytes: ByteArray): ByteArray = H264Utils.splitFrame(ByteBuffer.wrap(bytes))
        .filter { it.get(0).toInt() and 31 in setOf(7, 8) }
        .fold(byteArrayOf()) { result, nal -> result + byteArrayOf(0, 0, 0, 1) + ByteArray(nal.remaining()).also { nal.get(it) } }

    @Test
    fun recordsWithUnknownTimeBeforeAClockEpochBoundaryStayOutsideTheReplayWindow() {
        // sparse: no frames from 6 to 19 seconds, so the unknown-time frame at 5 seconds is still displayed at the window start.
        // keyAfterGap: the frame at 20 seconds starts a new GOP; the clock gap before it cannot prove the 5 second frame was still on screen.
        for ((sparse, keyAfterGap) in listOf(false to false, true to false, true to true)) unknownTimeWindowFixture(sparse, keyAfterGap)
    }

    @Test
    fun unknownClockMappingStillTrimsTheWindowByHostReceiveTime() {
        val root = Files.createTempDirectory("replay-host-window-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime() + 60_000_000_000
        val bytes = sample()
        fun at(second: Long) = host + (second - 1) * 1_000_000_000
        try {
            store.generation(1)
            store.status("device_log", StreamState.CAPTURING, null, 1)
            store.session(VideoPacket.Session(32, 32), 1)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            for (second in 1L..3L) anchor(store.clock, second * 1_000_000_000, at(second))
            store.clock.boundary(at(3))
            for (second in 1L..40L) {
                store.frame(VideoPacket.Frame(second * 1_000_000, false, second == 1L || second == 20L, bytes), 1, at(second))
                store.log(DeviceLog(1_700_000_000_000_000_000 + second * 1_000_000_000, 12, 12, 10001, 0, 4, "Fixture",
                    "second $second", byteArrayOf(1)), 1, at(second))
            }
            val selected = store.capture(ReplaySettings(replaySeconds = 10), host = at(40))!!
            assertFalse(selected.windowKnown)
            assertEquals((30L..40L).map { "second $it" }, selected.logs.map { it.source.message })
            assertEquals((20L..40L).map { it * 1_000_000 }, selected.video.map { it.pts })
            val output = SaveWriter().write(selected, root, { false }) { publication ->
                Files.move(publication.partial, publication.complete)
            }
            val presented = Files.readAllLines(output.directory.resolve("video-frames.jsonl"))
                .map { JsonParser.parseString(it).asJsonObject }
                .filter { it["presented"].asBoolean }.map { it["source_pts_us"].asLong }
            assertEquals((29L..40L).map { it * 1_000_000 }, presented)
            store.release(selected.id)
        } finally { store.close(); root.toFile().deleteRecursively() }
    }

    private fun unknownTimeWindowFixture(sparse: Boolean, keyAfterGap: Boolean) {
        val root = Files.createTempDirectory("replay-epoch-window-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime() + 60_000_000_000
        val bytes = sample()
        fun at(second: Long) = host + (second - 1) * 1_000_000_000
        fun record(second: Long) = DeviceLog(1_700_000_000_000_000_000 + second * 1_000_000_000, 12, 12, 10001, 0, 4,
            "Fixture", "second $second", byteArrayOf(1))
        try {
            store.generation(1); store.status("device_log", StreamState.CAPTURING, null, 1)
            store.session(VideoPacket.Session(32, 32), 1)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            for (second in 1L..40L) {
                // No clock reads between 4 and 9 seconds, and the next read shows a discontinuity: a new clock epoch.
                if (second == 10L) store.clock.boundary()
                if (second !in 4L..9L) anchor(store.clock, second * 1_000_000_000, at(second))
                if (!sparse || second !in 6L..19L)
                    assertTrue(store.frame(VideoPacket.Frame(second * 1_000_000, false, second == 1L || second == 5L || keyAfterGap && second == 20L, bytes), 1, at(second)))
                store.log(record(second), 1, at(second))
            }
            assertEquals(1, store.clock.currentEpoch())
            val selected = store.capture(ReplaySettings(replaySeconds = 29), host = at(40))!!
            assertTrue(selected.windowKnown)
            assertEquals(10_000_000_000, selected.start) // sequence 0 is the first clock read at 1 second.
            assertTrue(selected.logs.none { it.source.message in (1L..9L).map { "second $it" } }, selected.logs.map { it.source.message }.toString())
            val output = SaveWriter().write(selected, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
            val presented = Files.readAllLines(output.directory.resolve("video-frames.jsonl")).map { JsonParser.parseString(it).asJsonObject }
                .filter { it["presented"].asBoolean }.map { it["source_pts_us"].asLong }
            assertEquals((if (!sparse) (11L..40L).toList() else if (keyAfterGap) (20L..40L).toList() else listOf(5L) + (20L..40L)).map { it * 1_000_000 }, presented, "sparse=$sparse keyAfterGap=$keyAfterGap")
            store.release(selected.id)
            store.prune(29)
            val next = store.capture(ReplaySettings(replaySeconds = 29), host = at(40))!!
            assertTrue(next.video.none { it.pts in 1_000_000L..4_000_000L }, next.video.map { it.pts }.toString())
            assertEquals(!keyAfterGap, next.video.any { it.pts == 5_000_000L }, next.video.map { it.pts }.toString())
            store.release(next.id)
        } finally { store.close(); root.toFile().deleteRecursively() }
    }

    @Test
    fun clockOutageOpenedByARebootsRejectedFirstReadIsNotResolvedByTheNewBootsRecovery() {
        val root = Files.createTempDirectory("replay-reboot-clock-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime()
        try {
            store.generation(1); store.status("device_log", StreamState.CAPTURING, null, 1)
            anchor(store.clock, 1_000_000_000, host)
            anchor(store.clock, 2_000_000_000, host + 1_000_000_000)
            // A row before the reboot opens the window on the old boot, so the outage lies inside it.
            assertNotNull(store.log(DeviceLog(1_700_000_001_500_000_000, 12, 12, 10001, 0, 4, "Fixture", "before-reboot",
                byteArrayOf(1)), 1, host + 500_000_000))
            val oldEpoch = store.clock.currentEpoch()
            // The new boot's first read is wider than 2 ms: add() opens the new epoch, then the outage opens at the old boot's last read.
            val healthy = store.clock.add("2", listOf(100_000_000, 100_000_000, 1_700_000_010_000_000_000, 103_000_000),
                host + 3_000_000_000, host + 3_000_100_000)
            assertFalse(healthy)
            assertTrue(store.clock.currentEpoch() > oldEpoch)
            store.clockStatus(healthy, 1)
            for (elapsed in listOf(200_000_000L, 300_000_000L)) {
                assertTrue(store.clock.add("2", listOf(elapsed, elapsed, 1_700_000_010_000_000_000 + elapsed, elapsed),
                    host + 2_900_000_000 + elapsed, host + 2_900_000_000 + elapsed))
                store.clockStatus(true, 1)
            }
            store.log(DeviceLog(1_700_000_010_300_000_000, 12, 12, 10001, 0, 4, "Fixture", "after-reboot", byteArrayOf(1)),
                1, host + 3_200_000_000)
            val capture = store.capture(ReplaySettings(replaySeconds = 30), host = host + 3_300_000_000)!!
            val gap = capture.gaps.single { it.stream == "clock" }
            assertEquals(oldEpoch, gap.clockEpoch)
            assertNotNull(gap.toNs)
            assertNotEquals(gap.clockEpoch, capture.clockRecoveries[gap]?.epoch)
            assertFalse(gap.resolvedClock(capture.clockRecoveries))
            // Without the fix the gap took the new epoch, so the new boot's recovery read hid the warning.
            assertTrue(capture.gaps.inWindow(capture.start, capture.end, capture.endUncertainty, capture.clocks,
                capture.clockRecoveries, capture.windowSegment).contains(gap))
            assertTrue("device_log" in capture.missingKinds())
            store.release(capture.id)
        } finally {
            store.close()
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }

    @Test
    fun aRebootSpanningVideoGapStaysClosedByTheNewBootsFrameAfterTheOldBootsFilesArePruned() {
        val root = Files.createTempDirectory("replay-reboot-video-cut-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime()
        val bytes = sample()
        try {
            store.generation(1); store.status("device_log", StreamState.CAPTURING, null, 1); store.session(VideoPacket.Session(32, 32), 1)
            anchor(store.clock, 1_000_000_000, host)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            store.frame(VideoPacket.Frame(1_000_000, false, true, bytes), 1, host)
            store.status("video", StreamState.RECOVERING, "端末との取得接続が中断しています", 1)
            store.generation(2); store.status("device_log", StreamState.CAPTURING, null, 2); store.session(VideoPacket.Session(32, 32), 2)
            fun read(elapsed: Long) = assertTrue(store.clock.add("2", listOf(elapsed, elapsed, 1_700_000_020_000_000_000 + elapsed, elapsed),
                host + 19_900_000_000 + elapsed, host + 19_900_000_000 + elapsed))
            read(100_000_000); read(200_000_000)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 2)
            // The new boot's first frame closes the gap at its own source time, before the next clock read.
            for ((pts, received) in listOf(250_000L to 20_160_000_000L, 350_000L to 20_260_000_000L, 450_000L to 20_360_000_000L)) {
                store.frame(VideoPacket.Frame(pts, false, pts == 250_000L, bytes), 2, host + received)
                if (pts == 250_000L) read(400_000_000)
            }
            read(500_000_000)
            store.prune(1, host + 20_400_000_000)
            val capture = store.capture(ReplaySettings(replaySeconds = 1), host = host + 20_400_000_000)!!
            assertEquals(listOf(2L), capture.video.map { it.generation }.distinct())
            val gap = capture.gaps.single { it.stream == "video" }
            assertNotNull(capture.videoCuts[gap]?.after)
            // The window starts before the read after the closing frame, so only the cut proves that frame continues the run.
            assertTrue(capture.start < capture.video.first { it.pts == 250_000L }.time.sequence!!)
            val output = SaveWriter().write(capture, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
            val part = JsonParser.parseString(Files.readString(output.directory.resolve("replay.json"))).asJsonObject["parts"].asJsonArray.single().asJsonObject
            assertTrue(part["clock_alignment_known"].asBoolean)
            store.release(capture.id)
        } finally {
            store.close()
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }

    private fun anchor(clock: CaptureClock, elapsed: Long, host: Long) {
        clock.add("1", listOf(elapsed, elapsed, 1_700_000_000_000_000_000 + elapsed, elapsed), host, host)
    }

    @Test
    fun pinnedRequestSurvivesPruneAndFailedSaveThenExportsMatchingLogsAndOriginalVfrPts() {
        val root = Files.createTempDirectory("replay-save-fixture-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime()
        val bytes = sample()
        try {
            store.generation(1); store.status("device_log", StreamState.CAPTURING, null, 1)
            store.session(VideoPacket.Session(32, 32), 1)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            anchor(store.clock, 1_000_000_000, host)
            store.app("com.example.app", 10001, setOf(12), 1)
            for (pts in listOf(0L, 100_000L, 350_000L)) {
                anchor(store.clock, 1_000_000_000 + pts * 1000, host + pts * 1000)
                store.frame(VideoPacket.Frame(1_000_000 + pts, false, true, bytes), 1, host + pts * 1000)
            }
            store.log(DeviceLog(1_700_000_001_350_000_000, 12, 12, 10001, 0, 4, "Fixture", "hello\nworld", byteArrayOf(1, 2)), 1, host + 350_000_000)
            anchor(store.clock, 1_400_000_000, host + 400_000_000)
            store.status("video", StreamState.RECOVERING, "schema gap fixture", 1)
            store.status("video", StreamState.CAPTURING, null, 1)
            val capture = store.capture(ReplaySettings())!!
            assertEquals(0, capture.start)
            assertEquals(400_000_000, capture.end)
            assertEquals(true, capture.logs.single().app)
            assertFailsWith<IllegalStateException> { store.capture(ReplaySettings()) }
            assertFailsWith<SaveFailure> { SaveWriter().write(capture, root.resolve("missing"), { false }) { publication -> Files.move(publication.partial, publication.complete) } }
            anchor(store.clock, 10_000_000_000, host + 9_000_000_000)
            store.prune(1)
            assertTrue(capture.video.all { Files.exists(it.file) })
            val output = SaveWriter().write(capture, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
            assertEquals(listOf("video"), output.missingKinds) // The explicit gap remains a known interruption.
            val completed = output.directory
            val manifest = JsonParser.parseString(Files.readString(completed.resolve("replay.json"))).asJsonObject
            assertFalse(manifest["files_sha256"].asJsonObject.has("replay.json"))
            assertEquals(setOf("state", "requested", "availableSeconds", "reason"), manifest["coverage"].asJsonObject["video"].asJsonObject.keySet())
            assertTrue(manifest["coverage"].asJsonObject["video"].asJsonObject["requested"].asBoolean)
            val gap = manifest["gaps"].asJsonArray.single().asJsonObject
            assertTrue(gap["from_t_ns"].asJsonPrimitive.isString && gap["to_t_ns"].asJsonPrimitive.isString)
            assertTrue(manifest["build"].asJsonObject["source"].asString.matches(Regex("[a-f0-9]{40}")))
            manifest["files_sha256"].asJsonObject.entrySet().forEach { (name, value) -> assertEquals(value.asString, sha256(completed.resolve(name))) }
            val device = Files.readAllLines(completed.resolve("logcat-device.jsonl"))
            assertEquals(device, Files.readAllLines(completed.resolve("logcat-app.jsonl")))
            assertEquals(capture.logs.single().id, JsonParser.parseString(device.single()).asJsonObject["record_id"].asString)
            NIOUtils.readableChannel(completed.resolve("video.mp4").toFile()).use { channel ->
                val mux = MP4Demuxer.createRawMP4Demuxer(channel)
                val track = mux.videoTrack
                for ((pts, duration) in listOf(0L to 100_000L, 100_000L to 250_000L, 350_000L to 50_000L)) {
                    val frame = track.nextFrame()
                    assertEquals(pts, frame.pts)
                    assertEquals(duration, frame.duration)
                    assertEquals(1_000_000, frame.timescale)
                }
            }
            val logOnly = SaveWriter().write(capture.copy(video = emptyList()), root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
            assertEquals(listOf("video"), logOnly.missingKinds)
            for ((directory, videoCount) in listOf(completed to 1, logOnly.directory to 0)) {
                assertEquals(videoCount.toLong(), Files.list(directory).use { paths -> paths.filter { it.toString().endsWith(".mp4") }.count() })
                val readme = Files.readString(directory.resolve("README.md"))
                assertTrue(readme.contains(if (videoCount == 1) "| 動画 | 1本" else "| 動画 | なし |"))
                assertTrue(readme.contains("動画がある保存は1本のMP4です。動画がない保存にはMP4とvideo-frames.jsonlはありません。"))
                assertFalse(readme.contains("動画は1本のMP4です。"))
                assertTrue(readme.contains("replay.jsonのcoverage.video"))
                val saved = JsonParser.parseString(Files.readString(directory.resolve("replay.json"))).asJsonObject
                assertTrue(saved["coverage"].asJsonObject["video"].asJsonObject.has("reason"))
                assertEquals(saved["files_sha256"].asJsonObject["README.md"].asString, sha256(directory.resolve("README.md")))
            }
            val hash = sha256(completed.resolve("replay.json"))
            assertFailsWith<CancellationException> { SaveWriter().write(capture, root, { true }) { publication -> Files.move(publication.partial, publication.complete) } }
            assertEquals(hash, sha256(completed.resolve("replay.json")))
            store.release(capture.id)
            assertTrue(capture.video.all { !Files.exists(it.file) || it.file == capture.video.last().file })
        } finally {
            store.close()
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }

    @Test
    fun cutPrerollAndDelayedBoundaryPartsUseNonnegativeEditsAndActualWindowOffsets() {
        val root = Files.createTempDirectory("replay-edit-fixture-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime()
        val bytes = sample()
        try {
            store.generation(1); store.status("device_log", StreamState.CAPTURING, null, 1); store.session(VideoPacket.Session(32, 32), 1)
            anchor(store.clock, 1_000_000_000, host)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            for (pts in listOf(0L, 100_000L, 350_000L)) {
                anchor(store.clock, 1_000_000_000 + pts * 1000, host + pts * 1000)
                store.frame(VideoPacket.Frame(1_000_000 + pts, false, true, bytes), 1, host + pts * 1000)
            }
            anchor(store.clock, 1_400_000_000, host + 400_000_000)
            val original = store.capture(ReplaySettings())!!
            val cut = SaveWriter().write(original.copy(start = 50_000_000), root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
            val cutJson = JsonParser.parseString(Files.readString(cut.directory.resolve("replay.json"))).asJsonObject
            val part = cutJson["parts"].asJsonArray.single().asJsonObject
            assertEquals("0", part["t_start_ns"].asString)
            assertEquals("50000", part["edit_start_us"].asString)
            NIOUtils.readableChannel(cut.directory.resolve("video.mp4").toFile()).use { channel ->
                val track = MP4Demuxer.createRawMP4Demuxer(channel).videoTrack
                assertEquals(listOf(0L, 50_000L, 300_000L), (1..3).map { track.nextFrame().pts })
            }
            val indices = Files.readAllLines(cut.directory.resolve("video-frames.jsonl")).map { JsonParser.parseString(it).asJsonObject }
            assertEquals(listOf("0", "50000", "300000"), indices.map { it["presentation_pts_us"].asString })
            val delayed = original.copy(start = 0, video = original.video.drop(1))
            val delayedOutput = SaveWriter().write(delayed, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
            val delayedPart = JsonParser.parseString(Files.readString(delayedOutput.directory.resolve("replay.json"))).asJsonObject["parts"].asJsonArray.single().asJsonObject
            assertEquals("0", delayedPart["edit_start_us"].asString)
            assertEquals("100000000", delayedPart["t_start_ns"].asString)
            assertEquals(listOf("video"), delayedOutput.missingKinds)
            val lateFrame = original.video.first().let { it.copy(pts = it.pts + 1_000_000,
                time = it.time.copy(elapsed = it.time.elapsed!! + 1_000_000_000, sequence = 1_000_000_000)) }
            val late = SaveWriter().write(original.copy(start = 0, end = 1_200_000_000, video = listOf(lateFrame)), root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
            val latePart = JsonParser.parseString(Files.readString(late.directory.resolve("replay.json"))).asJsonObject["parts"].asJsonArray.single().asJsonObject
            assertEquals("0", latePart["edit_start_us"].asString)
            assertEquals("1000000000", latePart["t_start_ns"].asString)
            for (boundary in listOf("session", "clock", "config")) {
                val second = original.video.last().let { frame -> when (boundary) {
                    "session" -> frame.copy(session = frame.session + 1)
                    "clock" -> frame.copy(time = frame.time.copy(epoch = frame.time.epoch + 1))
                    else -> frame.copy(config = byteArrayOf(0) + frame.config)
                } }
                val split = SaveWriter().write(original.copy(video = listOf(original.video.first(), second)), root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
                val parts = JsonParser.parseString(Files.readString(split.directory.resolve("replay.json"))).asJsonObject["parts"].asJsonArray
                assertEquals(2, parts.size())
                assertEquals(parts[1].asJsonObject["media_start_us"], parts[1].asJsonObject["edit_start_us"])
                assertEquals("350000000", parts[1].asJsonObject["t_start_ns"].asString)
            }
            store.release(original.id)
            anchor(store.clock, 2_050_000_000, host + 1_050_000_000)
            // Prune and cut at the anchored host instant, not the current one: the test may run slower than its anchored timeline.
            store.prune(1, host = host + 1_050_000_000)
            val realCut = store.capture(ReplaySettings(replaySeconds = 1), host = host + 1_050_000_000)!!
            assertEquals(50_000_000, realCut.start)
            assertEquals(1_000_000, realCut.video.first().pts)
            store.release(realCut.id)
        } finally {
            store.close()
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }

    @Test
    fun uniqueUidFollowsNewPidImmediatelyAndSharedUidRemainsUnknown() {
        val root = Files.createTempDirectory("replay-app-fixture-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime()
        try {
            store.generation(1); store.status("device_log", StreamState.CAPTURING, null, 1); anchor(store.clock, 1_000_000_000, host)
            store.app("com.example.app", 10001, setOf(12), 1, uidExclusive = true)
            anchor(store.clock, 2_000_000_000, host + 1_000_000_000)
            store.log(DeviceLog(1_700_000_002_000_000_000, 99, 99, 10001, 0, 4, "app", "new PID", byteArrayOf(1)), 1, host + 1_000_000_000)
            val unique = store.capture(ReplaySettings())!!
            assertEquals(true, unique.logs.single().app)
            store.release(unique.id)
            store.app("com.example.app", 10001, setOf(12), 1, uidExclusive = false)
            store.log(DeviceLog(1_700_000_002_000_000_000, 100, 100, 10001, 0, 4, "app", "shared UID", byteArrayOf(2)), 1, host + 1_000_000_000)
            val shared = store.capture(ReplaySettings())!!
            assertEquals(null, shared.logs.last().app)
            store.release(shared.id)
        } finally { store.close(); Files.delete(root) }
    }

    @Test
    fun byteCeilingsRecordSequenceLossAndPreserveThePinnedOriginalBytes() {
        val root = Files.createTempDirectory("replay-cap-fixture-")
        val bytes = sample()
        val store = CaptureStore(root.resolve("ring"), videoLimit = bytes.size.toLong(), logLimit = 258, minFree = 0)
        val host = System.nanoTime()
        try {
            store.generation(1); store.status("device_log", StreamState.CAPTURING, null, 1); store.session(VideoPacket.Session(32, 32), 1)
            anchor(store.clock, 1_000_000_000, host)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            store.frame(VideoPacket.Frame(1_000_000, false, true, bytes), 1, host)
            val pin = store.capture(ReplaySettings())!!
            val pinnedFile = pin.video.single().file
            val hash = sha256(pinnedFile)
            store.log(DeviceLog(1_700_000_001_000_000_000, 12, 12, 10001, 0, 4, "a", "a", byteArrayOf(1, 2)), 1, host)
            anchor(store.clock, 2_000_000_000, host + 1_000_000_000)
            store.frame(VideoPacket.Frame(2_000_000, false, true, bytes), 1, host + 1_000_000_000)
            store.log(DeviceLog(1_700_000_002_000_000_000, 12, 12, 10001, 0, 4, "b", "b", byteArrayOf(3, 4)), 1, host + 1_000_000_000)
            store.prune(180)
            assertEquals(hash, sha256(pinnedFile))
            store.release(pin.id)
            val live = store.capture(ReplaySettings())!!
            assertEquals(1, live.video.size)
            assertEquals(1, live.logs.size)
            assertTrue(live.gaps.any { it.stream == "video" && it.fromNs == 0L && it.generation == 1L })
            assertTrue(live.gaps.any { it.stream == "device_log" && it.fromNs == 0L })
            store.release(live.id)
            assertFalse(Files.exists(pinnedFile))
            assertFailsWith<IllegalArgumentException> {
                store.frame(VideoPacket.Frame(0, true, false, ByteArray(ReplaySettings.MAX_CONFIG_PACKET_BYTES + 1)), 1)
            }
        } finally { store.close(); Files.delete(root) }
    }

    @Test
    fun completeDisconnectFreezesPruneAndLowDiskStopsBeforeWritingPacket() {
        val root = Files.createTempDirectory("replay-ring-fixture-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime()
        val bytes = sample()
        try {
            store.generation(1); store.status("device_log", StreamState.CAPTURING, null, 1); store.session(VideoPacket.Session(32, 32), 1)
            anchor(store.clock, 1_000_000_000, host)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            store.frame(VideoPacket.Frame(1_000_000, false, true, bytes), 1, host)
            store.freeze()
            val frozen = store.end()
            store.clock.boundary() // The read after the disconnect shows a source-clock discontinuity.
            anchor(store.clock, 11_000_000_000, host + 10_000_000_000)
            store.prune(1)
            assertEquals(frozen, store.end())
            assertTrue(store.hasData())
            val fixed = store.capture(ReplaySettings(replaySeconds = 1))!!
            assertEquals(frozen, fixed.end)
            assertTrue(fixed.windowKnown)
            assertEquals(0, fixed.windowClockEpoch)
            store.release(fixed.id)
            store.resume(); store.prune(1)
            assertFalse(store.hasData())
            assertEquals(10_000_000_000, store.end())
        } finally { store.close(); Files.delete(root) }
        val lowRoot = Files.createTempDirectory("replay-low-disk-")
        CaptureStore(lowRoot.resolve("ring"), minFree = Long.MAX_VALUE).use { low ->
            low.generation(1); low.session(VideoPacket.Session(32, 32), 1)
            low.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            assertFailsWith<IllegalStateException> { low.frame(VideoPacket.Frame(1, false, true, bytes), 1) }
            assertFalse(low.hasData())
        }
        Files.delete(lowRoot)
    }
}
