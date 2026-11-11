package io.github.shinma06.replaybuffer.core

import java.nio.ByteBuffer
import java.nio.channels.FileChannel
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardOpenOption.CREATE_NEW
import java.nio.file.StandardOpenOption.WRITE
import java.util.ArrayDeque
import java.util.UUID

/** pts is source µs; offset/size select original packet bytes, while mapped/retention/host times are ns. */
internal data class VideoEntry(
    val file: Path, val offset: Long, val size: Int, val pts: Long, val key: Boolean,
    val width: Int, val height: Int, val generation: Long, val time: MappedTime,
    val retainedAt: Long, val config: ByteArray, val host: Long, val session: Long,
    val decodeRun: Long = 0, val packetOrdinal: Long = 0,
)
internal data class VideoSourcePosition(
    val file: Path, val offset: Long, val generation: Long, val session: Long, val host: Long,
)
/** One AAC access unit; [config] is the AudioSpecificConfig of its server session, [time] maps [pts] like video. */
internal class AudioEntry(val pts: Long, val bytes: ByteArray, val config: ByteArray, val time: MappedTime,
                          val generation: Long, val session: Long, val host: Long) {
    fun mapped(time: MappedTime) = AudioEntry(pts, bytes, config, time, generation, session, host)
}
internal data class VideoCut(val before: VideoSourcePosition?, val after: VideoSourcePosition? = null)
internal data class LogEntry(
    val id: String, val source: DeviceLog, val generation: Long, val time: MappedTime,
    val retainedAt: Long, val app: Boolean?, val host: Long,
)
internal data class AppPeriod(
    val packageName: String?, val uid: Long?, val pids: Set<Int>, val from: Long?, val to: Long?, val epoch: Int,
    val uidExclusive: Boolean = false, val endUnknown: Boolean = false,
)
/**
 * One admitted window on the sequence ns axis, with fixed clocks, membership and original gap history.
 *
 * Packet files are borrowed from the store's pin; keep it until the writer finishes, including failed retries.
 * Later acquisition/clock samples must not reinterpret this request. See docs/plugin-design.md.
 */
internal data class FrozenCapture(
    val id: String, val sequence: String, val generation: Long, val start: Long, val end: Long,
    val seconds: Int, val video: List<VideoEntry>, val logs: List<LogEntry>, val gaps: List<CaptureGap>,
    val clocks: List<ClockSample>, val apps: List<AppPeriod>, val settings: ReplaySettings,
    val states: Map<String, StreamSnapshot>, val device: ReplayDevice? = null,
    val endUncertainty: Long = 0, val windowClockEpoch: Int? = null,
    val clockRecoveries: Map<CaptureGap, ClockSample> = emptyMap(),
    val videoCuts: Map<CaptureGap, VideoCut> = emptyMap(),
    val autoTrigger: AutoTrigger? = null,
    /** Host receive time before which nothing can be in the window when the device clock mapping is unknown. */
    val hostStart: Long? = null,
    val touches: List<TouchEntry> = emptyList(),
    /** Touch reader state at freeze; null when video (and therefore touch capture) was not requested. */
    val touchState: StreamSnapshot? = null,
    val touchGaps: List<TouchGap> = emptyList(),
    val touchDropped: Long = 0,
    /** Whether the visible dots were on during this generation; null before the touch reader decided. */
    val touchDots: TouchDots? = null,
    val audio: List<AudioEntry> = emptyList(),
    /** Audio state at freeze; null when audio was not requested (setting OFF or LOG_ONLY). */
    val audioState: StreamSnapshot? = null,
) {
    val empty: Boolean get() = video.isEmpty() && logs.isEmpty()
    val windowKnown: Boolean get() = endUncertainty != Long.MAX_VALUE
}

/**
 * Owns the disk ring and one save pin. Packet bytes are append-only; snapshots read indexed lengths only.
 *
 * Store mutations are synchronized; combined snapshots acquire this store before its clock.
 * close() removes owned packet files, so the caller must first finish the pinned writer.
 */
internal class CaptureStore(
    val directory: Path,
    val clock: CaptureClock = CaptureClock(),
    private val videoLimit: Long = ReplaySettings.VIDEO_BYTES,
    private val logLimit: Long = ReplaySettings.LOG_BYTES,
    private val minFree: Long = ReplaySettings.MIN_FREE_BYTES,
    private val created: (Path) -> Unit = {},
    private val removed: (Path) -> Unit = {},
    val captureMode: CaptureMode = CaptureMode.VIDEO_AND_LOGS,
    val autosave: CrashAutosave = CrashAutosave(),
    audioRequested: Boolean = false,
) : AutoCloseable {
    private val audioRequested = audioRequested && captureMode.videoRequested
    private val audio = ArrayDeque<AudioEntry>()
    private var audioBytes = 0L
    private var audioConfig: ByteArray? = null
    private var audioSession = 0L
    private var audioPrevious = -1L
    private var audioState = StreamSnapshot(StreamState.WAITING)
    val sequence: String = UUID.randomUUID().toString()
    private val video by lazy { ArrayDeque<VideoEntry>() }
    private val logs = ArrayDeque<LogEntry>()
    private val touches = ArrayDeque<TouchEntry>()
    private val touchGaps = ArrayDeque<TouchGap>()
    /** Open SYN_DROPPED gaps per input device, identified by reference in [touchGaps]. */
    private val touchLostOpen = mutableMapOf<String, TouchGap>()
    private var touchState = StreamSnapshot(StreamState.RECOVERING)
    private var touchDropped = 0L
    /** Latest window time of a row evicted by [MAX_TOUCH_SAMPLES]; MAX_VALUE when that time is unknown. */
    private var touchEvictedThrough: Long? = null
    private var touchDots: TouchDots? = null
    private val gaps = ArrayDeque<CaptureGap>()
    private val clockRecoveries = mutableMapOf<CaptureGap, ClockSample>()
    private val videoCuts = mutableMapOf<CaptureGap, VideoCut>()
    private val apps = ArrayDeque<AppPeriod>()
    private val states = mutableMapOf("video" to StreamSnapshot(if (captureMode.videoRequested) StreamState.WAITING else StreamState.NOT_REQUESTED), "device_log" to StreamSnapshot(), "app_log" to StreamSnapshot())
    private var channel: FileChannel? = null
    private var currentFile: Path? = null
    private var config = byteArrayOf()
    private var width = 0
    private var height = 0
    private var previousPts = -1L
    private var videoSession = 0L
    private var decodeRun = 0L
    private var packetOrdinal = 0L
    private var logRetentionSeconds: Int? = null
    private var logBytes = 0L
    private var videoBytes = 0L
    private var configBytes = 0L
    private val configRefs = java.util.IdentityHashMap<ByteArray, Int>()
    private var started: Long? = null
    private var fixedEnd: Long? = null
    private var fixedUncertainty: Long? = null
    private var fixedEpoch: Int? = null
    private var generation = 0L
    private var generationClockSample: ClockSample? = null
    private var appResolved = false
    private var closed = false
    private var pinned: FrozenCapture? = null
    private val ownedFiles = mutableSetOf<Path>()
    private val videoFileRefs = mutableMapOf<Path, Int>()
    private var pinnedFiles = emptySet<Path>()
    private val unusedCandidates = mutableSetOf<Path>()
    private var reporter: ReporterPoll? = null
    private var autoDevice: String? = null
    private var autoTarget: String? = null

    init {
        PrivateFiles.createDirectory(directory)
        try { created(directory) } catch (failure: Exception) {
            try { Files.delete(directory) } catch (cleanup: Exception) { failure.addSuppressed(cleanup) }
            throw failure
        }
    }

    @Synchronized
    fun generation(value: Long) {
        autosave.refresh(null)
        reporter = null
        decodeRun = Math.incrementExact(decodeRun)
        generation = value
        generationClockSample = clock.snapshot().lastOrNull()
        appResolved = false
        closeCurrentFile()
        previousPts = -1
        config = byteArrayOf()
        states.replaceAll { kind, _ -> StreamSnapshot(if (kind == "video" && !captureMode.videoRequested) StreamState.NOT_REQUESTED else StreamState.RECOVERING) }
        if (captureMode.videoRequested) setTouchState(StreamState.RECOVERING, "取得を復旧しています")
        touchDots = null
        if (audioRequested) {
            if (audioState.state == StreamState.CAPTURING) addGap("audio", clock.now(), null, "端末の接続が切り替わりました")
            audioState = StreamSnapshot(StreamState.WAITING)
            audioConfig = null
        }
    }

    /** Touch samples exist only while video is requested; their time comes from the device input clock. */
    @Synchronized
    fun touch(sample: TouchSample, width: Int, height: Int, owner: Long, host: Long = System.nanoTime()) {
        if (closed || owner != generation || !captureMode.videoRequested) return
        touches += TouchEntry(sample, owner, clock.mono(sample.monoNs, host), host, width, height)
        while (touches.size > MAX_TOUCH_SAMPLES) {
            val evicted = touches.removeFirst().time
            touchDropped++
            val through = if (evicted.uncertainty == Long.MAX_VALUE) null else evicted.sequence?.let { it + evicted.uncertainty }
            touchEvictedThrough = maxOf(touchEvictedThrough ?: Long.MIN_VALUE, through ?: Long.MAX_VALUE)
        }
    }

    /**
     * The kernel dropped input (SYN_DROPPED) of [device] from [fromMono]; the gap stays open until [touchResynced].
     * An unmapped start stays unknown.
     */
    @Synchronized
    fun touchLost(device: String, fromMono: Long, owner: Long, host: Long = System.nanoTime()) {
        if (closed || owner != generation || !captureMode.videoRequested) return
        if (device in touchLostOpen) return
        val gap = TouchGap(touchBound(fromMono, -1, host), null, "端末の入力bufferがあふれ、タップ位置の一部が失われました（SYN_DROPPED）")
        touchGaps += gap
        touchLostOpen[device] = gap
        while (touchGaps.size > 4096) touchGaps.removeFirst()
    }

    /** [device] reported a new pointer after a drop at [toMono], so its input is known again. */
    @Synchronized
    fun touchResynced(device: String, toMono: Long, owner: Long, host: Long = System.nanoTime()) {
        if (closed || owner != generation || !captureMode.videoRequested) return
        // The resync is read after it happened, so the receive time bounds an unmapped end from above.
        closeTouchLost(device, touchBound(toMono, 1, host) ?: clock.now(host))
    }

    private fun touchBound(mono: Long, sign: Int, host: Long): Long? = clock.mono(mono, host).let { time ->
        val sequence = time.sequence ?: return@let null
        if (time.uncertainty == Long.MAX_VALUE) null
        else runCatching { Math.addExact(sequence, sign * time.uncertainty) }.getOrNull()
    }

    private fun closeTouchLost(device: String, to: Long?) {
        val open = touchLostOpen.remove(device) ?: return
        val rows = touchGaps.map { if (it === open) it.copy(to = to) else it }
        touchGaps.clear()
        touchGaps.addAll(rows)
    }

    @Synchronized
    fun touchStatus(state: StreamState, reason: String?, owner: Long) {
        if (closed || owner != generation || !captureMode.videoRequested) return
        setTouchState(state, reason)
    }

    @Synchronized
    fun touchDots(dots: TouchDots, owner: Long) {
        if (closed || owner != generation || !captureMode.videoRequested) return
        touchDots = dots
    }

    private fun setTouchState(state: StreamState, reason: String?) {
        val now = clock.now()
        val capturing = touchState.state == StreamState.CAPTURING
        // A reader that stops never resyncs: its drop gaps end where reading ended.
        if (capturing && state != StreamState.CAPTURING) touchLostOpen.keys.toList().forEach { closeTouchLost(it, now) }
        if (capturing && state != StreamState.CAPTURING) touchGaps += TouchGap(now, null, reason ?: "タップ位置の取得が中断しました")
        if (!capturing && state == StreamState.CAPTURING) {
            val open = touchGaps.peekLast()?.takeIf { it.to == null }
            if (open != null) { touchGaps.removeLast(); touchGaps.addLast(open.copy(to = now)) }
            else if (touchGaps.isEmpty()) touchGaps += TouchGap(null, now, "タップ位置の取得開始前です")
        }
        touchState = StreamSnapshot(state, reason = reason)
        while (touchGaps.size > 4096) touchGaps.removeFirst()
    }

    @Synchronized
    fun status(kind: String, state: StreamState, reason: String?, owner: Long) = status(kind, state, reason, owner, null)

    private fun status(kind: String, state: StreamState, reason: String?, owner: Long, received: VideoEntry?) {
        if (closed || owner != generation) return
        val old = states.getValue(kind)
        if (old.state == StreamState.NOT_REQUESTED) return
        if (state != StreamState.CAPTURING && old.state == StreamState.CAPTURING) {
            addGap(kind, clock.now(), null, reason ?: "取得が中断しました")
        }
        if (state == StreamState.CAPTURING && (old.state != state || kind == "app_log")) synchronized(clock) {
            val waiting = if (kind == "app_log") when {
                fixedEnd != null -> "取得窓の復旧を待っています"
                states.getValue("device_log").state != StreamState.CAPTURING ->
                    states.getValue("device_log").reason ?: "全体ログ取得の復旧を待っています"
                clock.snapshot().lastOrNull() === generationClockSample || !clock.certain() -> "時計対応を確認できません"
                !appResolved || apps.peekLast()?.let { it.uid != null && it.from != null && it.to == null &&
                    it.epoch == clock.currentEpoch() } != true -> "現在の時計区間で対象アプリの確認を待っています"
                else -> null
            } else null
            if (waiting != null) {
                states[kind] = old.copy(state = if (old.state == StreamState.CAPTURING) old.state else StreamState.RECOVERING,
                    reason = waiting)
                return
            }
            val index = gaps.lastOrNull { it.stream == kind && it.toNs == null }
            if (index != null) {
                val closedGap = index.copy(toNs = clock.now(), boundaryUncertaintyNs = if (kind == "app_log")
                    maxOf(index.boundaryUncertaintyNs ?: Long.MAX_VALUE, clock.endUncertainty()) else index.boundaryUncertaintyNs)
                gaps.remove(index); gaps.add(closedGap)
                val cut = videoCuts.remove(index)
                if (kind == "video" && cut != null && received != null)
                    videoCuts[closedGap] = cut.copy(after = sourcePosition(received, false))
            }
        }
        states[kind] = old.copy(state = state, reason = reason)
        // Target resolution and aggregate reader readiness may arrive in either order.
        if (kind == "device_log" && state == StreamState.CAPTURING && appResolved)
            status("app_log", StreamState.CAPTURING, null, owner)
        if (started == null && logWindowReady()) started = clock.now()
    }

    @Synchronized
    fun clockStatus(healthy: Boolean, owner: Long) {
        if (closed || owner != generation) return
        if (!healthy && gaps.none { it.stream == "clock" && it.toNs == null }) {
            val anchor = clock.snapshot().lastOrNull { it.valid }
            addGap("clock", anchor?.sequenceOffset?.let { anchor.elapsed + it }, null, "時計測定が不達または正常条件を満たしません")
        } else if (healthy) {
            val gap = gaps.lastOrNull { it.stream == "clock" && it.toNs == null }
            if (gap != null) synchronized(clock) {
                val recovery = clock.snapshot().lastOrNull()?.takeIf { it.valid && it.epoch == clock.currentEpoch() }
                val closedGap = gap.copy(toNs = clock.now())
                gaps.remove(gap); gaps.add(closedGap)
                if (recovery != null) clockRecoveries[closedGap] = recovery
            }
        }
        if (healthy && appResolved) status("app_log", StreamState.CAPTURING, null, owner)
    }

    /** A new server attempt starts a fresh AAC stream whose config packet comes first. */
    @Synchronized
    fun audioSession(owner: Long) {
        if (closed || owner != generation || !audioRequested) return
        audioSession++
        audioConfig = null
        audioPrevious = -1
        setAudioState(StreamState.RECOVERING, "音声の取得を準備しています")
    }

    @Synchronized
    fun audioStatus(state: StreamState, reason: String?, owner: Long) {
        if (closed || owner != generation || !audioRequested) return
        setAudioState(state, reason)
    }

    @Synchronized
    fun audio(packet: AudioPacket, owner: Long, host: Long = System.nanoTime()) {
        if (closed || owner != generation || !audioRequested) return
        if (packet.config) { audioConfig = packet.bytes.copyOf(); return }
        val config = checkNotNull(audioConfig) { "音声configがありません" }
        if (packet.pts <= audioPrevious) return // A non-monotonic AAC unit cannot be placed on the video timeline.
        audioPrevious = packet.pts
        audio += AudioEntry(packet.pts, packet.bytes, config, clock.video(packet.pts, host), generation, audioSession, host)
        audioBytes += packet.bytes.size + 128
        while (audioBytes > logLimit && audio.isNotEmpty()) audioBytes -= audio.removeFirst().bytes.size + 128
        if (audioState.state != StreamState.CAPTURING) setAudioState(StreamState.CAPTURING, null)
    }

    private fun setAudioState(state: StreamState, reason: String?) {
        if (audioState.state == StreamState.UNAVAILABLE && state != StreamState.UNAVAILABLE) return
        if (audioState.state == StreamState.CAPTURING && state != StreamState.CAPTURING)
            addGap("audio", clock.now(), null, reason ?: "音声の取得が中断しました")
        if (audioState.state != StreamState.CAPTURING && state == StreamState.CAPTURING) {
            gaps.lastOrNull { it.stream == "audio" && it.toNs == null }?.let { open ->
                gaps.remove(open); gaps.add(open.copy(toNs = clock.now()))
            }
        }
        audioState = StreamSnapshot(state, reason = reason)
    }

    private fun pruneAudio(expired: (MappedTime, Long, Long) -> Boolean) {
        audio.removeIf { row ->
            val retained = row.time.sequence ?: row.host
            val gone = expired(row.time, retained, row.host)
            if (gone) audioBytes -= row.bytes.size + 128
            gone
        }
    }

    @Synchronized
    fun session(packet: VideoPacket.Session, owner: Long) {
        if (closed || owner != generation || !captureMode.videoRequested) return
        decodeRun = Math.incrementExact(decodeRun)
        status("video", StreamState.RECOVERING, "動画sessionを準備しています", owner)
        closeCurrentFile()
        videoSession++
        width = packet.width
        height = packet.height
        previousPts = -1
        config = byteArrayOf()
    }

    @Synchronized
    fun frame(packet: VideoPacket.Frame, owner: Long, host: Long = System.nanoTime()): Boolean {
        if (closed || owner != generation || !captureMode.videoRequested) return false
        if (packet.config) {
            require(packet.bytes.size <= ReplaySettings.MAX_CONFIG_PACKET_BYTES) { "動画configが64KiBを超えました" }
            if (!config.contentEquals(packet.bytes)) {
                decodeRun = Math.incrementExact(decodeRun)
                status("video", StreamState.RECOVERING, "動画configが切り替わりました", owner)
                closeCurrentFile()
            }
            config = packet.bytes.copyOf(); return videoReady()
        }
        // Consume an ordinal before guards/IDR waiting: rejected source packets leave a detectable hole.
        packetOrdinal = Math.incrementExact(packetOrdinal)
        require(width > 0 && height > 0 && config.isNotEmpty()) { "動画の寸法/configがありません" }
        if (packet.pts <= previousPts) { clock.boundary(); error("動画PTSが単調ではありません") }
        previousPts = packet.pts
        if (packet.key) {
            closeCurrentFile()
            deleteUnused() // Retire unreferenced files even while T is frozen/unknown, before registration.
            val file = directory.resolve("gop-${UUID.randomUUID()}.h264")
            val opened = PrivateFiles.openNew(file, setOf(CREATE_NEW, WRITE))
            try { created(file) } catch (failure: Exception) {
                try { opened.close() } finally {
                    try { Files.delete(file) } catch (cleanup: Exception) { failure.addSuppressed(cleanup) }
                }
                throw failure
            }
            currentFile = file
            channel = opened
            ownedFiles.add(file)
        }
        val out = channel ?: run { addGap("video", clock.now(host), clock.now(host), "IDR待ちでframeを保持できません"); return false }
        require(out.position() + packet.bytes.size <= 32L * 1024 * 1024) { "GOPが32MiBを超えました" }
        check(Files.getFileStore(directory).usableSpace >= minFree) { "取得用一時領域の空き容量が不足しています" }
        val time = clock.video(packet.pts, host)
        val retained = time.sequence ?: clock.now(host) ?: 0L
        if (started == null && time.sequence != null) started = retained
        val offset = out.position()
        val data = ByteBuffer.wrap(packet.bytes)
        while (data.hasRemaining()) out.write(data)
        video += VideoEntry(currentFile!!, offset, packet.bytes.size, packet.pts, packet.key, width, height,
            generation, time, retained, config, host, videoSession, decodeRun, packetOrdinal)
        videoFileRefs[currentFile!!] = (videoFileRefs[currentFile!!] ?: 0) + 1
        videoBytes += packet.bytes.size
        val references = configRefs[config] ?: 0
        if (references == 0) configBytes += config.size
        configRefs[config] = references + 1
        enforceVideoLimit()
        status("video", StreamState.CAPTURING, if (time.elapsed == null || time.sequence == null || time.uncertainty > 20_000_000) "動画の時刻対応を確認できません" else null, owner, video.peekLast())
        return videoReady()
    }

    /** Ready only while the current GOP's decode-start frame remains indexed after byte/packet limits. */
    private fun videoReady(): Boolean = channel != null && video.lastOrNull { it.key }?.file == currentFile

    @Synchronized
    fun app(packageName: String?, uid: Long?, pids: Set<Int>, owner: Long, uidExclusive: Boolean = false,
            host: Long = System.nanoTime(), poll: ReporterPoll? = null) {
        if (closed || owner != generation || poll != null && poll.generation != owner) return
        require(pids.size <= 1024)
        appResolved = uid != null
        val last = apps.peekLast()
        val samples = clock.snapshot()
        val anchor = samples.lastOrNull { it.valid }
        if (last == null || last.packageName != packageName || last.uid != uid || last.pids != pids ||
            last.uidExclusive != uidExclusive || last.from == null || last.epoch != anchor?.epoch) {
            val now = anchor?.let { it.elapsed + (host - it.received).coerceAtLeast(0) }
            if (last != null) {
                val end = if (last.epoch == anchor?.epoch) now else samples.lastOrNull { it.epoch == last.epoch }?.elapsed
                // A previous sample cannot close a period that began later by host extrapolation.
                val knownEnd = end?.takeIf { last.from == null || it >= last.from }
                apps.removeLast()
                apps.add(last.copy(to = knownEnd, endUnknown = knownEnd == null))
            }
            apps += AppPeriod(packageName, uid, java.util.Set.copyOf(pids), now, null, anchor?.epoch ?: 0, uidExclusive)
            while (apps.size > 2048) apps.removeFirst()
        }
        if (poll != null) reporter = poll
        status("app_log", if (uid != null) StreamState.CAPTURING else StreamState.UNAVAILABLE,
            if (uid == null) "対象アプリを解決できません" else null, owner)
    }

    @Synchronized
    fun reporter(value: ReporterPoll) {
        if (!closed && value.generation == generation) reporter = value
    }

    @Synchronized
    fun logReaderBoundary(owner: Long) {
        if (!closed && owner == generation) autosave.refresh(null)
    }

    @Synchronized
    fun configureAuto(settings: ReplaySettings, revision: Long, device: String?, active: Boolean,
                      host: Long = System.nanoTime()) {
        autoDevice = device
        autoTarget = settings.application.packageName
        autosave.configure(settings.autoSaveOnCrash, active, autoTarget, revision)
        refreshAuto(host)
    }

    private fun refreshAuto(host: Long) = synchronized(clock) {
        val anchor = clock.snapshot().lastOrNull()
        val device = autoDevice
        val ready = !closed && fixedEnd == null && device != null && autoTarget != null &&
            apps.peekLast()?.packageName == autoTarget && anchor != null && anchor !== generationClockSample &&
            clock.certain(host) && host >= anchor.received
        val boundary = if (ready) try {
            CrashBarrier(device, anchor.boot, generation, clock.currentEpoch(), host,
                Math.addExact(anchor.wall, host - anchor.received))
        } catch (_: ArithmeticException) { null } else null
        autosave.refresh(boundary)
    }

    @Synchronized
    fun log(source: DeviceLog, owner: Long, host: Long = System.nanoTime()): String? {
        if (closed || owner != generation) return null
        val time = clock.log(source.wall, host)
        val retained = time.sequence ?: clock.now(host) ?: 0L
        if (started == null && time.sequence != null) started = retained
        val period = apps.lastOrNull { it.epoch == time.epoch && time.elapsed != null && it.from != null &&
            time.elapsed >= it.from && (it.to == null || time.elapsed <= it.to) }
        val app = when {
            period?.uid == null || period.endUnknown || time.elapsed == null -> null
            source.uid == period.uid && period.uidExclusive -> true
            source.pid in period.pids -> source.uid == null || source.uid == period.uid
            source.uid == null || source.uid == period.uid -> null // shared/unknown UID: a new PID is not evidence of non-membership.
            else -> false
        }
        val row = LogEntry(UUID.randomUUID().toString(), source, generation, time, retained, app, host)
        logs += row
        refreshAuto(host)
        val selectedUid = (period?.takeIf { it.packageName == autoTarget } ?: apps.peekLast())
            ?.takeIf { it.packageName == autoTarget && it.uidExclusive }?.uid
        autosave.observe(row, period, reporter, selectedUid)
        logBytes += source.raw.size + 256
        if (logBytes > logLimit && fixedEnd == null) logRetentionSeconds?.let { seconds ->
            synchronized(clock) {
                val end = end()
                val uncertainty = clock.endUncertainty()
                if (end != null && uncertainty != Long.MAX_VALUE)
                    pruneLogs(expiredBefore(end - seconds * 1_000_000_000L - uncertainty))
            }
        }
        while (logBytes > logLimit && logs.isNotEmpty()) {
            val lost = logs.removeFirst()
            logBytes -= lost.source.raw.size + 256
            addGap("device_log", lost.time.sequence, lost.time.sequence, "ログのbyte上限でrecordを失いました")
        }
        // Reader readiness owns transitions/gaps; another reader's row cannot recover a failed source.
        if (states.getValue("device_log").state == StreamState.CAPTURING) {
            val reason = if (time.elapsed == null || time.sequence == null || time.uncertainty > 20_000_000)
                "ログの時刻対応を確認できません" else null
            states["device_log"] = states.getValue("device_log").copy(reason = reason)
        }
        return row.id // Accepted into the store; retention and membership are separate decisions.
    }

    /** Freezes the disconnected live window; this is separate from the admitted save pin. */
    @Synchronized
    fun freeze() {
        if (fixedEnd == null) synchronized(clock) {
            fixedEnd = clock.now() ?: latest()
            fixedUncertainty = clock.endUncertainty()
            fixedEpoch = clock.currentEpoch()
        }
    }
    @Synchronized
    fun resume() {
        fixedEnd = null; fixedUncertainty = null; fixedEpoch = null
        if (appResolved && states.getValue("app_log").state != StreamState.CAPTURING)
            status("app_log", StreamState.CAPTURING, null, generation)
    }
    @Synchronized
    fun end(): Long? = fixedEnd ?: clock.now() ?: latest()
    @Synchronized
    fun frozen(): Boolean = fixedEnd != null

    private fun latest(): Long? = listOfNotNull(if (captureMode.videoRequested) video.peekLast()?.retainedAt else null, logs.peekLast()?.retainedAt).maxOrNull()

    @Synchronized
    fun prune(seconds: Int) {
        if (closed) return
        logRetentionSeconds = seconds
        if (fixedEnd != null) return
        val end = end() ?: return
        val uncertainty = clock.endUncertainty()
        // An unknown boot bridge/current T cannot prove that old known-epoch records are outside the window.
        if (uncertainty == Long.MAX_VALUE) {
            // A record is generated before it is received, so one received before every possible window start is outside.
            val cutoff = System.nanoTime() - seconds * 1_000_000_000L - HOST_WINDOW_MARGIN_NS
            if (captureMode.videoRequested) {
                val all = video.toList()
                val firstInside = all.indices.firstOrNull { (all.getOrNull(it + 1)?.host ?: Long.MAX_VALUE) >= cutoff } ?: all.size
                repeat((minOf(firstInside, all.lastIndex) downTo 0).firstOrNull { all[it].key } ?: 0) { removeVideo() }
            }
            pruneLogs { _, _, host -> host < cutoff }
            pruneAudio { _, _, host -> host < cutoff }
            touches.removeIf { it.host < cutoff }
            enforceVideoLimit()
            deleteUnused()
            return
        }
        val cutoff = end - seconds * 1_000_000_000L - uncertainty
        val latest = clock.latestSequence()
        if (captureMode.videoRequested) {
            // Keep the complete preceding GOP for decoding the first frame inside the logical window.
            val all = video.toList()
            val firstInside = all.indices.firstOrNull { i ->
                val frame = all[i]
                if (frame.time.sequence == null || frame.time.uncertainty == Long.MAX_VALUE) !displayEndedBefore(all, i, latest, cutoff)
                else frame.retainedAt >= cutoff - frame.time.uncertainty
            } ?: -1
            if (firstInside > 0) {
                val first = if (continuousAcrossCut(all[firstInside - 1], all[firstInside], cutoff)) firstInside - 1 else firstInside
                val key = (first downTo 0).firstOrNull { all[it].key } ?: 0
                repeat(key) { removeVideo() }
            } else if (firstInside < 0) {
                val last = all.lastOrNull()?.takeIf { currentVideo(it) }
                val key = if (last == null) all.size else (all.lastIndex downTo 0).firstOrNull { all[it].key } ?: 0
                repeat(key) { removeVideo() }
            }
        }
        pruneLogs(expiredBefore(cutoff, latest))
        pruneAudio(expiredBefore(cutoff, latest))
        touches.removeIf { row ->
            val point = row.time.sequence
            point != null && row.time.uncertainty != Long.MAX_VALUE && point < cutoff - row.time.uncertainty
        }
        enforceVideoLimit()
        deleteUnused()
        while (gaps.size > 4096) removeGapProofs(gaps.removeFirst())
    }

    /** Unknown-time records are expired only when a later clock read already precedes the cutoff. */
    private fun expiredBefore(cutoff: Long, latest: (Long) -> Long? = clock.latestSequence()): (MappedTime, Long, Long) -> Boolean =
        { time, retainedAt, host ->
            if (time.sequence == null || time.uncertainty == Long.MAX_VALUE) latest(host)?.let { it < cutoff } == true
            else retainedAt < cutoff - time.uncertainty
        }

    /** An unknown-time frame stays on screen until the next frame, so only a next frame starting before `cutoff` ends it. */
    private fun displayEndedBefore(all: List<VideoEntry>, index: Int, latest: (Long) -> Long?, cutoff: Long): Boolean {
        val next = all.getOrNull(index + 1) ?: return false
        val upper = if (next.time.sequence != null && next.time.uncertainty != Long.MAX_VALUE) try {
            Math.addExact(next.retainedAt, next.time.uncertainty)
        } catch (_: ArithmeticException) { null } else latest(next.host)
        return upper != null && upper < cutoff
    }

    private fun pruneLogs(expired: (MappedTime, Long, Long) -> Boolean) {
        // ponytail: scan the byte-bounded ring; add a time index only if measured pruning cost requires it.
        logs.removeIf { row ->
            val gone = expired(row.time, row.retainedAt, row.host)
            if (gone) logBytes -= row.source.raw.size + 256
            gone
        }
    }

    /** Null/no admissible content or AutoSaveRejected creates no new pin; an existing pin rejects another capture. */
    @Synchronized
    fun capture(settings: ReplaySettings, trigger: CrashPair? = null, host: Long = System.nanoTime()): FrozenCapture? = synchronized(clock) {
        check(pinned == null) { "保存対象は既に固定されています" }
        require(settings.captureMode == captureMode) { "取得中のモードと保存設定が一致しません" }
        val end = fixedEnd ?: clock.now(host) ?: latest() ?: return null
        val uncertainty = fixedUncertainty ?: clock.endUncertainty(host)
        val known = uncertainty != Long.MAX_VALUE
        val start = maxOf(0L, started ?: end, end - settings.replaySeconds * 1_000_000_000L)
        val hostStart = if (known || fixedEnd != null) null else host - settings.replaySeconds * 1_000_000_000L - HOST_WINDOW_MARGIN_NS
        val logTime = clock.logMapper()
        val latest = clock.latestSequence()
        // An unknown-time record received before a clock read that is already before the window cannot be inside it.
        fun beforeWindow(host: Long) = known && uncertainty >= 0 && latest(host)?.let { it < start - uncertainty } == true
        fun insideWindow(time: MappedTime, host: Long): Boolean {
            val point = time.sequence
            val error = time.uncertainty
            return if (!known) hostStart == null || host >= hostStart
            else if (error < 0 || uncertainty < 0) true
            else if (point == null || error == Long.MAX_VALUE) !beforeWindow(host)
            else try {
                val margin = Math.addExact(error, uncertainty)
                val distance = when {
                    point < start -> Math.subtractExact(start, point)
                    point > end -> Math.subtractExact(point, end)
                    else -> 0L
                }
                distance <= margin
            } catch (_: ArithmeticException) { true } // Overflow cannot prove that the record is outside.
        }
        val rows = logs.map { row -> row.copy(time = logTime(row.source.wall, row.host)) }.filter { insideWindow(it.time, it.host) }
        val touchRows = if (!captureMode.videoRequested) emptyList() else {
            val touchTime = clock.monoMapper()
            touches.map { row -> row.copy(time = touchTime(row.sample.monoNs, row.host, row.time.epoch)) }
                .filter { insideWindow(it.time, it.host) }
        }
        val audioRows = if (!audioRequested) emptyList() else
            audio.map { it.mapped(clock.video(it.pts, it.host, it.time.epoch)) }.filter { insideWindow(it.time, it.host) }
        val all = if (captureMode.videoRequested) video.toList() else emptyList()
        val files = mutableSetOf<Path>()
        all.forEachIndexed { index, entry ->
            val unknownTime = entry.time.sequence == null || entry.time.uncertainty == Long.MAX_VALUE
            val inside = if (!known) hostStart == null || (all.getOrNull(index + 1)?.host ?: Long.MAX_VALUE) >= hostStart
                else unknownTime && !(uncertainty >= 0 && displayEndedBefore(all, index, latest, start - uncertainty)) ||
                    entry.retainedAt <= end && entry.retainedAt >= start
            if (inside) {
                files.add(entry.file)
                if (index > 0 && continuousAcrossCut(all[index - 1], entry, start)) files.add(all[index - 1].file)
            }
        }
        all.lastOrNull()?.takeIf { known && it.retainedAt < start && currentVideo(it) }?.let { files.add(it.file) }
        val frames = all.filter { (!known || it.retainedAt <= end) && it.file in files }.map { frame ->
            frame.copy(time = clock.video(frame.pts, frame.host, frame.time.epoch))
        }
        var value = FrozenCapture(UUID.randomUUID().toString(), sequence, generation, start, end,
            settings.replaySeconds, frames.toList(), rows.toList(), gaps.toList(), clock.snapshot(), apps.toList(), settings,
            streams(settings.replaySeconds, end), endUncertainty = uncertainty, windowClockEpoch = fixedEpoch ?: clock.currentEpoch(),
            clockRecoveries = clockRecoveries.toMap(), videoCuts = videoCuts.toMap(), hostStart = hostStart,
            touches = touchRows, touchState = touchState.takeIf { captureMode.videoRequested }, touchGaps = touchGaps.toList() +
                listOfNotNull(touchEvictedThrough?.takeIf { it >= start }?.let { TouchGap(null, it.takeIf { it != Long.MAX_VALUE }, "保持上限を超えた古いタップ位置を破棄しました") }),
            touchDropped = touchDropped, touchDots = touchDots.takeIf { captureMode.videoRequested },
            audio = audioRows, audioState = audioState.takeIf { audioRequested })
        if (trigger != null) {
            autosave.admissionProblem(trigger, host)?.let { throw AutoSaveRejected(it) }
            if (fixedEnd != null || !clock.certain(host)) throw AutoSaveRejected(AutoSaveReason.NOT_SAVED_CLOCK_UNKNOWN)
            value = value.copy(autoTrigger = frozenAutoTrigger(trigger, value, logTime, host))
        }
        if (value.empty && (trigger != null || !(logWindowReady() && start < end))) return null
        pinned = value
        pinnedFiles = value.video.map { it.file }.toSet()
        return value
    }

    /** Releases only the matching request after all reads finish; a failed save remains pinned for retry. */
    @Synchronized
    fun release(id: String) {
        if (pinned?.id == id) {
            unusedCandidates.addAll(pinnedFiles)
            pinned = null
            pinnedFiles = emptySet()
            deleteUnused()
        }
    }

    /** Window-filtered warnings for live/frozen display; row span is not proof of continuous coverage. */
    @Synchronized
    fun streams(seconds: Int = 180, atEnd: Long? = null): Map<String, StreamSnapshot> = synchronized(clock) {
        val end = atEnd ?: end() ?: 0
        val windowGaps = gaps.toList().inWindow(windowStart(seconds, end), end,
            fixedUncertainty ?: clock.endUncertainty(), clock.snapshot(), clockRecoveries)
        states.mapValues { (kind, value) ->
            if (value.state == StreamState.NOT_REQUESTED) return@mapValues value
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
                gaps = java.util.List.copyOf(windowGaps.filter { gap ->
                    gap.stream == kind || gap.stream == "clock" || kind == "app_log" && gap.stream == "device_log"
                }))
        }
    }

    @Synchronized
    fun windowStart(seconds: Int, atEnd: Long? = null): Long? = (atEnd ?: end())?.let {
        maxOf(0L, started ?: it, it - seconds * 1_000_000_000L)
    }

    @Synchronized
    fun hasData(): Boolean = (captureMode.videoRequested && video.isNotEmpty()) || logs.isNotEmpty() ||
        (logWindowReady() && started?.let { it < (end() ?: it) } == true)

    /** A validated reader, current clock and target prove a quiet window, never row-span coverage. */
    private fun logWindowReady(): Boolean = synchronized(clock) {
        captureMode == CaptureMode.LOG_ONLY && !closed && fixedEnd == null &&
            clock.snapshot().lastOrNull() !== generationClockSample && clock.certain() &&
            gaps.none { it.stream == "clock" && it.toNs == null } &&
            listOf("device_log", "app_log").all { states.getValue(it).let { state ->
                state.state == StreamState.CAPTURING && state.reason == null
            } } && appResolved && apps.peekLast()?.let {
                it.uid != null && it.from != null && it.to == null && !it.endUnknown && it.epoch == clock.currentEpoch()
            } == true
    }

    private fun continuousAcrossCut(a: VideoEntry, b: VideoEntry, cut: Long): Boolean =
        a.time.sequence != null && b.time.sequence != null && a.time.sequence < cut && b.time.sequence >= cut &&
            a.continuousTo(b, gaps.toList().inWindow(a.time.sequence, b.time.sequence,
                maxOf(a.time.uncertainty, b.time.uncertainty), clock.snapshot(), clockRecoveries))

    private fun currentVideo(frame: VideoEntry): Boolean = frame.generation == generation && frame.session == videoSession &&
        frame.time.epoch == clock.currentEpoch() && frame.width == width && frame.height == height && frame.config.contentEquals(config)

    private fun removeVideo(): VideoEntry {
        val entry = video.removeFirst()
        val files = videoFileRefs.getValue(entry.file) - 1
        if (files == 0) { videoFileRefs.remove(entry.file); unusedCandidates.add(entry.file) }
        else videoFileRefs[entry.file] = files
        videoBytes -= entry.size
        val refs = configRefs.getValue(entry.config) - 1
        if (refs == 0) { configRefs.remove(entry.config); configBytes -= entry.config.size } else configRefs[entry.config] = refs
        return entry
    }

    private fun enforceVideoLimit() {
        if (!captureMode.videoRequested) return
        while (video.isNotEmpty() && (videoBytes > videoLimit || video.size > ReplaySettings.MAX_VIDEO_PACKETS ||
                configBytes > ReplaySettings.CONFIG_MEMORY_BYTES)) {
            val entry = removeVideo()
            addGap("video", entry.time.sequence, entry.time.sequence, "動画のbyte/packet/config上限でframeを失いました", lostVideo = entry)
        }
    }

    private fun sourcePosition(entry: VideoEntry, end: Boolean): VideoSourcePosition? {
        if (entry.file !in ownedFiles || entry.generation <= 0 || entry.session <= 0 || entry.offset < 0 || entry.size <= 0) return null
        val offset = try { if (end) Math.addExact(entry.offset, entry.size.toLong()) else entry.offset }
            catch (_: ArithmeticException) { return null }
        return VideoSourcePosition(entry.file, offset, entry.generation, entry.session, entry.host)
    }

    private fun removeGapProofs(gap: CaptureGap) { clockRecoveries.remove(gap); videoCuts.remove(gap) }

    private fun addGap(stream: String, from: Long?, to: Long?, reason: String, boundary: MappedTime? = null,
                       lostVideo: VideoEntry? = null) {
        // Source interruption survives gap coalescing. Evicting an old packet does not interrupt live capture.
        if (stream == "video" && lostVideo == null) decodeRun = Math.incrementExact(decodeRun)
        val previous = gaps.peekLast()
        if (previous?.let { it.stream == stream && it.reason == reason && it.toNs == to } == true) {
            videoCuts.remove(previous) // Coalesced events no longer have one unambiguous source cut.
            return
        }
        val anchor = clock.snapshot().lastOrNull { it.valid }
        val uncertainty = boundary?.uncertainty ?: anchor?.let { if (System.nanoTime() - it.received > 5_000_000_000 || it.bridgeError == Long.MAX_VALUE) Long.MAX_VALUE else
            it.readError + (it.received - it.sent) / 2 + it.bridgeError }
        val gap = CaptureGap(stream, from, to, reason, if (boundary != null || clock.certain()) uncertainty else Long.MAX_VALUE,
            generation, boundary?.epoch ?: clock.currentEpoch())
        gaps += gap
        if (stream == "video") {
            // A retention hole occupies the deleted packet's bytes, not the current source tail.
            videoCuts[gap] = if (lostVideo != null) VideoCut(sourcePosition(lostVideo, false), sourcePosition(lostVideo, true))
                else VideoCut(video.peekLast()?.takeIf { it.generation == generation }?.let { sourcePosition(it, true) })
        }
        while (gaps.size > 4096) removeGapProofs(gaps.removeFirst())
    }

    private fun closeCurrentFile() {
        channel?.close()
        channel = null
        currentFile?.let { unusedCandidates.add(it) }
        currentFile = null
    }

    private fun deleteUnused() {
        val candidates = unusedCandidates.iterator()
        while (candidates.hasNext()) {
            val file = candidates.next()
            if (file !in ownedFiles || file in videoFileRefs || file in pinnedFiles || file == currentFile) {
                candidates.remove()
                continue
            }
            val deleted = Files.deleteIfExists(file)
            ownedFiles.remove(file)
            candidates.remove()
            videoCuts.entries.removeIf { (_, cut) -> cut.before?.file == file || cut.after?.file == file }
            if (deleted) removed(file)
        }
    }

    @Synchronized
    override fun close() {
        closed = true
        closeCurrentFile()
        pinned = null
        pinnedFiles = emptySet()
        if (captureMode.videoRequested) video.clear()
        videoFileRefs.clear()
        unusedCandidates.addAll(ownedFiles)
        logs.clear()
        touches.clear()
        touchGaps.clear()
        touchLostOpen.clear()
        touchEvictedThrough = null
        apps.clear()
        gaps.clear()
        clockRecoveries.clear()
        videoCuts.clear()
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

/**
 * One sequence-ns envelope per original gap, in input order, for conservative overlap checks only.
 *
 * Supported anchors may bound a recovered gap without confirming its exact duration; retain original history.
 * videoProofs optionally receives the video anchors used by the saved manifest, not source-adjacency proof.
 */
internal fun List<CaptureGap>.gapScopes(
    clocks: List<ClockSample>, recoveries: Map<CaptureGap, ClockSample> = emptyMap(),
    videoProofs: MutableMap<CaptureGap, Pair<ClockSample, ClockSample>>? = null,
): List<CaptureGap> {
    fun anchor(sample: ClockSample): Pair<Long, Long>? {
        val offset = sample.sequenceOffset ?: return null
        if (!sample.valid || sample.bridgeError !in 0..20_000_000L) return null
        return try {
            Math.addExact(sample.elapsed, offset) to Math.addExact(sample.readError,
                Math.addExact(Math.subtractExact(sample.received, sample.sent) / 2, sample.bridgeError))
        } catch (_: ArithmeticException) { null }
    }
    val anchors = clocks.mapNotNull { sample -> anchor(sample)?.let { Triple(it.first, it.second, sample) } }.sortedBy { it.first }
    val acquisitionAnchors = anchors.filter { (_, error, sample) ->
        error >= 0 && sample.before >= 0 && sample.after >= sample.before && sample.received >= sample.sent
    }
    fun boundarySample(candidates: List<Triple<Long, Long, ClockSample>>, time: Long, after: Boolean): Triple<Long, Long, ClockSample>? {
        var low = 0
        var high = candidates.size
        while (low < high) {
            val middle = low + (high - low) / 2
            val point = candidates[middle].first
            if (point < time || !after && point == time) low = middle + 1 else high = middle
        }
        return candidates.getOrNull(if (after) low else low - 1)
    }
    return map { gap ->
        val acquisition = gap.stream == "video" || gap.stream == "device_log" || gap.stream == "app_log"
        if (!acquisition && gap.stream != "clock" || gap.boundaryUncertaintyNs != null && gap.boundaryUncertaintyNs != Long.MAX_VALUE) return@map gap
        // Acquisition loss needs both observed boundaries; clocks do not make an open/unknown-owner loss finite.
        if (acquisition && (gap.generation <= 0 || gap.fromNs == null || gap.toNs == null || gap.fromNs > gap.toNs)) return@map gap
        val candidates = if (acquisition) acquisitionAnchors else anchors
        // Widen to observed normal samples, never treat nominal from/to as zero-error boundaries.
        val before = gap.fromNs?.let { from -> boundarySample(candidates, from, after = false) }
        val after = gap.toNs?.let { to ->
            // Status closes after receiving its sample; preserve nominal closure and that sample's error.
            recoveries[gap]?.takeIf { gap.stream == "clock" }?.let { sample -> anchor(sample)?.let {
                Triple(maxOf(to, it.first), it.second, sample)
            } } ?: boundarySample(candidates, to, after = true)
        }
        if (acquisition && (before == null || after == null) ||
            before == null && after == null || before != null && after != null && before.first > after.first) gap
        else {
            if (gap.stream == "video") videoProofs?.put(gap, before!!.third to after!!.third)
            gap.copy(fromNs = before?.first, toNs = after?.first,
                boundaryUncertaintyNs = maxOf(before?.second ?: 0, after?.second ?: 0))
        }
    }
}

/**
 * Returns original gaps whose envelopes may overlap `[start, end]` (sequence ns, including both errors).
 *
 * Unknown window/bounds stay visible. Supply this window's clocks/recoveries, never later samples on retry.
 */
internal fun List<CaptureGap>.inWindow(
    start: Long?, end: Long, uncertainty: Long, clocks: List<ClockSample>,
    recoveries: Map<CaptureGap, ClockSample> = emptyMap(),
): List<CaptureGap> {
    if (start == null || uncertainty == Long.MAX_VALUE) return toList()
    val scopes = gapScopes(clocks, recoveries)
    return filterIndexed { index, _ -> scopes[index].intersects(start, end, uncertainty) }
}

/** The same retained IDR/config requirement is used for both MP4 selection and its display metadata. */
internal fun List<VideoEntry>.decodeStart(frame: VideoEntry): Int? = (indexOf(frame) downTo 0).firstOrNull { i ->
    val key = this[i]
    key.key && key.generation == frame.generation && key.session == frame.session && key.width == frame.width &&
        key.height == frame.height && key.config.contentEquals(frame.config) && key.pts <= frame.pts
}

/** Frozen inputs only: a later frame, clock sample or retry cannot confirm this request's tail. */
internal fun FrozenCapture.videoTail(): VideoTailSnapshot? {
    val last = video.lastOrNull() ?: return null
    val source = last.time.sequence.takeIf { windowKnown && last.time.uncertainty != Long.MAX_VALUE }
    if (source != null && source >= end) return null
    val boundaries = gaps.filter { (it.stream == "video" || it.stream == "clock") &&
        !(source != null && it.toNs != null && it.toNs <= source) &&
        it.intersects(source, end, maxOf(endUncertainty, last.time.uncertainty)) }
    val boundary = boundaries.mapNotNull { it.fromNs }.minOrNull()?.coerceAtMost(end) ?: end
    val from = source?.let { maxOf(start, it) }
    val to = boundary.takeIf { windowKnown && source != null && it > maxOf(start, source) }
    if (from != null && to == null) return null
    val held = from != null && to != null && last.time.epoch == windowClockEpoch &&
        boundaries.all { it.fromNs != null && it.fromNs >= to && it.boundaryUncertaintyNs != null && it.boundaryUncertaintyNs != Long.MAX_VALUE } &&
        (states["video"]?.state == StreamState.CAPTURING || boundaries.isNotEmpty()) && video.decodeStart(last) != null
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

/** Covers device/host clock rate drift over the longest replay window. */
private const val HOST_WINDOW_MARGIN_NS = 250_000_000L
