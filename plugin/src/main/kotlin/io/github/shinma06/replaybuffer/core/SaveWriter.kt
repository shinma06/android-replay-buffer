package io.github.shinma06.replaybuffer.core

import com.google.gson.GsonBuilder
import org.jcodec.common.Codec
import org.jcodec.common.VideoCodecMeta
import org.jcodec.common.io.NIOUtils
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

internal class SaveFailure(val partial: Path?, message: String, cause: Throwable? = null) : Exception(message, cause)

/** No external encoder/process: immutable packet indices -> AVC MP4 and matching JSONL/manifest. */
internal class SaveWriter {
    private val gson = GsonBuilder().serializeNulls().disableHtmlEscaping().create()

    fun write(capture: FrozenCapture, folder: Path, cancelled: () -> Boolean,
              publish: (Path, Path) -> Unit): Path {
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
            val framesFile = file("frames.jsonl")
            Files.newBufferedWriter(framesFile, Charsets.UTF_8, CREATE_NEW, WRITE).use { index ->
                val all = capture.video
                val groups = mutableListOf<MutableList<VideoEntry>>()
                all.forEach { frame ->
                    val previous = groups.lastOrNull()?.lastOrNull()
                    if (previous == null || previous.generation != frame.generation || previous.session != frame.session || previous.width != frame.width ||
                        previous.height != frame.height || previous.time.epoch != frame.time.epoch ||
                        !previous.config.contentEquals(frame.config) || frame.pts <= previous.pts || frame.pts - previous.pts > 3_000_000) {
                        groups += mutableListOf(frame)
                    } else groups.last() += frame
                }
                groups.forEach { group ->
                    checkActive()
                    val visible = group.filter { it.time.sequence == null || it.time.sequence in capture.start..capture.end }
                    if (visible.isEmpty()) return@forEach
                    val first = visible.first()
                    val firstIndex = all.indexOf(first)
                    val keyIndex = (firstIndex downTo 0).firstOrNull { i ->
                        all[i].key && all[i].generation == first.generation && all[i].session == first.session && all[i].width == first.width &&
                            all[i].height == first.height && all[i].config.contentEquals(first.config) && all[i].pts <= first.pts
                    }
                    if (keyIndex == null) { losses += "IDRを失った動画区間を復号できません"; return@forEach }
                    val prefix = all.subList(keyIndex, firstIndex).filter { it.pts < first.pts }
                    val samples = prefix + group.filter { it.pts >= first.pts }
                    val origin = samples.first().pts
                    val visibleStart = first.time.sequence?.let { maxOf(capture.start, it) }
                    val last = samples.last()
                    val typical = if (samples.size > 1) (last.pts - samples[samples.lastIndex - 1].pts).coerceIn(1, 100_000) else 33_333L
                    val remaining = last.time.sequence?.let { ((capture.end - it).coerceAtLeast(0) / 1000).coerceAtMost(typical) } ?: typical
                    val lastDuration = remaining.coerceAtLeast(1)
                    val endUs = last.pts - origin + lastDuration
                    val startUs = if (first.time.sequence != null) first.pts - origin +
                        ((capture.start - first.time.sequence) / 1000) else first.pts - origin
                    val videoName = "video-${(parts.size + 1).toString().padStart(3, '0')}.mp4"
                    val target = file(videoName)
                    // CREATE_NEW reserves the output; the JCodec channel writes only this request's new file.
                    Files.newByteChannel(target, CREATE_NEW, WRITE).close()
                    NIOUtils.writableChannel(target.toFile()).use { output ->
                        val mux = MP4Muxer.createMP4MuxerToChannel(output)
                        val track = mux.addVideoTrack(Codec.H264, VideoCodecMeta.createSimpleVideoCodecMeta(
                            Size(first.width, first.height), ColorSpace.YUV420J)) as CodecMP4MuxerTrack
                        samples.forEachIndexed { n, frame ->
                            checkActive()
                            val bytes = readFrame(frame)
                            val data = if (n == 0) ByteBuffer.wrap(frame.config + bytes) else ByteBuffer.wrap(bytes)
                            val duration = if (n < samples.lastIndex) samples[n + 1].pts - frame.pts else lastDuration
                            require(duration in 1..Int.MAX_VALUE.toLong()) { "動画durationが不正です" }
                            track.addFrame(Packet.createPacket(data, frame.pts - origin, 1_000_000, duration, n.toLong(),
                                if (frame.key) Packet.FrameType.KEY else Packet.FrameType.INTER, null))
                            val sequence = frame.time.sequence
                            val presented = sequence == null && n >= prefix.size || sequence != null &&
                                sequence <= capture.end && sequence + duration * 1000 > capture.start
                            index.write(gson.toJson(mapOf("part" to videoName, "sample_index" to n,
                                "source_pts_us" to frame.pts.toString(), "media_pts_us" to (frame.pts - origin).toString(),
                                "elapsed_ns" to frame.time.elapsed?.toString(), "clock_epoch" to frame.time.epoch,
                                "window_ns" to sequence?.let { (it - capture.start).toString() },
                                "presented" to presented, "preroll" to !presented,
                                "uncertainty_ns" to frame.time.uncertainty.toString())))
                            index.newLine()
                        }
                        require(startUs >= 0 && endUs > startUs)
                        track.setEdits(listOf(Edit(endUs - startUs, startUs, 1f)))
                        mux.finish()
                    }
                    parts += mapOf("file" to videoName, "clock_epoch" to first.time.epoch,
                        "generation" to first.generation, "source_pts_origin_us" to origin.toString(),
                        "edit_start_us" to startUs.toString(), "duration_us" to (endUs - startUs).toString(),
                        "window_start_ns" to visibleStart?.let { (it - capture.start).toString() },
                        "window_end_ns" to last.time.sequence?.let { (it - capture.start + lastDuration * 1000).toString() },
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
                            "elapsed_ns" to row.time.elapsed?.toString(), "window_ns" to row.time.sequence?.let { (it - capture.start).toString() },
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
                appendLine("動画は各partで0秒から始まります。session.jsonのwindow_start_ns + 表示秒×1,000,000,000を共通窓の時刻として両ログと照合してください。")
                appendLine("part間の空白は欠落です。動画を連結して詰めた時間として扱わないでください。window_ns=nullは時計対応不明です。")
                appendLine("MP4内部には論理窓の前のdecoder preroll画像を含むことがあります。edit listで表示範囲を指定しています。標準playerの互換性は製品QAで別途確認します。")
                appendLine("全体ログはshell権限で読めるlogcat bufferです。security等の全端末ログ取得を保証しません。app_membership=nullは対象未確定です。")
                appendLine("0行は正常な無出力の場合もあります。取得状態・時計・アプリ履歴・lossをsession.jsonで確認してください。")
                parts.forEach { appendLine("${it["file"]}: 窓開始ns=${it["window_start_ns"]} / 窓終了ns=${it["window_end_ns"]} / preroll=${it["preroll_samples"]}") }
                capture.gaps.forEach { appendLine("${it.stream}: ${it.fromNs}〜${it.toNs}ns / ${it.reason}") }
                losses.forEach { appendLine("loss: $it") }
            }, Charsets.UTF_8, CREATE_NEW, WRITE)
            checkActive()
            val hashes = own.associate { it.fileName.toString() to sha256(it) }
            val manifest = mapOf("schema" to 1, "complete" to true, "save_id" to capture.id,
                "sequence_id" to capture.sequence, "generation" to capture.generation, "device" to capture.device,
                "window_clock_epoch" to capture.clocks.lastOrNull { it.valid }?.epoch,
                "application_at_save" to capture.settings.application,
                "window_start_ns" to capture.start.toString(), "window_end_ns" to capture.end.toString(),
                "replay_seconds" to capture.seconds, "created_at" to Instant.now().toString(),
                "build" to javaClass.`package`.implementationVersion,
                "dependencies" to CaptureResources.identity(), "settings" to mapOf("replay_seconds" to capture.seconds,
                    "video_byte_limit" to ReplaySettings.VIDEO_BYTES, "log_byte_limit" to ReplaySettings.LOG_BYTES,
                    "min_free_bytes" to ReplaySettings.MIN_FREE_BYTES, "video" to "H.264 / 1920 / 30fps / 8Mbps / no B-frame"),
                "clock_samples" to capture.clocks.map { clockJson(it) }, "application_history" to capture.apps.map { appJson(it) },
                "coverage" to capture.states, "gaps" to capture.gaps.map { gap -> mapOf("kind" to gap.stream,
                    "from_ns" to gap.fromNs?.toString(), "to_ns" to gap.toNs?.toString(), "reason" to gap.reason,
                    "duration_uncertain" to (gap.fromNs == null || gap.toNs == null)) },
                "loss" to losses, "logcat_overflow_count" to null,
                "logcat_loss_note" to "reader/framing/byte上限の中断はgapsへ記録。Android buffer内の未観測overflow数は不明。tail以前をbackfillしたとは主張しない", "watermarks" to mapOf("video_pts_us" to capture.video.lastOrNull()?.pts?.toString(),
                    "device_log_record_id" to capture.logs.lastOrNull()?.id, "app_log_record_id" to capture.logs.lastOrNull { it.app == true }?.id),
                "parts" to parts, "files_sha256" to hashes)
            Files.writeString(file("session.json"), gson.toJson(manifest), Charsets.UTF_8, CREATE_NEW, WRITE)
            own.forEach { FileChannel.open(it, WRITE).use { ch -> ch.force(true) } }
            checkActive()
            publish(partial, complete)
            return complete
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
        "pids" to a.pids, "from_elapsed_ns" to a.from?.toString(), "to_elapsed_ns" to a.to?.toString(), "clock_epoch" to a.epoch)
}
