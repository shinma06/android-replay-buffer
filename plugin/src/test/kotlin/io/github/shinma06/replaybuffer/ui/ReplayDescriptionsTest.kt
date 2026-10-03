package io.github.shinma06.replaybuffer.ui

import io.github.shinma06.replaybuffer.core.ApplicationMode
import io.github.shinma06.replaybuffer.core.ApplicationPeriodSnapshot
import io.github.shinma06.replaybuffer.core.ApplicationTarget
import io.github.shinma06.replaybuffer.core.CaptureGap
import io.github.shinma06.replaybuffer.core.CaptureState
import io.github.shinma06.replaybuffer.core.CaptureStore
import io.github.shinma06.replaybuffer.core.DeviceKind
import io.github.shinma06.replaybuffer.core.DeviceLog
import io.github.shinma06.replaybuffer.core.ReplayDevice
import io.github.shinma06.replaybuffer.core.ReplaySettings
import io.github.shinma06.replaybuffer.core.ReplaySnapshot
import io.github.shinma06.replaybuffer.core.SavePhase
import io.github.shinma06.replaybuffer.core.SaveSnapshot
import io.github.shinma06.replaybuffer.core.StreamSnapshot
import io.github.shinma06.replaybuffer.core.StreamState
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Path

/** Uses the fixed core API; no IDE, Swing panel, adb, or device is started. */
class ReplayDescriptionsTest {
    @TempDir lateinit var directory: Path

    private fun snapshot(streams: Map<String, StreamSnapshot>): ReplaySnapshot = ReplaySnapshot(
        revision = 1, generation = 1, enabled = true, captureState = CaptureState.CAPTURING,
        settings = ReplaySettings(replaySeconds = 1), settingsRevision = 1,
        video = streams.getValue("video"), deviceLog = streams.getValue("device_log"), appLog = streams.getValue("app_log"),
    )

    @Test
    fun `real recovered gaps show reasons and ranges until the core window expires them`() {
        CaptureStore(directory.resolve("ring"), minFree = 0).use { store ->
            val host = System.nanoTime()
            fun anchor(elapsed: Long, time: Long) {
                store.clock.add("1", listOf(elapsed, elapsed, 1_700_000_000_000_000_000 + elapsed, elapsed), time, time)
            }
            store.generation(1)
            anchor(1_000_000_000, host)
            store.app("com.fixture.app", 10001, setOf(12), 1, true)
            store.log(DeviceLog(1_700_000_001_000_000_000, 12, 12, 10001, 0, 4, "Fixture", "synthetic", byteArrayOf(1)), 1, host)
            store.status("device_log", StreamState.RECOVERING, "ログの取得が中断しました", 1)
            store.clockStatus(false, 1)
            anchor(1_100_000_000, host + 100_000_000)
            store.status("device_log", StreamState.CAPTURING, null, 1)
            store.clockStatus(true, 1)
            val recovered = snapshot(store.streams(1, 100_000_000))
            assertEquals(StreamState.CAPTURING, recovered.deviceLog.state)
            val status = streamDescription("端末ログ", recovered.deviceLog)
            assertTrue(status.contains("取得中"))
            assertTrue(status.contains("現在窓に欠落2件"))
            val detail = currentGapDescription(recovered)
            assertTrue(detail.contains("ログの取得が中断しました"))
            val logGap = recovered.deviceLog.gaps.first { it.stream == "device_log" }
            assertTrue(detail.contains("端末ログ: ${recordTime(logGap.fromNs)}〜${recordTime(logGap.toNs)}"))
            assertEquals(1, detail.lines().count { it.startsWith("時計:") })
            // The core owns expiration; the UI renders the supplied immutable window verbatim.
            anchor(6_000_000_000, host + 5_000_000_000)
            val expired = snapshot(store.streams(1, 5_000_000_000))
            assertEquals("", currentGapDescription(expired))
            assertFalse(streamDescription("端末ログ", expired.deviceLog).contains("欠落"))
            assertTrue(currentGapDescription(recovered).contains("ログの取得が中断しました"))
        }
    }

    @Test
    fun `uncertain gap boundaries stay explicit and healthy zero logs are not missing`() {
        val healthy = StreamSnapshot(StreamState.CAPTURING, 0.0)
        assertEquals("端末ログ: 取得中 / 0.0秒分", streamDescription("端末ログ", healthy))
        val gap = CaptureGap("clock", null, null, "時計対応を確認できません", Long.MAX_VALUE)
        val known = CaptureGap("video", 9_000_000_000, 10_000_000_000, "動画の取得が中断しました", 20_000_000)
        val uncertain = healthy.copy(gaps = listOf(gap, known))
        val detail = currentGapDescription(snapshot(mapOf("video" to uncertain, "device_log" to healthy, "app_log" to healthy)))
        assertTrue(detail.contains("未確定〜未確定"))
        assertTrue(detail.contains("境界の誤差は未確定"))
        assertFalse(detail.contains("9223372036"))
        assertTrue(detail.contains("動画: 9.000秒〜10.000秒"))
        assertTrue(detail.contains("境界の誤差±0.020秒"))
        assertEquals("", currentGapDescription(snapshot(mapOf("video" to healthy, "device_log" to healthy, "app_log" to healthy))))
    }

    @Test
    fun `pending remote cleanup is separate from capture and gap status`() {
        assertEquals("", cleanupDescription(0))
        assertTrue(cleanupDescription(2).contains("片付け待ち: 2件"))
        assertTrue(cleanupDescription(2).contains("対象端末を接続"))
        assertFalse(cleanupDescription(2).contains("欠落"))
        assertFalse(cleanupDescription(2).contains("取得中"))
    }

    @Test
    fun `failed target description uses only fixed device and application history through retry`() {
        val fixed = SaveSnapshot(
            phase = SavePhase.FAILED, requestId = "request-old", directory = directory.resolve("old"),
            device = ReplayDevice("fixture-old", "旧端末", DeviceKind.EMULATOR, true),
            application = ApplicationTarget("com.fixture.old", ApplicationMode.MANUAL),
            applicationHistory = java.util.List.copyOf(listOf(
                ApplicationPeriodSnapshot("com.fixture.first", 0, 100_000_000, 0, true),
                ApplicationPeriodSnapshot("com.fixture.old", 100_000_000, 200_000_000, 0, true),
                ApplicationPeriodSnapshot(null, null, null, 1, false),
            )),
        )
        val expected = frozenTargetDescription(fixed)
        val current = snapshot(mapOf("video" to StreamSnapshot(), "device_log" to StreamSnapshot(), "app_log" to StreamSnapshot())).copy(
            settings = ReplaySettings(application = ApplicationTarget("com.fixture.new")),
            device = ReplayDevice("fixture-new", "新端末", DeviceKind.PHYSICAL, true), save = fixed,
        )
        assertEquals(expected, frozenTargetDescription(current.save))
        assertEquals(expected, frozenTargetDescription(fixed.copy(directory = directory.resolve("retry"), phase = SavePhase.WRITING)))
        assertTrue(expected.contains("旧端末（Emulator・fixture-old）"))
        assertTrue(expected.contains("com.fixture.old（手動）"))
        assertTrue(expected.contains("com.fixture.first: 0.000秒〜0.100秒"))
        assertTrue(expected.contains("対象未確定: 未確定〜未確定（帰属未確定）"))
        assertFalse(expected.contains("com.fixture.new"))
        assertFalse(expected.contains("新端末"))
    }
}
