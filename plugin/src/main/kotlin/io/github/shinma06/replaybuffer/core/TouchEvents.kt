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
    val rotation: Int? = null,
)

/** Window-time interval (sequence ns) where touch input was not being read; null means outside the known range. */
internal data class TouchGap(val from: Long?, val to: Long?, val reason: String)

/** How the device show_touches setting stood while touches were captured. */
internal enum class TouchDots(val wire: String) {
    OFF("off"),
    APPLIED("applied"),
    ALREADY_ON("already_on"),
    FAILED("failed"),
}

/**
 * Original show_touches values per device serial, kept across capture generations so that a restore that failed
 * while the device was disconnected is retried by a later generation instead of mistaking our own "1" for the original.
 */
internal class ShowTouchesRestore {
    private val originals = java.util.concurrent.ConcurrentHashMap<String, String>()

    fun pending(serial: String): Boolean = originals.containsKey(serial)

    /** Remembers the first original only; empty means the key was unset. */
    fun remember(serial: String, original: String) { originals.putIfAbsent(serial, original) }

    fun original(serial: String): String? = originals[serial]

    fun restored(serial: String, original: String) { originals.remove(serial, original) }
}

internal const val MAX_TOUCH_SAMPLES = 200_000

private val deviceLine = Regex("""add device \d+: (/dev/input/event\d+)""")
private val slotLine = Regex("""ABS_MT_SLOT\s*:""")
private val axisLine = Regex("""ABS_MT_POSITION_([XY])\s*:\s*value -?\d+, min (-?\d+), max (-?\d+)""")
private val eventLine = Regex("""\[\s*(\d+)\.(\d{6})\]\s+(?:(/dev/input/event\d+):\s+)?(EV_\w+)\s+(\w+)\s+([0-9a-fA-F]{8})""")

/**
 * Multi-touch devices from `getevent -lp`; devices without both MT position axes are not touchscreens. Only protocol B
 * (ABS_MT_SLOT) is parsed, so protocol-A devices are left out and touch capture reports them as unavailable.
 */
internal fun parseTouchDevices(output: String): Map<String, TouchAxes> {
    val result = linkedMapOf<String, TouchAxes>()
    var device: String? = null
    var maxX: Int? = null
    var maxY: Int? = null
    var slots = false
    fun flush() {
        val d = device ?: return
        if (slots && maxX != null && maxY != null && maxX!! > 0 && maxY!! > 0) result[d] = TouchAxes(maxX!!, maxY!!)
    }
    output.lineSequence().forEach { line ->
        deviceLine.find(line)?.let { flush(); device = it.groupValues[1]; maxX = null; maxY = null; slots = false; return@forEach }
        if (slotLine.containsMatchIn(line)) slots = true
        axisLine.find(line)?.let {
            if (it.groupValues[2].toInt() != 0) return@let
            if (it.groupValues[1] == "X") maxX = it.groupValues[3].toInt() else maxY = it.groupValues[3].toInt()
        }
    }
    flush()
    return result
}

/** Current slot and its raw MT position, as `getevent -lp` reports them now (the `value` of each axis). */
internal data class TouchCurrent(val slot: Int?, val x: Int?, val y: Int?)

private val currentLine = Regex("""(ABS_MT_SLOT|ABS_MT_POSITION_X|ABS_MT_POSITION_Y)\s*:\s*value (-?\d+)""")

internal fun parseTouchCurrent(output: String): Map<String, TouchCurrent> {
    val result = linkedMapOf<String, TouchCurrent>()
    var device: String? = null
    output.lineSequence().forEach { line ->
        deviceLine.find(line)?.let { device = it.groupValues[1]; return@forEach }
        val d = device ?: return@forEach
        val m = currentLine.find(line) ?: return@forEach
        val value = m.groupValues[2].toInt()
        val old = result[d] ?: TouchCurrent(null, null, null)
        result[d] = when (m.groupValues[1]) {
            "ABS_MT_SLOT" -> old.copy(slot = value)
            "ABS_MT_POSITION_X" -> old.copy(x = value)
            else -> old.copy(y = value)
        }
    }
    return result
}

/** Hooks for SYN_DROPPED: report the lost interval and re-read the device's current slot state. */
internal interface TouchSync {
    fun lost(device: String, fromNs: Long) {}
    fun current(device: String): TouchCurrent? = null
    fun resynced(device: String, toNs: Long) {}
}

/** Display size from `wm size`; an override wins over the physical size, matching what the screen shows. */
internal fun parseDisplaySize(output: String): Pair<Int, Int>? {
    val sizes = Regex("""(Physical|Override) size: (\d+)x(\d+)""").findAll(output).associate {
        it.groupValues[1] to (it.groupValues[2].toInt() to it.groupValues[3].toInt())
    }
    return sizes["Override"] ?: sizes["Physical"]
}

/** Natural-orientation display size and the screen rotation in quarter turns (Surface.ROTATION_*); null when unknown. */
internal data class DisplayGeometry(val width: Int, val height: Int, val rotation: Int?)

private val viewportLine = Regex("""Viewport[^\n]*\bdisplayId=0,[^\n]*\borientation=([0-3])\b""")

/** Rotation of the built-in display (id 0) from `dumpsys input` viewports, which is what input uses for touches. */
internal fun parseDisplayRotation(output: String): Int? = viewportLine.find(output)?.groupValues?.get(1)?.toInt()

/** `wm size` followed by `dumpsys input` viewport lines, read in one adb shell call. */
internal fun parseDisplayGeometry(output: String): DisplayGeometry? =
    parseDisplaySize(output)?.let { (width, height) -> DisplayGeometry(width, height, parseDisplayRotation(output)) }

/** A natural-orientation point as the rotated screen (and therefore the video) shows it. */
internal fun screenPoint(x: Int, y: Int, width: Int, height: Int, rotation: Int): Pair<Int, Int> = when (rotation) {
    1 -> y to width - 1 - x
    2 -> width - 1 - x to height - 1 - y
    3 -> height - 1 - y to x
    else -> x to y
}

/**
 * Re-reads the display geometry when input arrives at least [intervalNs] after the last read, or on the first input after
 * [invalidate] (the video stream reported a new screen size, e.g. rotation). A `wm size` override or an emulator resize
 * changes the scale of later samples. A failed read keeps the previous geometry.
 */
internal class DisplaySizeWatch(
    initial: DisplayGeometry, private val read: () -> DisplayGeometry?, private val intervalNs: Long = 1_000_000_000,
) {
    var geometry: DisplayGeometry = initial
        private set
    private var checked: Long? = null
    private val stale = java.util.concurrent.atomic.AtomicBoolean()

    fun invalidate() = stale.set(true)

    fun current(host: Long): DisplayGeometry {
        val last = checked
        val forced = stale.getAndSet(false)
        if (last == null && !forced) checked = host
        else if (forced || host - last!! >= intervalNs) {
            checked = host
            runCatching(read).getOrNull()?.takeIf { it.width > 0 && it.height > 0 }?.let { geometry = it }
        }
        return geometry
    }
}

/**
 * Folds `getevent -lt` lines (protocol B slots) into pointer samples emitted at each SYN_REPORT.
 * Rotation is not applied: coordinates are in the panel's natural orientation, as the kernel reports them; see [screenPoint].
 * Raw axis values are kept and scaled at each report, so [resize] applies to every later sample as a whole.
 * [initial] holds each device's current slot and its X/Y at reader start; other slots start without axis values.
 *
 * evdev only reports changed axes, so SYN_DROPPED does not clear state. Each slot keeps its last X/Y; the current slot
 * and its X/Y are re-read through [TouchSync.current]. Pointers that were down end with `up` at the drop time, events
 * are ignored through the next SYN_REPORT, and every slot's pointer stays unknown (no samples) until a new
 * ABS_MT_TRACKING_ID arrives for it. The lost interval runs from the drop to the first report with a non-negative ID.
 */
internal class TouchParser(
    private val axes: Map<String, TouchAxes>, width: Int, height: Int, private val sync: TouchSync = object : TouchSync {},
    initial: Map<String, TouchCurrent> = emptyMap(),
) {
    private class Slot(
        var tracking: Int = -1, var x: Int? = null, var y: Int? = null, var down: Boolean = false, var dirty: Boolean = false,
        var known: Boolean = true,
    )
    private val slots = mutableMapOf<String, MutableMap<Int, Slot>>()
    private val current = mutableMapOf<String, Int>()
    /** Devices whose events are ignored through the next SYN_REPORT after SYN_DROPPED. */
    private val discarding = mutableSetOf<String>()
    /** Devices with an open lost interval, and whether the frame being read carries a new tracking ID. */
    private val resyncing = mutableMapOf<String, Boolean>()
    var width = width
        private set
    var height = height
        private set

    init {
        // evdev does not resend current values to a new reader, so start from the slot and X/Y read just before it.
        initial.filterKeys { it in axes }.forEach { (device, now) -> applyCurrent(device, now, known = true) }
    }

    fun resize(width: Int, height: Int) {
        this.width = width
        this.height = height
    }

    fun feed(line: String): List<TouchSample> {
        val m = eventLine.find(line) ?: return emptyList()
        // getevent omits the device prefix when it reads a single device.
        val device = m.groupValues[3].ifEmpty { axes.keys.singleOrNull() ?: return emptyList() }
        val axis = axes[device] ?: return emptyList()
        val monoNs = Math.addExact(Math.multiplyExact(m.groupValues[1].toLong(), 1_000_000_000L), m.groupValues[2].toLong() * 1000)
        val code = m.groupValues[5]
        if (device in discarding) {
            if (code == "SYN_REPORT") discarding.remove(device)
            return emptyList()
        }
        val value = m.groupValues[6].toLong(16).toInt()
        val deviceSlots = slots.getOrPut(device) { mutableMapOf() }
        val slot = { deviceSlots.getOrPut(current[device] ?: 0) { Slot(known = device !in resyncing) } }
        when (code) {
            "ABS_MT_SLOT" -> current[device] = value
            "ABS_MT_TRACKING_ID" -> {
                slot().apply { tracking = value; dirty = true; known = true }
                // A release (-1) only ends a pointer held across the drop; only a new pointer makes input known again.
                if (device in resyncing && value >= 0) resyncing[device] = true
            }
            "ABS_MT_POSITION_X" -> slot().apply { x = value; dirty = true }
            "ABS_MT_POSITION_Y" -> slot().apply { y = value; dirty = true }
            "SYN_REPORT" -> return report(monoNs, device, axis, deviceSlots).also {
                if (resyncing[device] == true) {
                    resyncing.remove(device)
                    sync.resynced(device, monoNs)
                }
            }
            "SYN_DROPPED" -> return drop(monoNs, device, axis, deviceSlots)
        }
        return emptyList()
    }

    private fun report(monoNs: Long, device: String, axis: TouchAxes, deviceSlots: Map<Int, Slot>): List<TouchSample> =
        deviceSlots.toSortedMap().mapNotNull { (index, s) ->
            if (!s.dirty) return@mapNotNull null
            s.dirty = false
            if (!s.known) return@mapNotNull null
            val x = s.x ?: return@mapNotNull null
            val y = s.y ?: return@mapNotNull null
            val action = when {
                s.tracking >= 0 && !s.down -> TouchAction.DOWN.also { s.down = true }
                s.tracking >= 0 -> TouchAction.MOVE
                s.down -> TouchAction.UP.also { s.down = false }
                else -> return@mapNotNull null
            }
            TouchSample(monoNs, device, index, action, scale(x, axis.maxX, width), scale(y, axis.maxY, height))
        }

    private fun drop(monoNs: Long, device: String, axis: TouchAxes, deviceSlots: MutableMap<Int, Slot>): List<TouchSample> {
        val ended = deviceSlots.toSortedMap().mapNotNull { (index, s) ->
            val x = s.x
            val y = s.y
            val up = if (s.down && x != null && y != null)
                TouchSample(monoNs, device, index, TouchAction.UP, scale(x, axis.maxX, width), scale(y, axis.maxY, height)) else null
            s.down = false
            s.tracking = -1
            s.dirty = false
            s.known = false
            up
        }
        sync.current(device)?.let { applyCurrent(device, it, known = false) }
        discarding += device
        if (device !in resyncing) sync.lost(device, monoNs)
        resyncing[device] = false
        return ended
    }

    private fun applyCurrent(device: String, now: TouchCurrent, known: Boolean) {
        now.slot?.let { current[device] = it }
        val s = slots.getOrPut(device) { mutableMapOf() }.getOrPut(current[device] ?: 0) { Slot(known = known) }
        now.x?.let { s.x = it }
        now.y?.let { s.y = it }
    }

    private fun scale(value: Int, max: Int, size: Int): Int =
        ((value.coerceIn(0, max).toLong() * (size - 1) + max / 2) / max).toInt()
}
