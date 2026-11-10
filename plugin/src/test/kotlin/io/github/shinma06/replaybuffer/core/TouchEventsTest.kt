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
}
