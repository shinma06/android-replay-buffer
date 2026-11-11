package io.github.shinma06.replaybuffer.core

import org.jcodec.common.model.ColorSpace
import org.jcodec.common.model.Picture
import org.junit.jupiter.api.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull
import kotlin.test.assertTrue

class InjectedTouchTest {
    // Verbatim from an API 36 emulator after `setprop log.tag.InputDispatcherInboundEvent DEBUG; input tap 300 600`.
    private val api36 = "11-11 04:30:03.250   667  2304 I InputDispatcher: injectInputEvent: targetUid=<not set>, syncMode=WAIT_FOR_FINISHED, " +
        "timeout=30000ms, policyFlags=0x8000000, event=MotionEvent { action=DOWN, id[0]=0, x[0]=300, y[0]=600, eventTime=327054000000, " +
        "downTime=327054000000, deviceId=2, source=TOUCHSCREEN, displayId=0, eventId=0xde834b43}"

    @Test fun `injectInputEvent log lines become motions on the device monotonic clock`() {
        val motion = assertNotNullValue(parseInjectedMotion(api36))
        assertEquals(327_054_000_000, motion.monoNs)
        assertEquals("DOWN", motion.action)
        assertEquals(listOf(Triple(0, 300f, 600f)), motion.pointers)
        val samples = injectedSamples(motion, DisplayGeometry(1080, 2400, 0))
        assertEquals(listOf(TouchSample(327_054_000_000, INJECTED_INPUT_DEVICE, 0, TouchAction.DOWN, 300, 600)), samples)
        assertTrue(samples.single().injected)
    }

    @Test fun `non touchscreen, other display and unrelated lines are ignored`() {
        assertNull(parseInjectedMotion(api36.replace("source=TOUCHSCREEN", "source=MOUSE")))
        assertNull(parseInjectedMotion(api36.replace("displayId=0", "displayId=2")))
        assertNull(parseInjectedMotion(api36.replace("MotionEvent", "KeyEvent")))
        assertNull(parseInjectedMotion("I InputDispatcher: Focus entered window"))
        assertEquals("DOWN", parseInjectedMotion(api36.replace("source=TOUCHSCREEN", "source=TOUCHSCREEN | STYLUS"))?.action)
    }

    @Test fun `pointer actions keep ids and logical coordinates map back to the natural orientation`() {
        val line = api36.replace("action=DOWN, id[0]=0, x[0]=300, y[0]=600", "action=POINTER_DOWN(1), id[0]=0, x[0]=300, y[0]=600, id[1]=1, x[1]=2000.4, y[1]=100.0")
        val motion = assertNotNullValue(parseInjectedMotion(line))
        // Landscape (rotation 1) on a 1080x2400 panel: the logical point (2000, 100) is natural (979, 2000).
        val sample = injectedSamples(motion, DisplayGeometry(1080, 2400, 1)).single()
        assertEquals(TouchSample(327_054_000_000, INJECTED_INPUT_DEVICE, 1, TouchAction.DOWN, 979, 2000), sample)
        assertEquals(2000 to 100, screenPoint(sample.x, sample.y, 1080, 2400, 1))
        assertEquals(listOf(TouchAction.UP, TouchAction.UP), injectedSamples(motion.copy(action = "CANCEL"), DisplayGeometry(1080, 2400, 0)).map { it.action })
        assertEquals(emptyList(), injectedSamples(motion.copy(action = "HOVER_MOVE"), DisplayGeometry(1080, 2400, 0)))
    }

    @Test fun `naturalPoint inverts screenPoint for every rotation`() {
        for (rotation in 0..3) for ((x, y) in listOf(0 to 0, 1079 to 2399, 17 to 1234)) {
            val (sx, sy) = screenPoint(x, y, 1080, 2400, rotation)
            assertEquals(x to y, naturalPoint(sx, sy, 1080, 2400, rotation), "rotation $rotation")
        }
    }

    private fun entry(mono: Long, action: TouchAction, x: Int, y: Int, device: String = "/dev/input/event2", rotation: Int? = 0,
                      generation: Long = 1) =
        TouchEntry(TouchSample(mono, device, 0, action, x, y), generation, MappedTime(mono, 0, 0), mono, 1080, 2400, rotation)

    @Test fun `a still frame draws every tap the same pointer makes while it is shown`() {
        val taps = listOf(540 to 1200, 0 to 0, 1079 to 2399).flatMapIndexed { i, (x, y) ->
            val down = 1_000_000_000L + i * 500_000_000L
            listOf(entry(down, TouchAction.DOWN, x, y), entry(down + 50_000_000, TouchAction.UP, x, y))
        }
        val overlay = TapOverlay(taps)
        assertEquals(listOf(OverlayDot(270, 600), OverlayDot(0, 0), OverlayDot(539, 1199)), overlay.dots(0, 540, 1200, until = 3_000_000_000))
        // The pointer down at frame start is drawn together with the later taps at other positions.
        assertEquals(listOf(OverlayDot(270, 600), OverlayDot(0, 0), OverlayDot(539, 1199)),
            overlay.dots(1_010_000_000, 540, 1200, until = 3_000_000_000))
        assertEquals(listOf(OverlayDot(0, 0)), overlay.dots(1_400_000_000, 540, 1200, until = 1_600_000_000))
        val many = TapOverlay((0 until 40).flatMap { i ->
            listOf(entry(1_000_000_000L + i * 10_000_000L, TouchAction.DOWN, i * 20, 100), entry(1_000_000_000L + i * 10_000_000L + 1, TouchAction.UP, i * 20, 100))
        })
        assertEquals(16, many.dots(0, 1080, 2400, until = 2_000_000_000).size)
    }

    @Test fun `the last frame before an interruption does not draw taps of the gap or a later run`() {
        val overlay = TapOverlay(listOf(entry(1_500_000_000, TouchAction.DOWN, 540, 1200), entry(1_550_000_000, TouchAction.UP, 540, 1200),
            entry(3_000_000_000, TouchAction.DOWN, 0, 0), entry(3_050_000_000, TouchAction.UP, 0, 0)))
        fun drawn(until: Long) = overlay.dots(1_000_000_000, 540, 1200, until = until)
        // Inside a run the next confirmed frame bounds the interval.
        assertEquals(1_200_000_000, frameShownUntil(1_000_000_000, 1_200_000_000, 5_000_000_000, listOf(1_100_000_000)))
        // The run's last frame shown 1 us (terminal tick) draws neither tap.
        assertEquals(emptyList(), drawn(frameShownUntil(1_000_000_000, null, 1_000_001_000, emptyList())))
        // Held until a video gap at 2 s: the tap at 1.5 s is drawn, the one in the later run at 3 s is not.
        assertEquals(2_000_000_000, frameShownUntil(1_000_000_000, null, 5_000_000_000, listOf(500_000_000, 2_000_000_000, 4_000_000_000)))
        assertEquals(listOf(OverlayDot(270, 600)), drawn(frameShownUntil(1_000_000_000, null, 5_000_000_000, listOf(2_000_000_000))))
        assertEquals(listOf(OverlayDot(270, 600), OverlayDot(0, 0)), drawn(5_000_000_000))
    }

    @Test fun `a pointer whose UP was lost in a reader gap is not drawn from the gap on`() {
        val physical = entry(1_000_000_000, TouchAction.DOWN, 540, 1200)
        val injected = entry(1_000_000_000, TouchAction.DOWN, 1079, 2399, INJECTED_INPUT_DEVICE)
        val gap = listOf(TouchGap(1_500_000_000, 2_500_000_000, "切断"))
        val overlay = TapOverlay(listOf(physical, injected, entry(3_000_000_000, TouchAction.DOWN, 0, 0)), gaps = gap)
        assertEquals(listOf(OverlayDot(270, 600), OverlayDot(539, 1199)), overlay.dots(1_200_000_000, 540, 1200))
        // getevent stopped at 1.5 s: its pointer is gone, the injected reader kept reading so its pointer stays.
        assertEquals(listOf(OverlayDot(539, 1199)), overlay.dots(1_500_000_000, 540, 1200))
        assertEquals(listOf(OverlayDot(539, 1199)), overlay.dots(2_800_000_000, 540, 1200))
        // A new touch after the gap is drawn again.
        assertEquals(listOf(OverlayDot(0, 0), OverlayDot(539, 1199)), overlay.dots(3_000_000_000, 540, 1200))
        val injectedLost = TapOverlay(listOf(physical, injected), injectedGaps = gap)
        assertEquals(listOf(OverlayDot(270, 600)), injectedLost.dots(2_000_000_000, 540, 1200))
        // A gap with an unknown start cannot be placed after the pointer's last row.
        assertEquals(2, TapOverlay(listOf(physical, injected), gaps = listOf(TouchGap(null, 2_000_000_000, "開始前")),
            injectedGaps = listOf(TouchGap(null, null, "不明"))).dots(2_200_000_000, 540, 1200).size)
    }

    @Test fun `a finger held down across a reconnect is not drawn on the next generation's frames`() {
        // Generation 1 disconnected while the finger was down, so its last row is a DOWN with no UP.
        val overlay = TapOverlay(listOf(entry(1_000_000_000, TouchAction.DOWN, 540, 1200, generation = 1),
            entry(3_000_000_000, TouchAction.DOWN, 1079, 2399, generation = 2), entry(3_050_000_000, TouchAction.UP, 1079, 2399, generation = 2)))
        assertEquals(listOf(OverlayDot(270, 600)), overlay.dots(2_000_000_000, 540, 1200, generation = 1))
        assertEquals(emptyList(), overlay.dots(2_000_000_000, 540, 1200, generation = 2))
        assertEquals(listOf(OverlayDot(539, 1199)), overlay.dots(3_010_000_000, 540, 1200, generation = 2))
        assertEquals(emptyList(), overlay.dots(4_000_000_000, 540, 1200, generation = 2))
    }

    @Test fun `overlay modes choose which taps the saved video draws`() {
        val physical = entry(1_000, TouchAction.DOWN, 10, 10)
        val injected = entry(1_000, TouchAction.DOWN, 10, 10, INJECTED_INPUT_DEVICE)
        val rows = listOf(physical, injected)
        assertEquals(emptyList(), overlayTouches(TapDisplay.OFF, TouchDots.OFF, rows))
        assertEquals(rows, overlayTouches(TapDisplay.VIDEO, TouchDots.APPLIED, rows))
        assertEquals(listOf(injected), overlayTouches(TapDisplay.DEVICE, TouchDots.APPLIED, rows))
        assertEquals(listOf(injected), overlayTouches(TapDisplay.DEVICE, TouchDots.ALREADY_ON, rows))
        assertEquals(rows, overlayTouches(TapDisplay.DEVICE, TouchDots.FAILED, rows))
    }

    @Test fun `overlay draws a pointer while down and briefly after it lifts, scaled to the frame`() {
        val overlay = TapOverlay(listOf(entry(1_000_000_000, TouchAction.DOWN, 540, 1200), entry(1_500_000_000, TouchAction.MOVE, 1079, 2399),
            entry(2_000_000_000, TouchAction.UP, 1079, 2399)))
        assertTrue(overlay.within(0, 1_000_000_000))
        assertTrue(!overlay.within(2_200_000_000, 3_000_000_000))
        assertEquals(emptyList(), overlay.dots(999_999_999, 540, 1200))
        assertEquals(listOf(OverlayDot(270, 600)), overlay.dots(1_200_000_000, 540, 1200))
        assertEquals(listOf(OverlayDot(539, 1199)), overlay.dots(2_050_000_000, 540, 1200))
        assertEquals(emptyList(), overlay.dots(2_200_000_000, 540, 1200))
        // A still frame shown from 0.5 s to 1.1 s gets the tap that starts at 1.0 s; one that ends before it does not.
        assertEquals(listOf(OverlayDot(270, 600)), overlay.dots(500_000_000, 540, 1200, until = 1_100_000_000))
        assertEquals(emptyList(), overlay.dots(500_000_000, 540, 1200, until = 1_000_000_000))
        // Rotation 1 shows the natural point (540, 1200) at screen (1200, 539) of a 2400x1080 frame.
        val landscape = TapOverlay(listOf(entry(1_000, TouchAction.DOWN, 540, 1200, rotation = 1)))
        assertEquals(listOf(OverlayDot(600, 269)), landscape.dots(1_000, 1200, 540))
        // Unknown rotation with a frame in the other orientation cannot be placed.
        assertEquals(emptyList(), TapOverlay(listOf(entry(1_000, TouchAction.DOWN, 1, 1, rotation = null))).dots(1_000, 1200, 540))
    }

    @Test fun `dots brighten the centre and stay inside the source rectangle`() {
        val picture = Picture.create(64, 64, ColorSpace.YUV420J)
        java.util.Arrays.fill(picture.getPlaneData(0), 0)
        drawTapDots(picture, 16, 16, 32, 32, listOf(OverlayDot(16, 16), OverlayDot(0, 0)))
        val luma = picture.getPlaneData(0)
        assertTrue(luma[(16 + 16) * 64 + 16 + 16] > 0)
        assertEquals(0, luma[0].toInt())
        assertEquals(0, luma[15 * 64 + 15].toInt())
    }

    private fun <T : Any> assertNotNullValue(value: T?): T = kotlin.test.assertNotNull(value)
}
