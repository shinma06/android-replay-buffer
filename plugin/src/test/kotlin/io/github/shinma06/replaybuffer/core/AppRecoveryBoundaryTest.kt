package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonParser
import java.nio.file.Files
import java.nio.file.Path
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue

class AppRecoveryBoundaryTest {
    @TempDir lateinit var root: Path

    @Test
    fun targetResolutionFlapsBeforeReaderRecoveryKeepOneOpenGap() = fixture { store, host ->
        interrupt(store)
        sample(store, host, 2)
        target(store, 2)
        assertEquals(StreamState.RECOVERING, store.streams().getValue("app_log").state)
        store.app("com.example.app", null, emptySet(), 2)
        target(store, 2)
        store.status("device_log", StreamState.CAPTURING, null, 2)
        val gap = appGap(store)
        assertEquals(1_000_000_000, gap.toNs)
        assertEquals("connection interrupted", gap.reason)
        target(store, 2)
        assertEquals(gap, appGap(store))
    }

    @Test
    fun recentPreviousBackendClockCannotCloseNewGenerationGap() = fixture { store, host ->
        interrupt(store)
        target(store, 2)
        store.status("device_log", StreamState.CAPTURING, null, 2)
        store.clockStatus(true, 1)
        store.clockStatus(true, 2) // A healthy callback without a new sample is not a new backend clock.
        assertNull(appGap(store).toNs)
        assertEquals(StreamState.RECOVERING, store.streams().getValue("app_log").state)
        sample(store, host, 2)
        store.clockStatus(true, 2)
        assertEquals(1_000_000_000, appGap(store).toNs)
    }

    @Test
    fun recoveryErrorIsRetainedInSavedGapAndCoverage() = fixture { store, host ->
        interrupt(store)
        val before = appGap(store)
        assertEquals(1_000_000, before.boundaryUncertaintyNs)
        sample(store, host, 2, readWidth = 2_000_000, roundTrip = 30_000_000)
        target(store, 2)
        store.status("device_log", StreamState.CAPTURING, null, 2)
        val gap = appGap(store)
        assertEquals(17_000_000, gap.boundaryUncertaintyNs)
        val capture = store.capture(ReplaySettings())!!
        val saved = SaveWriter().write(capture, root, { false }) { publication ->
            Files.move(publication.partial, publication.complete)
        }
        val manifest = JsonParser.parseString(Files.readString(saved.directory.resolve("session.json"))).asJsonObject
        val exported = manifest["gaps"].asJsonArray.single { it.asJsonObject["kind"].asString == "app_log" }.asJsonObject
        assertEquals(gap.toNs.toString(), exported["to_ns"].asString)
        assertEquals("17000000", exported["boundary_uncertainty_ns"].asString)
        assertFalse(exported["duration_uncertain"].asBoolean)
        assertEquals(capture.states.getValue("app_log").availableSeconds,
            manifest["coverage"].asJsonObject["app_log"].asJsonObject["availableSeconds"].asDouble)
    }

    @Test
    fun asynchronousRecoveryWaitsForResumeAndCannotChangeFrozenSave() = fixture { store, host ->
        store.freeze()
        val end = store.end()
        interrupt(store)
        val original = appGap(store)
        val pinned = store.capture(ReplaySettings())!!
        sample(store, host, 2)
        target(store, 2)
        store.status("device_log", StreamState.CAPTURING, null, 2)
        store.clockStatus(true, 2)
        assertTrue(store.frozen())
        assertEquals(end, store.end())
        assertEquals(original, appGap(store))
        store.release(pinned.id)
        val frozen = store.capture(ReplaySettings())!!
        assertEquals(end, frozen.end)
        assertEquals(original, frozen.gaps.single { it.stream == "app_log" })
        assertEquals(StreamState.RECOVERING, frozen.states.getValue("app_log").state)
        store.resume()
        assertFalse(store.frozen())
        assertEquals(1_000_000_000, appGap(store).toNs)
        assertEquals(StreamState.CAPTURING, store.streams().getValue("app_log").state)
        assertEquals(original, pinned.gaps.single { it.stream == "app_log" })
        assertEquals(original, frozen.gaps.single { it.stream == "app_log" })
        assertEquals(end, frozen.end)
    }

    @Test
    fun longSameBootRecoveryWaitsForCurrentEpochTargetAndPreservesSave() = fixture { store, host ->
        store.freeze()
        interrupt(store)
        val original = appGap(store)
        val pinned = store.capture(ReplaySettings())!!
        target(store, 2) // DeviceCapture's app poll resolves against the last old-epoch anchor.
        store.status("device_log", StreamState.CAPTURING, null, 2)
        sample(store, host, 8) // The real clock.add -> clockStatus callback starts a new epoch after >5 seconds.
        store.clockStatus(true, 2)
        store.resume() // ReplayCore's next control poll releases T, but cannot register a target period.
        assertEquals(1, store.clock.currentEpoch())
        assertNull(appGap(store).toNs)
        assertEquals(StreamState.RECOVERING, store.streams().getValue("app_log").state)
        val unassigned = row(store, host, 8)
        store.release(pinned.id)
        val waiting = store.capture(ReplaySettings())!!
        assertEquals(0, waiting.apps.last().epoch)
        assertNull(waiting.logs.single { it.id == unassigned }.app)
        assertEquals(original, waiting.gaps.single { it.stream == "app_log" })
        store.release(waiting.id)
        target(store, 2) // Only the next actual target poll registers current-epoch identity.
        assertEquals(7_000_000_000, appGap(store).toNs)
        assertEquals(StreamState.CAPTURING, store.streams().getValue("app_log").state)
        sample(store, host, 9)
        val recovered = row(store, host, 9)
        val capture = store.capture(ReplaySettings())!!
        assertEquals(1, capture.apps.last().epoch)
        assertTrue(capture.logs.single { it.id == recovered }.app == true)
        assertNull(capture.logs.single { it.id == unassigned }.app)
        val saved = SaveWriter().write(capture, root, { false }) { publication ->
            Files.move(publication.partial, publication.complete)
        }
        fun ids(file: String) = Files.readAllLines(saved.directory.resolve(file)).map {
            JsonParser.parseString(it).asJsonObject["record_id"].asString
        }
        assertEquals(capture.logs.map { it.id }, ids("logcat-device.jsonl"))
        assertEquals(capture.logs.filter { it.app == true }.map { it.id }, ids("logcat-app.jsonl"))
        assertTrue(recovered in ids("logcat-app.jsonl"))
        assertFalse(unassigned in ids("logcat-app.jsonl"))
        val manifest = JsonParser.parseString(Files.readString(saved.directory.resolve("session.json"))).asJsonObject
        val gap = manifest["gaps"].asJsonArray.single { it.asJsonObject["kind"].asString == "app_log" }.asJsonObject
        assertEquals("7000000000", gap["to_ns"].asString)
        assertEquals(capture.states.getValue("app_log").availableSeconds,
            manifest["coverage"].asJsonObject["app_log"].asJsonObject["availableSeconds"].asDouble)
        assertEquals(original, pinned.gaps.single { it.stream == "app_log" })
        assertEquals(original, waiting.gaps.single { it.stream == "app_log" })
    }

    @Test
    fun currentEpochPeriodIsRequiredForEveryRecoveryOrder() {
        val orders = listOf(
            listOf("target", "reader", "clock", "resume", "target"),
            listOf("target", "clock", "resume", "reader", "target"),
            listOf("reader", "target", "resume", "clock", "target"),
            listOf("clock", "target", "reader", "resume"),
            listOf("reader", "clock", "resume", "target"),
            listOf("resume", "clock", "target", "reader"),
        )
        orders.forEachIndexed { index, order ->
            CaptureStore(root.resolve("order-$index"), minFree = 0).use { store ->
                val host = System.nanoTime() + 1_000_000_000
                store.generation(1)
                sample(store, host, 1)
                store.status("device_log", StreamState.CAPTURING, null, 1)
                target(store, 1)
                store.freeze()
                interrupt(store)
                order.forEachIndexed { step, callback ->
                    when (callback) {
                        "target" -> target(store, 2)
                        "reader" -> store.status("device_log", StreamState.CAPTURING, null, 2)
                        "clock" -> { sample(store, host, 8); store.clockStatus(true, 2) }
                        "resume" -> store.resume()
                    }
                    if (step < order.lastIndex) {
                        assertNull(appGap(store).toNs, order.toString())
                        assertEquals(StreamState.RECOVERING, store.streams().getValue("app_log").state, order.toString())
                    }
                }
                assertEquals(7_000_000_000, appGap(store).toNs, order.toString())
                assertEquals(StreamState.CAPTURING, store.streams().getValue("app_log").state)
                assertEquals(1, store.clock.currentEpoch())
            }
        }
    }

    @Test
    fun bootChangeRequiresNewTargetAndUnknownBridgeCannotRecover() {
        for (knownBridge in listOf(true, false)) {
            CaptureStore(root.resolve("boot-$knownBridge"), minFree = 0).use { store ->
                val host = System.nanoTime() + 1_000_000_000
                store.generation(1)
                assertTrue(store.clock.add("1", listOf(1_000_000_000, 1_000_000_000,
                    1_700_000_001_000_000_000, 1_000_000_000), host, host, 100_000_000_000))
                store.status("device_log", StreamState.CAPTURING, null, 1)
                target(store, 1)
                interrupt(store)
                target(store, 2)
                store.status("device_log", StreamState.CAPTURING, null, 2)
                assertTrue(store.clock.add("2", listOf(100_000_000, 100_000_000,
                    1_700_000_008_000_000_000, 100_000_000), host + 7_000_000_000, host + 7_000_000_000,
                    if (knownBridge) 107_000_000_000 else 200_000_000_000))
                store.clockStatus(true, 2)
                assertNull(appGap(store).toNs)
                store.app("com.example.app", null, emptySet(), 2)
                target(store, 1) // A stopped owner's positive result cannot replace unresolved current target.
                store.clockStatus(true, 2)
                assertNull(appGap(store).toNs)
                assertEquals(StreamState.UNAVAILABLE, store.streams().getValue("app_log").state)
                target(store, 2)
                if (knownBridge) {
                    assertEquals(7_000_000_000, appGap(store).toNs)
                    assertEquals(StreamState.CAPTURING, store.streams().getValue("app_log").state)
                } else {
                    assertNull(appGap(store).toNs)
                    assertEquals(StreamState.RECOVERING, store.streams().getValue("app_log").state)
                    assertEquals(Long.MAX_VALUE, store.clock.endUncertainty())
                }
            }
        }
    }

    private fun row(store: CaptureStore, host: Long, second: Long): String = store.log(DeviceLog(
        1_700_000_000_000_000_000 + second * 1_000_000_000, 12, 12, 10001, 0, 4,
        "Fixture", "row-$second", byteArrayOf(1)), 2, host + (second - 1) * 1_000_000_000)!!

    private fun fixture(test: (CaptureStore, Long) -> Unit) {
        CaptureStore(root.resolve("ring"), minFree = 0).use { store ->
            val host = System.nanoTime() + 1_000_000_000
            store.generation(1)
            sample(store, host, 1)
            store.status("device_log", StreamState.CAPTURING, null, 1)
            target(store, 1)
            store.log(DeviceLog(1_700_000_001_000_000_000, 12, 12, 10001, 0, 4,
                "Fixture", "before", byteArrayOf(1)), 1, host)
            test(store, host)
        }
    }

    private fun interrupt(store: CaptureStore) {
        store.status("device_log", StreamState.RECOVERING, "connection interrupted", 1)
        store.status("app_log", StreamState.RECOVERING, "connection interrupted", 1)
        store.generation(2)
    }

    private fun target(store: CaptureStore, owner: Long) = store.app("com.example.app", 10001, setOf(12), owner, true)
    private fun appGap(store: CaptureStore) = store.streams().getValue("app_log").gaps.single { it.stream == "app_log" }

    private fun sample(store: CaptureStore, host: Long, second: Long, readWidth: Long = 0, roundTrip: Long = 0) {
        val elapsed = second * 1_000_000_000
        val received = host + (second - 1) * 1_000_000_000
        assertTrue(store.clock.add("1", listOf(elapsed - readWidth / 2, elapsed,
            1_700_000_000_000_000_000 + elapsed, elapsed + readWidth / 2), received - roundTrip, received))
    }
}
