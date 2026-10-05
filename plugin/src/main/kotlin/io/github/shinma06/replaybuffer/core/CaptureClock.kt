package io.github.shinma06.replaybuffer.core

import java.util.ArrayDeque
import kotlin.math.abs

internal data class ClockSample(
    val epoch: Int, val boot: String, val before: Long, val mono: Long, val wall: Long, val after: Long,
    val sent: Long, val received: Long, val hostWall: Long, val sequenceOffset: Long?, val bridgeError: Long,
) {
    val elapsed: Long get() = before + (after - before) / 2
    val monoOffset: Long get() = elapsed - mono
    val wallOffset: Long get() = elapsed - wall
    val readError: Long get() = (after - before) / 2 + 1_000_000
    val valid: Boolean get() = after - before <= 2_000_000 && received - sent <= 40_000_000
}

internal data class MappedTime(
    val elapsed: Long?, val epoch: Int, val uncertainty: Long, val sequence: Long? = elapsed,
)

/** Source clocks remain authoritative. Host time only estimates T and the explicitly uncertain boot bridge. */
internal class CaptureClock {
    private val samples = ArrayDeque<ClockSample>()
    private var epoch = 0
    private var firstHost: Long? = null
    private val closures = linkedMapOf<Int, Long>()

    @Synchronized
    fun add(boot: String, values: List<Long>, sent: Long, received: Long,
            hostWall: Long = System.currentTimeMillis() * 1_000_000): Boolean {
        require(values.size == 4 && boot.length in 1..64)
        val (before, mono, wall, after) = values
        require(before >= 0 && after >= before && mono >= 0 && wall >= 0 && received >= sent)
        val last = samples.peekLast()
        val measured = samples.lastOrNull { it.after - it.before <= 2_000_000 }
        val normal = samples.lastOrNull { it.valid }
        val elapsed = before + (after - before) / 2
        val good = after - before <= 2_000_000 && received - sent <= 40_000_000
        val bootChanged = last != null && (last.boot != boot || elapsed < last.elapsed)
        val sourceBreak = after - before <= 2_000_000 && measured != null &&
            (boot != measured.boot || elapsed < measured.elapsed ||
                abs(elapsed - mono - measured.monoOffset) > measured.readError + (after - before) / 2 + 1_000_000 ||
                abs(elapsed - wall - measured.wallOffset) > measured.readError + (after - before) / 2 + 1_000_000)
        val missingAnchor = good && normal != null && normal.epoch == epoch && sent - normal.received > 5_000_000_000
        if (sourceBreak || missingAnchor) boundary(normal?.received ?: sent)
        val hostDelta = last?.let { sent - it.received }
        val bridgeKnown = last != null && last.sequenceOffset != null && hostDelta != null && hostDelta >= 0 &&
            abs(hostWall - last.hostWall - (received - last.received)) <= 100_000_000
        val offset = when {
            last == null -> -elapsed
            !bootChanged -> last.sequenceOffset
            bridgeKnown -> Math.addExact(last.elapsed + last.sequenceOffset, hostDelta) - elapsed
            else -> null
        }
        val bridgeError = if (!bootChanged) last?.bridgeError ?: 0 else if (bridgeKnown)
            last.bridgeError + last.readError + (received - sent + last.received - last.sent) / 2 else Long.MAX_VALUE
        samples += ClockSample(epoch, boot, before, mono, wall, after, sent, received, hostWall, offset, bridgeError)
        if (firstHost == null) firstHost = sent + (received - sent) / 2
        while (samples.size > 4096) {
            // A still-displayed frame may be older than the recent clock ring. Keep its epoch's original anchor.
            val first = samples.peekFirst()
            if (first.epoch == epoch && first.valid) {
                samples.removeFirst(); samples.removeFirst(); samples.addFirst(first)
            } else samples.removeFirst()
        }
        return good
    }

    @Synchronized
    fun now(host: Long = System.nanoTime()): Long? {
        val latest = samples.peekLast() ?: return null
        val anchor = if (latest.valid) samples.filter { it.valid && it.epoch == latest.epoch && latest.received - it.received < 100_000_000 }
            .minBy { it.received - it.sent } else latest
        return if (anchor.sequenceOffset == null) firstHost?.let { (host - it).coerceAtLeast(0) } else
            maxOf(latest.elapsed + latest.sequenceOffset!!,
                Math.addExact(anchor.elapsed + anchor.sequenceOffset, (host - anchor.received).coerceAtLeast(0)))
    }

    @Synchronized
    fun video(pts: Long, host: Long, hint: Int? = null): MappedTime = map(Math.multiplyExact(pts, 1000), host, false, hint ?: epoch)

    @Synchronized
    fun log(wall: Long, host: Long): MappedTime = map(wall, host, true, null)

    private fun map(source: Long, host: Long, log: Boolean, hint: Int?): MappedTime {
        val groups = samples.filter { it.valid }.groupBy { it.epoch }.values
        val candidates = mutableListOf<MappedTime>()
        groups.forEach { group ->
            val first = group.first()
            val last = group.last()
            if (!log && hint != null && hint != first.epoch) return@forEach
            val sourceOf: (ClockSample) -> Long = { if (log) it.wall else it.mono }
            val next = samples.firstOrNull { it.valid && it.epoch > last.epoch }
            // For the active epoch, bound progress from the earliest possible measurement between sent and received.
            // The stale check below still measures five seconds from receipt, independently of source-clock error.
            val upper = if (next == null) sourceOf(last) + (host - last.sent).coerceIn(0,
                5_000_000_000 + (last.received - last.sent)) else
                sourceOf(last) + (next.sent - last.received).coerceAtLeast(0)
            if (source < sourceOf(first) - 20_000_000 || source > upper + 20_000_000) return@forEach
            val low = group.lastOrNull { sourceOf(it) <= source } ?: first
            val high = group.firstOrNull { sourceOf(it) >= source } ?: last
            val offsetOf: (ClockSample) -> Long = { if (log) it.wallOffset else it.monoOffset }
            val delta = sourceOf(high) - sourceOf(low)
            val interpolated = if (delta > 0) offsetOf(low) +
                ((offsetOf(high) - offsetOf(low)).toDouble() * (source - sourceOf(low)).toDouble() / delta).toLong() else offsetOf(low)
            val error = maxOf(low.readError, high.readError) + abs(offsetOf(high) - offsetOf(low))
            val stale = next == null && host - last.received > 5_000_000_000
            val boundary = first.epoch < epoch && (source > sourceOf(last) ||
                log && closures[first.epoch]?.let { host > it } == true) || first.epoch > 0 && source < sourceOf(first)
            val elapsed = if (stale || boundary || error > 20_000_000) null else Math.addExact(source, interpolated)
            val sequence = if (elapsed != null && last.sequenceOffset != null) Math.addExact(elapsed, last.sequenceOffset) else null
            candidates += MappedTime(elapsed, first.epoch, if (elapsed == null) Long.MAX_VALUE else
                if (last.bridgeError == Long.MAX_VALUE) Long.MAX_VALUE else error + last.bridgeError, sequence)
        }
        return candidates.singleOrNull() ?: MappedTime(null, hint ?: epoch, Long.MAX_VALUE, null)
    }

    @Synchronized
    fun clear() { samples.clear(); closures.clear(); firstHost = null; epoch = 0 }

    @Synchronized
    fun boundary(host: Long? = null) {
        closures[epoch] = host ?: samples.lastOrNull { it.valid }?.received ?: System.nanoTime()
        epoch++
        while (closures.size > 4096) closures.remove(closures.keys.first())
    }

    @Synchronized
    fun certain(host: Long = System.nanoTime()): Boolean = samples.peekLast()?.let {
        it.valid && it.epoch == epoch && host - it.received <= 5_000_000_000 &&
            it.sequenceOffset != null && it.bridgeError <= 20_000_000
    } == true

    @Synchronized
    fun currentEpoch(): Int = epoch

    @Synchronized
    fun endUncertainty(host: Long = System.nanoTime()): Long = samples.peekLast()?.let {
        if (it.epoch != epoch || it.sequenceOffset == null || it.bridgeError == Long.MAX_VALUE ||
            host - it.received > 5_000_000_000) Long.MAX_VALUE else
            it.readError + (it.received - it.sent) / 2 + it.bridgeError
    } ?: Long.MAX_VALUE

    @Synchronized
    fun snapshot(): List<ClockSample> = samples.toList()
}
