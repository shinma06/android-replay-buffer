package io.github.shinma06.replaybuffer.core

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
import kotlin.test.assertTrue

class CaptureStoreSaveTest {
    @Test
    fun delayedValidClockKeepsNewTargetLogMembershipWithoutReclassifyingTrueUnknown() {
        val root = Files.createTempDirectory("replay-clock-receipt-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime()
        fun record(elapsed: Long, message: String) = DeviceLog(1_700_000_000_000_000_000 + elapsed,
            12, 12, 10001, 0, 4, "Fixture", message, byteArrayOf(1))
        try {
            store.generation(1)
            assertTrue(store.clock.add("1", listOf(1_000_000_000L, 1_000_000_000L,
                1_700_000_001_000_000_000, 1_000_000_000), host, host + 200_000))
            store.app("com.example.target", 10001, setOf(12), 1, uidExclusive = true)
            store.log(record(1_100_000_000, "known window start"), 1, host + 100_100_000)
            assertTrue(store.clock.add("1", listOf(2_000_000_000L, 2_000_000_000L,
                1_700_000_002_000_000_000, 2_000_000_000), host + 1_000_000_000, host + 1_031_842_708))
            val normal = record(2_034_000_000, "causally arrived target")
            val unknown = record(40_000_000_000, "unsupported source")
            store.log(normal, 1, host + 1_035_100_000)
            store.log(unknown, 1, host + 1_060_000_000)
            anchor(store.clock, 3_000_000_000, host + 2_000_000_000)
            val capture = store.capture(ReplaySettings(replaySeconds = 180))!!
            val target = capture.logs.single { it.source === normal }
            val unsupported = capture.logs.single { it.source === unknown }
            assertEquals(true, target.app)
            assertEquals(1_034_000_000, target.retainedAt)
            assertEquals(1_034_000_000, target.time.sequence)
            assertEquals(null, unsupported.app)
            assertEquals(null, unsupported.time.sequence)
            assertEquals(Long.MAX_VALUE, unsupported.time.uncertainty)
            val output = SaveWriter().write(capture, root, { false }) { a, b -> Files.move(a, b) }
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
            store.generation(1)
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
                SaveWriter().write(capture, root, { true }) { a, b -> Files.move(a, b) }
            }
            assertFailsWith<IllegalStateException> { store.capture(ReplaySettings(replaySeconds = 8)) }
            anchor(store.clock, 8_000_000_000, host + 7_000_000_000)
            store.app("com.example.other", 10003, setOf(99), 1, uidExclusive = true)
            store.log(record(8_000_000_000, "later target", 10003), 1, host + 7_000_000_000)
            val output = SaveWriter().write(capture, root, { false }) { a, b -> Files.move(a, b) }
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
            store.generation(1)
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
    fun oneInvalidClockRoundTripDoesNotFragmentKnownVideoBeforeAndAfterRecovery() {
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
        val before = ClockSample(0, "1", 2_000_000_000, 2_000_000_000, 0, 2_000_000_000, 0, 0, 0, -1_000_000_000, 0)
        val recovery = before.copy(before = 4_000_000_000, after = 4_000_000_000, mono = 4_000_000_000)
        val gap = CaptureGap("clock", 1_000_000_000, 3_000_009_083, "failure", Long.MAX_VALUE)
        val clocks = listOf(before, recovery)
        for (unproven in listOf(recovery.copy(sequenceOffset = null), recovery.copy(bridgeError = Long.MAX_VALUE),
                recovery.copy(bridgeError = 20_000_001), recovery.copy(sequenceOffset = Long.MAX_VALUE),
                recovery.copy(received = 100_000_000))) {
            assertEquals(null, listOf(gap).videoScopes(clocks, mapOf(gap to unproven)).single().toNs)
        }
        val proven = listOf(gap).videoScopes(clocks, mapOf(gap to recovery)).single()
        assertEquals(gap.toNs, proven.toNs)
        assertEquals(1_000_000, proven.boundaryUncertaintyNs)
        val unknownStart = gap.copy(fromNs = null)
        assertEquals(null, listOf(unknownStart).videoScopes(clocks, mapOf(unknownStart to recovery)).single().fromNs)
        val open = gap.copy(toNs = null)
        assertEquals(null, listOf(open).videoScopes(clocks, mapOf(open to recovery)).single().toNs)
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
            store.generation(1); store.session(VideoPacket.Session(32, 32), 1)
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
                    1_700_000_003_000_000_000, 3_000_000_000), host + 2_000_000_000, host + 2_100_000_000)
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
            val output = SaveWriter().write(capture, root, { false }) { a, b -> Files.move(a, b) }
            val manifest = JsonParser.parseString(Files.readString(output.directory.resolve("session.json"))).asJsonObject
            val parts = manifest["parts"].asJsonArray
            assertEquals(2, parts.size())
            assertEquals("400001", parts[0].asJsonObject["duration_us"].asString)
            if (immediateRecovery) assertTrue(parts[1].asJsonObject["duration_us"].asString.toLong() >= 400001)
            else assertEquals("900000", parts[1].asJsonObject["duration_us"].asString)
            assertEquals(listOf("video"), output.missingKinds) // Unknown clock information remains visible.
            assertEquals(Long.MAX_VALUE.toString(), manifest["gaps"].asJsonArray.single().asJsonObject["boundary_uncertainty_ns"].asString)
            assertTrue(manifest["gaps"].asJsonArray.single().asJsonObject["duration_uncertain"].asBoolean)
            val scope = manifest["video_clock_gap_scopes"].asJsonArray.single().asJsonObject
            assertTrue(scope["derived_from_valid_samples"].asBoolean)
            assertTrue(scope["from_ns"].asString.toLong() <= gap.fromNs)
            assertTrue(scope["to_ns"].asString.toLong() >= gap.toNs)
            assertTrue(scope["boundary_uncertainty_ns"].asString.toLong() > 0)
            assertFalse(manifest["video_missing_ranges"].asJsonArray.isEmpty)
            val frames = Files.readAllLines(output.directory.resolve("frames.jsonl")).map { JsonParser.parseString(it).asJsonObject }
            assertEquals(capture.video.map { it.pts.toString() }, frames.filter { it["presented"].asBoolean }.map { it["source_pts_us"].asString })
            assertEquals(6, frames.count { it["presented"].asBoolean })
            assertEquals(if (immediateRecovery) 3 else 0, frames.count { it["preroll"].asBoolean })
            if (immediateRecovery) {
                assertEquals(if (processFailure) 3 else 4, capture.clocks.size) // No next periodic sample has been added.
                assertEquals(capture.clocks.last(), capture.clockRecoveries[gap])
                assertTrue(scope["recovery_sample"].asJsonObject["valid"].asBoolean)
                val noRecoveryProof = capture.copy(clockRecoveries = emptyMap())
                val unproven = SaveWriter().write(noRecoveryProof, root, { false }) { a, b -> Files.move(a, b) }
                val unprovenManifest = JsonParser.parseString(Files.readString(unproven.directory.resolve("session.json"))).asJsonObject
                assertEquals(4, unprovenManifest["parts"].asJsonArray.size())
                anchor(store.clock, 5_000_000_000, recoveredAt + 1_000_000_000)
                val retry = SaveWriter().write(capture, root, { false }) { a, b -> Files.move(a, b) }
                val retryManifest = JsonParser.parseString(Files.readString(retry.directory.resolve("session.json"))).asJsonObject
                assertEquals(parts, retryManifest["parts"])
                assertEquals(gap, capture.gaps.single())
                store.release(capture.id)
                val next = store.capture(ReplaySettings())!!
                assertEquals(if (processFailure) 4 else 5, next.clocks.size)
                val nextOutput = SaveWriter().write(next, root, { false }) { a, b -> Files.move(a, b) }
                val nextManifest = JsonParser.parseString(Files.readString(nextOutput.directory.resolve("session.json"))).asJsonObject
                assertEquals(2, nextManifest["parts"].asJsonArray.size())
                assertEquals(scope, nextManifest["video_clock_gap_scopes"].asJsonArray.single())
                assertEquals(capture.video.map { it.pts }, next.video.map { it.pts })
                assertEquals(gap, next.gaps.single())
                store.release(next.id)
                return
            }
            val unknown = capture.copy(gaps = listOf(gap.copy(fromNs = null, toNs = null)))
            val unknownOutput = SaveWriter().write(unknown, root, { false }) { a, b -> Files.move(a, b) }
            val unknownManifest = JsonParser.parseString(Files.readString(unknownOutput.directory.resolve("session.json"))).asJsonObject
            assertEquals(6, unknownManifest["parts"].asJsonArray.size())
            assertEquals(listOf("video"), unknownOutput.missingKinds)
            assertFalse(unknownManifest["video_clock_gap_scopes"].asJsonArray.single().asJsonObject["derived_from_valid_samples"].asBoolean)
            val open = capture.copy(gaps = listOf(gap.copy(toNs = null)))
            val openOutput = SaveWriter().write(open, root, { false }) { a, b -> Files.move(a, b) }
            val openManifest = JsonParser.parseString(Files.readString(openOutput.directory.resolve("session.json"))).asJsonObject
            assertEquals(4, openManifest["parts"].asJsonArray.size()) // Three known frames before failure remain together.
            assertEquals("400001", openManifest["parts"].asJsonArray[0].asJsonObject["duration_us"].asString)
            val noStart = capture.copy(gaps = listOf(gap.copy(fromNs = null)))
            val noStartOutput = SaveWriter().write(noStart, root, { false }) { a, b -> Files.move(a, b) }
            val noStartManifest = JsonParser.parseString(Files.readString(noStartOutput.directory.resolve("session.json"))).asJsonObject
            assertEquals(4, noStartManifest["parts"].asJsonArray.size()) // Three known frames after recovery remain together.
            assertEquals("900000", noStartManifest["parts"].asJsonArray.last().asJsonObject["duration_us"].asString)
            val unknownClock = capture.copy(clocks = emptyList(), clockRecoveries = emptyMap())
            val noProofOutput = SaveWriter().write(unknownClock, root, { false }) { a, b -> Files.move(a, b) }
            val noProof = JsonParser.parseString(Files.readString(noProofOutput.directory.resolve("session.json"))).asJsonObject
            assertEquals(6, noProof["parts"].asJsonArray.size())
            anchor(store.clock, 8_000_000_000, host + 7_000_000_000)
            for (pts in listOf(7_100_000L, 7_300_000L, 7_500_000L)) {
                store.frame(VideoPacket.Frame(pts, false, true, bytes), 1, host + (pts - 1_000_000) * 1000)
            }
            val retry = SaveWriter().write(capture, root, { false }) { a, b -> Files.move(a, b) }
            val retryManifest = JsonParser.parseString(Files.readString(retry.directory.resolve("session.json"))).asJsonObject
            assertEquals(parts, retryManifest["parts"])
            assertEquals(gap, capture.gaps.single())
            store.release(capture.id)
            val later = store.capture(ReplaySettings(replaySeconds = 1))!!
            val laterOutput = SaveWriter().write(later, root, { false }) { a, b -> Files.move(a, b) }
            val laterManifest = JsonParser.parseString(Files.readString(laterOutput.directory.resolve("session.json"))).asJsonObject
            assertEquals(1, laterManifest["parts"].asJsonArray.size())
            assertEquals("900000", laterManifest["parts"].asJsonArray.single().asJsonObject["duration_us"].asString)
            assertEquals(listOf("video"), laterOutput.missingKinds) // The original unbounded clock warning is not erased.
            assertEquals(gap, later.gaps.single())
            store.release(later.id)
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
            store.generation(1); store.session(VideoPacket.Session(32, 32), 1)
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
            val output = SaveWriter().write(capture, root, { false }) { a, b -> Files.move(a, b) }
            val manifest = JsonParser.parseString(Files.readString(output.directory.resolve("session.json"))).asJsonObject
            assertEquals(1, manifest["parts"].asJsonArray.size())
            assertTrue(manifest["complete"].asBoolean)
            assertFalse(manifest["video_tail"].asJsonObject["display_held"].asBoolean)
            assertFalse(manifest["video_missing_ranges"].asJsonArray.isEmpty)
            assertEquals(listOf("video"), output.missingKinds)
            val missing = manifest["video_missing_ranges"].asJsonArray.last().asJsonObject
            assertEquals((capture.end - capture.start).toString(), missing["to_window_ns"].asString)
            anchor(store.clock, 2_000_000_000, host + 1_000_000_000)
            store.frame(VideoPacket.Frame(2_000_000, false, true, bytes), 1, host + 1_000_000_000)
            assertEquals(tail, capture.videoTail())
            val retry = SaveWriter().write(capture, root, { false }) { a, b -> Files.move(a, b) }
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
            store.generation(1); store.session(VideoPacket.Session(32, 32), 1)
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
            val output = SaveWriter().write(capture, root, { false }) { a, b -> Files.move(a, b) }
            val manifest = JsonParser.parseString(Files.readString(output.directory.resolve("session.json"))).asJsonObject
            assertTrue(manifest["parts"].asJsonArray.isEmpty)
            assertFalse(manifest["video_tail"].asJsonObject["display_held"].asBoolean)
            assertEquals(listOf("video"), output.missingKinds)
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
            store.generation(1); store.session(VideoPacket.Session(32, 32), 1)
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
            val output = SaveWriter().write(capture, root, { false }) { a, b -> Files.move(a, b) }
            val manifest = JsonParser.parseString(Files.readString(output.directory.resolve("session.json"))).asJsonObject
            assertEquals(1, manifest["parts"].asJsonArray.size())
            assertTrue(manifest["video_missing_ranges"].asJsonArray.isEmpty)
            val part = manifest["parts"].asJsonArray.single().asJsonObject
            assertEquals("0", part["window_start_ns"].asString)
            assertEquals("3000000", part["edit_start_us"].asString)
            NIOUtils.readableChannel(output.directory.resolve("video-001.mp4").toFile()).use { channel ->
                val frame = MP4Demuxer.createRawMP4Demuxer(channel).videoTrack.nextFrame()
                assertEquals(5_000_000, frame.duration)
            }
            for (gap in listOf(
                CaptureGap("video", 2_000_000_000, 2_100_000_000, "lost", 0),
                CaptureGap("clock", null, null, "unknown clock"),
            )) {
                val broken = SaveWriter().write(capture.copy(gaps = listOf(gap)), root, { false }) { a, b -> Files.move(a, b) }
                val brokenManifest = JsonParser.parseString(Files.readString(broken.directory.resolve("session.json"))).asJsonObject
                assertFalse(brokenManifest["video_missing_ranges"].asJsonArray.isEmpty)
            }
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
            store.generation(1); store.session(VideoPacket.Session(32, 32), 1)
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
            val output = SaveWriter().write(capture, root, { false }) { a, b -> Files.move(a, b) }
            assertTrue(output.missingKinds.isEmpty())
            val manifest = JsonParser.parseString(Files.readString(output.directory.resolve("session.json"))).asJsonObject
            assertTrue(manifest["complete"].asBoolean)
            assertFalse(manifest["video_tail"].asJsonObject["new_frame_confirmed"].asBoolean)
            assertTrue(manifest["video_tail"].asJsonObject["display_held"].asBoolean)
            assertEquals("2000000", manifest["parts"].asJsonArray.single().asJsonObject["duration_us"].asString)
            assertTrue(manifest["parts"].asJsonArray.single().asJsonObject["confirmed_window_end_ns"].isJsonNull)
            assertEquals(2, Files.readAllLines(output.directory.resolve("frames.jsonl")).size)
            val unknown = capture.copy(endUncertainty = Long.MAX_VALUE)
            assertFalse(unknown.videoTail()!!.displayHeld)
            assertEquals(null, unknown.videoTail()!!.fromNs)
            val broken = capture.copy(start = 0, gaps = listOf(CaptureGap("video", 5_000_000_000, 6_000_000_000, "known loss", 0)))
            assertEquals(5_000_000_000, broken.videoTail()!!.toNs)
            val brokenOutput = SaveWriter().write(broken, root, { false }) { a, b -> Files.move(a, b) }
            assertEquals(listOf("video"), brokenOutput.missingKinds)
            val longEnd = Int.MAX_VALUE.toLong() * 1000 + 20_000_000_000
            val longWindow = capture.copy(start = longEnd - 2_000_000_000, end = longEnd)
            val longOutput = SaveWriter().write(longWindow, root, { false }) { a, b -> Files.move(a, b) }
            assertTrue(longOutput.missingKinds.isEmpty())
            val longManifest = JsonParser.parseString(Files.readString(longOutput.directory.resolve("session.json"))).asJsonObject
            val longPart = longManifest["parts"].asJsonArray.single().asJsonObject
            assertTrue(longPart["media_timeline_clipped"].asBoolean)
            assertEquals("2000000", longPart["duration_us"].asString)
            assertEquals(capture.video.map { it.pts.toString() }, Files.readAllLines(longOutput.directory.resolve("frames.jsonl")).map {
                JsonParser.parseString(it).asJsonObject["source_pts_us"].asString
            })
            val following = capture.video.last().let { it.copy(pts = 1_000_000 + longEnd / 1000,
                time = it.time.copy(elapsed = 1_000_000_000 + longEnd, sequence = longEnd)) }
            val confirmedLong = longWindow.copy(video = listOf(capture.video.first(), following))
            val confirmedOutput = SaveWriter().write(confirmedLong, root, { false }) { a, b -> Files.move(a, b) }
            assertTrue(confirmedOutput.missingKinds.isEmpty())
            val confirmed = JsonParser.parseString(Files.readString(confirmedOutput.directory.resolve("session.json"))).asJsonObject
            assertTrue(confirmed["video_tail"].isJsonNull)
            assertEquals(1, confirmed["parts"].asJsonArray.size())
            assertEquals("2000000000", confirmed["parts"].asJsonArray.single().asJsonObject["confirmed_window_end_ns"].asString)
            NIOUtils.readableChannel(confirmedOutput.directory.resolve("video-001.mp4").toFile()).use { channel ->
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
            val agedOutput = SaveWriter().write(aged, root, { false }) { a, b -> Files.move(a, b) }
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
            store.generation(1); store.session(VideoPacket.Session(32, 32), 1)
            anchor(store.clock, 1_000_000_000, host)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            store.frame(VideoPacket.Frame(1_000_000, false, true, bytes), 1, host)
            val pin = store.capture(ReplaySettings())!!
            val pinnedFile = pin.video.single().file
            val hash = sha256(pinnedFile)
            store.clock.add("2", listOf(100_000_000, 100_000_000, 1_700_000_020_000_000_000, 100_000_000),
                host + 20_000_000_000, host + 20_000_000_000, 100_000_000_000)
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
    fun unknownBootBridgeRetainsOldRecordsAndExportsUnknownWindowCoordinates() {
        val root = Files.createTempDirectory("replay-unknown-boot-fixture-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime()
        val bytes = sample()
        try {
            store.generation(1); store.session(VideoPacket.Session(32, 32), 1)
            anchor(store.clock, 1_000_000_000, host)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            store.frame(VideoPacket.Frame(1_000_000, false, true, bytes), 1, host)
            store.log(DeviceLog(1_700_000_001_000_000_000, 12, 12, 10001, 0, 4, "Fixture", "old boot", byteArrayOf(1)), 1, host)
            store.status("video", StreamState.RECOVERING, "prior known gap", 1)
            store.status("video", StreamState.CAPTURING, null, 1)
            store.clock.add("2", listOf(100_000_000, 100_000_000, 1_700_000_020_000_000_000, 100_000_000),
                host + 20_000_000_000, host + 20_000_000_000, 100_000_000_000)
            store.prune(1)
            val capture = store.capture(ReplaySettings(replaySeconds = 1))!!
            assertFalse(capture.windowKnown)
            assertTrue(capture.gaps.any { it.reason == "prior known gap" })
            assertTrue(capture.states.getValue("video").gaps.any { it.reason == "prior known gap" })
            assertTrue(store.streams(1).getValue("video").gaps.any { it.reason == "prior known gap" })
            assertEquals(1, capture.video.size)
            assertEquals(1, capture.logs.size)
            val output = SaveWriter().write(capture, root, { false }) { a, b -> Files.move(a, b) }
            val manifest = JsonParser.parseString(Files.readString(output.directory.resolve("session.json"))).asJsonObject
            assertFalse(manifest["window_clock_known"].asBoolean)
            assertEquals(store.clock.currentEpoch(), manifest["window_clock_epoch"].asInt)
            assertTrue(manifest["parts"].asJsonArray.single().asJsonObject["window_start_ns"].isJsonNull)
            for (name in listOf("frames.jsonl", "logcat-device.jsonl")) {
                assertTrue(JsonParser.parseString(Files.readAllLines(output.directory.resolve(name)).single()).asJsonObject["window_ns"].isJsonNull)
            }
        } finally {
            store.close()
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }

    @Test
    fun nullTargetAndRecoveredClockAndLogGapsRemainImmutableOnlyWhileInWindow() {
        val root = Files.createTempDirectory("replay-gap-fixture-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val host = System.nanoTime()
        try {
            store.generation(1)
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
                host + 20_000_000_000, host + 20_000_000_000, 100_000_000_000)
            assertEquals(Long.MAX_VALUE, store.clock.endUncertainty())
            assertEquals(fixedEnd, store.end())
            assertTrue(store.streams(1).values.all { it.gaps.isEmpty() }) // Frozen known window is not replaced by the new unknown clock.
            store.resume()
            assertEquals(2, store.streams(1).getValue("app_log").gaps.size)
            assertEquals(2, appGaps.size)
        } finally { store.close(); Files.delete(root) }
    }

    private fun sample(): ByteArray {
        val picture = Picture.create(32, 32, ColorSpace.YUV420J).apply { fill(15) }
        val data = H264Encoder.createH264Encoder().encodeIDRFrame(picture, ByteBuffer.allocate(65536))
        return ByteArray(data.remaining()).also { data.get(it) }
    }

    private fun config(bytes: ByteArray): ByteArray = H264Utils.splitFrame(ByteBuffer.wrap(bytes))
        .filter { it.get(0).toInt() and 31 in setOf(7, 8) }
        .fold(byteArrayOf()) { result, nal -> result + byteArrayOf(0, 0, 0, 1) + ByteArray(nal.remaining()).also { nal.get(it) } }

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
            store.generation(1)
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
            assertFailsWith<SaveFailure> { SaveWriter().write(capture, root.resolve("missing"), { false }) { a, b -> Files.move(a, b) } }
            anchor(store.clock, 10_000_000_000, host + 9_000_000_000)
            store.prune(1)
            assertTrue(capture.video.all { Files.exists(it.file) })
            val output = SaveWriter().write(capture, root, { false }) { a, b -> Files.move(a, b) }
            assertEquals(listOf("video"), output.missingKinds) // The explicit gap remains a known interruption.
            val completed = output.directory
            val manifest = JsonParser.parseString(Files.readString(completed.resolve("session.json"))).asJsonObject
            assertFalse(manifest["files_sha256"].asJsonObject.has("session.json"))
            assertEquals(setOf("state", "availableSeconds", "reason"), manifest["coverage"].asJsonObject["video"].asJsonObject.keySet())
            val gap = manifest["gaps"].asJsonArray.single().asJsonObject
            assertTrue(gap["from_ns"].asJsonPrimitive.isString && gap["to_ns"].asJsonPrimitive.isString)
            assertTrue(manifest["build"].asJsonObject["source"].asString.matches(Regex("[a-f0-9]{40}")))
            manifest["files_sha256"].asJsonObject.entrySet().forEach { (name, value) -> assertEquals(value.asString, sha256(completed.resolve(name))) }
            val device = Files.readAllLines(completed.resolve("logcat-device.jsonl"))
            assertEquals(device, Files.readAllLines(completed.resolve("logcat-app.jsonl")))
            assertEquals(capture.logs.single().id, JsonParser.parseString(device.single()).asJsonObject["record_id"].asString)
            NIOUtils.readableChannel(completed.resolve("video-001.mp4").toFile()).use { channel ->
                val mux = MP4Demuxer.createRawMP4Demuxer(channel)
                val track = mux.videoTrack
                for ((pts, duration) in listOf(0L to 100_000L, 100_000L to 250_000L, 350_000L to 50_000L)) {
                    val frame = track.nextFrame()
                    assertEquals(pts, frame.pts)
                    assertEquals(duration, frame.duration)
                    assertEquals(1_000_000, frame.timescale)
                }
            }
            val logOnly = SaveWriter().write(capture.copy(video = emptyList()), root, { false }) { a, b -> Files.move(a, b) }
            assertEquals(listOf("video"), logOnly.missingKinds)
            val hash = sha256(completed.resolve("session.json"))
            assertFailsWith<CancellationException> { SaveWriter().write(capture, root, { true }) { a, b -> Files.move(a, b) } }
            assertEquals(hash, sha256(completed.resolve("session.json")))
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
            store.generation(1); store.session(VideoPacket.Session(32, 32), 1)
            anchor(store.clock, 1_000_000_000, host)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            for (pts in listOf(0L, 100_000L, 350_000L)) {
                anchor(store.clock, 1_000_000_000 + pts * 1000, host + pts * 1000)
                store.frame(VideoPacket.Frame(1_000_000 + pts, false, true, bytes), 1, host + pts * 1000)
            }
            anchor(store.clock, 1_400_000_000, host + 400_000_000)
            val original = store.capture(ReplaySettings())!!
            val cut = SaveWriter().write(original.copy(start = 50_000_000), root, { false }) { a, b -> Files.move(a, b) }
            val cutJson = JsonParser.parseString(Files.readString(cut.directory.resolve("session.json"))).asJsonObject
            val part = cutJson["parts"].asJsonArray.single().asJsonObject
            assertEquals("0", part["window_start_ns"].asString)
            assertEquals("50000", part["edit_start_us"].asString)
            NIOUtils.readableChannel(cut.directory.resolve("video-001.mp4").toFile()).use { channel ->
                val track = MP4Demuxer.createRawMP4Demuxer(channel).videoTrack
                assertEquals(listOf(0L, 50_000L, 300_000L), (1..3).map { track.nextFrame().pts })
            }
            val indices = Files.readAllLines(cut.directory.resolve("frames.jsonl")).map { JsonParser.parseString(it).asJsonObject }
            assertEquals(listOf("0", "50000", "300000"), indices.map { it["presentation_pts_us"].asString })
            val delayed = original.copy(start = 0, video = original.video.drop(1))
            val delayedOutput = SaveWriter().write(delayed, root, { false }) { a, b -> Files.move(a, b) }
            val delayedPart = JsonParser.parseString(Files.readString(delayedOutput.directory.resolve("session.json"))).asJsonObject["parts"].asJsonArray.single().asJsonObject
            assertEquals("0", delayedPart["edit_start_us"].asString)
            assertEquals("100000000", delayedPart["window_start_ns"].asString)
            assertEquals(listOf("video"), delayedOutput.missingKinds)
            val lateFrame = original.video.first().let { it.copy(pts = it.pts + 1_000_000,
                time = it.time.copy(elapsed = it.time.elapsed!! + 1_000_000_000, sequence = 1_000_000_000)) }
            val late = SaveWriter().write(original.copy(start = 0, end = 1_200_000_000, video = listOf(lateFrame)), root, { false }) { a, b -> Files.move(a, b) }
            val latePart = JsonParser.parseString(Files.readString(late.directory.resolve("session.json"))).asJsonObject["parts"].asJsonArray.single().asJsonObject
            assertEquals("0", latePart["edit_start_us"].asString)
            assertEquals("1000000000", latePart["window_start_ns"].asString)
            for (boundary in listOf("session", "clock", "config")) {
                val second = original.video.last().let { frame -> when (boundary) {
                    "session" -> frame.copy(session = frame.session + 1)
                    "clock" -> frame.copy(time = frame.time.copy(epoch = frame.time.epoch + 1))
                    else -> frame.copy(config = byteArrayOf(0) + frame.config)
                } }
                val split = SaveWriter().write(original.copy(video = listOf(original.video.first(), second)), root, { false }) { a, b -> Files.move(a, b) }
                val parts = JsonParser.parseString(Files.readString(split.directory.resolve("session.json"))).asJsonObject["parts"].asJsonArray
                assertEquals(2, parts.size())
                assertEquals("0", parts[1].asJsonObject["edit_start_us"].asString)
                assertEquals("350000000", parts[1].asJsonObject["window_start_ns"].asString)
            }
            store.release(original.id)
            anchor(store.clock, 2_050_000_000, host + 1_050_000_000)
            store.prune(1)
            val realCut = store.capture(ReplaySettings(replaySeconds = 1))!!
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
            store.generation(1); anchor(store.clock, 1_000_000_000, host)
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
            store.generation(1); store.session(VideoPacket.Session(32, 32), 1)
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
            store.generation(1); store.session(VideoPacket.Session(32, 32), 1)
            anchor(store.clock, 1_000_000_000, host)
            store.frame(VideoPacket.Frame(0, true, false, config(bytes)), 1)
            store.frame(VideoPacket.Frame(1_000_000, false, true, bytes), 1, host)
            store.freeze()
            val frozen = store.end()
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
