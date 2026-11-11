package io.github.shinma06.replaybuffer.core

import java.time.ZoneOffset
import java.time.ZonedDateTime
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotEquals
import kotlin.test.assertTrue

class SaveLayoutTest {
    private val at = ZonedDateTime.of(2026, 11, 11, 14, 3, 12, 345_000_000, ZoneOffset.ofHours(9))

    @Test fun `folder names carry local time, trigger, logs-only and an id`() {
        assertEquals("replay-20261111-140312-manual-a1b2c3d4", replayFolderName(at, crash = false, logsOnly = false, id = "a1b2c3d4"))
        assertEquals("replay-20261111-140312-crash-a1b2c3d4", replayFolderName(at, crash = true, logsOnly = false, id = "a1b2c3d4"))
        assertEquals("replay-20261111-140312-manual-logs-a1b2c3d4", replayFolderName(at, crash = false, logsOnly = true, id = "a1b2c3d4"))
        assertEquals("replay-20261111-140312-crash-logs-a1b2c3d4", replayFolderName(at, crash = true, logsOnly = true, id = "a1b2c3d4"))
        assertEquals("screenshot-20261111-140312-a1b2c3d4.png", screenshotFileName(at, "a1b2c3d4"))
    }

    @Test fun `generated names are short, portable and unique per attempt`() {
        val names = List(64) { replayFolderName(at, crash = true, logsOnly = true) }
        assertTrue(names.all { Regex("replay-[0-9]{8}-[0-9]{6}-(manual|crash)(-logs)?-[0-9a-f]{8}").matches(it) && it.length <= 43 })
        assertEquals(names.size, names.toSet().size)
        assertNotEquals(screenshotFileName(at), screenshotFileName(at))
    }
}
