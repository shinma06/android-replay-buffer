package io.github.shinma06.replaybuffer.core

import java.nio.ByteBuffer
import java.nio.channels.FileChannel
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardOpenOption.CREATE_NEW
import java.nio.file.StandardOpenOption.WRITE
import java.util.ArrayDeque
import java.util.UUID

internal data class VideoEntry(
    val file: Path, val offset: Long, val size: Int, val pts: Long, val key: Boolean,
    val width: Int, val height: Int, val generation: Long, val time: MappedTime,
    val retainedAt: Long, val config: ByteArray, val host: Long, val session: Long,
)
internal data class LogEntry(
    val id: String, val source: DeviceLog, val generation: Long, val time: MappedTime,
    val retainedAt: Long, val app: Boolean?, val host: Long,
)
internal data class AppPeriod(
    val packageName: String?, val uid: Long?, val pids: Set<Int>, val from: Long?, val to: Long?, val epoch: Int,
    val uidExclusive: Boolean = false,
)
internal data class FrozenCapture(
    val id: String, val sequence: String, val generation: Long, val start: Long, val end: Long,
    val seconds: Int, val video: List<VideoEntry>, val logs: List<LogEntry>, val gaps: List<CaptureGap>,
    val clocks: List<ClockSample>, val apps: List<AppPeriod>, val settings: ReplaySettings,
    val states: Map<String, StreamSnapshot>, val device: ReplayDevice? = null,
    val endUncertainty: Long = 0, val windowClockEpoch: Int? = null,
) {
    val empty: Boolean get() = video.isEmpty() && logs.isEmpty()
    val windowKnown: Boolean get() = endUncertainty != Long.MAX_VALUE
}

/** Packet bytes are append-only; a frozen request reads exactly each indexed packet's original length. */
internal class CaptureStore(
    val directory: Path,
    val clock: CaptureClock = CaptureClock(),
    private val videoLimit: Long = ReplaySettings.VIDEO_BYTES,
    private val logLimit: Long = ReplaySettings.LOG_BYTES,
    private val minFree: Long = ReplaySettings.MIN_FREE_BYTES,
) : AutoCloseable {
    val sequence: String = UUID.randomUUID().toString()
    private val video = ArrayDeque<VideoEntry>()
    private val logs = ArrayDeque<LogEntry>()
    private val gaps = ArrayDeque<CaptureGap>()
    private val apps = ArrayDeque<AppPeriod>()
    private val states = mutableMapOf("video" to StreamSnapshot(), "device_log" to StreamSnapshot(), "app_log" to StreamSnapshot())
    private var channel: FileChannel? = null
    private var currentFile: Path? = null
    private var config = byteArrayOf()
    private var width = 0
    private var height = 0
    private var previousPts = -1L
    private var videoSession = 0L
    private var logBytes = 0L
    private var videoBytes = 0L
    private var configBytes = 0L
    private val configRefs = java.util.IdentityHashMap<ByteArray, Int>()
    private var started: Long? = null
    private var fixedEnd: Long? = null
    private var fixedUncertainty: Long? = null
    private var fixedEpoch: Int? = null
    private var generation = 0L
    private var closed = false
    private var pinned: FrozenCapture? = null
    private val ownedFiles = mutableSetOf<Path>()

    init { Files.createDirectories(directory) }

    @Synchronized
    fun generation(value: Long) {
        generation = value
        channel?.close()
        channel = null
        currentFile = null
        previousPts = -1
        config = byteArrayOf()
        states.replaceAll { _, _ -> StreamSnapshot(StreamState.RECOVERING) }
    }

    @Synchronized
    fun status(kind: String, state: StreamState, reason: String?, owner: Long) {
        if (closed || owner != generation) return
        val old = states.getValue(kind)
        if (state != StreamState.CAPTURING && old.state == StreamState.CAPTURING) {
            addGap(kind, clock.now(), null, reason ?: "取得が中断しました")
        }
        if (state == StreamState.CAPTURING && old.state != state) {
            val index = gaps.lastOrNull { it.stream == kind && it.toNs == null }
            if (index != null) { gaps.remove(index); gaps.add(index.copy(toNs = clock.now())) }
        }
        states[kind] = old.copy(state = state, reason = reason)
    }

    @Synchronized
    fun clockStatus(healthy: Boolean, owner: Long) {
        if (closed || owner != generation) return
        if (!healthy && gaps.none { it.stream == "clock" && it.toNs == null }) {
            val anchor = clock.snapshot().lastOrNull { it.valid }
            addGap("clock", anchor?.sequenceOffset?.let { anchor.elapsed + it }, null, "時計測定が不達または正常条件を満たしません")
        } else if (healthy) {
            val gap = gaps.lastOrNull { it.stream == "clock" && it.toNs == null }
            if (gap != null) { gaps.remove(gap); gaps.add(gap.copy(toNs = clock.now())) }
        }
    }

    @Synchronized
    fun session(packet: VideoPacket.Session, owner: Long) {
        if (closed || owner != generation) return
        status("video", StreamState.RECOVERING, "動画sessionを準備しています", owner)
        channel?.close()
        channel = null
        currentFile = null
        videoSession++
        width = packet.width
        height = packet.height
        previousPts = -1
        config = byteArrayOf()
    }

    @Synchronized
    fun frame(packet: VideoPacket.Frame, owner: Long, host: Long = System.nanoTime()) {
        if (closed || owner != generation) return
        if (packet.config) {
            require(packet.bytes.size <= ReplaySettings.MAX_CONFIG_PACKET_BYTES) { "動画configが64KiBを超えました" }
            if (!config.contentEquals(packet.bytes)) {
                status("video", StreamState.RECOVERING, "動画configが切り替わりました", owner)
                channel?.close(); channel = null; currentFile = null
            }
            config = packet.bytes.copyOf(); return
        }
        require(width > 0 && height > 0 && config.isNotEmpty()) { "動画の寸法/configがありません" }
        if (packet.pts <= previousPts) { clock.boundary(); error("動画PTSが単調ではありません") }
        previousPts = packet.pts
        if (packet.key) {
            channel?.close()
            currentFile = directory.resolve("gop-${UUID.randomUUID()}.h264")
            channel = FileChannel.open(currentFile, CREATE_NEW, WRITE)
            ownedFiles.add(currentFile!!)
        }
        val out = channel ?: run { addGap("video", clock.now(host), clock.now(host), "IDR待ちでframeを保持できません"); return }
        require(out.position() + packet.bytes.size <= 32L * 1024 * 1024) { "GOPが32MiBを超えました" }
        check(Files.getFileStore(directory).usableSpace >= minFree) { "取得用一時領域の空き容量が不足しています" }
        val time = clock.video(packet.pts, host)
        val retained = time.sequence ?: clock.now(host) ?: 0L
        if (started == null && time.sequence != null) started = retained
        val offset = out.position()
        val data = ByteBuffer.wrap(packet.bytes)
        while (data.hasRemaining()) out.write(data)
        video += VideoEntry(currentFile!!, offset, packet.bytes.size, packet.pts, packet.key, width, height,
            generation, time, retained, config, host, videoSession)
        videoBytes += packet.bytes.size
        val references = configRefs[config] ?: 0
        if (references == 0) configBytes += config.size
        configRefs[config] = references + 1
        enforceVideoLimit()
        status("video", StreamState.CAPTURING, if (time.elapsed == null || time.sequence == null || time.uncertainty > 20_000_000) "動画の時刻対応を確認できません" else null, owner)
    }

    @Synchronized
    fun app(packageName: String?, uid: Long?, pids: Set<Int>, owner: Long, uidExclusive: Boolean = false) {
        if (closed || owner != generation) return
        require(pids.size <= 1024)
        val last = apps.peekLast()
        val anchor = clock.snapshot().lastOrNull { it.valid }
        if (last != null && last.packageName == packageName && last.uid == uid && last.pids == pids && last.uidExclusive == uidExclusive && last.from != null && last.epoch == anchor?.epoch) {
            states["app_log"] = StreamSnapshot(if (uid != null) StreamState.CAPTURING else StreamState.UNAVAILABLE,
                reason = if (uid == null) "対象アプリを解決できません" else null)
            return
        }
        states["app_log"] = StreamSnapshot(if (uid != null) StreamState.CAPTURING else StreamState.UNAVAILABLE,
            reason = if (uid == null) "対象アプリを解決できません" else null)
        val now = anchor?.let { it.elapsed + (System.nanoTime() - it.received).coerceAtLeast(0) }
        if (last != null) { apps.removeLast(); apps.add(last.copy(to = if (last.epoch == anchor?.epoch) now else clock.snapshot().lastOrNull { it.epoch == last.epoch }?.elapsed)) }
        apps += AppPeriod(packageName, uid, java.util.Set.copyOf(pids), now, null, anchor?.epoch ?: 0, uidExclusive)
        while (apps.size > 2048) apps.removeFirst()
        states["app_log"] = StreamSnapshot(if (uid != null) StreamState.CAPTURING else StreamState.UNAVAILABLE,
            reason = if (uid == null) "対象アプリを解決できません" else null)
    }

    @Synchronized
    fun log(source: DeviceLog, owner: Long, host: Long = System.nanoTime()) {
        if (closed || owner != generation) return
        val time = clock.log(source.wall, host)
        val retained = time.sequence ?: clock.now(host) ?: 0L
        if (started == null && time.sequence != null) started = retained
        val period = apps.lastOrNull { it.epoch == time.epoch && time.elapsed != null && it.from != null &&
            time.elapsed >= it.from && (it.to == null || time.elapsed <= it.to) }
        val app = when {
            period?.uid == null || time.elapsed == null -> null
            source.uid == period.uid && period.uidExclusive -> true
            source.pid in period.pids -> source.uid == null || source.uid == period.uid
            source.uid == null || source.uid == period.uid -> null // shared/unknown UID: a new PID is not evidence of non-membership.
            else -> false
        }
        logs += LogEntry(UUID.randomUUID().toString(), source, generation, time, retained, app, host)
        logBytes += source.raw.size + 256
        while (logBytes > logLimit && logs.isNotEmpty()) {
            val lost = logs.removeFirst()
            logBytes -= lost.source.raw.size + 256
            addGap("device_log", lost.time.sequence, lost.time.sequence, "ログのbyte上限でrecordを失いました")
        }
        status("device_log", StreamState.CAPTURING, if (time.elapsed == null || time.sequence == null || time.uncertainty > 20_000_000) "ログの時刻対応を確認できません" else null, owner)
    }

    @Synchronized
    fun freeze() {
        if (fixedEnd == null) synchronized(clock) {
            fixedEnd = clock.now() ?: latest()
            fixedUncertainty = clock.endUncertainty()
            fixedEpoch = clock.currentEpoch()
        }
    }
    @Synchronized
    fun resume() { fixedEnd = null; fixedUncertainty = null; fixedEpoch = null }
    @Synchronized
    fun end(): Long? = fixedEnd ?: clock.now() ?: latest()
    @Synchronized
    fun frozen(): Boolean = fixedEnd != null

    private fun latest(): Long? = listOfNotNull(video.peekLast()?.retainedAt, logs.peekLast()?.retainedAt).maxOrNull()

    @Synchronized
    fun prune(seconds: Int) {
        if (closed || fixedEnd != null) return
        val end = end() ?: return
        val uncertainty = clock.endUncertainty()
        // An unknown boot bridge/current T cannot prove that old known-epoch records are outside the window.
        if (uncertainty == Long.MAX_VALUE) {
            enforceVideoLimit()
            deleteUnused()
            return
        }
        val cutoff = end - seconds * 1_000_000_000L - uncertainty
        // Keep the complete preceding GOP for decoding the first frame inside the logical window.
        val firstInside = video.indexOfFirst { it.time.sequence == null || it.time.uncertainty == Long.MAX_VALUE || it.retainedAt >= cutoff - it.time.uncertainty }
        if (firstInside > 0) {
            val all = video.toList()
            val first = if (continuousAcrossCut(all[firstInside - 1], all[firstInside], cutoff)) firstInside - 1 else firstInside
            val key = (first downTo 0).firstOrNull { all[it].key } ?: 0
            repeat(key) { removeVideo() }
        } else if (firstInside < 0) {
            val all = video.toList()
            val last = all.lastOrNull()?.takeIf { currentVideo(it) }
            val key = if (last == null) all.size else (all.lastIndex downTo 0).firstOrNull { all[it].key } ?: 0
            repeat(key) { removeVideo() }
        }
        while (logs.isNotEmpty() && logs.peekFirst().time.sequence != null && logs.peekFirst().time.uncertainty != Long.MAX_VALUE &&
            logs.peekFirst().retainedAt < cutoff - logs.peekFirst().time.uncertainty) {
            logBytes -= logs.removeFirst().source.raw.size + 256
        }
        enforceVideoLimit()
        deleteUnused()
        while (gaps.size > 4096) gaps.removeFirst()
    }

    @Synchronized
    fun capture(settings: ReplaySettings): FrozenCapture? = synchronized(clock) {
        check(pinned == null) { "保存対象は既に固定されています" }
        val end = end() ?: return null
        val uncertainty = fixedUncertainty ?: clock.endUncertainty()
        val known = uncertainty != Long.MAX_VALUE
        val start = maxOf(0L, started ?: end, end - settings.replaySeconds * 1_000_000_000L)
        val rows = logs.filter { !known || it.time.sequence == null || it.retainedAt in start..end }.map { row ->
            row.copy(time = clock.log(row.source.wall, row.host))
        }
        val inside = video.filter { !known || it.time.sequence == null || it.retainedAt <= end && it.retainedAt >= start }
        val files = inside.map { it.file }.toMutableSet()
        val all = video.toList()
        inside.forEach { entry ->
            val index = all.indexOf(entry)
            if (index > 0 && continuousAcrossCut(all[index - 1], entry, start)) files.add(all[index - 1].file)
        }
        all.lastOrNull()?.takeIf { known && it.retainedAt < start && currentVideo(it) }?.let { files.add(it.file) }
        val frames = video.filter { (!known || it.retainedAt <= end) && it.file in files }.map { frame ->
            frame.copy(time = clock.video(frame.pts, frame.host, frame.time.epoch))
        }
        val value = FrozenCapture(UUID.randomUUID().toString(), sequence, generation, start, end,
            settings.replaySeconds, frames.toList(), rows.toList(), gaps.toList(), clock.snapshot(), apps.toList(), settings,
            streams(settings.replaySeconds, end), endUncertainty = uncertainty, windowClockEpoch = fixedEpoch ?: clock.currentEpoch())
        if (value.empty) return null
        pinned = value
        return value
    }

    @Synchronized
    fun release(id: String) {
        if (pinned?.id == id) { pinned = null; deleteUnused() }
    }

    @Synchronized
    fun streams(seconds: Int = 180, atEnd: Long? = null): Map<String, StreamSnapshot> = states.mapValues { (kind, value) ->
        val end = atEnd ?: end() ?: 0
        val cutoff = maxOf(0, end - seconds * 1_000_000_000L)
        val times = if (kind == "video") video.mapNotNull { it.time.sequence }.filter { it >= cutoff } else
            logs.filter { kind == "device_log" || it.app == true }.mapNotNull { it.time.sequence }.filter { it >= cutoff }
        val span = if (times.isEmpty()) 0L else (times.max() - times.min()).coerceAtLeast(0)
        val missing = gaps.filter { it.stream == kind }.sumOf { gap ->
            val from = gap.fromNs ?: return@sumOf 0L
            val to = gap.toNs ?: end
            (minOf(to, end) - maxOf(from, cutoff)).coerceAtLeast(0)
        }
        val effective = if (kind == "app_log" && value.state == StreamState.CAPTURING && states.getValue("device_log").state != StreamState.CAPTURING)
            value.copy(state = states.getValue("device_log").state, reason = states.getValue("device_log").reason) else value
        val uncertain = !clock.certain()
        effective.copy(availableSeconds = (span - missing).coerceAtLeast(0) / 1e9,
            reason = effective.reason ?: if (uncertain && effective.state == StreamState.CAPTURING) "時計対応を確認できません" else null,
            gaps = java.util.List.copyOf(gaps.filter { gap ->
                (gap.stream == kind || gap.stream == "clock" || kind == "app_log" && gap.stream == "device_log") &&
                    gap.intersects(windowStart(seconds, end), end, fixedUncertainty ?: clock.endUncertainty())
            }))
    }

    @Synchronized
    fun windowStart(seconds: Int, atEnd: Long? = null): Long? = (atEnd ?: end())?.let {
        maxOf(0L, started ?: it, it - seconds * 1_000_000_000L)
    }

    @Synchronized
    fun hasData(): Boolean = video.isNotEmpty() || logs.isNotEmpty()

    private fun continuousAcrossCut(a: VideoEntry, b: VideoEntry, cut: Long): Boolean =
        a.time.sequence != null && b.time.sequence != null && a.time.sequence < cut && b.time.sequence >= cut &&
            a.continuousTo(b, gaps)

    private fun currentVideo(frame: VideoEntry): Boolean = frame.generation == generation && frame.session == videoSession &&
        frame.time.epoch == clock.currentEpoch() && frame.width == width && frame.height == height && frame.config.contentEquals(config)

    private fun removeVideo(): VideoEntry {
        val entry = video.removeFirst()
        videoBytes -= entry.size
        val refs = configRefs.getValue(entry.config) - 1
        if (refs == 0) { configRefs.remove(entry.config); configBytes -= entry.config.size } else configRefs[entry.config] = refs
        return entry
    }

    private fun enforceVideoLimit() {
        while (video.isNotEmpty() && (videoBytes > videoLimit || video.size > ReplaySettings.MAX_VIDEO_PACKETS ||
                configBytes > ReplaySettings.CONFIG_MEMORY_BYTES)) {
            val entry = removeVideo()
            addGap("video", entry.time.sequence, entry.time.sequence, "動画のbyte/packet/config上限でframeを失いました")
        }
    }

    private fun addGap(stream: String, from: Long?, to: Long?, reason: String, boundary: MappedTime? = null) {
        if (gaps.peekLast()?.let { it.stream == stream && it.reason == reason && it.toNs == to } == true) return
        val anchor = clock.snapshot().lastOrNull { it.valid }
        val uncertainty = boundary?.uncertainty ?: anchor?.let { if (System.nanoTime() - it.received > 5_000_000_000 || it.bridgeError == Long.MAX_VALUE) Long.MAX_VALUE else
            it.readError + (it.received - it.sent) / 2 + it.bridgeError }
        gaps += CaptureGap(stream, from, to, reason, if (boundary != null || clock.certain()) uncertainty else Long.MAX_VALUE,
            generation, boundary?.epoch ?: clock.currentEpoch())
        while (gaps.size > 4096) gaps.removeFirst()
    }

    private fun deleteUnused() {
        val retain = video.map { it.file }.toSet() + pinned?.video.orEmpty().map { it.file } + listOfNotNull(currentFile)
        ownedFiles.filter { it !in retain }.forEach { Files.deleteIfExists(it); ownedFiles.remove(it) }
    }

    @Synchronized
    override fun close() {
        closed = true
        channel?.close()
        channel = null
        currentFile = null
        pinned = null
        video.clear()
        logs.clear()
        apps.clear()
        gaps.clear()
        configRefs.clear()
        config = byteArrayOf()
        videoBytes = 0
        configBytes = 0
        logBytes = 0
        clock.clear()
        deleteUnused()
        Files.deleteIfExists(directory)
    }
}

/** Source PTS, not an assumed frame rate, defines continuity within an uninterrupted stream. */
internal fun VideoEntry.continuousTo(next: VideoEntry, gaps: Iterable<CaptureGap>): Boolean =
    generation == next.generation && session == next.session && time.epoch == next.time.epoch &&
        width == next.width && height == next.height && config.contentEquals(next.config) && next.pts > pts &&
        time.sequence != null && next.time.sequence != null && next.time.sequence > time.sequence &&
        time.uncertainty != Long.MAX_VALUE && next.time.uncertainty != Long.MAX_VALUE &&
        gaps.none { gap -> (gap.stream == "video" || gap.stream == "clock") &&
            gap.intersects(time.sequence, next.time.sequence, maxOf(time.uncertainty, next.time.uncertainty)) }

/** Frozen inputs only: a later frame, clock sample or retry cannot confirm this request's tail. */
internal fun FrozenCapture.videoTail(): VideoTailSnapshot? {
    val last = video.lastOrNull() ?: return null
    val source = last.time.sequence.takeIf { windowKnown && last.time.uncertainty != Long.MAX_VALUE }
    if (source != null && source >= end) return null
    val boundaries = gaps.filter { (it.stream == "video" || it.stream == "clock") &&
        it.intersects(source, end, maxOf(endUncertainty, last.time.uncertainty)) }
    val boundary = boundaries.mapNotNull { it.fromNs }.minOrNull()?.coerceAtMost(end) ?: end
    val from = source?.let { maxOf(start, it) }
    val to = boundary.takeIf { windowKnown && source != null && it > maxOf(start, source) }
    if (from != null && to == null) return null
    val held = from != null && to != null && last.time.epoch == windowClockEpoch &&
        boundaries.all { it.fromNs != null && it.fromNs >= to && it.boundaryUncertaintyNs != null && it.boundaryUncertaintyNs != Long.MAX_VALUE } &&
        (states["video"]?.state == StreamState.CAPTURING || boundaries.isNotEmpty())
    return VideoTailSnapshot(from, to, last.pts, source, last.time.epoch, last.generation, held)
}

/** Unknown boundaries remain visible; uncertainty can place a nominally outside boundary in the window. */
internal fun CaptureGap.intersects(start: Long?, end: Long, windowUncertainty: Long = 0): Boolean {
    if (start == null || boundaryUncertaintyNs == null || boundaryUncertaintyNs == Long.MAX_VALUE || windowUncertainty == Long.MAX_VALUE) return true
    val margin = (boundaryUncertaintyNs.coerceAtLeast(0) + windowUncertainty.coerceAtLeast(0)).let { if (it < 0) Long.MAX_VALUE else it }
    return (fromNs == null || fromNs <= end || fromNs - end <= margin) &&
        (toNs == null || toNs >= start || start - toNs <= margin)
}

internal fun FrozenCapture.applicationHistory(): List<ApplicationPeriodSnapshot> = java.util.List.copyOf(apps.mapNotNull { app ->
    val offset = if (windowKnown) clocks.lastOrNull { it.valid && it.epoch == app.epoch }?.sequenceOffset else null
    val from = app.from?.let { time -> offset?.let { Math.addExact(time, it) } }
    val to = app.to?.let { time -> offset?.let { Math.addExact(time, it) } }
    if (windowKnown && offset != null && (from != null && from > end || to != null && to < start)) null else
        ApplicationPeriodSnapshot(app.packageName, from?.coerceAtLeast(start), to?.coerceAtMost(end), app.epoch, app.uid != null)
})
