package io.github.shinma06.replaybuffer.core

import com.google.gson.GsonBuilder
import org.jcodec.common.Codec
import org.jcodec.common.VideoCodecMeta
import org.jcodec.common.model.ColorSpace
import org.jcodec.common.model.Packet
import org.jcodec.common.model.Size
import org.jcodec.containers.mp4.boxes.Edit
import org.jcodec.containers.mp4.muxer.CodecMP4MuxerTrack
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
internal class SaveWriter {
    private val gson = GsonBuilder().serializeNulls().disableHtmlEscaping().create()

    fun write(capture: FrozenCapture, folder: Path, cancelled: () -> Boolean,
              publish: (Path, Path) -> Unit): SaveOutput {
        var partial: Path? = null
        val own = mutableListOf<Path>()
        fun checkActive() { if (cancelled() || Thread.currentThread().isInterrupted) throw CancellationException() }
        try {
            checkActive()
            val parent = folder.toRealPath()
            require(Files.isDirectory(parent) && Files.isWritable(parent)) { "保存先に書き込めません" }
            val estimate = capture.video.sumOf { it.size.toLong() } + capture.logs.sumOf { it.source.raw.size.toLong() * 3 + 1024 }
            check(Files.getFileStore(parent).usableSpace >= estimate + ReplaySettings.MIN_FREE_BYTES) { "保存先の空き容量が不足しています" }
            val name = "replay-${Instant.now().toString().replace(':', '-')}-${UUID.randomUUID()}"
            partial = Files.createDirectory(parent.resolve(".$name.partial"))
            runCatching { Files.setPosixFilePermissions(partial, java.nio.file.attribute.PosixFilePermissions.fromString("rwx------")) }
            val complete = parent.resolve(name)
            fun file(name: String): Path = partial.resolve(name).also { own.add(it) }
            val parts = mutableListOf<Map<String, Any?>>()
            val losses = mutableListOf<String>()
            val tail = capture.videoTail()
            val videoProofs = mutableMapOf<CaptureGap, Pair<ClockSample, ClockSample>>()
            val videoScopes = capture.gaps.videoScopes(capture.clocks, capture.clockRecoveries, videoProofs)
            fun aligned(frame: VideoEntry): Boolean {
                val sequence = frame.time.sequence ?: return false
                return capture.windowKnown && frame.time.uncertainty != Long.MAX_VALUE && videoScopes.none { gap ->
                    gap.stream == "clock" && gap.intersects(sequence, sequence, frame.time.uncertainty)
                }
            }
            val framesFile = file("frames.jsonl")
            Files.newBufferedWriter(framesFile, Charsets.UTF_8, CREATE_NEW, WRITE).use { index ->
                val all = capture.video
                val sourceOnlyGops = capture.sourceOnlyGops()
                val groups = mutableListOf<MutableList<VideoEntry>>()
                all.forEach { frame ->
                    val previous = groups.lastOrNull()?.lastOrNull()
                    val sourceOnly = previous != null && previous.file == frame.file && frame.file in sourceOnlyGops &&
                        previous.sourceTimeUnknown() && frame.sourceTimeUnknown()
                    if (previous == null || !previous.muxContinuousTo(frame, videoScopes) && !sourceOnly) {
                        groups += mutableListOf(frame)
                    } else groups.last() += frame
                }
                groups.forEach { group ->
                    checkActive()
                    val visible = group.filterIndexed { i, frame ->
                        val time = frame.time.sequence
                        val next = group.getOrNull(i + 1)?.time?.sequence
                        !capture.windowKnown || time == null || time <= capture.end && (time >= capture.start || next != null && next > capture.start ||
                            tail?.displayHeld == true && frame === all.lastOrNull())
                    }
                    if (visible.isEmpty()) return@forEach
                    val first = visible.first()
                    val firstIndex = all.indexOf(first)
                    val keyIndex = all.decodeStart(first)
                    if (keyIndex == null) { losses += "IDRを失った動画区間を復号できません"; return@forEach }
                    val prefix = all.subList(keyIndex, firstIndex).filter { it.pts < first.pts }
                    val samples = prefix + group.filter { it.pts >= first.pts && (!capture.windowKnown || it.time.sequence == null || it.time.sequence <= capture.end) }
                    val origin = samples.first().pts
                    val alignmentKnown = visible.all(::aligned)

                    val last = samples.last()
                    val lastDuration = if (last === all.lastOrNull() && tail?.displayHeld == true)
                        ((tail.toNs!! - last.time.sequence!!) / 1000).coerceAtLeast(1) else 1L
                    var endUs = last.pts - origin + lastDuration
                    val projectedStart = first.time.sequence?.takeIf { capture.windowKnown }?.let { first.pts - origin + (capture.start - it) / 1000 } ?: (first.pts - origin)
                    // A part cannot present samples before its acquisition/clock/config boundary.
                    var startUs = maxOf(0L, group.first().pts - origin, projectedStart)
                    val visibleStart = first.time.sequence?.takeIf { capture.windowKnown }?.let { it - (first.pts - origin - startUs) * 1000 }
                    val sourceDurations = samples.mapIndexed { n, frame -> if (n < samples.lastIndex) samples[n + 1].pts - frame.pts else lastDuration }
                    val clipped = endUs > Int.MAX_VALUE || sourceDurations.any { it > Int.MAX_VALUE }
                    val durations = if (!clipped) sourceDurations else {
                        // A days-old static image may overlap a <=900s window. Compact only decoder-only time.
                        val originalStart = startUs
                        val originalEnd = endUs
                        val values = samples.mapIndexed { n, frame ->
                            (minOf(frame.pts - origin + sourceDurations[n], originalEnd) - maxOf(frame.pts - origin, originalStart)).coerceAtLeast(1)
                        }
                        startUs = samples.indices.takeWhile { samples[it].pts - origin + sourceDurations[it] <= originalStart }.sumOf { values[it] }
                        endUs = startUs + originalEnd - originalStart
                        values
                    }
                    var media = 0L
                    val videoName = "video-${(parts.size + 1).toString().padStart(3, '0')}.mp4"
                    val target = file(videoName)
                    // CREATE_NEW reserves the output; the JCodec channel writes only this request's new file.
                    org.jcodec.common.io.FileChannelWrapper(FileChannel.open(target, CREATE_NEW, READ, WRITE, NOFOLLOW_LINKS)).use { output ->
                        val mux = MP4Muxer.createMP4MuxerToChannel(output)
                        val track = mux.addVideoTrack(Codec.H264, VideoCodecMeta.createSimpleVideoCodecMeta(
                            Size(first.width, first.height), ColorSpace.YUV420J)) as CodecMP4MuxerTrack
                        samples.forEachIndexed { n, frame ->
                            checkActive()
                            val bytes = readFrame(frame)
                            val data = if (n == 0) ByteBuffer.wrap(frame.config + bytes) else ByteBuffer.wrap(bytes)
                            val duration = durations[n]
                            require(duration in 1..Int.MAX_VALUE.toLong()) { "動画durationが不正です" }
                            track.addFrame(Packet.createPacket(data, media, 1_000_000, duration, n.toLong(),
                                if (frame.key) Packet.FrameType.KEY else Packet.FrameType.INTER, null))
                            val sequence = frame.time.sequence
                            val frameAlignmentKnown = alignmentKnown && aligned(frame)
                            val presented = media < endUs && media + duration > startUs
                            index.write(gson.toJson(mapOf("part" to videoName, "sample_index" to n,
                                "source_pts_us" to frame.pts.toString(), "source_duration_us" to sourceDurations[n].takeIf { n < samples.lastIndex }?.toString(),
                                "display_duration_us" to duration.toString(), "media_pts_us" to media.toString(),
                                "elapsed_ns" to frame.time.elapsed?.toString(), "clock_epoch" to frame.time.epoch,
                                "window_ns" to sequence?.takeIf { frameAlignmentKnown }?.let { (it - capture.start).toString() },
                                "mapped_window_ns" to sequence?.takeIf { capture.windowKnown }?.let { (it - capture.start).toString() },
                                "clock_alignment_known" to frameAlignmentKnown,
                                "presentation_pts_us" to (media - startUs).coerceAtLeast(0).toString(),
                                "presented" to presented, "preroll" to (!presented && media < startUs),
                                "uncertainty_ns" to frame.time.uncertainty.toString())))
                            index.newLine()
                            media += duration
                        }
                        require(startUs >= 0 && endUs > startUs)
                        track.setEdits(listOf(Edit(endUs - startUs, startUs, 1f)))
                        mux.finish()
                    }
                    parts += mapOf("file" to videoName, "clock_epoch" to first.time.epoch,
                        "generation" to first.generation, "source_pts_origin_us" to origin.toString(),
                        "edit_start_us" to startUs.toString(), "duration_us" to (endUs - startUs).toString(),
                        "clock_alignment_known" to alignmentKnown,
                        "window_start_ns" to visibleStart?.takeIf { alignmentKnown }?.let { (it - capture.start).toString() },
                        "window_end_ns" to last.time.sequence?.takeIf { alignmentKnown }?.let { (it - capture.start + lastDuration * 1000).toString() },
                        "confirmed_window_end_ns" to last.time.sequence?.takeIf { alignmentKnown && visibleStart != null && it > visibleStart }?.let { (it - capture.start).toString() },
                        "media_timeline_clipped" to clipped,
                        "preroll_samples" to prefix.size)
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
                appendLine("動画${parts.size}part / 全体ログ${capture.logs.size}行 / アプリログ${capture.logs.count { it.app == true }}行")
                appendLine("動画は各partで0秒から始まります。clock_alignment_known=trueのpartだけ、session.jsonのwindow_start_ns + 表示秒×1,000,000,000を共通窓の時刻として両ログと照合してください。")
                appendLine("clock_alignment_known=falseのpartも元PTSで再生できますが、窓への時計対応は不明です。window_ns=null、mapped_window_nsは不確実性を含む変換値で、同期確定やcoverageには使えません。")
                appendLine("video_missing_rangesは映像欠落又は時計対応未確認の範囲です。動画を連結して詰めた時間として扱わないでください。")
                appendLine("次sampleのない末尾はsource_duration_us=nullです。MP4の最小1tickは表示のための値で、観測した画像の継続時間や取得coverageではありません。")
                appendLine("MP4内部には論理窓の前のdecoder preroll画像を含むことがあります。edit listで表示範囲を指定しています。標準playerの互換性は製品QAで別途確認します。")
                if (tail != null) appendLine("動画末尾: ${tail.fromNs}〜${tail.toNs}nsは新frame未確認。前の画像の表示保持=${tail.displayHeld}。再生時間は確認済み取得時間とは異なります。")
                appendLine("全体ログはshell権限で読めるlogcat bufferです。security等の全端末ログ取得を保証しません。app_membership=nullは対象未確定です。")
                appendLine("0行は正常な無出力の場合もあります。取得状態・時計・アプリ履歴・lossをsession.jsonで確認してください。")
                parts.forEach { appendLine("${it["file"]}: 時計対応確認=${it["clock_alignment_known"]} / 窓開始ns=${it["window_start_ns"]} / 窓終了ns=${it["window_end_ns"]} / preroll=${it["preroll_samples"]}") }
                capture.gaps.forEach { appendLine("${it.stream}: ${it.fromNs}〜${it.toNs}ns / ${it.reason}") }
                losses.forEach { appendLine("loss: $it") }
            }, Charsets.UTF_8, CREATE_NEW, WRITE)
            checkActive()
            val hashes = own.associate { it.fileName.toString() to sha256(it) }
            val videoHoles = mutableListOf<Map<String, String>>()
            var coveredUntil = 0L
            val windowLength = capture.end - capture.start
            val knownParts = parts.mapNotNull { part ->
                val from = (part["window_start_ns"] as? String)?.toLongOrNull()
                val to = (part["window_end_ns"] as? String)?.toLongOrNull()
                if (from != null && to != null) from to to else null
            }.sortedBy { it.first }
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
            val videoIncomplete = parts.isEmpty() || losses.isNotEmpty() || knownLoss || videoHoles.isNotEmpty() || knownParts.size != parts.size
            val manifest = mapOf("schema" to 1, "complete" to true, "save_id" to capture.id,
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
                "parts" to parts, "video_missing_ranges" to videoHoles,
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
            own.forEach { FileChannel.open(it, WRITE).use { ch -> ch.force(true) } }
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

    private fun readFrame(frame: VideoEntry): ByteArray {
        require(Files.isRegularFile(frame.file, NOFOLLOW_LINKS))
        val result = ByteBuffer.allocate(frame.size)
        FileChannel.open(frame.file, READ, NOFOLLOW_LINKS).use { input ->
            require(input.size() >= frame.offset + frame.size)
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

/** Mux only: preserve source playback without assigning an old stream's loss to a newer stream. */
internal fun VideoEntry.muxContinuousTo(next: VideoEntry, gaps: List<CaptureGap>): Boolean = continuousTo(next,
    gaps.filterNot { gap ->
        (gap.stream == "video" && gap.toNs != null && gap.generation > 0 && gap.generation < generation) ||
        (gap.stream == "clock" && gap.fromNs != null && gap.toNs != null && gap.fromNs <= gap.toNs &&
            gap.boundaryUncertaintyNs != null && gap.boundaryUncertaintyNs in 0 until Long.MAX_VALUE &&
            time.sequence != null && next.time.sequence != null &&
            gap.intersects(time.sequence, time.sequence, time.uncertainty) &&
            gap.intersects(next.time.sequence, next.time.sequence, next.time.uncertainty))
    })

internal fun VideoEntry.sourceTimeUnknown(): Boolean =
    time.elapsed == null && time.sequence == null && time.uncertainty == Long.MAX_VALUE

/** Separate presentation proof: the entire retained GOP, including decoder preroll, lies outside every actual cut. */
internal fun FrozenCapture.sourceOnlyGops(): Set<Path> {
    val losses = gaps.filter { it.stream == "video" }
    if (losses.isEmpty() || losses.any { it !in videoCuts }) return emptySet()
    return video.groupBy { it.file }.filterValues { frames ->
        if (frames.count { it.sourceTimeUnknown() } < 2) return@filterValues false
        val first = frames.first()
        val last = frames.last()
        if (!first.key || first.offset != 0L || first.generation <= 0 || first.session <= 0 ||
            first.width <= 0 || first.height <= 0 || first.config.isEmpty() ||
            video.indexOf(last) - video.indexOf(first) + 1 != frames.size) return@filterValues false
        try {
            val end = Math.addExact(last.offset, last.size.toLong())
            val bytes = Files.size(first.file)
            if (Math.subtractExact(last.pts, first.pts) !in 1 until Int.MAX_VALUE.toLong() ||
                !Files.isRegularFile(first.file, NOFOLLOW_LINKS) || bytes < end) return@filterValues false
            if (frames.any { it.size <= 0 || it.offset < 0 || it.pts < 0 || it.generation != first.generation ||
                    it.session != first.session || it.time.epoch != first.time.epoch || it.width != first.width ||
                    it.height != first.height || !it.config.contentEquals(first.config) }) return@filterValues false
            if (frames.zipWithNext().any { (a, b) ->
                    Math.addExact(a.offset, a.size.toLong()) != b.offset ||
                        Math.subtractExact(b.pts, a.pts) !in 1..Int.MAX_VALUE.toLong() ||
                        Math.subtractExact(b.host, a.host) < 0
                }) return@filterValues false
            losses.all { gap ->
                val cut = videoCuts.getValue(gap)
                val before = cut.before
                val after = cut.after
                // Finite VIDEO loss is an independent veto; this route only disambiguates unknown clock envelopes.
                gap.generation > 0 && gap.fromNs != null && gap.toNs != null && gap.fromNs <= gap.toNs &&
                    gap.boundaryUncertaintyNs == Long.MAX_VALUE && before != null && after != null &&
                    before.generation == gap.generation && before.session > 0 && before.offset >= 0 &&
                    after.generation >= before.generation && after.session >= before.session && after.offset >= 0 &&
                    Math.subtractExact(after.host, before.host) >= 0 &&
                    (before.file != after.file || before.generation == after.generation &&
                        before.session == after.session && before.offset <= after.offset) &&
                    (first.file == before.file && first.generation == before.generation && first.session == before.session &&
                        end <= before.offset && before.offset <= bytes && Math.subtractExact(before.host, last.host) >= 0 ||
                        first.file == after.file && first.generation == after.generation && first.session == after.session &&
                        first.offset >= after.offset && Math.subtractExact(first.host, after.host) >= 0)
            }
        } catch (_: Exception) { false } // Missing bytes, overflow or incomplete proof cannot authorize grouping.
    }.keys
}
