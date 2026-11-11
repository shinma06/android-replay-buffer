package io.github.shinma06.replaybuffer.core

import com.google.gson.GsonBuilder
import org.jcodec.codecs.h264.H264Utils
import org.jcodec.common.model.Packet
import org.jcodec.containers.mp4.MP4TrackType
import org.jcodec.containers.mp4.boxes.Edit
import org.jcodec.containers.mp4.boxes.SampleEntry
import org.jcodec.containers.mp4.muxer.MP4MuxerTrack
import org.jcodec.containers.mp4.muxer.MP4Muxer
import java.nio.ByteBuffer
import java.nio.channels.Channels
import java.nio.channels.FileChannel
import java.nio.file.Files
import java.nio.file.LinkOption.NOFOLLOW_LINKS
import java.nio.file.Path
import java.nio.file.StandardOpenOption.READ
import java.time.ZonedDateTime
import java.time.format.DateTimeFormatter
import java.util.Base64
import java.util.IdentityHashMap
import java.util.concurrent.CancellationException

private val STREAM_NAMES = mapOf("video" to "動画", "device_log" to "全体ログ", "app_log" to "アプリログ")

internal data class SaveOutput(val directory: Path, val missingKinds: List<String>)

internal class SaveFailure(val partial: Path?, message: String, cause: Throwable? = null) : Exception(message, cause)

internal class SaveCancelled(val partial: Path?, cause: Throwable? = null) : CancellationException(
    "保存を取消しました" + (partial?.let { "。未完成の出力を保全しています: $it" } ?: ""),
) {
    init { if (cause != null) initCause(cause) }
}

/** No external encoder/process: pinned packet indices -> AVC MP4 and matching JSONL/manifest. Runs outside EDT. */
internal class SaveWriter(private val availableSpace: (Path) -> Long = { Files.getFileStore(it).usableSpace }) {
    private val gson = GsonBuilder().serializeNulls().disableHtmlEscaping().create()

    /**
     * Caller retains the capture pin until return/throw and serializes cancellation with the publication callback.
     *
     * Partial/failed attempts remain on disk; SaveFailure/SaveCancelled may identify an owned retained directory.
     * A failure after rename can leave the completed name present: failure is not proof that no output exists.
     */
    fun write(capture: FrozenCapture, folder: Path, cancelled: () -> Boolean,
              publish: (SavePublication) -> Unit): SaveOutput {
        var partial: Path? = null
        var complete: Path? = null
        var parentHandle: SaveOutputDirectory? = null
        var staging: SaveOutputDirectory? = null
        var acquired = false
        var manifest: Map<String, Any?>? = null
        val own = linkedMapOf<Path, SaveOutputFile>()
        fun directoryAt(directory: Path?): Boolean = directory?.let {
            runCatching {
                SaveOutputDirectory.open(directory.parent).use { it.key == parentHandle!!.key } &&
                    parentHandle!!.openChild(directory.fileName.toString()).use { it.key == staging!!.key }
            }.getOrDefault(false)
        } ?: false
        fun ownsDirectory(directory: Path?) = acquired && directoryAt(directory)
        fun checkOwned(path: Path) {
            check(ownsDirectory(path.parent)) { "未完成フォルダーの所有を確認できません" }
            staging!!.verifyFile(path.fileName.toString(), own.getValue(partial!!.resolve(path.fileName)))
        }
        fun checkEntries() = staging!!.requireEntries(own.mapKeys { it.key.fileName.toString() })
        fun createFile(path: Path): FileChannel {
            check(directoryAt(partial)) { "未完成フォルダーの所有を確認できません" }
            if (own.isEmpty()) staging!!.requireEntries(emptyMap())
            val output = staging!!.createFile(path.fileName.toString())
            own[path] = output
            staging!!.verifyFile(path.fileName.toString(), output)
            if (!acquired) {
                checkEntries()
                check(directoryAt(partial)) { "未完成フォルダーの所有を確認できません" }
                acquired = true
            }
            return output.channel()
        }
        fun writer(path: Path) = Channels.newWriter(createFile(path), Charsets.UTF_8).buffered()
        var normalizationStarted: Long? = null
        var normalizationCpuStarted = 0L
        val cpu = java.lang.management.ManagementFactory.getThreadMXBean()
        fun checkActive() {
            if (cancelled() || Thread.currentThread().isInterrupted) throw SaveCancelled(null)
            normalizationStarted?.let { start ->
                check(System.nanoTime() - start < 4L * 60 * 60 * 1_000_000_000 &&
                    (!cpu.isCurrentThreadCpuTimeSupported || cpu.currentThreadCpuTime - normalizationCpuStarted < 4L * 60 * 60 * 1_000_000_000)) {
                    "映像サイズ変換が処理時間上限（4時間）を超えました"
                }
            }
        }
        try {
            checkActive()
            val parent = folder.toRealPath()
            require(Files.isDirectory(parent) && Files.isWritable(parent)) { "保存先に書き込めません" }
            require(capture.video.size <= ReplaySettings.MAX_VIDEO_PACKETS)
            val videoExcluded = capture.request.excludesVideo(capture.settings.captureMode)
            val videoRequested = capture.settings.captureMode.videoRequested && !videoExcluded
            require(videoRequested || capture.video.isEmpty()) { "ログ専用の保存に動画packetがあります" }
            val tail = capture.videoTail()
            val all = capture.video
            val groups = mutableListOf<MutableList<VideoEntry>>()
            all.forEach { frame ->
                val previous = groups.lastOrNull()?.lastOrNull()
                if (previous == null || !previous.playbackContinuousTo(frame)) groups += mutableListOf(frame)
                else groups.last() += frame
            }
            val latest = latestSequence(capture.clocks)
            val nextHost = IdentityHashMap<VideoEntry, Long>().apply { all.zipWithNext { a, b -> put(a, b.host) } }
            fun visible(group: List<VideoEntry>, n: Int): Boolean {
                val frame = group[n]
                val sequence = frame.time.sequence
                // Without a clock mapping, a frame is on screen until the next one is received.
                if (!capture.windowKnown) return capture.hostStart == null || (nextHost[frame] ?: Long.MAX_VALUE) >= capture.hostStart
                if (sequence == null || frame.time.uncertainty == Long.MAX_VALUE) {
                    // Hidden only when the next frame, which ends this one's display, provably starts before the window.
                    val next = group.getOrNull(n + 1) ?: return true
                    val nextAt = next.time.sequence
                    val nextLatest = if (nextAt != null && next.time.uncertainty != Long.MAX_VALUE) try {
                        Math.addExact(nextAt, next.time.uncertainty)
                    } catch (_: ArithmeticException) { null } else latest(next.host)
                    return nextLatest?.let { it < capture.start - capture.endUncertainty } != true
                }
                val duration = group.getOrNull(n + 1)?.let { Math.subtractExact(it.pts, frame.pts) }
                val end = duration?.let { Math.addExact(sequence, Math.multiplyExact(it, 1000)) }
                return sequence <= capture.end && (sequence >= capture.start || end != null && end > capture.start ||
                    tail?.displayHeld == true && frame === all.lastOrNull())
            }
            val selected = groups.filter { group -> group.indices.any { visible(group, it) } }
            fun sampleRange(group: List<VideoEntry>): IntRange {
                checkActive()
                val first = group.indices.first { visible(group, it) }
                val last = group.indices.last { visible(group, it) }
                val key = (first downTo 0).firstOrNull { group[it].key }
                require(key != null) { "IDRを失った動画区間を復号できません" }
                return key..last
            }
            fun sourceDuration(group: List<VideoEntry>, index: Int): Long? =
                group.getOrNull(index + 1)?.let { Math.subtractExact(it.pts, group[index].pts) }
            val ranges = selected.map(::sampleRange)
            val sourceConfigs = selected.map { it.first().config }.distinctBy { ByteBuffer.wrap(it) }
            val sizes = sourceConfigs.map { config ->
                require(config.size in 1..ReplaySettings.MAX_CONFIG_PACKET_BYTES)
                val sps = H264Utils.getRawSPS(ByteBuffer.wrap(config))
                require(sps.isNotEmpty()) { "動画SPSがありません" }
                H264Utils.getPicSize(boundedSps(sps.first()))
            }
            val normalizer = if (sizes.distinct().size > 1) {
                var minimumSourceDuration: Long? = null
                selected.forEachIndexed { run, group ->
                    for (index in ranges[run]) {
                        checkActive()
                        sourceDuration(group, index)?.let { duration ->
                            minimumSourceDuration = minOf(minimumSourceDuration ?: duration, duration)
                        }
                    }
                }
                MixedVideoNormalizer(sourceConfigs, minimumSourceDuration)
            } else null
            if (normalizer != null) {
                normalizationStarted = System.nanoTime()
                normalizationCpuStarted = if (cpu.isCurrentThreadCpuTimeSupported) cpu.currentThreadCpuTime else 0
            }
            val sourceBytes = capture.video.fold(0L) { total, frame -> Math.addExact(total, frame.size.toLong()) }
            val estimate = Math.addExact(Math.multiplyExact(sourceBytes, if (normalizer == null) 1 else 2),
                Math.addExact(capture.logs.sumOf { it.source.raw.size.toLong() * 3 + 1024 },
                    capture.video.size.toLong() * 6144 + sourceConfigs.sumOf { it.size.toLong() } + 16 * 1024 * 1024))
            fun checkSpace(bytes: Long = 0) {
                check(availableSpace(parent) >= Math.addExact(bytes, ReplaySettings.MIN_FREE_BYTES)) { "保存先の空き容量が不足しています" }
            }
            checkSpace(estimate)
            val createdAt = ZonedDateTime.now()
            val crash = capture.autoTrigger != null || capture.request.origin == SaveOrigin.CRASH_AUTOSAVE
            val name = replayFolderName(createdAt, crash, !videoRequested)
            parentHandle = SaveOutputDirectory.open(parent)
            partial = parent.resolve(".$name.partial")
            staging = parentHandle.createDirectory(partial.fileName.toString())
            complete = parent.resolve(name)
            fun file(name: String): Path = partial.resolve(name)
            val parts = mutableListOf<Map<String, Any?>>()
            val frameCoverage = mutableListOf<Pair<Long, Long>>()
            val runBoundaries = mutableListOf<Map<String, Any?>>()
            val losses = mutableListOf<String>()
            val videoProofs = mutableMapOf<CaptureGap, Pair<ClockSample, ClockSample>>()
            val videoScopes = capture.gaps.gapScopes(capture.clocks, capture.clockRecoveries, videoProofs)
            fun aligned(frame: VideoEntry): Boolean {
                val sequence = frame.time.sequence ?: return false
                return capture.windowKnown && frame.time.uncertainty != Long.MAX_VALUE && videoScopes.none { gap ->
                    gap.stream == "clock" && gap.intersects(sequence, sequence, frame.time.uncertainty)
                }
            }
            // A video gap closed by a run's own frame ends at that frame's source time: it cannot interrupt that frame or later ones.
            /** For each capture gap, the index in [frames] of the frame that closed it, or null. */
            fun closedAt(frames: List<VideoEntry>): List<Int?> = firstIndexes(frames.map { it.file to it.offset }, capture.gaps.map { gap ->
                if (gap.stream == "video") capture.videoCuts[gap]?.after?.let { cut -> cut.file to cut.offset } else null
            })
            /** Whether a clock/video gap interrupts frames a→b, where b is frame [through] of the run [closed] was built for. */
            fun interrupted(a: VideoEntry, b: VideoEntry, closed: List<Int?>, through: Int): Boolean = videoScopes.withIndex().any { (i, gap) ->
                (gap.stream == "clock" || gap.stream == "video") && closed[i].let { it == null || it > through } &&
                    gap.intersects(a.time.sequence, b.time.sequence!!, maxOf(a.time.uncertainty, b.time.uncertainty))
            }
            /** Video gaps of one generation between two aligned runs, or null unless every boundary is known. */
            fun knownBoundaryGaps(before: VideoEntry, first: VideoEntry): List<CaptureGap>? {
                val beforeSequence = before.time.sequence
                val firstSequence = first.time.sequence
                return capture.gaps.filter { gap ->
                    val from = gap.fromNs
                    val to = gap.toNs
                    gap.stream == "video" && gap.generation == before.generation && from != null && to != null &&
                        aligned(before) && aligned(first) && beforeSequence != null && firstSequence != null &&
                        from >= beforeSequence && to <= firstSequence && from <= to
                }.takeIf { it.isNotEmpty() && it.all { gap ->
                    gap.boundaryUncertaintyNs != null && gap.boundaryUncertaintyNs in 0 until Long.MAX_VALUE
                } }
            }
            val movieAnchors = mutableListOf<MovieAnchor>()
            val framesFile = if (selected.isNotEmpty()) file(ReplayFiles.VIDEO_FRAMES) else null
            val rawPath = if (normalizer != null) file(ReplayFiles.VIDEO_SOURCE) else null
            var frameRows = 0
            (rawPath?.let(::createFile)).use { raw ->
                (framesFile?.let(::writer)).use { index ->
                    if (index != null) {
                        val videoName = ReplayFiles.VIDEO
                        val target = file(videoName)
                        org.jcodec.common.io.FileChannelWrapper(createFile(target)).use { output ->
                            val mux = MP4Muxer.createMP4MuxerToChannel(output)
                            val track = MP4MuxerTrack(mux.nextTrackId, MP4TrackType.VIDEO)
                            mux.addTrack(track)
                            val configs = mutableListOf<ByteArray>()
                            val entries = mutableListOf<SampleEntry>()
                            val edits = mutableListOf<Edit>()
                            var media = 0L
                            var movie = 0L
                            var sampleIndex = 0L
                            var previousLast: VideoEntry? = null
                            selected.forEachIndexed { runIndex, group ->
                                checkActive()
                                val visibleIndex = group.indices.first { visible(group, it) }
                                val keyIndex = ranges[runIndex].first
                                val samples = group.subList(keyIndex, ranges[runIndex].last + 1)
                                val closed = closedAt(samples)
                                val first = group[visibleIndex]
                                val last = samples.last()
                                val origin = samples.first().pts
                                // Like the tail, a run's last image stays on screen until a known boundary gap starts (e.g. rotation).
                                val heldUntil = selected.getOrNull(runIndex + 1)?.let { next ->
                                    knownBoundaryGaps(last, next[next.indices.first { visible(next, it) }])
                                }?.minOf { it.fromNs!! }
                                val terminal = if (last === all.lastOrNull() && tail?.displayHeld == true)
                                    ((tail.toNs!! - last.time.sequence!!) / 1000).coerceAtLeast(1)
                                    else heldUntil?.let { ((it - last.time.sequence!!) / 1000).coerceAtLeast(1) } ?: 1L
                                val sourceDurations = samples.indices.map { n ->
                                    sourceDuration(group, keyIndex + n)
                                }
                                var startUs = Math.subtractExact(first.pts, origin)
                                if (capture.windowKnown && first.time.sequence != null && first.time.uncertainty != Long.MAX_VALUE)
                                    startUs = Math.addExact(startUs, maxOf(0, (capture.start - first.time.sequence) / 1000))
                                var endUs = Math.addExact(Math.subtractExact(last.pts, origin), terminal)
                                if (sourceDurations.last() != null) {
                                    endUs = Math.addExact(Math.subtractExact(last.pts, origin), sourceDurations.last()!!)
                                    if (capture.windowKnown && last.time.sequence != null && last.time.uncertainty != Long.MAX_VALUE)
                                        endUs = minOf(endUs, Math.addExact(last.pts - origin, maxOf(1, (capture.end - last.time.sequence) / 1000)))
                                }
                                require(startUs >= 0 && endUs > startUs) { "動画の表示範囲が不正です" }
                                val visibleStart = first.time.sequence?.takeIf { aligned(first) }?.let {
                                    Math.addExact(it, Math.multiplyExact(startUs - (first.pts - origin), 1000))
                                }
                                val originalStart = startUs
                                val originalEnd = endUs
                                val clipped = endUs > Int.MAX_VALUE || sourceDurations.any { it != null && it > Int.MAX_VALUE }
                                val durations = samples.mapIndexed { n, frame ->
                                    val duration = sourceDurations[n] ?: terminal
                                    if (!clipped) duration else {
                                        // ponytail: compress only hidden decoder time; never invent cadence for visible VFR.
                                        val at = Math.subtractExact(frame.pts, origin)
                                        (minOf(Math.addExact(at, duration), originalEnd) - maxOf(at, originalStart)).coerceAtLeast(1)
                                    }
                                }
                                if (clipped) {
                                    require(capture.windowKnown && samples.filterIndexed { n, _ -> keyIndex + n >= visibleIndex }.all(::aligned)) {
                                        "時計不明の長い動画区間を短縮できません"
                                    }
                                    startUs = durations.indices.takeWhile { samples[it].pts - origin + (sourceDurations[it] ?: terminal) <= originalStart }
                                        .sumOf { durations[it] }
                                    endUs = Math.addExact(startUs, originalEnd - originalStart)
                                }
                                val before = previousLast
                                val knownGap = before?.let { knownBoundaryGaps(it, first) }?.sortedBy { it.fromNs }?.let { gaps ->
                                    var end: Long? = null
                                    var total = 0L
                                    gaps.forEach { gap ->
                                        val from = maxOf(gap.fromNs!!, end ?: gap.fromNs)
                                        if (gap.toNs!! > from) total = Math.addExact(total, Math.subtractExact(gap.toNs, from))
                                        end = maxOf(end ?: gap.toNs, gap.toNs)
                                    }
                                    total / 1000
                                }
                                val gapUs = if (before == null) visibleStart?.let { maxOf(0, (it - capture.start) / 1000) } ?: 0L else knownGap ?: 0L
                                if (before != null) runBoundaries += mapOf("before_segment" to runIndex - 1, "after_segment" to runIndex,
                                    "actual_gap_duration_us" to knownGap?.toString(), "authored_gap_duration_us" to gapUs.toString())
                                if (gapUs > 0) { edits += Edit(gapUs, -1, 1f); movie = Math.addExact(movie, gapUs) }
                                val runMedia = media
                                val movieStart = movie
                                var normalizedEntry = 0
                                normalizer?.startRun(samples.first().config)
                                val sourceSize = H264Utils.getPicSize(boundedSps(H264Utils.getRawSPS(ByteBuffer.wrap(samples.first().config)).first()))
                                require(samples.all { it.width == sourceSize.width && it.height == sourceSize.height }) { "sessionとSPSの動画寸法が一致しません" }
                                val configOffset = raw?.position()
                                if (raw != null) {
                                    checkActive(); checkSpace(samples.first().config.size.toLong())
                                    val data = ByteBuffer.wrap(samples.first().config)
                                    while (data.hasRemaining()) { checkActive(); raw.write(data) }
                                    checkActive()
                                }
                                val remuxEntry = if (normalizer != null) 0 else configs.indexOfFirst { it.contentEquals(samples.first().config) }.let { existing ->
                                    if (existing >= 0) existing + 1 else {
                                        samples.first().config.let { bytes ->
                                            H264Utils.getRawSPS(ByteBuffer.wrap(bytes)).forEach(::boundedSps)
                                            H264Utils.getRawPPS(ByteBuffer.wrap(bytes)).forEach(::boundedPps)
                                        }
                                        val sampleEntry = H264Utils.createMOVSampleEntryFromBytes(ByteBuffer.wrap(samples.first().config))
                                        val sps = H264Utils.getRawSPS(ByteBuffer.wrap(samples.first().config)).map { boundedSps(it) }
                                        require(sps.isNotEmpty()) { "動画SPSがありません" }
                                        val size = H264Utils.getPicSize(sps.first())
                                        require(sps.all { value ->
                                            value.picWidthInMbsMinus1 in 0..119 && org.jcodec.codecs.h264.io.model.SeqParameterSet.getPicHeightInMbs(value) in 1..120 &&
                                                H264Utils.getPicSize(value) == size && (value.vuiParams?.let { vui ->
                                                    !vui.aspectRatioInfoPresentFlag || vui.aspectRatio.value == 1 || vui.sarWidth > 0 && vui.sarWidth == vui.sarHeight
                                                } ?: true)
                                        }) { "動画SPSの寸法・pixel aspectを確認できません" }
                                        require(size.width in 1..1920 && size.height in 1..1920) { "動画SPSの寸法が上限を超えています" }
                                        configs += samples.first().config
                                        entries += sampleEntry
                                        track.addSampleEntry(sampleEntry)
                                        entries.size
                                    }
                                }
                                val segmentDuration = endUs - startUs
                                samples.forEachIndexed { n, frame ->
                                    checkActive()
                                    val bytes = readFrame(frame)
                                    require(!frame.key || H264Utils.isByteBufferIDRSlice(ByteBuffer.wrap(bytes))) { "IDR packetを復号できません" }
                                    val rawOffset = raw?.position()
                                    if (raw != null) {
                                        checkSpace(bytes.size.toLong() + 16 * 1024 * 1024)
                                        val source = ByteBuffer.wrap(bytes)
                                        while (source.hasRemaining()) { checkActive(); raw.write(source) }
                                        checkActive()
                                    }
                                    val codecStarted = System.nanoTime()
                                    val encoded = try { normalizer?.encode(bytes) } catch (e: Exception) {
                                        throw IllegalStateException("動画packetの復号・サイズ変換に失敗しました", e)
                                    }
                                    checkActive()
                                    if (normalizer != null) check(System.nanoTime() - codecStarted < 10L * 1_000_000_000) {
                                        "1frameの映像サイズ変換が処理時間上限（10秒）を超えました"
                                    }
                                    if (normalizer != null && n == 0) {
                                        require(H264Utils.isByteBufferIDRSlice(encoded!!.duplicate())) { "変換区間の先頭IDRがありません" }
                                        val sampleEntry = H264Utils.createMOVSampleEntryFromBytes(org.jcodec.common.io.NIOUtils.duplicate(encoded))
                                        val avcc = H264Utils.parseAVCC(sampleEntry as org.jcodec.containers.mp4.boxes.VideoSampleEntry)
                                        require(avcc.spsList.isNotEmpty() && avcc.ppsList.isNotEmpty()) { "変換出力avcCが不正です" }
                                        track.addSampleEntry(sampleEntry)
                                        entries += sampleEntry
                                        normalizedEntry = entries.size
                                    }
                                    val entry = if (normalizer != null) normalizedEntry else remuxEntry
                                    if (normalizer != null) sourceDurations[n]?.let { observed ->
                                        normalizer.verifyInterval(observed, encoded!!.remaining())
                                    }
                                    val outputBytes = encoded ?: ByteBuffer.wrap(bytes)
                                    val outputKey = H264Utils.isByteBufferIDRSlice(outputBytes.duplicate())
                                    val stripped = ByteBuffer.allocate(outputBytes.remaining())
                                    H264Utils.wipePS(outputBytes.duplicate(), stripped, mutableListOf(), mutableListOf())
                                    require(stripped.hasRemaining()) { "動画packetに映像sampleがありません" }
                                    val data = H264Utils.encodeMOVPacket(stripped)
                                    val duration = durations[n]
                                    require(duration in 1..Int.MAX_VALUE.toLong()) { "動画durationが不正です" }
                                    checkActive()
                                    if (normalizer != null) checkSpace(data.remaining().toLong())
                                    track.addFrameInternal(Packet.createPacket(data, media, 1_000_000, duration, sampleIndex,
                                        if (outputKey) Packet.FrameType.KEY else Packet.FrameType.INTER, null), entry)
                                    checkActive()
                                    val localMedia = media - runMedia
                                    val presented = localMedia < endUs && Math.addExact(localMedia, duration) > startUs
                                    val displayStart = maxOf(localMedia, startUs)
                                    val displayEnd = minOf(Math.addExact(localMedia, duration), endUs)
                                    val sourceDisplayOffset = if (clipped) maxOf(0, originalStart - (frame.pts - origin)) else displayStart - localMedia
                                    val moviePts = if (presented) Math.addExact(movieStart, displayStart - startUs) else null
                                    val known = aligned(frame)
                                    val sequence = frame.time.sequence
                                    val next = samples.getOrNull(n + 1)
                                    val confirmedEnd = if (known && next != null && aligned(next) && next.time.epoch == frame.time.epoch &&
                                        next.time.sequence!! > sequence!! && !interrupted(frame, next, closed, n))
                                        minOf(capture.end, next.time.sequence) else null
                                    val displayWindowStart = sequence?.takeIf { known && presented }?.let {
                                        Math.addExact(it, Math.multiplyExact(sourceDisplayOffset, 1000)) - capture.start
                                    }
                                    if (displayWindowStart != null && confirmedEnd != null && confirmedEnd - capture.start > displayWindowStart)
                                        frameCoverage += maxOf(0, displayWindowStart) to (confirmedEnd - capture.start)
                                    if (known && moviePts != null) movieAnchors += MovieAnchor(frame.time.epoch,
                                        Math.addExact(sequence!!, Math.multiplyExact(sourceDisplayOffset, 1000)), displayEnd - displayStart, moviePts)
                                    index.write(gson.toJson(mapOf("part" to videoName, "segment_index" to runIndex, "sample_index" to sampleIndex,
                                        "decode_run" to frame.decodeRun.toString(), "packet_ordinal" to frame.packetOrdinal.toString(),
                                        "generation" to frame.generation, "video_session" to frame.session, "sample_entry" to entry,
                                        "source_key" to frame.key, "output_key" to outputKey,
                                        "reencoded" to (normalizer != null), "source_rect" to normalizer?.rectangle(),
                                        "output_width" to (normalizer?.width ?: sizes[0].width), "output_height" to (normalizer?.height ?: sizes[0].height),
                                        "source_colour_known" to normalizer?.colourKnown,
                                        "source_file" to rawPath?.fileName?.toString(), "source_offset" to rawOffset?.toString(),
                                        "source_size" to if (raw != null) bytes.size else null,
                                        "source_sha256" to if (raw != null) bytesHash(bytes) else null,
                                        "source_config_offset" to configOffset?.toString(),
                                        "source_config_size" to if (raw != null) frame.config.size else null,
                                        "source_config_sha256" to if (raw != null) bytesHash(frame.config) else null,
                                        "source_pts_us" to frame.pts.toString(), "source_duration_us" to sourceDurations[n]?.toString(),
                                        "display_duration_us" to duration.toString(), "media_pts_us" to media.toString(),
                                        "movie_pts_us" to moviePts?.toString(), "movie_end_us" to moviePts?.let { Math.addExact(it, displayEnd - displayStart).toString() },
                                        "source_display_start_us" to if (presented) Math.addExact(frame.pts, sourceDisplayOffset).toString() else null,
                                        "elapsed_ns" to frame.time.elapsed?.toString(), "clock_epoch" to frame.time.epoch,
                                        "window_ns" to sequence?.takeIf { known }?.let { (it - capture.start).toString() },
                                        "display_window_start_ns" to displayWindowStart?.toString(),
                                        "confirmed_window_end_ns" to confirmedEnd?.let { (it - capture.start).toString() },
                                        "mapped_window_ns" to sequence?.takeIf { capture.windowKnown }?.let { (it - capture.start).toString() },
                                        "clock_alignment_known" to known,
                                        "presentation_pts_us" to (moviePts ?: movieStart).toString(),
                                        "presented" to presented, "preroll" to (!presented && localMedia < startUs),
                                        "terminal_authoring_tick" to (sourceDurations[n] == null && duration == 1L),
                                        "uncertainty_ns" to frame.time.uncertainty.toString())))
                                    index.newLine()
                                    frameRows++
                                    media = Math.addExact(media, duration)
                                    sampleIndex = Math.incrementExact(sampleIndex)
                                }
                                edits += Edit(segmentDuration, Math.addExact(runMedia, startUs), 1f)
                                movie = Math.addExact(movie, segmentDuration)
                                val displayed = samples.drop(visibleIndex - keyIndex)
                                val alignmentKnown = displayed.all(::aligned) && displayed.indices.drop(1).all { k ->
                                    val a = displayed[k - 1]
                                    val b = displayed[k]
                                    a.time.epoch == b.time.epoch && !interrupted(a, b, closed, visibleIndex - keyIndex + k - 1)
                                }
                                parts += mapOf("file" to videoName, "segment_index" to runIndex,
                                    "clock_epoch" to samples.map { it.time.epoch }.distinct().singleOrNull(),
                                    "clock_epochs" to samples.map { it.time.epoch }.distinct(), "generation" to first.generation,
                                    "decode_run" to first.decodeRun.toString(), "source_pts_origin_us" to origin.toString(),
                                    "movie_start_us" to movieStart.toString(), "movie_end_us" to movie.toString(),
                                    "media_start_us" to runMedia.toString(), "edit_start_us" to Math.addExact(runMedia, startUs).toString(),
                                    "duration_us" to segmentDuration.toString(), "clock_alignment_known" to alignmentKnown,
                                    "window_start_ns" to visibleStart?.takeIf { alignmentKnown }?.let { (it - capture.start).toString() },
                                    "window_end_ns" to last.time.sequence?.takeIf { alignmentKnown }?.let {
                                        Math.addExact(it - capture.start, Math.multiplyExact(originalEnd - (last.pts - origin), 1000)).toString()
                                    },
                                    "confirmed_window_end_ns" to last.time.sequence?.takeIf { alignmentKnown && visibleStart != null && it > visibleStart }?.let { (it - capture.start).toString() },
                                    "media_timeline_clipped" to clipped, "preroll_samples" to (visibleIndex - keyIndex))
                                previousLast = last
                            }
                            track.setEdits(edits)
                            val movieBox = mux.finalizeHeader()
                            require(movieBox.timescale == 1_000_000) { "動画movie timescaleが不正です" }
                            require(edits.fold(0L) { sum, edit -> Math.addExact(sum, edit.duration) } == movie)
                            movieBox.tracks.single().setDuration(movie)
                            movieBox.setDuration(movie)
                            mux.storeHeader(movieBox)
                        }
                    }
                }
            }
            val deviceFile = file(ReplayFiles.LOGCAT_DEVICE)
            val appFile = file(ReplayFiles.LOGCAT_APP)
            writer(deviceFile).use { device ->
                writer(appFile).use { app ->
                    capture.logs.forEach { row ->
                        checkActive()
                        val source = row.source
                        val record = gson.toJson(mapOf("record_id" to row.id, "clock_epoch" to row.time.epoch,
                            "generation" to row.generation, "epoch_ns" to source.wall.toString(),
                            "elapsed_ns" to row.time.elapsed?.toString(), "window_ns" to row.time.sequence?.takeIf { capture.windowKnown }?.let { (it - capture.start).toString() },
                            "uncertainty_ns" to row.time.uncertainty.toString(), "uid" to source.uid, "pid" to source.pid,
                            "tid" to source.tid, "lid" to source.lid, "priority" to source.priority, "tag" to source.tag,
                            "message" to source.message, "app_membership" to row.app,
                            "decode_status" to source.decodeStatus, "payload_base64" to Base64.getEncoder().encodeToString(source.raw)))
                        device.write(record); device.newLine()
                        if (row.app == true) { app.write(record); app.newLine() }
                    }
                }
            }
            val touchFile = if (videoRequested) file(ReplayFiles.TOUCHES) else null
            var touchesInMovie = 0
            if (touchFile != null) writer(touchFile).use { out ->
                val anchors = movieAnchors.groupBy { it.epoch }.mapValues { (_, list) -> list.sortedBy { it.from } }
                capture.touches.forEach { row ->
                    checkActive()
                    val point = row.time.sequence?.takeIf { point ->
                        capture.windowKnown && row.time.uncertainty != Long.MAX_VALUE && videoScopes.none { gap ->
                            gap.stream == "clock" && gap.intersects(point, point, row.time.uncertainty)
                        }
                    }
                    val movieUs = point?.let { anchors[row.time.epoch]?.movieTime(it) }
                    if (movieUs != null) touchesInMovie++
                    val sample = row.sample
                    val screen = row.rotation?.let { screenPoint(sample.x, sample.y, row.width, row.height, it) }
                    out.write(gson.toJson(mapOf("clock_epoch" to row.time.epoch, "generation" to row.generation,
                        "device_mono_ns" to sample.monoNs.toString(), "elapsed_ns" to row.time.elapsed?.toString(),
                        "window_ns" to point?.let { (it - capture.start).toString() }, "uncertainty_ns" to row.time.uncertainty.toString(),
                        "movie_us" to movieUs?.toString(), "action" to sample.action.wire, "pointer" to sample.pointer,
                        "x" to sample.x, "y" to sample.y, "display_width" to row.width, "display_height" to row.height,
                        "display_rotation" to row.rotation, "screen_x" to screen?.first, "screen_y" to screen?.second,
                        "input_device" to sample.device)))
                    out.newLine()
                }
            }
            val videoHoles = mutableListOf<Map<String, String>>()
            var coveredUntil = 0L
            val windowLength = capture.end - capture.start
            val knownParts = frameCoverage.sortedBy { it.first }
            knownParts.forEach { (from, to) ->
                if (from > coveredUntil + 1_000_000) videoHoles += mapOf("from_window_ns" to coveredUntil.toString(), "to_window_ns" to from.toString())
                coveredUntil = maxOf(coveredUntil, to)
            }
            if (videoRequested && coveredUntil + 1_000_000 < windowLength) videoHoles += mapOf("from_window_ns" to coveredUntil.toString(), "to_window_ns" to windowLength.toString())
            // Only a displayed tail can cover a hole without confirming a newly received image.
            if (tail?.displayHeld == true && tail.fromNs != null && tail.toNs != null) {
                val from = tail.fromNs - capture.start
                val to = tail.toNs - capture.start
                val remainingHoles = videoHoles.flatMap { hole ->
                    val a = hole.getValue("from_window_ns").toLong()
                    val b = hole.getValue("to_window_ns").toLong()
                    if (b <= from || a >= to) listOf(hole) else buildList {
                        if (a < from) add(mapOf("from_window_ns" to a.toString(), "to_window_ns" to from.toString()))
                        if (b > to) add(mapOf("from_window_ns" to to.toString(), "to_window_ns" to b.toString()))
                    }
                }
                videoHoles.clear(); videoHoles.addAll(remainingHoles)
            }
            val knownLoss = capture.gaps.inWindow(capture.start, capture.end, capture.endUncertainty, capture.clocks, capture.clockRecoveries)
                .any { it.stream == "video" || it.stream == "clock" }
            val videoIncomplete = videoRequested && (parts.isEmpty() || losses.isNotEmpty() || knownLoss || videoHoles.isNotEmpty())
            val files = linkedMapOf("readme" to ReplayFiles.SUMMARY, "record" to ReplayFiles.RECORD,
                "video" to ReplayFiles.VIDEO.takeIf { parts.isNotEmpty() }, "video_frames" to framesFile?.fileName?.toString(),
                "video_source" to rawPath?.fileName?.toString(), "logcat_device" to deviceFile.fileName.toString(),
                "logcat_app" to appFile.fileName.toString(), "touches" to touchFile?.fileName?.toString())
            val counts = linkedMapOf("device_log_rows" to capture.logs.size, "app_log_rows" to capture.logs.count { it.app == true },
                "video_frames" to frameRows, "touches" to touchFile?.let { capture.touches.size }, "touches_in_movie" to touchFile?.let { touchesInMovie })
            val readme = file(ReplayFiles.SUMMARY)
            writer(readme).use { it.write(buildString {
                fun cell(value: Any?) = (value ?: "—").toString().replace("|", "\\|").replace("\n", " ")
                fun row(vararg values: Any?) = appendLine(values.joinToString(" | ", "| ", " |") { cell(it) })
                val triggerLabel = if (crash) "クラッシュ時の自動保存" else "手動保存"
                val contentLabel = when {
                    videoExcluded -> "ログのみ（動画は意図的に除外）"
                    videoRequested -> "動画＋ログ"
                    else -> "ログ専用（動画は未要求）"
                }
                val local = createdAt.withNano(0)
                appendLine("# 再現記録 ${local.format(DateTimeFormatter.ofPattern("yyyy-MM-dd HH:mm:ss"))}（$triggerLabel / $contentLabel）")
                appendLine()
                appendLine("Android Replay Bufferが保存した不具合の再現記録です。機械向けの同じ事実は${ReplayFiles.RECORD}にあります。")
                appendLine()
                appendLine("## 概要")
                appendLine()
                row("項目", "内容")
                appendLine("|---|---|")
                row("保存日時", "${local.toOffsetDateTime()}（UTC ${local.toInstant()}）")
                row("きっかけ", if (crash) "$triggerLabel（event_id: ${capture.autoTrigger?.eventId}）" else triggerLabel)
                row("内容", when {
                    videoExcluded -> "$contentLabel。動画の取得は継続しています（coverage.videoはEXCLUDED）"
                    else -> contentLabel
                })
                row("端末", capture.device?.let { "${it.name}（serial: ${it.serial}、${if (it.kind == DeviceKind.EMULATOR) "エミュレータ" else "実機"}）" } ?: "不明")
                row("対象アプリ", capture.settings.application.packageName
                    ?: capture.apps.mapNotNull { it.packageName }.distinct().joinToString().ifEmpty { null } ?: "未確定")
                row("保存範囲", "直前${capture.seconds}秒の設定に対し${(capture.end - capture.start) / 1e9}秒")
                row("動画", if (parts.isEmpty()) "なし" else "1本（再生区間${parts.size}件、frame ${frameRows}件）")
                row("全体ログ", "${counts["device_log_rows"]}行")
                row("アプリログ", "${counts["app_log_rows"]}行")
                row("タップ", if (touchFile == null) "記録なし" else "${capture.touches.size}件（うち動画に重なる${touchesInMovie}件）")
                val missing = (capture.missingKinds() + if (videoIncomplete) listOf("video") else emptyList()).distinct()
                row("記録の不足・未確認", if (missing.isEmpty()) "なし" else missing.joinToString("、") { STREAM_NAMES[it] ?: it } +
                    "（${ReplayFiles.RECORD}のcoverage・gaps${if ("video" in missing) "・video_missing_ranges" else ""}を確認）")
                row("save_id", capture.id)
                appendLine()
                appendLine("## ファイル")
                appendLine()
                row("ファイル", "内容")
                appendLine("|---|---|")
                row(ReplayFiles.SUMMARY, "この要約")
                row(ReplayFiles.RECORD, "機械向けの記録（取得状態・時計・gap・アプリ履歴・各ファイルのSHA-256）")
                files["video"]?.let { row(it, "画面の動画（MP4 1本）") }
                files["video_frames"]?.let { row(it, "動画の各frameの時刻索引（1行1frame）") }
                files["video_source"]?.let { row(it, "再エンコード前の元映像（AnnexB）") }
                row(files["logcat_device"], "端末全体のログ（1行1レコード）")
                row(files["logcat_app"], "対象アプリのログ（全体ログのうち対象アプリの行）")
                files["touches"]?.let { row(it, "タップの座標と時刻（1行1タップ）") }
                appendLine()
                appendLine("## 記録の状態")
                appendLine()
                row("記録", "状態", "保持秒", "理由")
                appendLine("|---|---|---|---|")
                capture.states.forEach { (kind, state) -> row(kind, state.state, state.availableSeconds, state.reason) }
                if (parts.isNotEmpty()) {
                    appendLine()
                    row("再生区間", "動画µs", "時計対応確認", "窓開始ns", "窓終了ns", "preroll")
                    appendLine("|---|---|---|---|---|---|")
                    parts.forEach { row("${it["file"]} 区間${it["segment_index"]}", "${it["movie_start_us"]}〜${it["movie_end_us"]}",
                        it["clock_alignment_known"], it["window_start_ns"], it["window_end_ns"], it["preroll_samples"]) }
                }
                if (capture.gaps.isNotEmpty()) {
                    appendLine()
                    appendLine("取得履歴です。保存窓の外と確認できた履歴も保全しています。保存時の警告は保存窓内又は重なり未確認の記録が対象です。")
                    appendLine()
                    row("記録", "開始ns", "終了ns", "理由")
                    appendLine("|---|---|---|---|")
                    capture.gaps.forEach { row(it.stream, it.fromNs, it.toNs, it.reason) }
                }
                if (losses.isNotEmpty()) {
                    appendLine()
                    losses.forEach { appendLine("- loss: $it") }
                }
                if (tail != null) {
                    appendLine()
                    appendLine("- 動画末尾: ${tail.fromNs}〜${tail.toNs}nsは新frame未確認。前の画像の表示保持=${tail.displayHeld}。再生時間は確認済み取得時間とは異なります。")
                }
                capture.autoTrigger?.let { trigger ->
                    appendLine()
                    appendLine("## クラッシュ")
                    appendLine()
                    appendLine("- 対象アプリの例外報告を相互確認して自動保存しました。event_id: ${trigger.eventId}")
                    appendLine("- 基準はplatformの報告時刻で、processの死亡時刻ではありません。報告前の記録不足・各原行の包含は${ReplayFiles.RECORD}のauto_triggerを確認してください。")
                    appendLine("- 報告前の不足（概算）: ${trigger.facts["pre_report_shortfall_nominal_ns"]}ns。取得gapと未観測の端末ログは別で、完全なクラッシュ直前の記録を保証しません。")
                }
                appendLine()
                appendLine("## 動画とログの照合方法")
                appendLine()
                if (videoRequested) {
                    appendLine("- 動画がある保存は1本のMP4です。動画がない保存にはMP4と${ReplayFiles.VIDEO_FRAMES}はありません。取得状態・理由は${ReplayFiles.RECORD}のcoverage.videoを確認してください。")
                    appendLine("- 動画の表示秒に対応する${ReplayFiles.VIDEO_FRAMES}のmovie_pts_us〜movie_end_usとsource/window情報を使い、両ログと照合してください。${ReplayFiles.RECORD}のpartsは同じファイル内の再生区間です。")
                    appendLine("- タップは動画の取得中に自動で記録します。時刻は端末の入力時刻（getevent、CLOCK_MONOTONIC）で、動画PTSと同じ時計対応によりwindow_nsへ写し、表示中の動画位置をmovie_usに記録します。movie_usがnullの行は動画の表示区間外または時刻を確定できない行です。座標は画面の自然な向きのpx（display_width×display_height）です。screen_x/screen_yは回転後の画面（動画に写る向き）のpxで、display_rotationは端末の回転（0〜3、90度単位）、不明ならnullです。回転は最大1秒遅れで読み直し、動画のサイズ変更時はすぐ読み直します。")
                    appendLine("- タップ位置の表示（show_touches）: 設定${if (capture.settings.showTouches) "ON（取得中だけ端末設定を変更し、終了時に元へ戻します）" else "OFF"}、" +
                        when (capture.touchDots) {
                            TouchDots.APPLIED -> "この取得では端末に点を表示しました"
                            TouchDots.ALREADY_ON -> "端末で既にONのため動画に点が映っています（端末設定は変更していません）"
                            TouchDots.FAILED -> "端末設定を変更できず、動画に点は映っていません"
                            TouchDots.OFF -> "動画に点は映っていません"
                            null -> "点を表示できたか確認できていません"
                        })
                    appendLine("- frameのclock_alignment_known=falseでは時計対応が不明です。window_ns=null、mapped_window_nsは不確実性を含む変換値で、同期確定やcoverageには使えません。区間開始に動画全体の表示秒を足す方法では照合できません。")
                    if (rawPath != null) appendLine("- サイズ変更を含むため固定canvasへ再エンコードしています。拡縮・切捨てはせず、余白を追加しています。元AnnexB config/全packetは${ReplayFiles.VIDEO_SOURCE}へ保存し${ReplayFiles.VIDEO_FRAMES}のoffset/size/hashで照合できます。色未指定は不明のままで、色一致の証明ではありません。")
                    appendLine("- video_missing_rangesは映像欠落又は時計対応未確認の範囲です。既知の取得gapは空白として表示し、不明gapの実durationはnullのまま、MP4上の表示間隔と区別しています。")
                    appendLine("- 次sampleのない末尾はsource_duration_us=nullです。MP4の最小1tickは表示のための値で、観測した画像の継続時間や取得coverageではありません。")
                    appendLine("- MP4内部には論理窓の前のdecoder preroll画像を含むことがあります。edit listで表示範囲を指定しています。標準playerの互換性は製品QAで別途確認します。")
                } else {
                    appendLine("- 動画の取得・保存は要求していません。MP4・時刻索引・動画sourceはありません。動画未要求は取得失敗や欠落ではありません。")
                }
                appendLine("- ログの各行のwindow_nsは保存範囲の先頭からの時刻です。動画がある場合は${ReplayFiles.VIDEO_FRAMES}のwindow_nsと同じ時計です。")
                appendLine()
                appendLine("## 注意")
                appendLine()
                appendLine("- 全体ログはshell権限で読めるlogcat bufferです。security等の全端末ログ取得を保証しません。app_membership=nullは対象未確定です。")
                appendLine("- 0行は正常な無出力の場合もあります。取得状態・時計・アプリ履歴・lossを${ReplayFiles.RECORD}で確認してください。")
                appendLine("- 録画とログには個人情報や秘密情報が含まれることがあります。共有する前に中身を確認してください。")
            }) }
            checkActive()
            checkSpace()
            val hashes = own.entries.associate { (path, output) -> checkActive(); checkOwned(path); path.fileName.toString() to output.hash() }
            checkActive()
            manifest = mapOf("schema" to ReplayFiles.SCHEMA, "capture_mode" to capture.settings.captureMode, "complete" to true, "save_id" to capture.id,
                "folder_name" to name, "trigger" to if (crash) "crash" else "manual", "files" to files, "counts" to counts,
                "save_request" to mapOf("content" to capture.request.content, "origin" to capture.request.origin, "video_excluded" to videoExcluded),
                "auto_trigger" to capture.autoTrigger?.facts,
                "sequence_id" to capture.sequence, "generation" to capture.generation, "device" to capture.device,
                "window_clock_epoch" to capture.windowClockEpoch,
                "window_clock_known" to capture.windowKnown,
                "application_at_save" to capture.settings.application,
                "window_start_ns" to capture.start.toString(), "window_end_ns" to capture.end.toString(),
                "replay_seconds" to capture.seconds, "created_at" to createdAt.toInstant().toString(), "created_at_local" to createdAt.toOffsetDateTime().toString(),
                "build" to CaptureResources.identity().filterKeys { it in setOf("source", "dirty", "version") },
                "plugin_id" to "io.github.shinma06.android-replay-buffer",
                "dependencies" to CaptureResources.identity(), "settings" to mapOf("replay_seconds" to capture.seconds,
                    "capture_mode" to capture.settings.captureMode, "video_requested" to capture.settings.captureMode.videoRequested,
                    "video_byte_limit" to ReplaySettings.VIDEO_BYTES, "log_byte_limit" to ReplaySettings.LOG_BYTES,
                    "min_free_bytes" to ReplaySettings.MIN_FREE_BYTES,
                    "max_video_packets" to ReplaySettings.MAX_VIDEO_PACKETS, "max_config_packet_bytes" to ReplaySettings.MAX_CONFIG_PACKET_BYTES,
                    "config_memory_bytes" to ReplaySettings.CONFIG_MEMORY_BYTES,
                    "video" to capture.settings.videoQuality.let {
                        "H.264 / ${it.maxSize} / ${it.maxFps}fps / ${it.bitRate / 1_000_000}Mbps / no B-frame"
                    }.takeIf { videoRequested },
                    "video_request" to capture.settings.videoQuality.let {
                        mapOf("quality" to it, "max_size" to it.maxSize, "max_fps" to it.maxFps,
                            "video_bit_rate" to it.bitRate, "downsize_on_error" to false)
                    }.takeIf { videoRequested }),
                "window_end_uncertainty_ns" to capture.endUncertainty.toString(),
                "clock_samples" to capture.clocks.map { clockJson(it) },
                "clock_epochs" to (capture.clocks.map { it.epoch } + capture.video.map { it.time.epoch } + capture.logs.map { it.time.epoch } +
                    capture.touches.map { it.time.epoch } +
                    capture.gaps.mapNotNull { it.clockEpoch }).distinct().map { id -> mapOf("id" to id,
                    "has_valid_sample" to capture.clocks.any { it.epoch == id && it.valid }) }, "application_history" to capture.apps.map { appJson(it) },
                "coverage" to capture.states.mapValues { (kind, state) -> if (videoExcluded && kind == "video") mapOf("state" to "EXCLUDED",
                    "requested" to false, "excluded" to true, "capture_state" to state.state, "availableSeconds" to 0.0,
                    "reason" to "ログのみの保存として動画を意図的に除外しました（動画の取得は継続しています）") else mapOf("state" to state.state,
                    "requested" to (state.state != StreamState.NOT_REQUESTED),
                    "availableSeconds" to state.availableSeconds, "reason" to state.reason) },
                "gaps" to capture.gaps.map { gap -> mapOf("kind" to gap.stream,
                    "from_ns" to gap.fromNs?.toString(), "to_ns" to gap.toNs?.toString(), "reason" to gap.reason,
                    "time_axis" to "sequence_ns", "generation" to gap.generation, "clock_epoch" to gap.clockEpoch,
                    "boundary_uncertainty_ns" to gap.boundaryUncertaintyNs?.toString(),
                    "duration_uncertain" to (gap.fromNs == null || gap.toNs == null || gap.boundaryUncertaintyNs == null || gap.boundaryUncertaintyNs == Long.MAX_VALUE)) },
                "loss" to losses, "logcat_overflow_count" to null,
                "logcat_loss_note" to "reader/framing/byte上限の中断はgapsへ記録。Android buffer内の未観測overflow数は不明。tail以前をbackfillしたとは主張しない", "watermarks" to mapOf("video_pts_us" to capture.video.lastOrNull()?.pts?.toString(),
                    "device_log_record_id" to capture.logs.lastOrNull()?.id, "app_log_record_id" to capture.logs.lastOrNull { it.app == true }?.id),
                "touches" to capture.touchState?.let { state -> mapOf("state" to state.state, "reason" to state.reason,
                    "count" to capture.touches.size, "in_movie_count" to touchesInMovie, "dropped_count" to capture.touchDropped,
                    "show_touches" to capture.settings.showTouches, "show_touches_state" to capture.touchDots?.wire, "time_source" to "getevent -lt (device CLOCK_MONOTONIC)",
                    "coordinate_space" to "display_px_natural_orientation", "screen_coordinate_space" to "display_px_current_rotation",
                    "gaps" to capture.touchGaps.filter { (it.to == null || it.to >= capture.start) && (it.from == null || it.from <= capture.end) }
                        .map { mapOf("from_ns" to it.from?.toString(), "to_ns" to it.to?.toString(), "reason" to it.reason, "time_axis" to "sequence_ns") }) },
                "video_reencoded" to (normalizer != null),
                "video_canvas" to normalizer?.let { mapOf("width" to it.width, "height" to it.height, "colour_known" to it.colourKnown) },
                "video_movie_duration_us" to parts.lastOrNull()?.get("movie_end_us"),
                "parts" to parts, "video_run_boundaries" to runBoundaries, "video_missing_ranges" to videoHoles,
                "video_clock_gap_scopes" to capture.gaps.mapIndexedNotNull { i, gap ->
                    if (!videoRequested || gap.stream != "clock") null else videoScopes[i].let { scope -> mapOf(
                        "gap_index" to i, "from_ns" to scope.fromNs?.toString(), "to_ns" to scope.toNs?.toString(),
                        "boundary_uncertainty_ns" to scope.boundaryUncertaintyNs?.toString(),
                        "derived_from_valid_samples" to (scope !== gap),
                        "recovery_sample" to capture.clockRecoveries[gap]?.let(::clockJson)) }
                },
                "video_gap_scopes" to capture.gaps.mapIndexedNotNull { i, gap ->
                    if (gap.stream != "video") null else videoScopes[i].let { scope -> mapOf(
                        "gap_index" to i, "generation" to gap.generation, "clock_epoch" to gap.clockEpoch,
                        "from_ns" to scope.fromNs?.toString(), "to_ns" to scope.toNs?.toString(),
                        "boundary_uncertainty_ns" to scope.boundaryUncertaintyNs?.toString(),
                        "derived_from_valid_samples" to (scope !== gap),
                        "before_sample" to videoProofs[gap]?.first?.let(::clockJson),
                        "after_sample" to videoProofs[gap]?.second?.let(::clockJson)) }
                },
                "video_tail" to tail?.let { mapOf("from_ns" to it.fromNs?.toString(), "to_ns" to it.toNs?.toString(),
                    "source_pts_us" to it.sourcePtsUs.toString(), "source_sequence_ns" to it.sourceSequenceNs?.toString(),
                    "clock_epoch" to it.clockEpoch, "generation" to it.generation, "display_held" to it.displayHeld,
                    "time_axis" to "sequence_ns", "new_frame_confirmed" to false) }, "files_sha256" to hashes)
            writer(file(ReplayFiles.RECORD)).use { it.write(gson.toJson(manifest)) }
            own.forEach { (path, output) -> checkActive(); checkOwned(path); output.channel().use { ch -> ch.force(true) }; checkActive() }
            checkSpace()
            checkActive()
            own.keys.forEach(::checkOwned)
            checkEntries()
            // Verification needs only their identities, so close them where open files block renaming their directory.
            if (!SaveOutputDirectory.renamesWithOpenFiles) own.values.forEach(SaveOutputFile::close)
            publish(SavePublication(partial, complete, parentHandle, staging) { checkEntries() })
            own.keys.forEach { checkOwned(complete.resolve(it.fileName)) }
            checkEntries()
            return SaveOutput(complete, if (videoIncomplete) listOf("video") else emptyList())
        } catch (e: Throwable) {
            if (e !is Exception && e !is LinkageError) throw e
            // No path-based deletion: ownership checks cannot make a subsequent unlink conditional on identity.
            manifest?.let { value -> own[partial?.resolve(ReplayFiles.RECORD)]?.let { held -> runCatching {
                val output = if (!held.isClosed) held else staging!!.openFile(ReplayFiles.RECORD).also { reopened ->
                    if (reopened.key != held.key) { reopened.close(); error("保存出力の実体が変更されています") }
                }
                try { output.channel().use { channel ->
                    channel.position(0)
                    channel.truncate(0)
                    val bytes = ByteBuffer.wrap(gson.toJson(value + ("complete" to false)).toByteArray(Charsets.UTF_8))
                    while (bytes.hasRemaining()) channel.write(bytes)
                    channel.force(true)
                } } finally { if (output !== held) output.close() }
            } } }
            val retained = listOfNotNull(partial, complete).firstOrNull(::ownsDirectory)
            val location = retained?.let { "。未完成の出力を保全しています: $it" } ?: ""
            if (e is SaveCancelled) throw SaveCancelled(retained, e)
            val reason = if (e is IllegalArgumentException || e is IllegalStateException) e.message ?: "保存できません"
                else "保存に失敗しました（保存先・空き容量・アクセス権を確認してください）"
            throw SaveFailure(retained, reason + location, e)
        } finally {
            own.values.forEach { it.close() }
            staging?.close()
            parentHandle?.close()
        }
    }

    private fun bytesHash(bytes: ByteArray) = java.util.HexFormat.of().formatHex(java.security.MessageDigest.getInstance("SHA-256").digest(bytes))

    private fun readFrame(frame: VideoEntry): ByteArray {
        require(frame.offset >= 0 && frame.size in 1..16 * 1024 * 1024)
        val end = Math.addExact(frame.offset, frame.size.toLong())
        require(Files.isRegularFile(frame.file, NOFOLLOW_LINKS))
        val result = ByteBuffer.allocate(frame.size)
        FileChannel.open(frame.file, READ, NOFOLLOW_LINKS).use { input ->
            require(input.size() >= end)
            var position = frame.offset
            while (result.hasRemaining()) { val n = input.read(result, position); check(n > 0); position += n }
        }
        return result.array()
    }

    private fun clockJson(c: ClockSample): Map<String, Any?> = mapOf("epoch" to c.epoch, "boot" to c.boot,
        "elapsed_before_ns" to c.before.toString(), "mono_ns" to c.mono.toString(), "epoch_ns" to c.wall.toString(),
        "elapsed_after_ns" to c.after.toString(), "host_sent_ns" to c.sent.toString(), "host_received_ns" to c.received.toString(),
        "read_uncertainty_ns" to c.readError.toString(), "sequence_offset_ns" to c.sequenceOffset?.toString(),
        "bridge_uncertainty_ns" to c.bridgeError.toString(), "valid" to c.valid)

    private fun appJson(a: AppPeriod): Map<String, Any?> = mapOf("package" to a.packageName, "uid" to a.uid,
        "pids" to a.pids, "uid_exclusive" to a.uidExclusive, "from_elapsed_ns" to a.from?.toString(), "to_elapsed_ns" to a.to?.toString(),
        "to_elapsed_unknown" to a.endUnknown, "clock_epoch" to a.epoch)
}

/** Playback proof is captured at acquisition; clock quality never authorizes source adjacency. */
/** For each wanted key, the index of its first occurrence in [keys], or null; one pass over [keys], one lookup per key. */
internal fun <K : Any> firstIndexes(keys: List<K>, wanted: List<K?>): List<Int?> {
    val index = HashMap<K, Int>(keys.size * 2)
    keys.forEachIndexed { n, key -> index.putIfAbsent(key, n) }
    return wanted.map { key -> key?.let(index::get) }
}

internal fun VideoEntry.playbackContinuousTo(next: VideoEntry): Boolean {
    if (generation <= 0 || generation != next.generation || session <= 0 || session != next.session ||
        decodeRun <= 0 || decodeRun != next.decodeRun || packetOrdinal <= 0 || packetOrdinal == Long.MAX_VALUE ||
        next.packetOrdinal != packetOrdinal + 1 || width <= 0 || height <= 0 || width != next.width || height != next.height ||
        config.isEmpty() || !config.contentEquals(next.config) || pts < 0 || next.pts <= pts ||
        offset < 0 || size <= 0 || next.offset < 0 || next.size <= 0) return false
    return if (file != next.file) next.key && next.offset == 0L else try {
        Math.addExact(offset, size.toLong()) == next.offset
    } catch (_: ArithmeticException) { false }
}

/** A presented frame's display interval: [from] in sequence ns, [durationUs] and [movieUs] on the saved MP4 timeline. */
internal data class MovieAnchor(val epoch: Int, val from: Long, val durationUs: Long, val movieUs: Long)

/** Movie time showing [point], or null when no presented frame covers it; [this] is sorted by [MovieAnchor.from]. */
internal fun List<MovieAnchor>.movieTime(point: Long): Long? {
    var low = 0
    var high = size - 1
    var found = -1
    while (low <= high) {
        val mid = (low + high) ushr 1
        if (this[mid].from <= point) { found = mid; low = mid + 1 } else high = mid - 1
    }
    val anchor = getOrNull(found) ?: return null
    val offset = (point - anchor.from) / 1000
    return if (offset <= anchor.durationUs) anchor.movieUs + offset else null
}
