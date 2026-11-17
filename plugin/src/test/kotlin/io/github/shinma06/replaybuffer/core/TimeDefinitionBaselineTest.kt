package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonParser
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path
import java.util.ArrayDeque
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

/** T1: schema 4 quirks are intentional baselines, not schema 5's future coverage guarantees. */
class TimeDefinitionBaselineTest {
    @TempDir lateinit var root: Path

    private val second = 1_000_000_000L
    // Fixed future host origin also keeps legacy APIs without a host argument independent of test-runner speed:
    // their clock.now() clamps to the latest sample. No assertion uses their sampled wall/host time.
    private val host = 4_000_000_000_000_000_000L
    private val wall = 1_700_000_000_000_000_000L

    private fun at(ns: Long) = host + ns
    private fun anchor(clock: CaptureClock, ns: Long) {
        assertTrue(clock.add("baseline", listOf(second + ns, second + ns, wall + ns, second + ns), at(ns), at(ns)))
    }

    private fun log(ns: Long, message: String = "row $ns") =
        DeviceLog(wall + ns, 12, 12, 10001, 0, 4, "Baseline", message, byteArrayOf(1))

    private fun withStore(check: (CaptureStore) -> Unit) {
        CaptureStore(root.resolve("ring"), minFree = 0, captureMode = CaptureMode.LOG_ONLY).use { store ->
            store.generation(1)
            store.status("device_log", StreamState.CAPTURING, null, 1)
            anchor(store.clock, 0)
            check(store)
        }
    }

    private fun row(store: CaptureStore, ns: Long) = assertNotNull(store.log(log(ns), 1, at(ns)))
    private fun available(store: CaptureStore, end: Long = 10 * second) =
        store.streams(seconds = 30, atEnd = end).getValue("device_log").availableSeconds

    // status/addGap do not yet accept an explicit host. Install only the exact gap inputs to streams(),
    // rather than deriving boundaries from the runner's clock or changing production in this baseline Trail.
    @Suppress("UNCHECKED_CAST")
    private fun gaps(store: CaptureStore, vararg ranges: Pair<Long?, Long?>) {
        val field = CaptureStore::class.java.getDeclaredField("gaps").apply { isAccessible = true }
        val stored = field.get(store) as ArrayDeque<CaptureGap>
        ranges.forEachIndexed { i, (from, to) ->
            stored.add(CaptureGap("device_log", from, to, "baseline gap $i", 0, 1, 0))
        }
    }

    @Test
    fun spanCountsAcrossUnknownInterval() = withStore { store ->
        row(store, 0)
        row(store, 6 * second) // More than five seconds after the only sample: unknown at receipt.
        store.clock.startRegion()
        anchor(store.clock, 9 * second)
        // Beyond the overlapping source ranges at the region boundary, this row maps unambiguously.
        row(store, 10 * second)
        val capture = assertNotNull(store.capture(ReplaySettings(captureMode = CaptureMode.LOG_ONLY), host = at(10 * second)))
        assertEquals(listOf(0L, null, 10 * second), capture.logs.map { it.time.sequence })
        assertEquals(10.0, available(store)) // The unknown middle interval is still inside max - min.
    }

    @Test
    fun quietIntervalShortensAvailableSeconds() = withStore { store ->
        row(store, 0)
        anchor(store.clock, 2 * second)
        row(store, 2 * second)
        anchor(store.clock, 10 * second)
        assertEquals(StreamState.CAPTURING, store.streams(30, 10 * second).getValue("device_log").state)
        assertEquals(2.0, available(store)) // Eight seconds of quiet tail do not contribute.
    }

    @Test
    fun gapWithUnknownStartSubtractsZero() = withStore { store ->
        row(store, 0)
        anchor(store.clock, 10 * second)
        row(store, 10 * second)
        gaps(store, null to 8 * second)
        assertEquals(10.0, available(store))
    }

    @Test
    fun overlappingGapsAreSubtractedTwice() = withStore { store ->
        row(store, 0)
        anchor(store.clock, 10 * second)
        row(store, 10 * second)
        gaps(store, 2 * second to 6 * second, 4 * second to 8 * second)
        assertEquals(2.0, available(store)) // 10 - 4 - 4, rather than subtracting the six-second union.
    }

    @Test
    fun endFallsBackToLatestRecordWhenClockUnmapped() = withStore { store ->
        row(store, 5 * second)
        anchor(store.clock, 12 * second)
        row(store, 12 * second)
        store.clock.clear() // Exercise the no-anchor branch, separately from a merely stale anchor.
        assertNull(store.clock.now(at(20 * second)))
        val capture = assertNotNull(store.capture(ReplaySettings(captureMode = CaptureMode.LOG_ONLY), host = at(20 * second)))
        assertFalse(capture.windowKnown)
        assertEquals(12 * second, capture.end) // retainedAt of the latest row, not request host time.
        assertEquals(listOf(5 * second, 12 * second), capture.logs.map { it.retainedAt })
        assertTrue(capture.logs.all { it.time.sequence == null })
    }

    @Test
    fun unknownWindowUsesHostCutoffWithMargin() = withStore { store ->
        store.clock.startRegion()
        val saveHost = at(40 * second)
        val cutoff = saveHost - 10 * second - 250_000_000
        for (receipt in listOf(cutoff - 1, cutoff, cutoff + 1))
            assertNotNull(store.log(log(0, "receipt $receipt"), 1, receipt))
        val capture = assertNotNull(store.capture(ReplaySettings(replaySeconds = 10, captureMode = CaptureMode.LOG_ONLY), host = saveHost))
        assertFalse(capture.windowKnown)
        assertEquals(cutoff, capture.hostStart)
        assertEquals(listOf(cutoff, cutoff + 1), capture.logs.map { it.host })
        assertTrue(capture.logs.all { it.time.sequence == null })
        // #445's 137-second/clock-unknown regression is also covered by
        // CaptureStoreSaveTest.recordsWithUnknownTimeBeforeAClockEpochBoundaryStayOutsideTheReplayWindow.
        // #446's 49-second/delayed-receipt regression is also covered by
        // CaptureStoreSaveTest.unknownClockMappingStillTrimsTheWindowByHostReceiveTime.
        // Keep their existing sparse/preroll/retention inputs unchanged.
    }

    @Test
    fun tailExactlyFiveSecondsIsCertain() {
        val clock = CaptureClock()
        anchor(clock, 0)
        val lastFresh = at(5 * second)
        assertTrue(clock.certain(lastFresh))
        assertEquals(1_000_000, clock.endUncertainty(lastFresh))
        val mapped = clock.log(wall + 5 * second, lastFresh)
        assertEquals(5 * second, mapped.sequence)
        assertEquals(1_000_000, mapped.uncertainty)
    }

    @Test
    fun tailFiveSecondsPlusOneIsUnknown() {
        val clock = CaptureClock()
        anchor(clock, 0)
        val stale = at(5 * second + 1)
        assertFalse(clock.certain(stale))
        assertEquals(Long.MAX_VALUE, clock.endUncertainty(stale))
        assertNull(clock.log(wall + 5 * second, stale).sequence)
    }

    private fun mappingAtError(error: Long): MappedTime {
        // Test OffsetTrack's exact conversion-error boundary without add() opening a new region for
        // an abrupt offset change. Both samples are valid (zero read width, readError = 1 ms).
        val offset = error - 1_000_000
        val samples = listOf(
            ClockSample(0, "baseline", second, second, wall, second, at(0), at(0), -second),
            ClockSample(0, "baseline", 11 * second + offset, 11 * second, wall + 10 * second,
                11 * second + offset, at(10 * second), at(10 * second), -second),
        )
        return assertNotNull(OffsetTrack(samples, false, null).map(6 * second, at(10 * second), 0, 0))
    }

    @Test
    fun errorExactlyTwentyMsIsMapped() {
        val mapped = mappingAtError(20_000_000)
        assertEquals(5 * second + 9_500_000, mapped.sequence)
        assertEquals(20_000_000, mapped.uncertainty)
    }

    @Test
    fun errorTwentyMsPlusOneIsUnknown() {
        val mapped = mappingAtError(20_000_001)
        assertNull(mapped.elapsed)
        assertNull(mapped.sequence)
        assertEquals(Long.MAX_VALUE, mapped.uncertainty)
    }

    @Test
    fun rowTnsRequiresSequenceAndKnownWindow() {
        for (known in listOf(true, false)) {
            val rows = listOf(2 * second, null).mapIndexed { i, sequence ->
                LogEntry("row-$i", log(2 * second + i), 1, MappedTime(3 * second, 0, 1_000_000, sequence),
                    2 * second, true, at(2 * second))
            }
            val touches = rows.mapIndexed { i, row ->
                TouchEntry(TouchSample(3 * second + i, "baseline", i, TouchAction.DOWN, 10, 10),
                    1, row.time, row.host, 32, 32)
            }
            val settings = ReplaySettings()
            val capture = FrozenCapture("baseline-$known", "baseline", 1, second, 4 * second, 30,
                emptyList(), rows, emptyList(), emptyList(), emptyList(), settings,
                listOf("video", "device_log", "app_log").associateWith { StreamSnapshot() },
                endUncertainty = if (known) 1_000_000 else Long.MAX_VALUE, touches = touches)
            val output = SaveWriter().write(capture, root, { false }) { Files.move(it.partial, it.complete) }.directory
            for (file in listOf(ReplayFiles.LOGCAT_DEVICE, ReplayFiles.LOGCAT_APP, ReplayFiles.TOUCHES)) {
                val written = Files.readAllLines(output.resolve(file)).map { JsonParser.parseString(it).asJsonObject }
                assertEquals(2, written.size, file)
                assertEquals(if (known) "1000000000" else null, written[0]["t_ns"].takeUnless { it.isJsonNull }?.asString, file)
                assertTrue(written[1]["t_ns"].isJsonNull, file)
                assertTrue(written.all { it["elapsed_ns"].asString == "3000000000" }, file)
            }
        }
    }

    @Test
    fun autosaveRejectedWhenClockUncertain() {
        CrashFixture(root, host = host).use { f ->
            f.standard()
            val pair = f.pair()
            val saveHost = f.at(10_020_000_000)
            assertTrue(f.store.clock.certain(saveHost))
            assertNull(f.store.autosave.admissionProblem(pair, saveHost))
            f.store.clock.startRegion()
            assertFalse(f.store.clock.certain(saveHost))
            // Admission itself is still otherwise valid; capture's certainty guard rejects this request.
            assertNull(f.store.autosave.admissionProblem(pair, saveHost))
            val rejected = assertFailsWith<AutoSaveRejected> { f.store.capture(f.settings, pair, saveHost) }
            assertEquals(AutoSaveReason.NOT_SAVED_CLOCK_UNKNOWN, rejected.reason)
        }
    }
}
