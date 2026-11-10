package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonParser
import java.nio.file.Files
import java.nio.file.Path
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

class TouchCaptureTest {
    @TempDir lateinit var root: Path

    @Test
    fun touchTimeComesFromTheDeviceMonotonicClockAndIsFrozenWithTheSave() {
        CaptureStore(root.resolve("ring"), minFree = 0).use { store ->
            val host = System.nanoTime() + 1_000_000_000
            store.generation(1)
            sample(store, host, 1)
            store.status("device_log", StreamState.CAPTURING, null, 1)
            store.app("com.fixture.app", 10001, setOf(12), 1, true)
            store.touchStatus(StreamState.CAPTURING, null, 1)
            sample(store, host, 2)
            // Host arrival is 0.7 s after the kernel time; only the kernel time may place the touch.
            store.touch(TouchSample(2_000_000_000, "/dev/input/event2", 0, TouchAction.DOWN, 10, 20), 1080, 2400, 1, host + 1_700_000_000)
            // Before the window start by source time even though it arrived inside the window.
            store.touch(TouchSample(1_500_000_000, "/dev/input/event2", 1, TouchAction.DOWN, 5, 5), 1080, 2400, 1, host + 1_400_000_000)
            store.touch(TouchSample(2_100_000_000, "/dev/input/event2", 0, TouchAction.UP, 10, 20), 1080, 2400, 2, host + 1_200_000_000)
            // A touch alone does not make a save; the window also needs video or log rows.
            store.log(DeviceLog(1_700_000_001_800_000_000, 12, 12, 10001, 0, 4, "Fixture", "row", byteArrayOf(1)), 1, host + 1_000_000_000)
            val pinned = assertNotNull(store.capture(ReplaySettings(showTouches = true), null, host + 1_800_000_000))
            val row = pinned.touches.single()
            assertEquals(2_000_000_000L, row.time.elapsed)
            for (second in 3L..6L) sample(store, host, second)
            store.prune(1)
            assertEquals(1, pinned.touches.size)
            val output = SaveWriter().write(pinned, root, { false }) { Files.move(it.partial, it.complete) }
            val lines = Files.readAllLines(output.directory.resolve("touches.jsonl"))
            val touch = JsonParser.parseString(lines.single()).asJsonObject
            assertEquals("2000000000", touch["device_mono_ns"].asString)
            assertEquals((row.time.sequence!! - pinned.start).toString(), touch["window_ns"].asString)
            assertEquals("down", touch["action"].asString)
            assertEquals(10, touch["x"].asInt)
            assertEquals(2400, touch["display_height"].asInt)
            assertTrue(touch["movie_us"].isJsonNull) // No presented frame covers it.
            val json = JsonParser.parseString(Files.readString(output.directory.resolve("session.json"))).asJsonObject
            assertEquals("touches.jsonl", json["touch_file"].asString)
            val touches = json["touches"].asJsonObject
            assertEquals(1, touches["count"].asInt)
            assertTrue(touches["show_touches"].asBoolean)
            assertEquals("CAPTURING", touches["state"].asString)
            assertTrue(Files.readString(output.directory.resolve("README.txt")).contains("touches.jsonlに1件"))
            store.release(pinned.id)
        }
    }

    @Test
    fun logOnlyCaptureKeepsNoTouches() {
        CaptureStore(root.resolve("ring"), minFree = 0, captureMode = CaptureMode.LOG_ONLY).use { store ->
            val host = System.nanoTime() + 1_000_000_000
            store.generation(1)
            sample(store, host, 1)
            store.touchStatus(StreamState.CAPTURING, null, 1)
            store.touch(TouchSample(1_500_000_000, "/dev/input/event2", 0, TouchAction.DOWN, 10, 20), 1080, 2400, 1, host)
            store.status("device_log", StreamState.CAPTURING, null, 1)
            store.app("com.fixture.app", 10001, setOf(12), 1, true)
            sample(store, host, 2)
            val pinned = assertNotNull(store.capture(ReplaySettings(captureMode = CaptureMode.LOG_ONLY), null, host + 1_500_000_000))
            assertTrue(pinned.touches.isEmpty())
            assertNull(pinned.touchState)
            val output = SaveWriter().write(pinned, root, { false }) { Files.move(it.partial, it.complete) }
            assertTrue(Files.notExists(output.directory.resolve("touches.jsonl")))
            store.release(pinned.id)
        }
    }

    @Test
    fun movieTimeIsInsideThePresentedFrameOnly() {
        val anchors = listOf(MovieAnchor(0, 1_000_000_000, 33_333, 0), MovieAnchor(0, 1_033_333_000, 33_333, 33_333))
        assertNull(anchors.movieTime(999_999_999))
        assertEquals(10_000L, anchors.movieTime(1_010_000_000))
        assertEquals(43_333L, anchors.movieTime(1_043_333_000))
        assertNull(anchors.movieTime(1_200_000_000))
    }

    @Test
    fun rowsEvictedByTheSampleLimitInsideTheWindowAreReportedAsAGap() {
        CaptureStore(root.resolve("ring"), minFree = 0).use { store ->
            val host = System.nanoTime() + 1_000_000_000
            store.generation(1)
            sample(store, host, 1)
            store.status("device_log", StreamState.CAPTURING, null, 1)
            store.app("com.fixture.app", 10001, setOf(12), 1, true)
            store.touchStatus(StreamState.CAPTURING, null, 1)
            sample(store, host, 2)
            store.touchDots(TouchDots.FAILED, 1)
            repeat(MAX_TOUCH_SAMPLES + 1) {
                store.touch(TouchSample(2_000_000_000L + it, "/dev/input/event2", 0, TouchAction.MOVE, 1, 1), 1080, 2400, 1, host + 1_700_000_000)
            }
            store.log(DeviceLog(1_700_000_001_800_000_000, 12, 12, 10001, 0, 4, "Fixture", "row", byteArrayOf(1)), 1, host + 1_000_000_000)
            val pinned = assertNotNull(store.capture(ReplaySettings(showTouches = true), null, host + 1_800_000_000))
            assertEquals(MAX_TOUCH_SAMPLES, pinned.touches.size)
            assertEquals(1L, pinned.touchDropped)
            assertTrue(pinned.touchGaps.any { it.reason == "保持上限を超えた古いタップ位置を破棄しました" && it.from == null && it.to != null })
            val output = SaveWriter().write(pinned, root, { false }) { Files.move(it.partial, it.complete) }
            val touches = JsonParser.parseString(Files.readString(output.directory.resolve("session.json"))).asJsonObject["touches"].asJsonObject
            assertEquals("failed", touches["show_touches_state"].asString)
            assertTrue(touches["show_touches"].asBoolean)
        }
    }

    @Test
    fun inputDroppedByTheKernelIsSavedAsAGapAtTheSourceTime() {
        CaptureStore(root.resolve("ring"), minFree = 0).use { store ->
            val host = System.nanoTime() + 1_000_000_000
            store.generation(1)
            sample(store, host, 1)
            store.status("device_log", StreamState.CAPTURING, null, 1)
            store.app("com.fixture.app", 10001, setOf(12), 1, true)
            store.touchStatus(StreamState.CAPTURING, null, 1)
            sample(store, host, 2)
            val a = "/dev/input/event2"
            val b = "/dev/input/event3"
            store.touchLost(a, 2_000_000_000, 1, host + 1_700_000_000)
            store.touchLost(a, 2_010_000_000, 1, host + 1_700_000_000) // Already open.
            store.touchResynced(a, 2_050_000_000, 1, host + 1_700_000_000)
            // A start before every clock sample has no supported mapping and stays unknown.
            store.touchLost(b, -10_000_000_000, 1, host + 1_700_000_000)
            store.touchLost(a, 2_070_000_000, 2, host + 1_700_000_000) // Another generation.
            store.log(DeviceLog(1_700_000_001_800_000_000, 12, 12, 10001, 0, 4, "Fixture", "row", byteArrayOf(1)), 1, host + 1_000_000_000)
            val pinned = assertNotNull(store.capture(ReplaySettings(), null, host + 1_800_000_000))
            val lost = pinned.touchGaps.filter { it.reason.contains("SYN_DROPPED") }
            assertEquals(2, lost.size)
            val from = assertNotNull(store.clock.mono(2_000_000_000, host + 1_700_000_000).sequence)
            val to = assertNotNull(store.clock.mono(2_050_000_000, host + 1_700_000_000).sequence)
            assertTrue(lost[0].from!! <= from && lost[0].to!! >= to && lost[0].to!! - lost[0].from!! < 1_000_000_000)
            assertNull(lost[1].from)
            assertNull(lost[1].to) // Not resynced yet: input of that device is still unknown.
            val output = SaveWriter().write(pinned, root, { false }) { Files.move(it.partial, it.complete) }
            val gaps = JsonParser.parseString(Files.readString(output.directory.resolve("session.json"))).asJsonObject["touches"]
                .asJsonObject["gaps"].asJsonArray
            assertTrue(gaps.any { it.toString().contains("SYN_DROPPED") })
            store.release(pinned.id)
            store.touchStatus(StreamState.RECOVERING, "切断", 1)
            assertNotNull(store.capture(ReplaySettings(), null, host + 1_800_000_000)).let { later ->
                assertNotNull(later.touchGaps.single { it.reason.contains("SYN_DROPPED") && it.from == null }.to)
                store.release(later.id)
            }
        }
    }

    private fun sample(store: CaptureStore, host: Long, second: Long) {
        val elapsed = second * 1_000_000_000
        val received = host + (second - 1) * 1_000_000_000
        assertTrue(store.clock.add("1", listOf(elapsed, elapsed, 1_700_000_000_000_000_000 + elapsed, elapsed), received, received))
        store.clockStatus(true, 1)
    }
}
