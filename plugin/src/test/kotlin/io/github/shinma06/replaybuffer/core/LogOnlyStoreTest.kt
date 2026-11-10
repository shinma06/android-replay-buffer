package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonParser
import java.nio.file.Files
import java.nio.file.Path
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertFailsWith
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

class LogOnlyStoreTest {
    @TempDir lateinit var root: Path
    private val settings = ReplaySettings(captureMode = CaptureMode.LOG_ONLY)

    @Test
    fun quietWindowRequiresReadersCurrentClockAndTargetWithoutInventingRowsOrVideo() {
        CaptureStore(root.resolve("ring"), captureMode = CaptureMode.LOG_ONLY).use { store ->
            val host = System.nanoTime() + 1_000_000_000
            store.generation(1)
            assertFalse(store.hasData())
            assertNull(store.capture(settings))
            store.status("device_log", StreamState.CAPTURING, null, 1)
            store.app("com.fixture.app", 10001, setOf(12), 1, true)
            assertFalse(store.hasData())
            sample(store, host, 1)
            assertFalse(store.hasData()) // The target has not been confirmed in this clock epoch.
            store.app("com.fixture.app", 10001, setOf(12), 1, true)
            assertFalse(store.hasData()) // No positive window yet at the fixed fixture clock.
            sample(store, host, 2)
            assertTrue(store.hasData())
            // Defensive store guards reject packets before config validation, copying or GOP creation.
            store.session(VideoPacket.Session(32, 32), 1)
            assertFalse(store.frame(VideoPacket.Frame(0, true, true, byteArrayOf(1)), 1))
            store.status("video", StreamState.RECOVERING, "unrequested", 1)
            store.prune(1)
            assertEquals(0L, Files.list(store.directory).use { it.count() })
            val capture = assertNotNull(store.capture(settings))
            assertTrue(capture.empty)
            assertTrue(capture.start < capture.end)
            assertEquals(StreamState.NOT_REQUESTED, capture.states.getValue("video").state)
            assertTrue(capture.states.getValue("video").gaps.isEmpty())
            assertEquals(0.0, capture.states.getValue("device_log").availableSeconds)
            assertEquals(0.0, capture.states.getValue("app_log").availableSeconds)
            assertTrue(capture.gaps.none { it.stream == "video" })
            assertFailsWith<IllegalStateException> { store.capture(settings) }
            store.release(capture.id)
            assertFailsWith<IllegalArgumentException> { store.capture(ReplaySettings()) }
            store.status("device_log", StreamState.RECOVERING, "reader failed", 1)
            assertFalse(store.hasData())
            assertNull(store.capture(settings))
            store.status("device_log", StreamState.CAPTURING, null, 1)
            store.app("com.fixture.app", null, emptySet(), 1)
            assertFalse(store.hasData())
            store.app("com.fixture.app", 10001, setOf(12), 1, true)
            assertTrue(store.hasData())
            store.clockStatus(false, 1)
            assertFalse(store.hasData())
            assertNull(store.capture(settings))
            sample(store, host, 3)
            assertTrue(store.hasData())
            store.clock.boundary()
            store.clockStatus(false, 1)
            assertFalse(store.hasData())
            assertNull(store.capture(settings))
            assertTrue(capture.clocks.all { it.valid }) // The already pinned target remains unchanged.
            assertTrue(capture.gaps.isEmpty())
        }
    }

    @Test
    fun reconnectCannotReusePreviousGenerationEvidenceForAZeroRowSave() {
        CaptureStore(root.resolve("ring"), captureMode = CaptureMode.LOG_ONLY).use { store ->
            val host = System.nanoTime() + 1_000_000_000
            store.generation(1)
            sample(store, host, 1)
            store.status("device_log", StreamState.CAPTURING, null, 1)
            store.app("com.fixture.app", 10001, setOf(12), 1, true)
            sample(store, host, 2)
            assertTrue(store.hasData())
            store.freeze()
            store.status("device_log", StreamState.RECOVERING, "disconnected", 1)
            store.status("app_log", StreamState.RECOVERING, "disconnected", 1)
            store.generation(2)
            store.status("device_log", StreamState.CAPTURING, null, 2)
            store.app("com.fixture.app", 10001, setOf(12), 2, true)
            store.clockStatus(true, 2)
            store.resume()
            assertFalse(store.hasData())
            assertNull(store.capture(settings))
            sample(store, host, 3, 2)
            assertTrue(store.hasData())
            val capture = assertNotNull(store.capture(settings))
            assertTrue(capture.gaps.any { it.reason == "disconnected" })
            assertEquals(StreamState.NOT_REQUESTED, capture.states.getValue("video").state)
        }
    }

    @Test
    fun recoveredReconnectHistoryLeavesTheWindowButFailedRetryKeepsItsOriginalScope() {
        CaptureStore(root.resolve("reconnect"), captureMode = CaptureMode.LOG_ONLY).use { store ->
            val host = System.nanoTime() + 1_000_000_000
            store.generation(1)
            sample(store, host, 1)
            store.status("device_log", StreamState.CAPTURING, null, 1)
            store.app("com.fixture.app", 10001, setOf(12), 1, true)
            sample(store, host, 2)
            store.clock.boundary(host + 1_000_000_000)
            store.status("device_log", StreamState.RECOVERING, "disconnected", 1)
            store.status("app_log", StreamState.RECOVERING, "disconnected", 1)
            store.freeze()
            store.generation(2)
            sample(store, host, 4, 2)
            store.status("device_log", StreamState.CAPTURING, null, 2)
            store.app("com.fixture.app", 10001, setOf(12), 2, true)
            store.resume()
            val inside = assertNotNull(store.capture(settings.copy(replaySeconds = 2), host = host + 3_500_000_000))
            assertEquals(setOf("device_log", "app_log"), inside.gaps.map { it.stream }.toSet())
            assertTrue(inside.gaps.all { it.boundaryUncertaintyNs == Long.MAX_VALUE && it.toNs != null })
            assertTrue(inside.states.getValue("app_log").gaps.isNotEmpty())
            assertFailsWith<SaveFailure> {
                SaveWriter().write(inside, root.resolve("missing"), { false }) { publication -> Files.move(publication.partial, publication.complete) }
            }
            sample(store, host, 6, 2)
            store.prune(1)
            assertFailsWith<IllegalStateException> { store.capture(settings.copy(replaySeconds = 1)) }
            val retry = SaveWriter().write(inside, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
            assertTrue(inside.gaps.inWindow(inside.start, inside.end, inside.endUncertainty, inside.clocks, inside.clockRecoveries).isNotEmpty())
            assertEquals(inside.end.toString(), JsonParser.parseString(Files.readString(retry.directory.resolve("session.json"))).asJsonObject["window_end_ns"].asString)
            store.release(inside.id)
            val outside = assertNotNull(store.capture(settings.copy(replaySeconds = 1), host = host + 5_500_000_000))
            assertTrue(outside.logs.isEmpty())
            assertEquals(inside.gaps, outside.gaps)
            assertTrue(outside.states.values.all { it.gaps.isEmpty() })
            assertTrue(outside.gaps.inWindow(outside.start, outside.end, outside.endUncertainty, outside.clocks, outside.clockRecoveries).isEmpty())
            assertEquals(StreamState.NOT_REQUESTED, outside.states.getValue("video").state)
            store.release(outside.id)
        }
    }

    @Test
    fun logBoundsPartialSaveAndFrozenOwnershipReuseTheExistingWriter() {
        CaptureStore(root.resolve("ring"), logLimit = 514, minFree = 0, captureMode = CaptureMode.LOG_ONLY).use { store ->
            val host = System.nanoTime() + 1_000_000_000
            store.generation(1)
            sample(store, host, 1)
            store.status("device_log", StreamState.CAPTURING, null, 1)
            store.app("com.fixture.app", 10001, setOf(12), 1, true)
            sample(store, host, 2)
            repeat(3) { index ->
                store.log(DeviceLog(1_700_000_002_000_000_000, 12, 12, 10001, 0, 4,
                    "Fixture", "row-$index", byteArrayOf(1)), 1, host + 1_000_000_000)
            }
            store.status("device_log", StreamState.RECOVERING, "reader failed", 1)
            assertTrue(store.hasData())
            val pinned = assertNotNull(store.capture(settings))
            assertEquals(listOf("row-1", "row-2"), pinned.logs.map { it.source.message })
            assertTrue(pinned.gaps.any { it.reason.contains("byte上限") })
            assertEquals(StreamState.RECOVERING, pinned.states.getValue("device_log").state)
            assertFailsWith<SaveFailure> { SaveWriter().write(pinned, root.resolve("missing"), { false }) { } }
            // Retention and later reader state cannot rewrite the failed request's rows or mode.
            store.status("device_log", StreamState.CAPTURING, null, 1)
            for (second in 3L..6L) sample(store, host, second)
            store.prune(1)
            assertEquals(2, pinned.logs.size)
            val output = SaveWriter().write(pinned, root, { false }) { Files.move(it.partial, it.complete) }
            assertTrue(output.missingKinds.isEmpty()) // Core owns log shortages; the writer adds only video shortages.
            val json = JsonParser.parseString(Files.readString(output.directory.resolve("session.json"))).asJsonObject
            assertEquals(3, json["schema"].asInt)
            assertEquals("LOG_ONLY", json["capture_mode"].asString)
            assertFalse(json["coverage"].asJsonObject["video"].asJsonObject["requested"].asBoolean)
            assertTrue(json["video_missing_ranges"].asJsonArray.isEmpty)
            assertTrue(json["video_clock_gap_scopes"].asJsonArray.isEmpty)
            assertTrue(json["video_file"].isJsonNull)
            assertFalse(json["video_reencoded"].asBoolean)
            assertEquals(0L, Files.size(output.directory.resolve("frames.jsonl")))
            assertTrue(Files.readString(output.directory.resolve("README.txt")).contains("意図的に未要求"))
            assertFalse(Files.exists(output.directory.resolve("video-source.bin")))
            assertEquals(2, Files.readAllLines(output.directory.resolve("logcat-app.jsonl")).size)
            store.release(pinned.id)
            val quiet = assertNotNull(store.capture(settings.copy(replaySeconds = 1)))
            assertTrue(quiet.logs.isEmpty())
            assertTrue(quiet.end - quiet.start <= 1_000_000_000)
            store.release(quiet.id)
        }
    }

    private fun sample(store: CaptureStore, host: Long, second: Long, generation: Long = 1) {
        val elapsed = second * 1_000_000_000
        val received = host + (second - 1) * 1_000_000_000
        assertTrue(store.clock.add("1", listOf(elapsed, elapsed, 1_700_000_000_000_000_000 + elapsed, elapsed), received, received))
        store.clockStatus(true, generation)
    }
}
