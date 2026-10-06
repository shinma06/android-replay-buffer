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
import java.nio.channels.FileChannel
import java.nio.file.Files
import java.nio.file.LinkOption.NOFOLLOW_LINKS
import java.nio.file.Path
import java.nio.file.StandardOpenOption.CREATE_NEW
import java.nio.file.StandardOpenOption.READ
import java.nio.file.StandardOpenOption.WRITE
import java.time.Instant
import java.util.Base64
import java.util.UUID
import java.util.concurrent.CancellationException

internal data class SaveOutput(val directory: Path, val missingKinds: List<String>)

internal class SaveFailure(val partial: Path?, message: String, cause: Throwable? = null) : Exception(message, cause)

/** No external encoder/process: immutable packet indices -> AVC MP4 and matching JSONL/manifest. */
internal class SaveWriter(private val availableSpace: (Path) -> Long = { Files.getFileStore(it).usableSpace }) {
    private val gson = GsonBuilder().serializeNulls().disableHtmlEscaping().create()

    fun write(capture: FrozenCapture, folder: Path, cancelled: () -> Boolean,
              publish: (Path, Path) -> Unit): SaveOutput {
        var partial: Path? = null
        val own = mutableListOf<Path>()
        var normalizationStarted: Long? = null
        var normalizationCpuStarted = 0L
        val cpu = java.lang.management.ManagementFactory.getThreadMXBean()
        fun checkActive() {
            if (cancelled() || Thread.currentThread().isInterrupted) throw CancellationException()
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
            val tail = capture.videoTail()
            val all = capture.video
            val groups = mutableListOf<MutableList<VideoEntry>>()
            all.forEach { frame ->
                val previous = groups.lastOrNull()?.lastOrNull()
                if (previous == null || !previous.playbackContinuousTo(frame)) groups += mutableListOf(frame)
                else groups.last() += frame
            }
            fun visible(group: List<VideoEntry>, n: Int): Boolean {
                val frame = group[n]
                val sequence = frame.time.sequence
                if (!capture.windowKnown || sequence == null || frame.time.uncertainty == Long.MAX_VALUE) return true
                val duration = group.getOrNull(n + 1)?.let { Math.subtractExact(it.pts, frame.pts) }
                val end = duration?.let { Math.addExact(sequence, Math.multiplyExact(it, 1000)) }
                return sequence <= capture.end && (sequence >= capture.start || end != null && end > capture.start ||
                    tail?.displayHeld == true && frame === all.lastOrNull())
            }
            val selected = groups.filter { group -> group.indices.any { visible(group, it) } }
            val sourceConfigs = selected.map { it.first().config }.distinctBy { ByteBuffer.wrap(it) }
            val sizes = sourceConfigs.map { config ->
                require(config.size in 1..ReplaySettings.MAX_CONFIG_PACKET_BYTES)
                val sps = H264Utils.getRawSPS(ByteBuffer.wrap(config))
                require(sps.isNotEmpty()) { "動画SPSがありません" }
                H264Utils.getPicSize(boundedSps(sps.first()))
            }
            val normalizer = if (sizes.distinct().size > 1) MixedVideoNormalizer(sourceConfigs) else null
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
            val name = "replay-${Instant.now().toString().replace(':', '-')}-${UUID.randomUUID()}"
            partial = Files.createDirectory(parent.resolve(".$name.partial"))
            runCatching { Files.setPosixFilePermissions(partial, java.nio.file.attribute.PosixFilePermissions.fromString("rwx------")) }
            val complete = parent.resolve(name)
            fun file(name: String): Path = partial.resolve(name).also { own.add(it) }
            val parts = mutableListOf<Map<String, Any?>>()
            val frameCoverage = mutableListOf<Pair<Long, Long>>()
            val runBoundaries = mutableListOf<Map<String, Any?>>()
            val losses = mutableListOf<String>()
            val videoProofs = mutableMapOf<CaptureGap, Pair<ClockSample, ClockSample>>()
            val videoScopes = capture.gaps.videoScopes(capture.clocks, capture.clockRecoveries, videoProofs)
            fun aligned(frame: VideoEntry): Boolean {
                val sequence = frame.time.sequence ?: return false
                return capture.windowKnown && frame.time.uncertainty != Long.MAX_VALUE && videoScopes.none { gap ->
                    gap.stream == "clock" && gap.intersects(sequence, sequence, frame.time.uncertainty)
                }
            }
            val framesFile = file("frames.jsonl")
            val rawPath = if (normalizer != null) file("video-source.bin") else null
            (rawPath?.let { FileChannel.open(it, CREATE_NEW, WRITE, NOFOLLOW_LINKS) }).use { raw ->
                Files.newBufferedWriter(framesFile, Charsets.UTF_8, CREATE_NEW, WRITE).use { index ->
                    if (selected.isNotEmpty()) {
                        val videoName = "video-001.mp4"
                        val target = file(videoName)
                        org.jcodec.common.io.FileChannelWrapper(FileChannel.open(target, CREATE_NEW, READ, WRITE, NOFOLLOW_LINKS)).use { output ->
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
                                val lastIndex = group.indices.last { visible(group, it) }
                                val keyIndex = (visibleIndex downTo 0).firstOrNull { group[it].key }
                                require(keyIndex != null) { "IDRを失った動画区間を復号できません" }
                                val samples = group.subList(keyIndex, lastIndex + 1)
                                val first = group[visibleIndex]
                                val last = samples.last()
                                val origin = samples.first().pts
                                val terminal = if (last === all.lastOrNull() && tail?.displayHeld == true)
                                    ((tail.toNs!! - last.time.sequence!!) / 1000).coerceAtLeast(1) else 1L
                                val sourceDurations = samples.mapIndexed { n, frame ->
                                    group.getOrNull(keyIndex + n + 1)?.let { Math.subtractExact(it.pts, frame.pts) }
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
                                val beforeSequence = before?.time?.sequence
                                val firstSequence = first.time.sequence
                                val boundaryGaps = if (before == null) emptyList() else capture.gaps.filter { gap ->
                                    val from = gap.fromNs
                                    val to = gap.toNs
                                    gap.stream == "video" && gap.generation == before.generation && from != null && to != null &&
                                        aligned(before) && aligned(first) && beforeSequence != null && firstSequence != null &&
                                        from >= beforeSequence && to <= firstSequence && from <= to
                                }
                                val knownGap = boundaryGaps.takeIf { it.isNotEmpty() && it.all { gap ->
                                    gap.boundaryUncertaintyNs != null && gap.boundaryUncertaintyNs in 0 until Long.MAX_VALUE
                                } }?.sortedBy { it.fromNs }?.let { gaps ->
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
                                        require(normalizer.width.toLong() * normalizer.height / 256 * 1_000_000 <= 983_040L * minOf(observed, 1_000_000_000L) &&
                                            encoded!!.remaining().toLong() * 8 * 1_000_000 <= 240_000_000L * minOf(observed, 1_000_000_000L)) {
                                            "観測PTS間隔に対する変換出力のMBPS・bitrateがLevel 5.1上限を超えています"
                                        }
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
                                        next.time.sequence!! > sequence!! && videoScopes.none { gap ->
                                            (gap.stream == "video" || gap.stream == "clock") && gap.intersects(sequence, next.time.sequence,
                                                maxOf(frame.time.uncertainty, next.time.uncertainty))
                                        }) minOf(capture.end, next.time.sequence) else null
                                    val displayWindowStart = sequence?.takeIf { known && presented }?.let {
                                        Math.addExact(it, Math.multiplyExact(sourceDisplayOffset, 1000)) - capture.start
                                    }
                                    if (displayWindowStart != null && confirmedEnd != null && confirmedEnd - capture.start > displayWindowStart)
                                        frameCoverage += maxOf(0, displayWindowStart) to (confirmedEnd - capture.start)
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
                                    media = Math.addExact(media, duration)
                                    sampleIndex = Math.incrementExact(sampleIndex)
                                }
                                edits += Edit(segmentDuration, Math.addExact(runMedia, startUs), 1f)
                                movie = Math.addExact(movie, segmentDuration)
                                val displayed = samples.drop(visibleIndex - keyIndex)
                                val alignmentKnown = displayed.all(::aligned) && displayed.zipWithNext().all { (a, b) ->
                                    a.time.epoch == b.time.epoch && videoScopes.none { gap ->
                                        (gap.stream == "clock" || gap.stream == "video") && gap.intersects(a.time.sequence, b.time.sequence!!,
                                            maxOf(a.time.uncertainty, b.time.uncertainty))
                                    }
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
            val deviceFile = file("logcat-device.jsonl")
            val appFile = file("logcat-app.jsonl")
            Files.newBufferedWriter(deviceFile, Charsets.UTF_8, CREATE_NEW, WRITE).use { device ->
                Files.newBufferedWriter(appFile, Charsets.UTF_8, CREATE_NEW, WRITE).use { app ->
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
            val readme = file("README.txt")
            Files.writeString(readme, buildString {
                appendLine("Android Replay Buffer / 保存対象 ${capture.id}")
                appendLine("論理窓: ${(capture.end - capture.start) / 1e9}秒（設定${capture.seconds}秒）、sequence ${capture.sequence}")
                appendLine("動画${if (parts.isEmpty()) 0 else 1}本（再生区間${parts.size}件） / 全体ログ${capture.logs.size}行 / アプリログ${capture.logs.count { it.app == true }}行")
                appendLine("動画は1本のMP4です。表示秒に対応するframes.jsonlのmovie_pts_us〜movie_end_usとsource/window情報を使い、両ログと照合してください。session.jsonのpartsは同じファイル内の再生区間です。")
                appendLine("frameのclock_alignment_known=falseでは時計対応が不明です。window_ns=null、mapped_window_nsは不確実性を含む変換値で、同期確定やcoverageには使えません。区間開始に動画全体の表示秒を足す方法では照合できません。")
                if (rawPath != null) appendLine("サイズ変更を含むため固定canvasへ再エンコードしています。拡縮・切捨てはせず、余白を追加しています。元AnnexB config/全packetはvideo-source.binへ保存しframes.jsonlのoffset/size/hashで照合できます。色未指定は不明のままで、色一致の証明ではありません。")
                appendLine("video_missing_rangesは映像欠落又は時計対応未確認の範囲です。既知の取得gapは空白として表示し、不明gapの実durationはnullのまま、MP4上の表示間隔と区別しています。")
                appendLine("次sampleのない末尾はsource_duration_us=nullです。MP4の最小1tickは表示のための値で、観測した画像の継続時間や取得coverageではありません。")
                appendLine("MP4内部には論理窓の前のdecoder preroll画像を含むことがあります。edit listで表示範囲を指定しています。標準playerの互換性は製品QAで別途確認します。")
                if (tail != null) appendLine("動画末尾: ${tail.fromNs}〜${tail.toNs}nsは新frame未確認。前の画像の表示保持=${tail.displayHeld}。再生時間は確認済み取得時間とは異なります。")
                appendLine("全体ログはshell権限で読めるlogcat bufferです。security等の全端末ログ取得を保証しません。app_membership=nullは対象未確定です。")
                appendLine("0行は正常な無出力の場合もあります。取得状態・時計・アプリ履歴・lossをsession.jsonで確認してください。")
                parts.forEach { appendLine("${it["file"]} 区間${it["segment_index"]} / 動画µs=${it["movie_start_us"]}〜${it["movie_end_us"]}: 時計対応確認=${it["clock_alignment_known"]} / 窓開始ns=${it["window_start_ns"]} / 窓終了ns=${it["window_end_ns"]} / preroll=${it["preroll_samples"]}") }
                capture.gaps.forEach { appendLine("${it.stream}: ${it.fromNs}〜${it.toNs}ns / ${it.reason}") }
                losses.forEach { appendLine("loss: $it") }
            }, Charsets.UTF_8, CREATE_NEW, WRITE)
            checkActive()
            checkSpace()
            val hashes = own.associate { checkActive(); it.fileName.toString() to sha256(it) }
            checkActive()
            val videoHoles = mutableListOf<Map<String, String>>()
            var coveredUntil = 0L
            val windowLength = capture.end - capture.start
            val knownParts = frameCoverage.sortedBy { it.first }
            knownParts.forEach { (from, to) ->
                if (from > coveredUntil + 1_000_000) videoHoles += mapOf("from_window_ns" to coveredUntil.toString(), "to_window_ns" to from.toString())
                coveredUntil = maxOf(coveredUntil, to)
            }
            if (coveredUntil + 1_000_000 < windowLength) videoHoles += mapOf("from_window_ns" to coveredUntil.toString(), "to_window_ns" to windowLength.toString())
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
            val knownLoss = capture.gaps.any { (it.stream == "video" || it.stream == "clock") &&
                it.intersects(capture.start, capture.end, capture.endUncertainty) }
            val videoIncomplete = parts.isEmpty() || losses.isNotEmpty() || knownLoss || videoHoles.isNotEmpty()
            val manifest = mapOf("schema" to 2, "complete" to true, "save_id" to capture.id,
                "sequence_id" to capture.sequence, "generation" to capture.generation, "device" to capture.device,
                "window_clock_epoch" to capture.windowClockEpoch,
                "window_clock_known" to capture.windowKnown,
                "application_at_save" to capture.settings.application,
                "window_start_ns" to capture.start.toString(), "window_end_ns" to capture.end.toString(),
                "replay_seconds" to capture.seconds, "created_at" to Instant.now().toString(),
                "build" to CaptureResources.identity().filterKeys { it in setOf("source", "dirty", "version") },
                "plugin_id" to "io.github.shinma06.android-replay-buffer",
                "dependencies" to CaptureResources.identity(), "settings" to mapOf("replay_seconds" to capture.seconds,
                    "video_byte_limit" to ReplaySettings.VIDEO_BYTES, "log_byte_limit" to ReplaySettings.LOG_BYTES,
                    "min_free_bytes" to ReplaySettings.MIN_FREE_BYTES,
                    "max_video_packets" to ReplaySettings.MAX_VIDEO_PACKETS, "max_config_packet_bytes" to ReplaySettings.MAX_CONFIG_PACKET_BYTES,
                    "config_memory_bytes" to ReplaySettings.CONFIG_MEMORY_BYTES, "video" to "H.264 / 1920 / 30fps / 8Mbps / no B-frame"),
                "window_end_uncertainty_ns" to capture.endUncertainty.toString(),
                "clock_samples" to capture.clocks.map { clockJson(it) },
                "clock_epochs" to (capture.clocks.map { it.epoch } + capture.video.map { it.time.epoch } + capture.logs.map { it.time.epoch } +
                    capture.gaps.mapNotNull { it.clockEpoch }).distinct().map { id -> mapOf("id" to id,
                    "has_valid_sample" to capture.clocks.any { it.epoch == id && it.valid }) }, "application_history" to capture.apps.map { appJson(it) },
                "coverage" to capture.states.mapValues { (_, state) -> mapOf("state" to state.state,
                    "availableSeconds" to state.availableSeconds, "reason" to state.reason) },
                "gaps" to capture.gaps.map { gap -> mapOf("kind" to gap.stream,
                    "from_ns" to gap.fromNs?.toString(), "to_ns" to gap.toNs?.toString(), "reason" to gap.reason,
                    "time_axis" to "sequence_ns", "generation" to gap.generation, "clock_epoch" to gap.clockEpoch,
                    "boundary_uncertainty_ns" to gap.boundaryUncertaintyNs?.toString(),
                    "duration_uncertain" to (gap.fromNs == null || gap.toNs == null || gap.boundaryUncertaintyNs == null || gap.boundaryUncertaintyNs == Long.MAX_VALUE)) },
                "loss" to losses, "logcat_overflow_count" to null,
                "logcat_loss_note" to "reader/framing/byte上限の中断はgapsへ記録。Android buffer内の未観測overflow数は不明。tail以前をbackfillしたとは主張しない", "watermarks" to mapOf("video_pts_us" to capture.video.lastOrNull()?.pts?.toString(),
                    "device_log_record_id" to capture.logs.lastOrNull()?.id, "app_log_record_id" to capture.logs.lastOrNull { it.app == true }?.id),
                "video_file" to "video-001.mp4".takeIf { parts.isNotEmpty() },
                "video_reencoded" to (normalizer != null), "video_source_file" to rawPath?.fileName?.toString(),
                "video_canvas" to normalizer?.let { mapOf("width" to it.width, "height" to it.height, "colour_known" to it.colourKnown) },
                "video_movie_duration_us" to parts.lastOrNull()?.get("movie_end_us"),
                "parts" to parts, "video_run_boundaries" to runBoundaries, "video_missing_ranges" to videoHoles,
                "video_clock_gap_scopes" to capture.gaps.mapIndexedNotNull { i, gap ->
                    if (gap.stream != "clock") null else videoScopes[i].let { scope -> mapOf(
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
            Files.writeString(file("session.json"), gson.toJson(manifest), Charsets.UTF_8, CREATE_NEW, WRITE)
            own.forEach { checkActive(); FileChannel.open(it, WRITE).use { ch -> ch.force(true) }; checkActive() }
            checkSpace()
            checkActive()
            publish(partial, complete)
            return SaveOutput(complete, if (videoIncomplete) listOf("video") else emptyList())
        } catch (e: Exception) {
            // Delete exactly our files. An injected/unrelated entry prevents directory removal and is preserved.
            own.asReversed().forEach { runCatching { Files.deleteIfExists(it) } }
            partial?.let { runCatching { Files.delete(it) } }
            if (e is CancellationException) throw e
            throw SaveFailure(partial?.takeIf { Files.exists(it, NOFOLLOW_LINKS) },
                if (e is IllegalArgumentException || e is IllegalStateException) e.message ?: "保存できません" else "保存に失敗しました（保存先・空き容量・アクセス権を確認してください）", e)
        }
    }

    private fun bytesHash(bytes: ByteArray) = java.security.MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }

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
        "pids" to a.pids, "uid_exclusive" to a.uidExclusive, "from_elapsed_ns" to a.from?.toString(), "to_elapsed_ns" to a.to?.toString(), "clock_epoch" to a.epoch)
}

/** Playback proof is captured at acquisition; clock quality never authorizes source adjacency. */
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
