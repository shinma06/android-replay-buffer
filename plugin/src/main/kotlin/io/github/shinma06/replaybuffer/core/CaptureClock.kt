package io.github.shinma06.replaybuffer.core

import java.util.ArrayDeque
import kotlin.math.abs

/**
 * Device elapsed(BOOTTIME)/mono/wall and host send/receive values are nanoseconds from their separate clocks.
 * [segment] counts device boots; [sequenceOffset] places this boot's BOOTTIME on the sequence axis. Only segment 0 is
 * exact; a later boot's placement is a host-time lower bound for ordering and never an event time.
 */
internal data class ClockSample(
    val epoch: Int, val boot: String, val before: Long, val mono: Long, val wall: Long, val after: Long,
    val sent: Long, val received: Long, val sequenceOffset: Long, val segment: Int = 0,
) {
    val elapsed: Long get() = before + (after - before) / 2
    val monoOffset: Long get() = elapsed - mono
    val wallOffset: Long get() = elapsed - wall
    val readError: Long get() = (after - before) / 2 + 1_000_000
    /** Supports offsets and mapping. Offsets are read on the device, so the adb round trip does not affect them. */
    val valid: Boolean get() = after - before <= 2_000_000
    /** Also supports host-to-device estimates (T, barriers, start bounds), whose error includes half the round trip. */
    val hostAnchored: Boolean get() = valid && received - sent <= 40_000_000
    /** Position of the sequence axis relative to the first boot: exact (0) or approximate across a reboot. */
    val bridgeError: Long get() = if (segment == 0) 0 else Long.MAX_VALUE
}

/** Nanosecond correspondence: null times and Long.MAX_VALUE uncertainty preserve unsupported/ambiguous mapping. */
internal data class MappedTime(
    val elapsed: Long?, val epoch: Int, val uncertainty: Long, val sequence: Long? = elapsed,
)

/**
 * Device BOOTTIME of the current boot is the timeline; MONOTONIC and REALTIME sources map onto it by measured offsets.
 * Host time only schedules reads, judges staleness, estimates T and orders boots approximately.
 */
internal class CaptureClock {
    private val samples = ArrayDeque<ClockSample>()
    private var epoch = 0
    private val closures = linkedMapOf<Int, Long>()

    @Synchronized
    fun add(boot: String, values: List<Long>, sent: Long, received: Long): Boolean {
        require(values.size == 4 && boot.length in 1..64)
        val (before, mono, wall, after) = values
        require(before >= 0 && after >= before && mono >= 0 && wall >= 0 && received >= sent)
        val last = samples.peekLast()
        // Offsets are compared only within this boot; a new boot is a boundary even when its first read is too wide.
        val measured = samples.lastOrNull { it.after - it.before <= 2_000_000 && it.segment == last?.segment }
        val normal = samples.lastOrNull { it.valid }
        val elapsed = before + (after - before) / 2
        val good = after - before <= 2_000_000
        val bootChanged = last != null && (last.boot != boot || elapsed < last.elapsed)
        val sourceBreak = after - before <= 2_000_000 && measured != null &&
            (boot != measured.boot || elapsed < measured.elapsed ||
                abs(elapsed - mono - measured.monoOffset) > measured.readError + (after - before) / 2 + 1_000_000 ||
                abs(elapsed - wall - measured.wallOffset) > measured.readError + (after - before) / 2 + 1_000_000)
        // A measurement outage alone is no boundary: the next good read decides continuity from the device offsets.
        if (sourceBreak || bootChanged) boundary(normal?.received ?: sent)
        // A new boot starts its own exact segment. Host time cannot run faster than real time, so placing it after the
        // previous read by the host gap only orders boots; rows of another boot never get a time on this segment.
        val offset = when {
            last == null -> -elapsed
            !bootChanged -> last.sequenceOffset
            else -> Math.addExact(last.elapsed + last.sequenceOffset, (sent - last.received).coerceAtLeast(0)) - elapsed
        }
        val segment = if (last == null) 0 else if (bootChanged) last.segment + 1 else last.segment
        samples += ClockSample(epoch, boot, before, mono, wall, after, sent, received, offset, segment)
        while (samples.size > 4096) {
            // A still-displayed frame may be older than the recent clock ring. Keep its epoch's original anchor.
            val first = samples.peekFirst()
            if (first.epoch == epoch && first.valid) {
                samples.removeFirst(); samples.removeFirst(); samples.addFirst(first)
            } else samples.removeFirst()
        }
        latestCache = null
        return good
    }

    @Synchronized
    fun now(host: Long = System.nanoTime()): Long? {
        val latest = samples.peekLast() ?: return null
        val anchor = anchorFor(latest, host)
        return maxOf(latest.elapsed + latest.sequenceOffset,
            Math.addExact(anchor.elapsed + anchor.sequenceOffset, (host - anchor.received).coerceAtLeast(0)))
    }

    /** Newest round-trip-supported sample of [latest]'s epoch received within 5 s before [host], else [latest] itself. */
    private fun hostSample(latest: ClockSample, host: Long): ClockSample = if (!latest.valid) latest else
        samples.lastOrNull { it.hostAnchored && it.epoch == latest.epoch && host - it.received <= 5_000_000_000 } ?: latest

    /** Host-to-device anchor for T at [host]: the shortest round trip within 100 ms of [hostSample]. */
    private fun anchorFor(latest: ClockSample, host: Long): ClockSample = hostSample(latest, host).let { recent ->
        if (!recent.hostAnchored) recent else
            samples.filter { it.hostAnchored && it.epoch == recent.epoch && recent.received - it.received in 0 until 100_000_000 }
                .minBy { it.received - it.sent }
    }

    /** Sample for extrapolating device time from host time, or null before the first usable sample. */
    @Synchronized
    fun hostAnchor(host: Long = System.nanoTime()): ClockSample? = samples.lastOrNull { it.valid }?.let { hostSample(it, host) }

    @Synchronized
    fun video(pts: Long, host: Long, hint: Int? = null, reference: Int? = null): MappedTime =
        map(Math.multiplyExact(pts, 1000), host, false, hint ?: epoch, reference = reference ?: segment())

    /** Device CLOCK_MONOTONIC source time in nanoseconds (for example evdev input times), mapped like video PTS. */
    @Synchronized
    fun mono(ns: Long, host: Long, hint: Int? = null): MappedTime = map(ns, host, false, hint ?: epoch, reference = segment())

    @Synchronized
    fun log(wall: Long, host: Long): MappedTime = map(wall, host, true, null, reference = segment())

    /** Boot segment of [epoch] (default: the newest sample), the reference segment whose rows get sequence times. */
    @Synchronized
    fun segment(epoch: Int? = null): Int =
        (if (epoch == null) samples.peekLast() else samples.lastOrNull { it.epoch == epoch })?.segment ?: 0

    /** Same result as [mono] at this moment for many rows: samples and indexes are built once, not per row. */
    @Synchronized
    fun monoMapper(reference: Int = segment()): (Long, Long, Int?) -> MappedTime {
        val valid = samples.filter { it.valid }
        val groups = valid.groupBy { it.epoch }.values.toList()
        val byEpoch = groups.associateBy { it.first().epoch }
        val indexes = groups.associate { it.first().epoch to ClockSourceIndex(it) { sample -> sample.mono } }
        val epochs = if (valid.isEmpty()) null else ClockSourceIndex(valid) { it.epoch.toLong() }
        val next = groups.associate { group -> group.first().epoch to epochs?.after(group.last().epoch.toLong()) }
        val activeEpoch = epoch
        val closed = closures.toMap()
        return { source, host, hint ->
            val target = hint ?: activeEpoch
            map(source, host, false, target, listOfNotNull(byEpoch[target]), indexes, next, activeEpoch, closed, reference)
        }
    }

    /** Captures samples, epochs and closures once; later clock updates cannot change this log conversion. */
    @Synchronized
    fun logMapper(reference: Int = segment()): (Long, Long) -> MappedTime {
        val fixed = samples.toList()
        val valid = fixed.filter { it.valid }
        val groups = valid.groupBy { it.epoch }.values.toList()
        val epochs = if (valid.isEmpty()) null else ClockSourceIndex(valid) { it.epoch.toLong() }
        val indexes = groups.associate { it.first().epoch to ClockSourceIndex(it) }
        val next = groups.associate { group ->
            group.first().epoch to epochs?.after(group.last().epoch.toLong())
        }
        val ranges = ClockEpochIndex(groups.map { group ->
            val following = next[group.first().epoch]
            val last = group.last()
            val limit = 5_000_000_000 + (last.received - last.sent)
            // Conservative active bounds must preserve wrapped arithmetic and the original invalid-limit exception.
            val lower = if (following == null && limit < 0) Long.MIN_VALUE else group.first().wall - 20_000_000
            val upper = if (following == null) {
                if (limit < 0) Long.MAX_VALUE else try {
                    Math.addExact(Math.addExact(last.wall, limit), 20_000_000)
                } catch (_: ArithmeticException) { Long.MAX_VALUE }
            } else last.wall + (following.sent - last.received).coerceAtLeast(0) + 20_000_000
            lower to upper
        })
        val activeEpoch = epoch
        val closed = closures.toMap()
        return { source, host ->
            map(source, host, true, null, ranges.matches(source).map { groups[it] }, indexes, next, activeEpoch, closed, reference)
        }
    }

    private fun map(source: Long, host: Long, log: Boolean, hint: Int?,
                    groups: Collection<List<ClockSample>> = samples.filter { it.valid }.groupBy { it.epoch }.values,
                    indexes: Map<Int, ClockSourceIndex>? = null, nextSamples: Map<Int, ClockSample?>? = null,
                    activeEpoch: Int = epoch, closed: Map<Int, Long> = closures, reference: Int = 0): MappedTime {
        val candidates = mutableListOf<MappedTime>()
        groups.forEach { group ->
            val first = group.first()
            val last = group.last()
            if (!log && hint != null && hint != first.epoch) return@forEach
            val sourceOf: (ClockSample) -> Long = { if (log) it.wall else it.mono }
            val next = if (nextSamples == null) samples.firstOrNull { it.valid && it.epoch > last.epoch } else nextSamples[first.epoch]
            // For the active epoch, bound progress from the earliest possible measurement between sent and received.
            // The stale check below still measures five seconds from receipt, independently of source-clock error.
            val upper = if (next == null) sourceOf(last) + (host - last.sent).coerceIn(0,
                5_000_000_000 + (last.received - last.sent)) else
                sourceOf(last) + (next.sent - last.received).coerceAtLeast(0)
            if (source < sourceOf(first) - 20_000_000 || source > upper + 20_000_000) return@forEach
            val index = indexes?.get(first.epoch)
            val low = index?.low(source) ?: group.lastOrNull { sourceOf(it) <= source } ?: first
            val high = index?.high(source) ?: group.firstOrNull { sourceOf(it) >= source } ?: last
            val offsetOf: (ClockSample) -> Long = { if (log) it.wallOffset else it.monoOffset }
            val delta = sourceOf(high) - sourceOf(low)
            val interpolated = if (delta > 0) offsetOf(low) +
                ((offsetOf(high) - offsetOf(low)).toDouble() * (source - sourceOf(low)).toDouble() / delta).toLong() else offsetOf(low)
            val error = maxOf(low.readError, high.readError) + abs(offsetOf(high) - offsetOf(low))
            val stale = next == null && host - last.received > 5_000_000_000
            val boundary = first.epoch < activeEpoch && (source > sourceOf(last) ||
                log && closed[first.epoch]?.let { host > it } == true) || first.epoch > 0 && source < sourceOf(first)
            val elapsed = if (stale || boundary || error > 20_000_000) null else Math.addExact(source, interpolated)
            // Another boot keeps its source and BOOTTIME but has no exact place on the reference boot's timeline.
            val sequence = if (elapsed != null && last.segment == reference) Math.addExact(elapsed, last.sequenceOffset) else null
            candidates += MappedTime(elapsed, first.epoch, if (sequence == null) Long.MAX_VALUE else error, sequence)
        }
        return candidates.singleOrNull() ?: MappedTime(null, hint ?: activeEpoch, Long.MAX_VALUE, null)
    }

    @Synchronized
    fun clear() { samples.clear(); closures.clear(); epoch = 0; latestCache = null }

    @Synchronized
    fun boundary(host: Long? = null) {
        closures[epoch] = host ?: samples.lastOrNull { it.valid }?.received ?: System.nanoTime()
        epoch++
        while (closures.size > 4096) closures.remove(closures.keys.first())
    }

    @Synchronized
    fun certain(host: Long = System.nanoTime()): Boolean = samples.peekLast()?.let {
        it.valid && it.epoch == epoch && host - it.received <= 5_000_000_000
    } == true

    @Synchronized
    fun currentEpoch(): Int = epoch

    @Synchronized
    fun endUncertainty(host: Long = System.nanoTime()): Long = samples.peekLast()?.let {
        if (it.epoch != epoch || host - it.received > 5_000_000_000) Long.MAX_VALUE else
            anchorFor(it, host).let { anchor -> anchor.readError + (anchor.received - anchor.sent) / 2 }
    } ?: Long.MAX_VALUE

    @Synchronized
    fun snapshot(): List<ClockSample> = samples.toList()

    private var latestCache: ((Long) -> Long?)? = null

    @Synchronized
    fun latestSequence(): (Long) -> Long? {
        latestCache?.let { return it }
        return latestSequence(samples.toList()).also { latestCache = it }
    }
}

/**
 * Latest sequence a record received by the host at `host` can have: a clock read requested after that moment ran on the
 * device after the record was produced. This bounds records whose own mapping is unknown (e.g. across a clock epoch).
 * The first read of a later boot gives no bound: a record received before it may belong to the previous boot, whose
 * position on this axis is only approximate.
 */
internal fun latestSequence(samples: List<ClockSample>): (Long) -> Long? {
    val sorted = samples.sortedBy { it.sent }
    val sent = LongArray(sorted.size) { sorted[it].sent }
    val bound = arrayOfNulls<Long>(sorted.size)
    var min: Long? = null
    for (i in sorted.indices.reversed()) {
        val sample = sorted[i]
        if (sorted.getOrNull(i + 1)?.segment != sample.segment) min = null
        val upper = try { Math.addExact(sample.before, sample.sequenceOffset) } catch (_: ArithmeticException) { null }
        if (upper != null && (min == null || upper < min)) min = upper
        bound[i] = if (i > 0 && sorted[i - 1].segment != sample.segment) null else min
    }
    return { host ->
        var low = 0
        var high = sent.size
        while (low < high) { val mid = (low + high) ushr 1; if (sent[mid] <= host) low = mid + 1 else high = mid }
        bound.getOrNull(low)
    }
}

/** Sorted keys retain insertion-order extrema, including duplicate or locally decreasing wall clocks. */
internal class ClockSourceIndex(private val samples: List<ClockSample>, sourceOf: (ClockSample) -> Long = { it.wall }) {
    private val keys = LongArray(samples.size)
    private val lastBefore = IntArray(samples.size)
    private val firstAfter = IntArray(samples.size)

    init {
        val order = samples.indices.sortedBy { sourceOf(samples[it]) }
        var last = 0
        for (i in order.indices) {
            keys[i] = sourceOf(samples[order[i]])
            last = maxOf(last, order[i]); lastBefore[i] = last
        }
        var first = samples.lastIndex
        for (i in order.indices.reversed()) { first = minOf(first, order[i]); firstAfter[i] = first }
    }

    fun low(source: Long): ClockSample {
        val end = bound(source, true)
        return samples[if (end == 0) 0 else lastBefore[end - 1]]
    }

    fun high(source: Long): ClockSample {
        val start = bound(source, false)
        return samples[if (start == keys.size) samples.lastIndex else firstAfter[start]]
    }

    fun after(source: Long): ClockSample? {
        val start = bound(source, true)
        return if (start == keys.size) null else samples[firstAfter[start]]
    }

    private fun bound(source: Long, inclusive: Boolean): Int {
        var low = 0
        var high = keys.size
        while (low < high) {
            val mid = low + (high - low) / 2
            if (keys[mid] < source || inclusive && keys[mid] == source) low = mid + 1 else high = mid
        }
        return low
    }
}

/** Prune unsupported epochs while retaining the original candidate order and ambiguity checks. */
internal class ClockEpochIndex(ranges: List<Pair<Long, Long>>) {
    private val order = ranges.indices.sortedBy { ranges[it].first }
    private val lower = LongArray(ranges.size) { ranges[order[it]].first }
    private val upper = LongArray(ranges.size) { ranges[order[it]].second }
    private val maximum = LongArray(ranges.size * 4)

    init { if (ranges.isNotEmpty()) build(1, 0, ranges.size) }

    private fun build(node: Int, from: Int, until: Int): Long {
        val value = if (until - from == 1) upper[from] else {
            val mid = from + (until - from) / 2
            maxOf(build(node * 2, from, mid), build(node * 2 + 1, mid, until))
        }
        maximum[node] = value
        return value
    }

    fun matches(source: Long): List<Int> {
        val found = mutableListOf<Int>()
        if (order.isNotEmpty()) collect(1, 0, order.size, source, found)
        found.sort()
        return found
    }

    private fun collect(node: Int, from: Int, until: Int, source: Long, found: MutableList<Int>) {
        if (source < lower[from] || source > maximum[node]) return
        if (until - from == 1) {
            found += order[from]
        } else {
            val mid = from + (until - from) / 2
            collect(node * 2, from, mid, source, found)
            collect(node * 2 + 1, mid, until, source, found)
        }
    }
}
