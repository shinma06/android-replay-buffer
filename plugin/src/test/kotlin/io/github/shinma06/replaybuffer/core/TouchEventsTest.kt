package io.github.shinma06.replaybuffer.core

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

class TouchEventsTest {
    private val devices = """
        add device 1: /dev/input/event1
          name:     "AT Translated Set 2 keyboard"
        add device 3: /dev/input/event2
          name:     "virtio_input_multi_touch_1"
          events:
            ABS (0003): ABS_MT_SLOT           : value 0, min 0, max 9, fuzz 0, flat 0, resolution 0
                        ABS_MT_POSITION_X     : value 0, min 0, max 32767, fuzz 0, flat 0, resolution 0
                        ABS_MT_POSITION_Y     : value 0, min 0, max 32767, fuzz 0, flat 0, resolution 0
    """.trimIndent()

    @Test
    fun `only devices with both multi-touch axes are touchscreens`() {
        assertEquals(mapOf("/dev/input/event2" to TouchAxes(32767, 32767)), parseTouchDevices(devices))
    }

    @Test
    fun `protocol A devices without slots are not parsed`() {
        val protocolA = """
            add device 4: /dev/input/event5
              name:     "legacy_touch"
              events:
                ABS (0003): ABS_MT_POSITION_X     : value 0, min 0, max 4095, fuzz 0, flat 0, resolution 0
                            ABS_MT_POSITION_Y     : value 0, min 0, max 4095, fuzz 0, flat 0, resolution 0
        """.trimIndent()
        assertEquals(emptyMap<String, TouchAxes>(), parseTouchDevices(protocolA))
        assertEquals(mapOf("/dev/input/event2" to TouchAxes(32767, 32767)), parseTouchDevices(protocolA + "\n" + devices))
    }

    @Test
    fun `override display size wins`() {
        assertEquals(1080 to 2400, parseDisplaySize("Physical size: 1080x2400\n"))
        assertEquals(720 to 1600, parseDisplaySize("Physical size: 1080x2400\nOverride size: 720x1600\n"))
    }

    @Test
    fun `a tap becomes down and up in display pixels at the kernel time`() {
        val parser = TouchParser(parseTouchDevices(devices), 1080, 2400)
        val lines = """
            [     432.981039] /dev/input/event2: EV_ABS       ABS_MT_TRACKING_ID   00000000
            [     432.981039] /dev/input/event2: EV_ABS       ABS_MT_POSITION_X    00003fff
            [     432.981039] /dev/input/event2: EV_ABS       ABS_MT_POSITION_Y    00003fe4
            [     432.981039] /dev/input/event2: EV_SYN       SYN_REPORT           00000000
            [     432.990000] /dev/input/event1: EV_KEY       KEY_A                00000001
            [     432.981055] /dev/input/event2: EV_ABS       ABS_MT_TRACKING_ID   ffffffff
            [     432.981055] /dev/input/event2: EV_SYN       SYN_REPORT           00000000
        """.trimIndent().lines()
        val samples = lines.flatMap(parser::feed)
        assertEquals(listOf(
            TouchSample(432_981_039_000, "/dev/input/event2", 0, TouchAction.DOWN, 539, 1197),
            TouchSample(432_981_055_000, "/dev/input/event2", 0, TouchAction.UP, 539, 1197),
        ), samples)
    }

    @Test
    fun `single device output without prefix and a second pointer are folded per slot`() {
        val parser = TouchParser(parseTouchDevices(devices), 1080, 2400)
        val lines = """
            [     10.000000] EV_ABS       ABS_MT_SLOT          00000000
            [     10.000000] EV_ABS       ABS_MT_TRACKING_ID   00000005
            [     10.000000] EV_ABS       ABS_MT_POSITION_X    00000000
            [     10.000000] EV_ABS       ABS_MT_POSITION_Y    00000000
            [     10.000000] EV_SYN       SYN_REPORT           00000000
            [     10.016000] EV_ABS       ABS_MT_SLOT          00000001
            [     10.016000] EV_ABS       ABS_MT_TRACKING_ID   00000006
            [     10.016000] EV_ABS       ABS_MT_POSITION_X    00007fff
            [     10.016000] EV_ABS       ABS_MT_POSITION_Y    00007fff
            [     10.016000] EV_ABS       ABS_MT_SLOT          00000000
            [     10.016000] EV_ABS       ABS_MT_POSITION_X    00000100
            [     10.016000] EV_SYN       SYN_REPORT           00000000
        """.trimIndent().lines()
        assertEquals(listOf(
            TouchSample(10_000_000_000, "/dev/input/event2", 0, TouchAction.DOWN, 0, 0),
            TouchSample(10_016_000_000, "/dev/input/event2", 0, TouchAction.MOVE, 8, 0),
            TouchSample(10_016_000_000, "/dev/input/event2", 1, TouchAction.DOWN, 1079, 2399),
        ), lines.flatMap(parser::feed))
    }

    private class Sync(private val now: TouchCurrent? = null) : TouchSync {
        val events = mutableListOf<String>()
        override fun lost(device: String, fromNs: Long) { events += "lost $device $fromNs" }
        override fun current(device: String): TouchCurrent? = now.also { events += "current $device" }
        override fun resynced(device: String, toNs: Long) { events += "resynced $device $toNs" }
    }

    @Test
    fun `SYN_DROPPED ends pointers and a later tap with the same X still produces samples`() {
        val sync = Sync()
        val parser = TouchParser(parseTouchDevices(devices), 1080, 2400, sync)
        val lines = """
            [     20.000000] /dev/input/event2: EV_ABS       ABS_MT_SLOT          00000000
            [     20.000000] /dev/input/event2: EV_ABS       ABS_MT_TRACKING_ID   00000001
            [     20.000000] /dev/input/event2: EV_ABS       ABS_MT_POSITION_X    00007fff
            [     20.000000] /dev/input/event2: EV_ABS       ABS_MT_POSITION_Y    00000000
            [     20.000000] /dev/input/event2: EV_SYN       SYN_REPORT           00000000
            [     20.100000] /dev/input/event2: EV_SYN       SYN_DROPPED          00000000
            [     20.110000] /dev/input/event2: EV_ABS       ABS_MT_TRACKING_ID   ffffffff
            [     20.120000] /dev/input/event2: EV_SYN       SYN_REPORT           00000000
            [     20.130000] /dev/input/event2: EV_ABS       ABS_MT_POSITION_Y    00000100
            [     20.130000] /dev/input/event2: EV_SYN       SYN_REPORT           00000000
            [     20.140000] /dev/input/event2: EV_ABS       ABS_MT_TRACKING_ID   00000003
            [     20.140000] /dev/input/event2: EV_ABS       ABS_MT_POSITION_Y    00007fff
            [     20.140000] /dev/input/event2: EV_SYN       SYN_REPORT           00000000
            [     20.150000] /dev/input/event2: EV_ABS       ABS_MT_TRACKING_ID   ffffffff
            [     20.150000] /dev/input/event2: EV_SYN       SYN_REPORT           00000000
        """.trimIndent().lines()
        // The movement at 20.13 belongs to a pointer of unknown identity and is not guessed; the tap at 20.14 sends no
        // ABS_MT_SLOT or X because neither changed, and still uses the X kept from before the drop.
        assertEquals(listOf(
            TouchSample(20_000_000_000, "/dev/input/event2", 0, TouchAction.DOWN, 1079, 0),
            TouchSample(20_100_000_000, "/dev/input/event2", 0, TouchAction.UP, 1079, 0),
            TouchSample(20_140_000_000, "/dev/input/event2", 0, TouchAction.DOWN, 1079, 2399),
            TouchSample(20_150_000_000, "/dev/input/event2", 0, TouchAction.UP, 1079, 2399),
        ), lines.flatMap(parser::feed))
        assertEquals(listOf("current /dev/input/event2", "lost /dev/input/event2 20100000000",
            "resynced /dev/input/event2 20140000000"), sync.events)
    }

    @Test
    fun `releasing a finger held across a drop does not end the lost interval`() {
        val sync = Sync()
        val parser = TouchParser(parseTouchDevices(devices), 1080, 2400, sync)
        val lines = """
            [     70.000000] EV_ABS       ABS_MT_TRACKING_ID   00000001
            [     70.000000] EV_ABS       ABS_MT_POSITION_X    00000000
            [     70.000000] EV_ABS       ABS_MT_POSITION_Y    00000000
            [     70.000000] EV_SYN       SYN_REPORT           00000000
            [     70.100000] EV_SYN       SYN_DROPPED          00000000
            [     70.110000] EV_SYN       SYN_REPORT           00000000
            [     70.200000] EV_ABS       ABS_MT_TRACKING_ID   ffffffff
            [     70.200000] EV_SYN       SYN_REPORT           00000000
            [     70.300000] EV_ABS       ABS_MT_TRACKING_ID   00000002
            [     70.300000] EV_ABS       ABS_MT_POSITION_X    00007fff
            [     70.300000] EV_SYN       SYN_REPORT           00000000
        """.trimIndent().lines()
        assertEquals(listOf(
            TouchSample(70_000_000_000, "/dev/input/event2", 0, TouchAction.DOWN, 0, 0),
            TouchSample(70_100_000_000, "/dev/input/event2", 0, TouchAction.UP, 0, 0),
            TouchSample(70_300_000_000, "/dev/input/event2", 0, TouchAction.DOWN, 1079, 0),
        ), lines.flatMap(parser::feed))
        assertEquals(listOf("current /dev/input/event2", "lost /dev/input/event2 70100000000",
            "resynced /dev/input/event2 70300000000"), sync.events)
    }

    @Test
    fun `the slot and position re-read after a drop place the next pointer`() {
        val sync = Sync(TouchCurrent(slot = 1, x = 0, y = 0x7fff))
        val parser = TouchParser(parseTouchDevices(devices), 1080, 2400, sync)
        val lines = """
            [     40.000000] EV_ABS       ABS_MT_SLOT          00000000
            [     40.000000] EV_ABS       ABS_MT_TRACKING_ID   00000001
            [     40.000000] EV_ABS       ABS_MT_POSITION_X    00007fff
            [     40.000000] EV_ABS       ABS_MT_POSITION_Y    00000000
            [     40.000000] EV_SYN       SYN_REPORT           00000000
            [     40.100000] EV_SYN       SYN_DROPPED          00000000
            [     40.100000] EV_SYN       SYN_DROPPED          00000000
            [     40.110000] EV_SYN       SYN_REPORT           00000000
            [     40.120000] EV_ABS       ABS_MT_TRACKING_ID   00000007
            [     40.120000] EV_SYN       SYN_REPORT           00000000
        """.trimIndent().lines()
        assertEquals(listOf(
            TouchSample(40_000_000_000, "/dev/input/event2", 0, TouchAction.DOWN, 1079, 0),
            TouchSample(40_100_000_000, "/dev/input/event2", 0, TouchAction.UP, 1079, 0),
            TouchSample(40_120_000_000, "/dev/input/event2", 1, TouchAction.DOWN, 0, 2399),
        ), lines.flatMap(parser::feed))
        // A second drop inside the discard period does not open another gap.
        assertEquals(1, sync.events.count { it.startsWith("lost") })
        assertEquals("resynced /dev/input/event2 40120000000", sync.events.last())
    }

    @Test
    fun `a first tap that omits an axis uses the X and Y read at reader start`() {
        val parser = TouchParser(parseTouchDevices(devices), 1080, 2400,
            initial = mapOf("/dev/input/event2" to TouchCurrent(slot = 0, x = 0x7fff, y = 0)))
        val lines = """
            [     50.000000] EV_ABS       ABS_MT_TRACKING_ID   00000009
            [     50.000000] EV_ABS       ABS_MT_POSITION_Y    00007fff
            [     50.000000] EV_SYN       SYN_REPORT           00000000
            [     50.050000] EV_ABS       ABS_MT_TRACKING_ID   ffffffff
            [     50.050000] EV_SYN       SYN_REPORT           00000000
        """.trimIndent().lines()
        assertEquals(listOf(
            TouchSample(50_000_000_000, "/dev/input/event2", 0, TouchAction.DOWN, 1079, 2399),
            TouchSample(50_050_000_000, "/dev/input/event2", 0, TouchAction.UP, 1079, 2399),
        ), lines.flatMap(parser::feed))
    }

    @Test
    fun `a first tap that omits the slot event uses the slot read at reader start`() {
        val parser = TouchParser(parseTouchDevices(devices), 1080, 2400,
            initial = parseTouchCurrent(devices.replace("ABS_MT_SLOT           : value 0", "ABS_MT_SLOT           : value 2")))
        val lines = """
            [     60.000000] EV_ABS       ABS_MT_TRACKING_ID   0000000a
            [     60.000000] EV_ABS       ABS_MT_POSITION_X    00007fff
            [     60.000000] EV_SYN       SYN_REPORT           00000000
        """.trimIndent().lines()
        // Y was 0 in getevent -lp and is not resent.
        assertEquals(listOf(TouchSample(60_000_000_000, "/dev/input/event2", 2, TouchAction.DOWN, 1079, 0)), lines.flatMap(parser::feed))
    }

    @Test
    fun `current slot and position are read from getevent -lp values`() {
        val output = devices.replace("ABS_MT_SLOT           : value 0", "ABS_MT_SLOT           : value 3")
            .replace("ABS_MT_POSITION_X     : value 0", "ABS_MT_POSITION_X     : value 120")
        assertEquals(TouchCurrent(3, 120, 0), parseTouchCurrent(output)["/dev/input/event2"])
    }

    @Test
    fun `a resize scales later reports with the new display size`() {
        val parser = TouchParser(parseTouchDevices(devices), 1080, 2400)
        val down = """
            [     30.000000] EV_ABS       ABS_MT_TRACKING_ID   00000001
            [     30.000000] EV_ABS       ABS_MT_POSITION_X    00007fff
            [     30.000000] EV_ABS       ABS_MT_POSITION_Y    00007fff
            [     30.000000] EV_SYN       SYN_REPORT           00000000
        """.trimIndent().lines()
        val move = """
            [     30.016000] EV_ABS       ABS_MT_POSITION_Y    00000000
            [     30.016000] EV_SYN       SYN_REPORT           00000000
        """.trimIndent().lines()
        assertEquals(listOf(TouchSample(30_000_000_000, "/dev/input/event2", 0, TouchAction.DOWN, 1079, 2399)), down.flatMap(parser::feed))
        parser.resize(720, 1600)
        // X did not change after the resize but is still rescaled from its raw value.
        assertEquals(listOf(TouchSample(30_016_000_000, "/dev/input/event2", 0, TouchAction.MOVE, 719, 0)), move.flatMap(parser::feed))
        assertEquals(720 to 1600, parser.width to parser.height)
    }

    @Test
    fun `display geometry is re-read at most once per interval, at once after invalidate, and a failed read keeps the last`() {
        val answers = ArrayDeque<DisplayGeometry?>(listOf(DisplayGeometry(720, 1600, 0), null, DisplayGeometry(1080, 2400, 1)))
        var reads = 0
        val watch = DisplaySizeWatch(DisplayGeometry(1080, 2400, 0), { reads++; answers.removeFirst() }, intervalNs = 1_000)
        assertEquals(DisplayGeometry(1080, 2400, 0), watch.current(0))
        assertEquals(DisplayGeometry(1080, 2400, 0), watch.current(999))
        assertEquals(0, reads)
        assertEquals(DisplayGeometry(720, 1600, 0), watch.current(1_000))
        assertEquals(DisplayGeometry(720, 1600, 0), watch.current(2_000))
        assertEquals(2, reads)
        // The video stream reported a new screen size: the next input re-reads before the interval elapses.
        watch.invalidate()
        assertEquals(DisplayGeometry(1080, 2400, 1), watch.current(2_001))
        assertEquals(3, reads)
    }

    @Test
    fun `rotation comes from the built-in display viewport and rotates points as the screen shows them`() {
        val output = """
            Physical size: 1080x2400
                  Viewport INTERNAL: displayId=-1, uniqueId=, port=<none>, orientation=0, logicalFrame=[0, 0, 0, 0], isActive=[0]
                  Viewport INTERNAL: displayId=0, uniqueId=local:1, port=0, orientation=1, logicalFrame=[0, 0, 2400, 1080], isActive=[1]
        """.trimIndent()
        assertEquals(DisplayGeometry(1080, 2400, 1), parseDisplayGeometry(output))
        assertEquals(DisplayGeometry(1080, 2400, null), parseDisplayGeometry("Physical size: 1080x2400\n"))
        assertEquals(null, parseDisplayRotation("Viewport INTERNAL: displayId=2, orientation=1"))
        // Natural top-left (0, 0) is bottom-left after a 90 degree turn, bottom-right after 180, top-right after 270.
        assertEquals(10 to 20, screenPoint(10, 20, 1080, 2400, 0))
        assertEquals(20 to 1069, screenPoint(10, 20, 1080, 2400, 1))
        assertEquals(1069 to 2379, screenPoint(10, 20, 1080, 2400, 2))
        assertEquals(2379 to 10, screenPoint(10, 20, 1080, 2400, 3))
    }
}
