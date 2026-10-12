package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonParser
import java.nio.file.Files
import java.nio.file.Path
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNull
import kotlin.test.assertTrue

class ApplicationHistoryBoundaryTest {
    @TempDir lateinit var root: Path

    @Test
    fun appBeforeClockCannotCloseExtrapolatedStartByEarlierSample() = historyBoundary(false)

    @Test
    fun clockBeforeAppCannotCloseExtrapolatedStartByEarlierSample() = historyBoundary(true)

    private fun historyBoundary(clockFirst: Boolean) {
        val output = Files.createDirectory(root.resolve("order-$clockFirst"))
        CaptureStore(output.resolve("ring"), minFree = 0).use { store ->
            val host = System.nanoTime() - 2_000_000_000
            fun sample(second: Long) {
                val elapsed = second * 1_000_000_000
                val received = when {
                    second == 4L -> System.nanoTime() - 2_000_000_000
                    second >= 11 -> host + (second - 5) * 1_000_000_000
                    else -> host
                }
                assertTrue(store.clock.add("same-boot", listOf(elapsed, elapsed, WALL + elapsed, elapsed), received, received))
            }
            fun target(uid: Long?) = store.app("com.fixture.app", uid, if (uid == null) emptySet() else setOf(12), 1, uid != null)
            fun row(second: Long, uid: Long) = store.log(DeviceLog(WALL + second * 1_000_000_000,
                12, 12, uid, 0, 4, "Fixture", "row-$second-$uid", byteArrayOf(1)), 1,
                host + (second - 1) * 1_000_000_000)!!
            store.generation(1)
            target(null) // No clock: keep unknown start rather than inventing one.
            sample(1)
            target(10001)
            sample(4)
            val admitted = row(4, 10001)
            target(null)
            if (clockFirst) sample(11) else target(10001)
            if (clockFirst) target(10001) else sample(11)
            target(10001)
            sample(12)
            val recovered = row(12, 10001)
            val foreign = row(12, 10002)
            val capture = store.capture(ReplaySettings())!!
            val old = capture.apps.last { it.epoch == 0 }
            assertTrue(old.from!! > capture.clocks.last { it.epoch == 0 }.elapsed)
            assertNull(old.to, "Old sample is not a truthful closure after the extrapolated start")
            assertTrue(old.endUnknown)
            assertTrue(capture.apps.all { it.from == null || it.to == null || it.to >= it.from })
            assertEquals(listOf(admitted, recovered, foreign), capture.logs.map { it.id })
            assertEquals(listOf(true, true, false), capture.logs.map { it.app })
            val history = capture.applicationHistory()
            assertTrue(history.all { it.fromNs == null || it.toNs == null || it.toNs >= it.fromNs })
            val clocks = capture.clocks
            val apps = capture.apps
            assertFailsWith<SaveFailure> {
                SaveWriter().write(capture, output, { false }) { error("publication failed") }
            }
            sample(13)
            store.app("com.fixture.other", 10002, setOf(20), 1, true)
            assertEquals(apps, capture.apps)
            assertEquals(clocks, capture.clocks)
            assertEquals(history, capture.applicationHistory())
            val saved = SaveWriter().write(capture, output, { false }) { Files.move(it.partial, it.complete) }
            val manifest = JsonParser.parseString(Files.readString(saved.directory.resolve("session.json"))).asJsonObject
            val exported = manifest["application_history"].asJsonArray
            assertEquals(apps.size, exported.size())
            assertTrue(exported[apps.indexOf(old)].asJsonObject["to_elapsed_ns"].isJsonNull)
            assertTrue(exported[apps.indexOf(old)].asJsonObject["to_elapsed_unknown"].asBoolean)
            fun ids(file: String) = Files.readAllLines(saved.directory.resolve(file)).map {
                JsonParser.parseString(it).asJsonObject["record_id"].asString
            }
            assertEquals(capture.logs.map { it.id }, ids("logcat-device.jsonl"))
            assertEquals(listOf(admitted, recovered), ids("logcat-app.jsonl"))
            store.release(capture.id)
        }
    }

    companion object {
        private const val WALL = 1_700_000_000_000_000_000L
    }
}
