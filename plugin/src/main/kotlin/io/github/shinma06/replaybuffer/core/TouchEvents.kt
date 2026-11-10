package io.github.shinma06.replaybuffer.core

internal enum class TouchAction(val wire: String) { DOWN("down"), MOVE("move"), UP("up") }

/** One pointer change in display pixels; [monoNs] is the kernel input time on the device monotonic clock. */
internal data class TouchSample(
    val monoNs: Long, val device: String, val pointer: Int, val action: TouchAction, val x: Int, val y: Int,
)

internal data class TouchAxes(val maxX: Int, val maxY: Int)

/** A retained sample; [time] is mapped from the source time, never from [host] (host only supports the mapping). */
internal data class TouchEntry(
    val sample: TouchSample, val generation: Long, val time: MappedTime, val host: Long, val width: Int, val height: Int,
)

/** Window-time interval (sequence ns) where touch input was not being read; null means outside the known range. */
internal data class TouchGap(val from: Long?, val to: Long?, val reason: String)

internal const val MAX_TOUCH_SAMPLES = 200_000

private val deviceLine = Regex("""add device \d+: (/dev/input/event\d+)""")
private val axisLine = Regex("""ABS_MT_POSITION_([XY])\s*:\s*value -?\d+, min (-?\d+), max (-?\d+)""")
private val eventLine = Regex("""\[\s*(\d+)\.(\d{6})\]\s+(?:(/dev/input/event\d+):\s+)?(EV_\w+)\s+(\w+)\s+([0-9a-fA-F]{8})""")

/** Multi-touch devices from `getevent -lp`; devices without both MT position axes are not touchscreens. */
internal fun parseTouchDevices(output: String): Map<String, TouchAxes> {
    val result = linkedMapOf<String, TouchAxes>()
    var device: String? = null
    var maxX: Int? = null
    var maxY: Int? = null
    fun flush() {
        val d = device ?: return
        if (maxX != null && maxY != null && maxX!! > 0 && maxY!! > 0) result[d] = TouchAxes(maxX!!, maxY!!)
    }
    output.lineSequence().forEach { line ->
        deviceLine.find(line)?.let { flush(); device = it.groupValues[1]; maxX = null; maxY = null; return@forEach }
        axisLine.find(line)?.let {
            if (it.groupValues[2].toInt() != 0) return@let
            if (it.groupValues[1] == "X") maxX = it.groupValues[3].toInt() else maxY = it.groupValues[3].toInt()
        }
    }
    flush()
    return result
}

/** Display size from `wm size`; an override wins over the physical size, matching what the screen shows. */
internal fun parseDisplaySize(output: String): Pair<Int, Int>? {
    val sizes = Regex("""(Physical|Override) size: (\d+)x(\d+)""").findAll(output).associate {
        it.groupValues[1] to (it.groupValues[2].toInt() to it.groupValues[3].toInt())
    }
    return sizes["Override"] ?: sizes["Physical"]
}

/**
 * Folds `getevent -lt` lines (protocol B slots) into pointer samples emitted at each SYN_REPORT.
 * Rotation is not applied: coordinates are in the panel's natural orientation, as the kernel reports them.
 */
internal class TouchParser(private val axes: Map<String, TouchAxes>, private val width: Int, private val height: Int) {
    private class Slot(var tracking: Int = -1, var x: Int? = null, var y: Int? = null, var down: Boolean = false, var dirty: Boolean = false)
    private val slots = mutableMapOf<String, MutableMap<Int, Slot>>()
    private val current = mutableMapOf<String, Int>()

    fun feed(line: String): List<TouchSample> {
        val m = eventLine.find(line) ?: return emptyList()
        // getevent omits the device prefix when it reads a single device.
        val device = m.groupValues[3].ifEmpty { axes.keys.singleOrNull() ?: return emptyList() }
        val axis = axes[device] ?: return emptyList()
        val monoNs = Math.addExact(Math.multiplyExact(m.groupValues[1].toLong(), 1_000_000_000L), m.groupValues[2].toLong() * 1000)
        val value = m.groupValues[6].toLong(16).toInt()
        val deviceSlots = slots.getOrPut(device) { mutableMapOf() }
        val slot = { deviceSlots.getOrPut(current[device] ?: 0) { Slot() } }
        when (m.groupValues[5]) {
            "ABS_MT_SLOT" -> current[device] = value
            "ABS_MT_TRACKING_ID" -> slot().apply { tracking = value; dirty = true }
            "ABS_MT_POSITION_X" -> slot().apply { x = scale(value, axis.maxX, width); dirty = true }
            "ABS_MT_POSITION_Y" -> slot().apply { y = scale(value, axis.maxY, height); dirty = true }
            "SYN_REPORT" -> return report(monoNs, device, deviceSlots)
        }
        return emptyList()
    }

    private fun report(monoNs: Long, device: String, deviceSlots: Map<Int, Slot>): List<TouchSample> =
        deviceSlots.toSortedMap().mapNotNull { (index, s) ->
            if (!s.dirty) return@mapNotNull null
            s.dirty = false
            val x = s.x ?: return@mapNotNull null
            val y = s.y ?: return@mapNotNull null
            val action = when {
                s.tracking >= 0 && !s.down -> TouchAction.DOWN.also { s.down = true }
                s.tracking >= 0 -> TouchAction.MOVE
                s.down -> TouchAction.UP.also { s.down = false }
                else -> return@mapNotNull null
            }
            TouchSample(monoNs, device, index, action, x, y)
        }

    private fun scale(value: Int, max: Int, size: Int): Int =
        ((value.coerceIn(0, max).toLong() * (size - 1) + max / 2) / max).toInt()
}
