package io.github.shinma06.replaybuffer.core

import java.util.ArrayDeque
import kotlin.math.abs

/**
 * Device elapsed(BOOTTIME)/mono/wall and host send/receive values are nanoseconds from their separate clocks.
 * [region] numbers the continuous offset runs of the source clocks (a clock step or boot starts a new one); [segment]
 * counts device boots and [segmentOffset] places this boot's BOOTTIME on the sequence axis. Only segment 0 is exact; a
 * later boot's placement is a host-time lower bound for ordering and never an event time.
 */
internal data class ClockSample(
    val region: Int, val boot: String, val before: Long, val mono: Long, val wall: Long, val after: Long,
    val sent: Long, val received: Long, val segmentOffset: Long, val segment: Int = 0,
) {
    val elapsed: Long get() = before + (after - before) / 2
    val monoOffset: Long get() = elapsed - mono
    val wallOffset: Long get() = elapsed - wall
    val readError: Long get() = (after - before) / 2 + 1_000_000
    /** Supports offsets and mapping. Offsets are read on the device, so the adb round trip does not affect them. */
    val valid: Boolean get() = after - before <= 2_000_000
    /** Also supports host-to-device estimates (T, barriers, start bounds), whose error includes half the round trip. */
    val hostAnchored: Boolean get() = valid && received - sent <= 40_000_000
}

/** Nanosecond correspondence: null times and Long.MAX_VALUE uncertainty preserve unsupported/ambiguous mapping. */
internal data class MappedTime(
    val elapsed: Long?, val region: Int, val uncertainty: Long, val sequence: Long? = elapsed,
)

/**
 * Device BOOTTIME of the current boot is the timeline; MONOTONIC and REALTIME sources map onto it by measured offsets.
 * Host time only schedules reads, judges staleness, estimates T and orders boots approximately.
 */
internal class CaptureClock {
    private val samples = ArrayDeque<ClockSample>()
    private var region = 0

    @Synchronized
    fun add(boot: String, values: List<Long>, sent: Long, received: Long): Boolean {
        require(values.size == 4 && boot.length in 1..64)
        val (before, mono, wall, after) = values
        require(before >= 0 && after >= before && mono >= 0 && wall >= 0 && received >= sent)
        val last = samples.peekLast()
        // Offsets are compared only within this boot; a new boot is a boundary even when its first read is too wide.
        val measured = samples.lastOrNull { it.after - it.before <= 2_000_000 && it.segment == last?.segment }
        val elapsed = before + (after - before) / 2
        val good = after - before <= 2_000_000
        val bootChanged = last != null && (last.boot != boot || elapsed < last.elapsed)
        val sourceBreak = after - before <= 2_000_000 && measured != null &&
            (boot != measured.boot || elapsed < measured.elapsed ||
                abs(elapsed - mono - measured.monoOffset) > measured.readError + (after - before) / 2 + 1_000_000 ||
                abs(elapsed - wall - measured.wallOffset) > measured.readError + (after - before) / 2 + 1_000_000)
        // A measurement outage alone is no boundary: the next good read decides continuity from the device offsets.
        if (sourceBreak || bootChanged) startRegion()
        // A new boot starts its own exact segment. Host time cannot run faster than real time, so placing it after the
        // previous read by the host gap only orders boots; rows of another boot never get a time on this segment.
        val offset = when {
            last == null -> -elapsed
            !bootChanged -> last.segmentOffset
            else -> Math.addExact(last.elapsed + last.segmentOffset, (sent - last.received).coerceAtLeast(0)) - elapsed
        }
        val segment = if (last == null) 0 else if (bootChanged) last.segment + 1 else last.segment
        samples += ClockSample(region, boot, before, mono, wall, after, sent, received, offset, segment)
        while (samples.size > 4096) {
            // A still-displayed frame may be older than the recent clock ring. Keep its region's original anchor.
            val first = samples.peekFirst()
            if (first.region == region && first.valid) {
                samples.removeFirst(); samples.removeFirst(); samples.addFirst(first)
            } else samples.removeFirst()
        }
        latestCache = null
        trackCache = null
        return good
    }

    @Synchronized
    fun now(host: Long = System.nanoTime()): Long? {
        val latest = samples.peekLast() ?: return null
        val anchor = anchorFor(latest, host)
        return maxOf(latest.elapsed + latest.segmentOffset,
            Math.addExact(anchor.elapsed + anchor.segmentOffset, (host - anchor.received).coerceAtLeast(0)))
    }

    /** Newest round-trip-supported sample of [latest]'s region received within 5 s before [host], else [latest] itself. */
    private fun hostSample(latest: ClockSample, host: Long): ClockSample = if (!latest.valid) latest else
        samples.lastOrNull { it.hostAnchored && it.region == latest.region && host - it.received <= 5_000_000_000 } ?: latest

    /** Host-to-device anchor for T at [host]: the shortest round trip within 100 ms of [hostSample]. */
    private fun anchorFor(latest: ClockSample, host: Long): ClockSample = hostSample(latest, host).let { recent ->
        if (!recent.hostAnchored) recent else
            samples.filter { it.hostAnchored && it.region == recent.region && recent.received - it.received in 0 until 100_000_000 }
                .minBy { it.received - it.sent }
    }

    /** Sample for extrapolating device time from host time, or null before the first usable sample. */
    @Synchronized
    fun hostAnchor(host: Long = System.nanoTime()): ClockSample? = samples.lastOrNull { it.valid }?.let { hostSample(it, host) }

    @Synchronized
    fun video(pts: Long, host: Long, hint: Int? = null, reference: Int? = null): MappedTime =
        map(Math.multiplyExact(pts, 1000), host, false, hint ?: region, reference ?: segment())

    /** Device CLOCK_MONOTONIC source time in nanoseconds (for example evdev input times), mapped like video PTS. */
    @Synchronized
    fun mono(ns: Long, host: Long, hint: Int? = null): MappedTime = map(ns, host, false, hint ?: region, segment())

    @Synchronized
    fun log(wall: Long, host: Long): MappedTime = map(wall, host, true, null, segment())

    /** Boot segment of [region] (default: the newest sample), the reference segment whose rows get sequence times. */
    @Synchronized
    fun segment(region: Int? = null): Int =
        (if (region == null) samples.peekLast() else samples.lastOrNull { it.region == region })?.segment ?: 0

    /** Same result as [mono] at this moment for many rows: tracks and indexes are built once, not per row. */
    @Synchronized
    fun monoMapper(reference: Int = segment()): (Long, Long, Int?) -> MappedTime {
        val byRegion = tracks(samples.toList(), false).associateBy { it.region }
        val activeRegion = region
        return { source, host, hint ->
            val target = hint ?: activeRegion
            convert(source, host, listOfNotNull(byRegion[target]), target, activeRegion, reference)
        }
    }

    /** Captures samples and tracks once; later clock updates cannot change this log conversion. */
    @Synchronized
    fun logMapper(reference: Int = segment()): (Long, Long) -> MappedTime {
        val fixed = tracks(samples.toList(), true)
        val ranges = SourceRangeIndex(fixed.map { it.range })
        val activeRegion = region
        return { source, host -> convert(source, host, ranges.matches(source).map { fixed[it] }, null, activeRegion, reference) }
    }

    private fun tracks(all: List<ClockSample>, log: Boolean): List<OffsetTrack> {
        val groups = all.filter { it.valid }.groupBy { it.region }.values.toList()
        return groups.mapIndexed { i, group -> OffsetTrack(group, log, groups.getOrNull(i + 1)?.first()) }
    }

    private var trackCache: Pair<List<OffsetTrack>, List<OffsetTrack>>? = null

    private fun map(source: Long, host: Long, log: Boolean, hint: Int?, reference: Int): MappedTime {
        val cached = trackCache ?: (tracks(samples.toList(), false) to tracks(samples.toList(), true)).also { trackCache = it }
        val all = if (log) cached.second else cached.first
        return convert(source, host, if (log) all else all.filter { it.region == hint }, hint, region, reference)
    }

    private fun convert(source: Long, host: Long, candidates: List<OffsetTrack>, hint: Int?, activeRegion: Int,
                        reference: Int): MappedTime =
        candidates.mapNotNull { it.map(source, host, activeRegion, reference) }.singleOrNull()
            ?: MappedTime(null, hint ?: activeRegion, Long.MAX_VALUE, null)

    @Synchronized
    fun clear() { samples.clear(); region = 0; latestCache = null; trackCache = null }

    /** A source clock may have stepped (offset jump, boot, non-monotonic PTS): later samples open a new region. */
    @Synchronized
    fun startRegion() {
        region++
        trackCache = null
    }

    @Synchronized
    fun certain(host: Long = System.nanoTime()): Boolean = samples.peekLast()?.let {
        it.valid && it.region == region && host - it.received <= 5_000_000_000
    } == true

    @Synchronized
    fun currentRegion(): Int = region

    @Synchronized
    fun endUncertainty(host: Long = System.nanoTime()): Long = samples.peekLast()?.let {
        if (it.region != region || host - it.received > 5_000_000_000) Long.MAX_VALUE else
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
 * device after the record was produced. This bounds records whose own mapping is unknown (e.g. across a clock region).
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
        val upper = try { Math.addExact(sample.before, sample.segmentOffset) } catch (_: ArithmeticException) { null }
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

/**
 * One continuous offset run of a device source clock (CLOCK_MONOTONIC for video/input, REALTIME for logs) against
 * BOOTTIME: the valid samples of one clock region. [next] is the first sample after the run. A run is closed once a
 * later region starts; its last sample's receipt is the last host time that can still belong to it.
 */
internal class OffsetTrack(val samples: List<ClockSample>, private val log: Boolean, private val next: ClockSample?) {
    val region = samples.first().region
    private val first = samples.first()
    private val last = samples.last()
    private val index by lazy { ClockSourceIndex(samples) { source(it) } }

    private fun source(sample: ClockSample) = if (log) sample.wall else sample.mono
    private fun offset(sample: ClockSample) = if (log) sample.wallOffset else sample.monoOffset

    /** Conservative source range for any host time; it keeps wrapped arithmetic and the invalid-limit exception. */
    val range: Pair<Long, Long> by lazy {
        val limit = 5_000_000_000 + (last.received - last.sent)
        val lower = if (next == null && limit < 0) Long.MIN_VALUE else source(first) - 20_000_000
        val upper = if (next == null) {
            if (limit < 0) Long.MAX_VALUE else try {
                Math.addExact(Math.addExact(source(last), limit), 20_000_000)
            } catch (_: ArithmeticException) { Long.MAX_VALUE }
        } else source(last) + (next.sent - last.received).coerceAtLeast(0) + 20_000_000
        lower to upper
    }

    /** This run's reading of [source], or null when [source] cannot belong to it. */
    fun map(source: Long, host: Long, activeRegion: Int, reference: Int): MappedTime? {
        // For the open run, bound progress from the earliest possible measurement between sent and received.
        // The stale check below still measures five seconds from receipt, independently of source-clock error.
        val upper = if (next == null) source(last) + (host - last.sent).coerceIn(0,
            5_000_000_000 + (last.received - last.sent)) else
            source(last) + (next.sent - last.received).coerceAtLeast(0)
        if (source < source(first) - 20_000_000 || source > upper + 20_000_000) return null
        val low = index.low(source)
        val high = index.high(source)
        val delta = source(high) - source(low)
        val interpolated = if (delta > 0) offset(low) +
            ((offset(high) - offset(low)).toDouble() * (source - source(low)).toDouble() / delta).toLong() else offset(low)
        val error = maxOf(low.readError, high.readError) + abs(offset(high) - offset(low))
        val stale = next == null && host - last.received > 5_000_000_000
        val boundary = region < activeRegion && (source > source(last) ||
            log && host > last.received) || region > 0 && source < source(first)
        val elapsed = if (stale || boundary || error > 20_000_000) null else Math.addExact(source, interpolated)
        // Another boot keeps its source and BOOTTIME but has no exact place on the reference boot's timeline.
        val sequence = if (elapsed != null && last.segment == reference) Math.addExact(elapsed, last.segmentOffset) else null
        return MappedTime(elapsed, region, if (sequence == null) Long.MAX_VALUE else error, sequence)
    }
}

/** Indexes of the ranges holding a source time, in range order; prunes thousands of unsupported tracks. */
internal class SourceRangeIndex(ranges: List<Pair<Long, Long>>) {
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
