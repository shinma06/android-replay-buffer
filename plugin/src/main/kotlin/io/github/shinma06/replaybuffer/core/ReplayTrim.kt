package io.github.shinma06.replaybuffer.core

import com.google.gson.GsonBuilder
import com.google.gson.JsonObject
import org.jcodec.codecs.h264.H264Utils
import org.jcodec.common.io.NIOUtils
import org.jcodec.common.model.Packet
import org.jcodec.containers.mp4.MP4TrackType
import org.jcodec.containers.mp4.boxes.AudioSampleEntry
import org.jcodec.containers.mp4.boxes.Edit
import org.jcodec.containers.mp4.boxes.NodeBox
import org.jcodec.containers.mp4.boxes.SampleEntry
import org.jcodec.containers.mp4.boxes.SampleSizesBox
import org.jcodec.containers.mp4.boxes.VideoSampleEntry
import org.jcodec.containers.mp4.demuxer.AbstractMP4DemuxerTrack
import org.jcodec.containers.mp4.demuxer.MP4Demuxer
import org.jcodec.containers.mp4.muxer.MP4Muxer
import org.jcodec.containers.mp4.muxer.MP4MuxerTrack
import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import java.nio.channels.FileChannel
import java.nio.file.Files
import java.nio.file.Path
import java.time.ZonedDateTime

/** One stored sample converted to annex-B for the decoder; avcC supplies the NAL length size. */
internal fun movSampleToAnnexB(data: ByteBuffer, avcc: org.jcodec.codecs.h264.mp4.AvcCBox): ByteArray {
    val out = ByteArrayOutputStream()
    H264Utils.splitMOVPacket(data.duplicate(), avcc).forEach { nal ->
        out.write(byteArrayOf(0, 0, 0, 1))
        out.write(ByteArray(nal.remaining()).also { nal.get(it) })
    }
    return out.toByteArray()
}

/** Annex-B config bytes (SPS then PPS, each start-code prefixed) from one stored sample entry. */
internal fun annexBConfig(avcc: org.jcodec.codecs.h264.mp4.AvcCBox): ByteArray {
    val data = H264Utils.avcCToAnnexB(avcc)
    return ByteArray(data.remaining()).also { data.get(it) }
}

private fun JsonObject.text(name: String): String? =
    get(name)?.takeIf { it.isJsonPrimitive && (it.asJsonPrimitive.isString || it.asJsonPrimitive.isNumber) }?.asString

private fun JsonObject.longText(name: String): Long? = text(name)?.toLong()
private fun JsonObject.requiredLongText(name: String): Long =
    longText(name) ?: throw IllegalArgumentException("frame記録の${name}がありません")

internal fun readTrimSource(channel: FileChannel, offset: Long, size: Long, checkActive: () -> Unit): ByteBuffer {
    require(offset >= 0 && size in 0..Int.MAX_VALUE.toLong()) { "元映像packetの位置・サイズが不正です" }
    val data = ByteBuffer.allocate(size.toInt())
    var position = offset
    while (data.hasRemaining()) {
        checkActive()
        val count = channel.read(data, position)
        if (count <= 0) throw java.io.IOException("元映像packetを最後まで読み込めません。保存元のサイズ・状態を確認してください。")
        position = Math.addExact(position, count.toLong())
    }
    checkActive()
    return data.flip()
}

internal fun trimSourceRectangle(row: JsonObject, placement: Map<String, Int>, width: Int, height: Int): JsonObject {
    val prior = row["source_rect"]?.takeUnless { it.isJsonNull }?.asJsonObject
    if (prior == null) {
        require(row["reencoded"]?.asBoolean != true) { "正規化済みframeの元画像の配置情報がありません" }
        return JsonObject().apply { placement.forEach { (key, value) -> addProperty(key, value) } }
    }
    require(row["output_width"]?.asInt == placement.getValue("width") &&
        row["output_height"]?.asInt == placement.getValue("height")) { "元画像のcanvasと動画の寸法が一致しません" }
    val x = prior["x"].asInt
    val y = prior["y"].asInt
    val w = prior["width"].asInt
    val h = prior["height"].asInt
    require(x >= 0 && y >= 0 && w > 0 && h > 0 &&
        x.toLong() + w <= placement.getValue("width") && y.toLong() + h <= placement.getValue("height")) {
        "元画像の配置がcanvasの範囲外です"
    }
    val left = Math.addExact(x, placement.getValue("x"))
    val top = Math.addExact(y, placement.getValue("y"))
    return prior.deepCopy().apply {
        addProperty("x", left)
        addProperty("y", top)
        addProperty("padding_left", left)
        addProperty("padding_top", top)
        addProperty("padding_right", width - left - w)
        addProperty("padding_bottom", height - top - h)
    }
}

/** One stored video-frames.jsonl row plus its file-order index. */
private class StoredFrame(val row: JsonObject, val index: Int) {
    val part get() = row["segment_index"].asInt
    val outputKey get() = row["output_key"].asBoolean
    val presented get() = row["presented"].asBoolean
    val sampleEntry get() = row["sample_entry"].asInt
    val sourcePts get() = row.requiredLongText("source_pts_us")
    val sourceDuration get() = row.longText("source_duration_us")
    val sourceDisplayStart get() = row.longText("source_display_start_us")
    val displayDuration get() = row.requiredLongText("display_duration_us")
    val moviePts get() = row.longText("movie_pts_us")
    val movieEnd get() = row.longText("movie_end_us")
    val displayTns get() = row.longText("display_t_ns")
    val tNs get() = row.longText("t_ns")
    val confirmedEndTns get() = row.longText("confirmed_t_end_ns")
    val sourceOffset get() = row.longText("source_offset")
    val sourceSize get() = row.longText("source_size")
    val configOffset get() = row.longText("source_config_offset")
    val configSize get() = row.longText("source_config_size")
    val hasSource get() = row.text("source_file") == ReplayFiles.VIDEO_SOURCE
}

/**
 * Trim of a completed schema-4 save: re-encode the leading GOP, copy later packets, rebuild the
 * manifest/JSONL/log/touch side data on the rebased movie and t axes. The source folder is read-only.
 * Runs outside EDT; mirrors SaveWriter's staging, ownership, space and cancellation discipline.
 */
internal class ReplayTrim(
    private val availableSpace: (Path) -> Long = { Files.getFileStore(it).usableSpace },
    private val mp4Input: (FileChannel) -> org.jcodec.common.io.SeekableByteChannel = { org.jcodec.common.io.FileChannelWrapper(it) },
) {
    private val gson = GsonBuilder().serializeNulls().disableHtmlEscaping().create()

    data class Preflight(val fingerprint: String, val startUs: Long, val endUs: Long, val clockUnknown: Boolean) {
        val warning: String? get() = if (clockUnknown) "選択範囲に時計対応が不明なframeがあります。ログは除外を断定できない行を残します。" else null
    }

    private data class Selection(val frames: List<StoredFrame>, val parts: List<JsonObject>,
                                 val start: StoredFrame, val end: StoredFrame) {
        val startUs get() = start.moviePts!!
        val endUs get() = end.movieEnd!!
    }

    private class VideoRun(val rows: List<StoredFrame>, val first: Int, val last: Int, val fullStart: Boolean) {
        val gopStart = if (fullStart) 0 else (first downTo 0).firstOrNull { rows[it].outputKey }
            ?: throw IllegalArgumentException("開始frameの参照IDRがありません")
        val reencodeEnd = if (fullStart) first else
            ((first + 1)..last).firstOrNull { rows[it].outputKey } ?: (last + 1)
    }

    private class PacketSizes(track: AbstractMP4DemuxerTrack) {
        private val box = NodeBox.findFirstPath(track.box, SampleSizesBox::class.java, arrayOf("mdia", "minf", "stbl", "stsz"))
            ?: throw IllegalArgumentException("sampleのサイズ表がありません")
        val count get() = box.count
        operator fun get(index: Int): Long {
            require(index in 0 until count) { "sampleのサイズ範囲が不正です" }
            return (if (box.defaultSize == 0) box.sizes[index] else box.defaultSize).toLong().also {
                require(it >= 0) { "sampleのサイズが不正です" }
            }
        }
    }

    private data class AudioRun(val movieStart: Long, val duration: Long, val first: Int, val end: Int, val skip: Long, val preroll: Int)
    private data class AudioPlan(val track: AbstractMP4DemuxerTrack, val entry: AudioSampleEntry,
                                 val sampleRate: Int, val runs: List<AudioRun>, val sizes: PacketSizes)

    private fun select(saved: SavedReplay, fromUs: Long, toUs: Long): Selection {
        val frames = saved.frames.mapIndexed { index, row -> StoredFrame(row, index).also { frame ->
            require(row["sample_index"]?.asLong == index.toLong() && (frame.moviePts != null) == (frame.movieEnd != null) &&
                (frame.moviePts == null || frame.movieEnd!! > frame.moviePts!!) &&
                row.has("output_key") && row.has("presented") && row.has("sample_entry") &&
                row.has("media_pts_us") && row.has("display_duration_us") && row.has("source_pts_us") &&
                row.has("presentation_pts_us") && row.has("t_ns") && row.has("reencoded")) { "動画のframe記録が読めません" }
        } }
        val parts = (saved.manifest["parts"]?.takeIf { it.isJsonArray }?.asJsonArray
            ?: throw IllegalArgumentException("replay.jsonのpartsを読めません")).mapIndexed { index, element ->
            element.asJsonObject.also { part ->
                require(part["file"]?.asString == ReplayFiles.VIDEO && part["segment_index"]?.asInt == index) { "動画partの構成が読めません" }
            }
        }
        require(parts.isNotEmpty()) { "動画partがありません" }
        val duration = parts.last().requiredLongText("movie_end_us")
        require(fromUs in 0 until duration && toUs in 0..duration && fromUs < toUs) {
            "切り取り範囲が動画の長さ（${"%.3f".format(duration / 1e6)}秒）の外か空です"
        }
        val presented = frames.filter { it.presented && it.moviePts != null }
        val start = presented.firstOrNull { it.movieEnd!! > fromUs }
            ?: throw IllegalArgumentException("開始位置に表示frameがありません")
        val end = presented.lastOrNull { it.moviePts!! < toUs }
            ?: throw IllegalArgumentException("終了位置に表示frameがありません")
        require(start.index <= end.index && start.moviePts!! < end.movieEnd!!) { "切り取り範囲に表示frameがありません" }
        return Selection(frames, parts, start, end)
    }

    private fun fingerprint(saved: SavedReplay, fromUs: Long, toUs: Long): String =
        java.security.MessageDigest.getInstance("SHA-256").digest("${saved.recordSha256}:$fromUs:$toUs".toByteArray(Charsets.UTF_8))
            .joinToString("") { "%02x".format(it) }

    fun preflight(source: Path, fromUs: Long, toUs: Long): Preflight = try { SavedReplay.open(source).use { saved ->
        val selection = select(saved, fromUs, toUs)
        val unknown = selection.frames.any { frame ->
            frame.presented && frame.moviePts != null && frame.moviePts!! < selection.endUs && frame.movieEnd!! > selection.startUs &&
                frame.row["clock_alignment_known"]?.asBoolean != true
        }
        Preflight(fingerprint(saved, fromUs, toUs), selection.startUs, selection.endUs, unknown)
    } } catch (failure: Exception) {
        throw SaveFailure(null, failure.message ?: "切り取り範囲を確認できません", failure)
    }

    fun trim(source: Path, fromUs: Long, toUs: Long, cancelled: () -> Boolean,
             publish: (SavePublication) -> Unit,
             createdAt: ZonedDateTime = ZonedDateTime.now(), id: String = nameId(), expectedFingerprint: String? = null): Path {
        var staging: SaveStaging? = null
        var manifestWritten = false
        fun checkActive() {
            if (cancelled() || Thread.currentThread().isInterrupted) throw SaveCancelled(null)
        }
        try {
            checkActive()
            SavedReplay.open(source).use { saved ->
                require(expectedFingerprint == null || expectedFingerprint == fingerprint(saved, fromUs, toUs)) {
                    "事前確認後に保存フォルダまたは切り取り範囲が変更されています。もう一度範囲を確認してください。"
                }
                val (frames, parts, startFrame, endFrame) = select(saved, fromUs, toUs)
                val startUs = startFrame.moviePts!!
                val endUs = endFrame.movieEnd!!
                val originTns = startFrame.displayTns ?: startFrame.tNs
                val lastDisplayTns = endFrame.displayTns ?: endFrame.tNs
                val endTns = lastDisplayTns?.let {
                    Math.addExact(it, Math.multiplyExact(endFrame.movieEnd!! - endFrame.moviePts!!, 1000))
                } ?: endFrame.confirmedEndTns
                data class KeptPart(val part: JsonObject, val index: Int, val a: Long, val b: Long)
                val kept = parts.mapIndexedNotNull { index, part ->
                    val a = maxOf(startUs, part.requiredLongText("movie_start_us"))
                    val b = minOf(endUs, part.requiredLongText("movie_end_us"))
                    if (a < b) KeptPart(part, index, a, b) else null
                }
                require(kept.isNotEmpty()) { "切り取り範囲に表示frameがありません" }
                val byPart = frames.groupBy { it.part }
                val keptParts = kept.mapIndexed { keptIndex, keptPart ->
                    val partRows = byPart[keptPart.index] ?: throw IllegalArgumentException("動画partにframe記録がありません")
                    val fullStart = keptPart.a == keptPart.part.requiredLongText("movie_start_us")
                    val fullEnd = keptPart.b == keptPart.part.requiredLongText("movie_end_us")
                    val firstOut = if (fullStart && keptIndex != 0) 0 else partRows.indexOfFirst { it.index == startFrame.index }
                    val lastOut = if (fullEnd) partRows.lastIndex else partRows.indexOfFirst { it.index == endFrame.index }
                    require(firstOut >= 0 && lastOut >= firstOut) { "切り取り位置のframeを特定できません" }
                    VideoRun(partRows, firstOut, lastOut,
                        firstOut == 0 && keptPart.a == keptPart.part.requiredLongText("movie_start_us"))
                }
                checkActive()
                val parent = saved.directory.parent
                fun checkSpace(bytes: Long = 0) {
                    check(availableSpace(parent) >= Math.addExact(bytes, ReplaySettings.MIN_FREE_BYTES)) { "保存先の空き容量が不足しています" }
                }
                val name = trimFolderName(createdAt, id)
                fun file(fileName: String): Path = staging!!.file(fileName)
                val outputRows = mutableListOf<JsonObject>()
                var didReencode = false
                val newParts = mutableListOf<Map<String, Any?>>()
                val newBoundaries = mutableListOf<Map<String, Any?>>()
                var sampleIndex = 0L
                saved.channel(ReplayFiles.VIDEO).use { videoChannel ->
                    val demuxer = MP4Demuxer.createRawMP4Demuxer(mp4Input(videoChannel))
                    val videoTrack = demuxer.videoTrack
                    val sampleEntries = (videoTrack as org.jcodec.containers.mp4.demuxer.AbstractMP4DemuxerTrack).sampleEntries.toList()
                    val avcs = sampleEntries.map { entry ->
                        H264Utils.parseAVCC(entry as VideoSampleEntry).also {
                            require(it.spsList.isNotEmpty() && it.ppsList.isNotEmpty()) { "動画のavcCが読めません" }
                        }
                    }
                    val annexBConfigs = avcs.map(::annexBConfig)
                    val audioPlan = planAudio(saved, demuxer, startUs, endUs, ::checkActive)
                    val estimate = estimateOutput(saved, videoTrack, keptParts, annexBConfigs, audioPlan,
                        startUs, endUs, originTns, endTns, ::checkActive)
                    checkActive()
                    checkSpace(estimate)
                    checkActive()
                    staging = SaveStaging.create(parent, name)
                    fun annexB(data: ByteBuffer, entry: Int): ByteArray = movSampleToAnnexB(data, avcs[entry - 1])
                    val sourceChannel = if (saved.exists(ReplayFiles.VIDEO_SOURCE)) saved.channel(ReplayFiles.VIDEO_SOURCE) else null
                    try {
                        staging!!.createFile(file(ReplayFiles.VIDEO_FRAMES)).use { indexChannel ->
                            val index = java.nio.channels.Channels.newWriter(indexChannel, Charsets.UTF_8).buffered()
                            (sourceChannel?.let { staging!!.createFile(file(ReplayFiles.VIDEO_SOURCE)) }).use { raw ->
                                org.jcodec.common.io.FileChannelWrapper(staging!!.createFile(file(ReplayFiles.VIDEO))).use { output ->
                                    val mux = MP4Muxer.createMP4MuxerToChannel(output)
                                    val track = MP4MuxerTrack(mux.nextTrackId, MP4TrackType.VIDEO)
                                    mux.addTrack(track)
                                    val edits = mutableListOf<Edit>()
                                    val entries = mutableListOf<SampleEntry>()
                                    val entryMap = mutableMapOf<Int, Int>()
                                    var media = 0L
                                    var movie = 0L
                                    var prevKept: KeptPart? = null
                                    var packetIndex = 0
                                    kept.forEachIndexed { keptIndex, keptPart ->
                                        checkActive()
                                        val run = keptParts[keptIndex]
                                        val partRows = run.rows
                                        val firstOut = run.first
                                        val lastOut = run.last
                                        val fullStart = run.fullStart
                                        val editOffset = keptPart.part.requiredLongText("edit_start_us") +
                                            (keptPart.a - keptPart.part.requiredLongText("movie_start_us")) -
                                            partRows[firstOut].row.requiredLongText("media_pts_us")
                                        require(editOffset >= 0) { "表示frameのmedia位置が読めません" }
                                        val partial = !fullStart
                                        val gopStart = run.gopStart
                                        val reencodeEnd = run.reencodeEnd
                                        val normalizer = if (partial) {
                                            val config = annexBConfigs[partRows[firstOut].sampleEntry - 1]
                                            val minimum = partRows.subList(firstOut, reencodeEnd).mapNotNull { it.sourceDuration }.minOrNull()
                                            try { MixedVideoNormalizer(listOf(config), minimum).apply { startRun(config) } }
                                            catch (e: IllegalArgumentException) {
                                                throw IllegalStateException("開始位置の再エンコードに対応できません", e)
                                            }
                                        } else null
                                        if (normalizer != null) didReencode = true
                                        val gapUs = prevKept?.let { keptPart.part.requiredLongText("movie_start_us") - it.part.requiredLongText("movie_end_us") }
                                        if (prevKept != null) {
                                            val original = saved.manifest["video_run_boundaries"]?.asJsonArray?.firstOrNull {
                                                it.asJsonObject["before_segment"]?.asInt == prevKept!!.index &&
                                                    it.asJsonObject["after_segment"]?.asInt == keptPart.index
                                            }?.asJsonObject
                                            newBoundaries += mapOf("before_segment" to keptIndex - 1, "after_segment" to keptIndex,
                                                "actual_gap_duration_us" to original?.text("actual_gap_duration_us"),
                                                "authored_gap_duration_us" to gapUs?.toString())
                                        }
                                        if (gapUs != null && gapUs > 0) { edits += Edit(gapUs, -1, 1f); movie = Math.addExact(movie, gapUs) }
                                        val movieStart = movie
                                        val runMedia = media
                                        val configOffset = if (raw != null) {
                                            checkSpace(partRows[firstOut].configSize ?: 0)
                                            raw.position()
                                        } else null
                                        if (raw != null) {
                                            val first = partRows[firstOut]
                                            val size = first.configSize?.toInt() ?: 0
                                            if (size > 0) {
                                                val data = readTrimSource(sourceChannel!!, first.configOffset!!, size.toLong(), ::checkActive)
                                                while (data.hasRemaining()) { checkActive(); raw.write(data) }
                                            }
                                        }
                                        for (n in gopStart..lastOut) {
                                            checkActive()
                                            val row = partRows[n]
                                            if (packetIndex != row.index) {
                                                require(videoTrack.gotoFrame(row.index.toLong())) { "動画sampleが足りません" }
                                                packetIndex = row.index
                                            }
                                            checkActive()
                                            val packet = videoTrack.nextFrame()
                                                ?: throw IllegalArgumentException("動画sampleが足りません")
                                            packetIndex++
                                            checkActive()
                                            check(packet.frameNo == row.index.toLong()) { "動画sampleの順序がframe記録と一致しません" }
                                            if (n < firstOut) {
                                                require(normalizer != null) { "開始IDRの位置が不正です" }
                                                normalizer.decodeOnly(annexB(packet.data, row.sampleEntry))
                                                continue
                                            }
                                            if (n < reencodeEnd) {
                                                val encoded = try { normalizer!!.encode(annexB(packet.data, row.sampleEntry)) }
                                                catch (e: Exception) { throw IllegalStateException("開始位置の再エンコードに失敗しました", e) }
                                                checkActive()
                                                if (n == firstOut) {
                                                    require(H264Utils.isByteBufferIDRSlice(encoded.duplicate())) { "切り取り開始のIDRがありません" }
                                                    val sampleEntry = H264Utils.createMOVSampleEntryFromBytes(NIOUtils.duplicate(encoded))
                                                    track.addSampleEntry(sampleEntry)
                                                    entries += sampleEntry
                                                }
                                                row.sourceDuration?.let { normalizer.verifyInterval(it, encoded.remaining()) }
                                                val outputKey = H264Utils.isByteBufferIDRSlice(encoded.duplicate())
                                                val stripped = ByteBuffer.allocate(encoded.remaining())
                                                H264Utils.wipePS(encoded.duplicate(), stripped, mutableListOf(), mutableListOf())
                                                require(stripped.hasRemaining()) { "再エンコード出力に映像sampleがありません" }
                                                val data = H264Utils.encodeMOVPacket(stripped)
                                                checkSpace(data.remaining().toLong())
                                                track.addFrameInternal(Packet.createPacket(data, media, 1_000_000, row.displayDuration,
                                                    sampleIndex, if (outputKey) Packet.FrameType.KEY else Packet.FrameType.INTER, null), entries.size)
                                                writeRow(index, row, raw, sourceChannel, keptIndex, sampleIndex,
                                                    entries.size, outputKey, true, normalizer, startUs, originTns, media, configOffset, ::checkSpace, ::checkActive)
                                                outputRows += row.row
                                            } else {
                                                val entry = entryMap.getOrPut(row.sampleEntry) {
                                                    val sampleEntry = H264Utils.createMOVSampleEntryFromAvcC(avcs[row.sampleEntry - 1])
                                                    track.addSampleEntry(sampleEntry)
                                                    entries += sampleEntry
                                                    entries.size
                                                }
                                                val outputKey = packet.frameType == Packet.FrameType.KEY
                                                check(outputKey == row.outputKey) { "動画sampleのkeyframeがframe記録と一致しません" }
                                                checkSpace(packet.data.remaining().toLong())
                                                track.addFrameInternal(Packet.createPacket(packet.data, media, 1_000_000, row.displayDuration,
                                                    sampleIndex, packet.frameType, null), entry)
                                                writeRow(index, row, raw, sourceChannel, keptIndex, sampleIndex,
                                                    entry, outputKey, false, null, startUs, originTns, media, configOffset, ::checkSpace, ::checkActive)
                                                outputRows += row.row
                                            }
                                            media = Math.addExact(media, row.displayDuration)
                                            sampleIndex++
                                        }
                                        // A source edit may start inside the first retained sample.
                                        val editMedia = Math.addExact(runMedia, editOffset)
                                        val duration = Math.subtractExact(keptPart.b, keptPart.a)
                                        edits += Edit(duration, editMedia, 1f)
                                        movie = Math.addExact(movie, duration)
                                        val partEndTns = if (keptPart.b == keptPart.part.requiredLongText("movie_end_us"))
                                            keptPart.part.longText("t_end_ns") else endTns
                                        val confirmedEndTns = partRows.subList(firstOut, lastOut + 1).asSequence()
                                            .filter { it.presented }.mapNotNull { row ->
                                                val start = row.displayTns ?: row.tNs
                                                row.confirmedEndTns?.let { end -> partEndTns?.let { minOf(end, it) } ?: end }
                                                    ?.takeIf { start != null && it > start }
                                            }.maxOrNull()
                                        newParts += mapOf("file" to ReplayFiles.VIDEO, "segment_index" to keptIndex,
                                            "generation" to keptPart.part["generation"], "decode_run" to keptPart.part["decode_run"],
                                            "source_pts_origin_us" to keptPart.part["source_pts_origin_us"],
                                            "movie_start_us" to (keptPart.a - startUs).toString(), "movie_end_us" to (keptPart.b - startUs).toString(),
                                            "media_start_us" to runMedia.toString(), "edit_start_us" to editMedia.toString(),
                                            "duration_us" to duration.toString(),
                                            "clock_alignment_known" to (originTns != null && keptPart.part["clock_alignment_known"]?.asBoolean == true),
                                            "t_start_ns" to if (fullStart) shift(keptPart.part.text("t_start_ns"), originTns)
                                                else originTns?.let { "0" },
                                            "t_end_ns" to if (keptPart.b == keptPart.part.requiredLongText("movie_end_us"))
                                                shift(keptPart.part.text("t_end_ns"), originTns) else shift(endTns?.toString(), originTns),
                                            "confirmed_t_end_ns" to shift(confirmedEndTns?.toString(), originTns),
                                            "media_timeline_clipped" to keptPart.part["media_timeline_clipped"],
                                            "preroll_samples" to if (fullStart) keptPart.part["preroll_samples"] else 0)
                                        prevKept = keptPart
                                    }
                                    check(outputRows.size == sampleIndex.toInt()) { "出力frame数が一致しません" }
                                    track.setEdits(edits)
                                    val audio = muxAudio(audioPlan, mux, ::checkActive, ::checkSpace)
                                    val movieBox = mux.finalizeHeader()
                                    require(movieBox.timescale == 1_000_000) { "動画movie timescaleが不正です" }
                                    check(edits.fold(0L) { sum, edit -> Math.addExact(sum, edit.duration) } == movie &&
                                        movie == Math.subtractExact(endUs, startUs)) { "movie時間が一致しません" }
                                    movieBox.tracks.forEach { trak ->
                                        trak.setDuration(if (audio != null && trak.trackHeader.trackId == audio.second.trackId) audio.third else movie)
                                    }
                                    movieBox.setDuration(movie)
                                    mux.storeHeader(movieBox)
                                    index.flush()
                                    audioSegments = audio?.first
                                }
                            }
                        }
                    } finally {
                        sourceChannel?.close()
                    }
                }
                checkActive()
                val logCounts = writeLogs(saved, ::file, originTns, endTns, staging!!, ::checkSpace, ::checkActive)
                val touchCounts = writeTouches(saved, ::file, originTns, endTns, startUs, endUs, staging!!, ::checkSpace, ::checkActive)
                val summary = readme(saved, name, createdAt, startUs, endUs, fromUs, toUs, outputRows.size, audioSegments, logCounts, touchCounts)
                checkSpace(utf8Bytes(summary))
                staging!!.writer(file(ReplayFiles.SUMMARY)).use { it.write(summary) }
                checkActive()
                checkSpace()
                val hashes = staging!!.hashes(::checkActive)
                checkActive()
                val manifest = transformedManifest(saved, name, createdAt, startUs, endUs, fromUs, toUs,
                    originTns, endTns, newParts, newBoundaries, didReencode, audioSegments, outputRows, logCounts,
                    touchCounts, hashes)
                val record = gson.toJson(manifest)
                checkSpace(utf8Bytes(record))
                staging!!.writer(file(ReplayFiles.RECORD)).use { it.write(record) }
                manifestWritten = true
                staging!!.forceEach(::checkActive)
                checkSpace()
                checkActive()
                staging!!.outputs.forEach { staging!!.checkOwned(it) }
                staging!!.checkEntries()
                staging!!.closeForRename()
                publish(staging!!.publication())
                staging!!.outputs.forEach { staging!!.checkOwned(staging!!.complete.resolve(it.fileName)) }
                staging!!.checkEntries()
                return staging!!.complete
            }
        } catch (e: Throwable) {
            if (e !is Exception && e !is LinkageError) throw e
            if (manifestWritten) staging?.output(staging!!.file(ReplayFiles.RECORD))?.let { held -> runCatching {
                val output = staging!!.reopenOutput(staging!!.file(ReplayFiles.RECORD))
                try { output.channel().use { channel ->
                    channel.position(0)
                    channel.truncate(0)
                    val bytes = ByteBuffer.wrap("{\"complete\":false}".toByteArray(Charsets.UTF_8))
                    while (bytes.hasRemaining()) channel.write(bytes)
                    channel.force(true)
                } } finally { if (output !== held) output.close() }
            } }
            val retained = staging?.let { listOf(it.partial, it.complete).firstOrNull(it::ownsDirectory) }
            when (e) {
                is SaveCancelled -> throw SaveCancelled(retained, e)
                else -> throw SaveFailure(retained, e.message ?: "切り取った動画の保存に失敗しました", e)
            }
        } finally {
            staging?.close()
        }
    }

    private var audioSegments: List<Map<String, Any?>>? = null

    private fun utf8Bytes(text: String): Long = text.toByteArray(Charsets.UTF_8).size.toLong()

    private fun estimateOutput(saved: SavedReplay, video: AbstractMP4DemuxerTrack, runs: List<VideoRun>,
                               configs: List<ByteArray>, audio: AudioPlan?, startUs: Long, endUs: Long,
                               originTns: Long?, endTns: Long?, checkActive: () -> Unit): Long {
        // Fixed MP4 boxes, README, hashes and added manifest fields; sample tables are counted below.
        var bytes = 16 * 1024L
        fun add(value: Long) {
            require(value >= 0) { "出力サイズが不正です" }
            bytes = Math.addExact(bytes, value)
        }
        fun sample(size: Long) { add(size); add(128) }
        fun jsonGrowth(element: com.google.gson.JsonElement) {
            checkActive()
            // Rewritten numbers/nulls and added range keys, without dropping unknown source fields.
            add(if (element.isJsonObject || element.isJsonArray) 128 else 32)
            if (element.isJsonObject) element.asJsonObject.entrySet().forEach { jsonGrowth(it.value) }
            if (element.isJsonArray) element.asJsonArray.forEach(::jsonGrowth)
        }
        add(utf8Bytes(gson.toJson(saved.manifest)))
        jsonGrowth(saved.manifest)
        val sizes = PacketSizes(video)
        val entries = mutableSetOf<Int>()
        val raw = saved.exists(ReplayFiles.VIDEO_SOURCE)
        for (run in runs) {
            checkActive()
            add(2048) // Per-part manifest fields, edits and a run boundary.
            val encodedBound = if (run.fullStart) 0L else {
                val config = configs[run.rows[run.first].sampleEntry - 1]
                val size = H264Utils.getPicSize(boundedSps(H264Utils.getRawSPS(ByteBuffer.wrap(config)).first()))
                val width = Math.multiplyExact(Math.ceilDiv(size.width, 16), 16)
                val height = Math.multiplyExact(Math.ceilDiv(size.height, 16), 16)
                // MixedVideoNormalizer's width * height * 3 buffer; allow MOV NAL prefixes and avcC.
                Math.multiplyExact(Math.multiplyExact(width.toLong(), height.toLong()), 6L).also { add(it) }
            }
            if (raw) add(run.rows[run.first].configSize ?: 0)
            for (n in run.first..run.last) {
                checkActive()
                val row = run.rows[n]
                if (!run.fullStart && n < run.reencodeEnd) sample(encodedBound) else {
                    sample(sizes[row.index])
                    if (entries.add(row.sampleEntry)) {
                        add(Math.addExact(Math.multiplyExact(video.sampleEntries[row.sampleEntry - 1].estimateSize().toLong(), 2), 256))
                    }
                }
                // Existing JSON plus bounded numeric/rectangle additions from writeRow.
                add(Math.addExact(utf8Bytes(gson.toJson(row.row)), 2048))
                if (raw && row.hasSource) add(row.sourceSize ?: throw IllegalArgumentException("元映像packetのサイズがありません"))
            }
        }
        if (audio != null) {
            add(Math.addExact(Math.multiplyExact(audio.entry.estimateSize().toLong(), 2), 256))
            for (run in audio.runs) {
                checkActive()
                add(512)
                for (n in run.first - run.preroll until run.end) { checkActive(); sample(audio.sizes[n]) }
            }
        }
        var lastDeviceId: String? = null
        var lastAppId: String? = null
        saved.forEachLine(ReplayFiles.LOGCAT_DEVICE) { line ->
            checkActive()
            val row = trimmedRow(gson.fromJson(line, JsonObject::class.java), originTns, endTns) ?: return@forEachLine
            val copies = if (row["app_membership"]?.takeIf { it.isJsonPrimitive }?.asBoolean == true) 2L else 1L
            add(Math.multiplyExact(Math.addExact(utf8Bytes(gson.toJson(row)), 1), copies))
            lastDeviceId = row.text("record_id") ?: lastDeviceId
            if (copies == 2L) lastAppId = row.text("record_id") ?: lastAppId
        }
        add(utf8Bytes(gson.toJson(lastDeviceId)))
        add(utf8Bytes(gson.toJson(lastAppId)))
        if (saved.exists(ReplayFiles.TOUCHES)) saved.forEachLine(ReplayFiles.TOUCHES) { line ->
            checkActive()
            val row = trimmedRow(gson.fromJson(line, JsonObject::class.java), originTns, endTns, startUs until endUs)
                ?: return@forEachLine
            add(Math.addExact(utf8Bytes(gson.toJson(row)), 1))
        }
        checkActive()
        return bytes
    }

    private fun shift(value: String?, origin: Long?): String? =
        if (value == null || origin == null) null else Math.subtractExact(value.toLong(), origin).toString()

    private fun writeRow(index: java.io.BufferedWriter, row: StoredFrame,
                         raw: FileChannel?, sourceChannel: FileChannel?, partIndex: Int, sampleIndex: Long,
                         entry: Int, outputKey: Boolean, reencoded: Boolean, normalizer: MixedVideoNormalizer?,
                         startUs: Long, originTns: Long?, media: Long, configOffset: Long?, checkSpace: (Long) -> Unit,
                         checkActive: () -> Unit) {
        val out = row.row.deepCopy()
        out.addProperty("segment_index", partIndex)
        out.addProperty("sample_index", sampleIndex)
        out.addProperty("sample_entry", entry)
        out.addProperty("output_key", outputKey)
        out.addProperty("reencoded", reencoded || row.row["reencoded"].asBoolean)
        if (reencoded) {
            out.add("source_rect", trimSourceRectangle(row.row, normalizer!!.rectangle(), normalizer.width, normalizer.height))
            out.addProperty("output_width", normalizer.width)
            out.addProperty("output_height", normalizer.height)
            out.addProperty("source_colour_known", normalizer.colourKnown)
        }
        if (raw != null && row.hasSource) {
            checkSpace(row.sourceSize ?: 0)
            val offset = raw.position()
            val size = row.sourceSize!!
            val data = readTrimSource(sourceChannel!!, row.sourceOffset!!, size, checkActive)
            while (data.hasRemaining()) { checkActive(); raw.write(data) }
            out.addProperty("source_offset", offset.toString())
            out.addProperty("source_config_offset", configOffset?.toString())
        }
        out.addProperty("media_pts_us", media.toString())
        out.addProperty("movie_pts_us", row.moviePts?.let { Math.subtractExact(it, startUs).toString() })
        out.addProperty("movie_end_us", row.movieEnd?.let { Math.subtractExact(it, startUs).toString() })
        out.addProperty("presentation_pts_us", Math.subtractExact(row.row.requiredLongText("presentation_pts_us"), startUs).toString())
        out.addProperty("t_ns", shift(row.row.text("t_ns"), originTns))
        out.addProperty("display_t_ns", shift(row.row.text("display_t_ns"), originTns))
        out.addProperty("confirmed_t_end_ns", shift(row.row.text("confirmed_t_end_ns"), originTns))
        if (originTns == null) {
            out.addProperty("clock_alignment_known", false)
            out.addProperty("t_uncertainty_ns", Long.MAX_VALUE.toString())
        }
        val text = gson.toJson(out)
        checkSpace(Math.addExact(utf8Bytes(text), 1))
        index.write(text)
        index.write("\n")
    }

    private fun planAudio(saved: SavedReplay, demuxer: MP4Demuxer, startUs: Long, endUs: Long, checkActive: () -> Unit): AudioPlan? {
        val trackInfo = saved.manifest["audio_track"]?.takeIf { it.isJsonObject }?.asJsonObject ?: return null
        val segments = trackInfo["segments"]?.takeIf { it.isJsonArray }?.asJsonArray
            ?.map { it.asJsonObject } ?: return null
        val sampleRate = trackInfo["sample_rate"]?.asInt ?: return null
        val audioTrack = demuxer.audioTracks.firstOrNull() as? org.jcodec.containers.mp4.demuxer.AbstractMP4DemuxerTrack
            ?: throw IllegalArgumentException("音声trackが見つかりません")
        val entry = audioTrack.sampleEntries.filterIsInstance<AudioSampleEntry>().firstOrNull()
            ?: throw IllegalArgumentException("音声sample entryが見つかりません")
        data class AudioSegment(val movieStart: Long, val duration: Long, val mediaStart: Long, val count: Int)
        val plan = segments.map {
            checkActive()
            AudioSegment(it.requiredLongText("movie_start_us"), it.requiredLongText("duration_us"),
                it.requiredLongText("media_start_samples"), it["samples"].asInt)
        }
        val sizes = PacketSizes(audioTrack)
        var referencedEnd = 0L
        val runs = plan.mapNotNull { segment ->
            checkActive()
            // Unreferenced media before a segment may be a previous trim's hidden prefix.
            val prefixStart = minOf(segment.mediaStart / AAC_FRAME_SAMPLES, referencedEnd)
            if (segment.count > 0) referencedEnd = maxOf(referencedEnd, segment.mediaStart / AAC_FRAME_SAMPLES + segment.count)
            val from = maxOf(startUs, segment.movieStart)
            val to = minOf(endUs, Math.addExact(segment.movieStart, segment.duration))
            if (from >= to || segment.count <= 0) return@mapNotNull null
            val mediaFrom = Math.addExact(segment.mediaStart,
                Math.multiplyExact(from - segment.movieStart, sampleRate.toLong()) / 1_000_000)
            val mediaEnd = minOf(Math.addExact(segment.mediaStart,
                Math.ceilDiv(Math.multiplyExact(to - segment.movieStart, sampleRate.toLong()), 1_000_000L)),
                (segment.mediaStart / AAC_FRAME_SAMPLES + segment.count) * AAC_FRAME_SAMPLES)
            if (mediaFrom >= mediaEnd) return@mapNotNull null
            val first = Math.toIntExact(mediaFrom / AAC_FRAME_SAMPLES)
            val end = Math.toIntExact(Math.ceilDiv(mediaEnd, AAC_FRAME_SAMPLES))
            require(first >= 0 && end <= sizes.count) { "音声segmentのsample範囲が不正です" }
            AudioRun(from - startUs, to - from, first, end, mediaFrom % AAC_FRAME_SAMPLES,
                if (first > prefixStart) 1 else 0)
        }
        return if (runs.isEmpty()) null else AudioPlan(audioTrack, entry, sampleRate, runs, sizes)
    }

    private fun muxAudio(plan: AudioPlan?, mux: MP4Muxer, checkActive: () -> Unit,
                         checkSpace: (Long) -> Unit): Triple<List<Map<String, Any?>>, MP4MuxerTrack, Long>? {
        if (plan == null) return null
        val packets = mutableListOf<Packet>()
        while (true) {
            checkActive()
            val packet = plan.track.nextFrame() ?: break
            packets += packet
        }
        val sound = MP4MuxerTrack(mux.nextTrackId, MP4TrackType.SOUND)
        mux.addTrack(sound)
        sound.addSampleEntry(plan.entry) // Reuses the source esds as stored.
        val edits = mutableListOf<Edit>()
        val newSegments = mutableListOf<Map<String, Any?>>()
        var cursor = 0L
        var written = 0L
        plan.runs.forEach { run ->
            if (run.movieStart > cursor) edits += Edit(run.movieStart - cursor, -1, 1f)
            val mediaStart = Math.addExact((written + run.preroll) * AAC_FRAME_SAMPLES, run.skip)
            edits += Edit(run.duration, mediaStart, 1f)
            newSegments += mapOf("movie_start_us" to run.movieStart.toString(), "duration_us" to run.duration.toString(),
                "media_start_samples" to mediaStart.toString(), "samples" to run.end - run.first)
            for (i in run.first - run.preroll until run.end) {
                checkActive()
                checkSpace(packets[i].data.remaining().toLong())
                sound.addFrameInternal(Packet.createPacket(packets[i].data, written * 1024L, plan.sampleRate,
                    AAC_FRAME_SAMPLES, written, Packet.FrameType.KEY, null), 1)
                written++
            }
            cursor = Math.addExact(run.movieStart, run.duration)
        }
        sound.setEdits(edits)
        return Triple(newSegments, sound, cursor)
    }

    private data class LogCounts(val device: Int, val app: Int, val lastDeviceId: String?, val lastAppId: String?,
                                 val times: Map<String, LongRange>, val evidenceTimes: Map<String, Long?>)
    private data class TouchCounts(val count: Int, val inMovie: Int, val injected: Int)

    private fun triggerEvidence(record: JsonObject): List<JsonObject> {
        val trigger = record["auto_trigger"]?.takeIf { it.isJsonObject }?.asJsonObject ?: return emptyList()
        return listOf("java_evidence", "platform_evidence").mapNotNull { trigger[it]?.takeIf { row -> row.isJsonObject }?.asJsonObject }
    }

    private fun inTnsRange(line: JsonObject, originTns: Long?, endTns: Long?): Boolean {
        val t = line.longText("t_ns") ?: return true
        val uncertainty = line.longText("t_uncertainty_ns") ?: Long.MAX_VALUE
        val margin = if (uncertainty == Long.MAX_VALUE) Long.MAX_VALUE else maxOf(0L, uncertainty)
        fun addSat(base: Long): Long = if (margin == Long.MAX_VALUE) Long.MAX_VALUE else try { Math.addExact(base, margin) } catch (e: ArithmeticException) { Long.MAX_VALUE }
        fun subSat(base: Long): Long = if (margin == Long.MAX_VALUE) Long.MIN_VALUE else try { Math.subtractExact(base, margin) } catch (e: ArithmeticException) { Long.MIN_VALUE }
        return (originTns == null || addSat(t) >= originTns) && (endTns == null || subSat(t) <= endTns)
    }

    private fun trimmedRow(row: JsonObject, originTns: Long?, endTns: Long?, movie: LongRange? = null): JsonObject? {
        if (!inTnsRange(row, originTns, endTns)) return null
        row.addProperty("t_ns", shift(row.text("t_ns"), originTns))
        if (originTns == null) row.addProperty("t_uncertainty_ns", Long.MAX_VALUE.toString())
        if (movie != null) {
            row.addProperty("movie_us", row.longText("movie_us")?.takeIf { it in movie }?.let { Math.subtractExact(it, movie.first).toString() })
        }
        return row
    }

    private fun writeLogs(saved: SavedReplay, file: (String) -> Path, originTns: Long?, endTns: Long?,
                          staging: SaveStaging, checkSpace: (Long) -> Unit, checkActive: () -> Unit): LogCounts {
        var device = 0
        var app = 0
        var lastDevice: String? = null
        var lastApp: String? = null
        val times = mutableMapOf<String, LongRange>()
        val evidenceIds = triggerEvidence(saved.manifest).mapNotNull { it.text("record_id") }.toSet()
        val evidenceTimes = mutableMapOf<String, Long?>()
        fun recordTime(kind: String, row: JsonObject) {
            val t = row.longText("t_ns") ?: return
            val span = times[kind]
            times[kind] = minOf(span?.first ?: t, t)..maxOf(span?.last ?: t, t)
        }
        val deviceOut = staging.writer(file(ReplayFiles.LOGCAT_DEVICE))
        val appOut = staging.writer(file(ReplayFiles.LOGCAT_APP))
        deviceOut.use { deviceWriter -> appOut.use { appWriter ->
            saved.forEachLine(ReplayFiles.LOGCAT_DEVICE) { line ->
                checkActive()
                val row = trimmedRow(gson.fromJson(line, JsonObject::class.java), originTns, endTns) ?: return@forEachLine
                row.text("record_id")?.takeIf { it in evidenceIds }?.let { evidenceTimes[it] = row.longText("t_ns") }
                val text = gson.toJson(row)
                val appMember = row["app_membership"]?.takeIf { it.isJsonPrimitive }?.asBoolean == true
                checkSpace(Math.multiplyExact(Math.addExact(utf8Bytes(text), 1), if (appMember) 2L else 1L))
                deviceWriter.write(text); deviceWriter.write("\n")
                device++
                recordTime("device_log", row)
                lastDevice = row.text("record_id") ?: lastDevice
                if (appMember) {
                    appWriter.write(text); appWriter.write("\n"); app++; lastApp = row.text("record_id") ?: lastApp
                    recordTime("app_log", row)
                }
            }
        } }
        return LogCounts(device, app, lastDevice, lastApp, times, evidenceTimes)
    }

    private fun writeTouches(saved: SavedReplay, file: (String) -> Path, originTns: Long?, endTns: Long?,
                             startUs: Long, endUs: Long, staging: SaveStaging,
                             checkSpace: (Long) -> Unit, checkActive: () -> Unit): TouchCounts? {
        if (!saved.exists(ReplayFiles.TOUCHES)) return null
        var count = 0
        var inMovie = 0
        var injected = 0
        staging.writer(file(ReplayFiles.TOUCHES)).use { out ->
            saved.forEachLine(ReplayFiles.TOUCHES) { line ->
                checkActive()
                val row = trimmedRow(gson.fromJson(line, JsonObject::class.java), originTns, endTns, startUs until endUs)
                    ?: return@forEachLine
                if (row.longText("movie_us") != null) inMovie++
                val text = gson.toJson(row)
                checkSpace(Math.addExact(utf8Bytes(text), 1))
                out.write(text); out.write("\n")
                count++
                if (row.text("source") == "injected") injected++
            }
        }
        return TouchCounts(count, inMovie, injected)
    }

    private fun clipRanges(list: List<JsonObject>, originTns: Long?, endTns: Long?): List<JsonObject> =
        list.mapNotNull { entry ->
            val from = entry.longText("from_t_ns")
            val to = entry.longText("to_t_ns")
            if (to != null && originTns != null && to <= originTns) return@mapNotNull null
            if (from != null && endTns != null && from >= endTns) return@mapNotNull null
            entry.deepCopy().apply {
                addProperty("from_t_ns", shift(from?.toString(), originTns)?.toLong()?.coerceAtLeast(0L)?.toString())
                addProperty("to_t_ns", shift(to?.let { endTns?.let { end -> minOf(it, end) } ?: it }?.toString(), originTns))
                if (originTns == null && has("duration_uncertain")) addProperty("duration_uncertain", true)
            }
        }

    private fun transformedManifest(saved: SavedReplay, name: String, createdAt: ZonedDateTime,
                                    startUs: Long, endUs: Long, fromUs: Long, toUs: Long,
                                    originTns: Long?, endTns: Long?,
                                    newParts: List<Map<String, Any?>>, newBoundaries: List<Map<String, Any?>>,
                                    didReencode: Boolean, audio: List<Map<String, Any?>>?,
                                    outputRows: List<JsonObject>, logCounts: LogCounts, touchCounts: TouchCounts?,
                                    hashes: Map<String, String>): JsonObject {
        val out = saved.manifest.deepCopy()
        out["auto_trigger"]?.takeIf { it.isJsonObject }?.asJsonObject?.let { trigger ->
            for (name in listOf("arming_t_ns", "desired_pre_report_start_t_ns", "desired_pre_report_end_t_ns")) {
                if (trigger.has(name)) trigger.addProperty(name, shift(trigger.text(name), originTns))
            }
            for (evidence in triggerEvidence(out)) {
                val id = evidence.text("record_id")
                val present = id != null && logCounts.evidenceTimes.containsKey(id)
                evidence.addProperty("in_output", present)
                evidence.addProperty("frozen_t_ns", if (present) logCounts.evidenceTimes[id]?.toString()
                    else shift(evidence.text("frozen_t_ns"), originTns))
                if (originTns == null) evidence.addProperty("t_uncertainty_ns", Long.MAX_VALUE.toString())
            }
        }
        out.addProperty("save_id", nameId())
        out.addProperty("folder_name", name)
        out.addProperty("created_at", createdAt.toInstant().toString())
        out.addProperty("created_at_local", createdAt.toOffsetDateTime().toString())
        out.addProperty("video_movie_duration_us", Math.subtractExact(endUs, startUs).toString())
        out.addProperty("video_reencoded", (saved.manifest["video_reencoded"]?.asBoolean ?: false) || didReencode)
        val files = out["files"]?.takeIf { it.isJsonObject }?.asJsonObject ?: JsonObject()
        files.addProperty("readme", ReplayFiles.SUMMARY)
        files.addProperty("record", ReplayFiles.RECORD)
        files.addProperty("video", ReplayFiles.VIDEO)
        files.addProperty("video_frames", ReplayFiles.VIDEO_FRAMES)
        if (saved.exists(ReplayFiles.VIDEO_SOURCE)) files.addProperty("video_source", ReplayFiles.VIDEO_SOURCE) else files.remove("video_source")
        files.addProperty("logcat_device", ReplayFiles.LOGCAT_DEVICE)
        files.addProperty("logcat_app", ReplayFiles.LOGCAT_APP)
        if (saved.exists(ReplayFiles.TOUCHES)) files.addProperty("touches", ReplayFiles.TOUCHES) else files.remove("touches")
        out.add("files", files)
        val counts = out["counts"]?.takeIf { it.isJsonObject }?.asJsonObject ?: JsonObject()
        counts.addProperty("device_log_rows", logCounts.device)
        counts.addProperty("app_log_rows", logCounts.app)
        counts.addProperty("video_frames", outputRows.size)
        touchCounts?.let { counts.addProperty("touches", it.count); counts.addProperty("touches_in_movie", it.inMovie) }
        out.add("counts", counts)
        out.add("parts", gson.toJsonTree(newParts))
        out.add("video_run_boundaries", gson.toJsonTree(newBoundaries))
        fun rangeArray(name: String, parent: JsonObject = out, transform: (List<JsonObject>) -> List<JsonObject> = { clipRanges(it, originTns, endTns) }) {
            parent[name]?.takeIf { it.isJsonArray }?.asJsonArray?.map { it.asJsonObject }?.let { rows ->
                parent.add(name, gson.toJsonTree(transform(rows)))
            }
        }
        fun shiftSamples(row: JsonObject) {
            for (name in listOf("before_sample", "after_sample", "recovery_sample")) {
                (row[name] as? JsonObject)?.let { it.addProperty("t_ns", shift(it.text("t_ns"), originTns)) }
            }
        }
        val keptGapIndexes = (out["gaps"]?.takeIf { it.isJsonArray }?.asJsonArray?.map { it.asJsonObject } ?: emptyList())
            .indices.filter { i ->
                val g = (out["gaps"]?.takeIf { it.isJsonArray }?.asJsonArray?.map { it.asJsonObject } ?: emptyList())[i]
                val from = g.longText("from_t_ns"); val to = g.longText("to_t_ns")
                !(to != null && originTns != null && to <= originTns) && !(from != null && endTns != null && from >= endTns)
            }
        rangeArray("gaps")
        rangeArray("video_missing_ranges")
        rangeArray("video_gap_scopes") { rows ->
            val indexMap = keptGapIndexes.withIndex().associate { (new, old) -> old to new }
            clipRanges(rows, originTns, endTns).mapNotNull { row ->
                val oldIndex = row["gap_index"]?.asInt ?: return@mapNotNull null
                row.addProperty("gap_index", indexMap[oldIndex] ?: return@mapNotNull null)
                shiftSamples(row)
                row
            }
        }
        out["video_tail"]?.takeIf { it.isJsonObject }?.asJsonObject?.let { tail ->
            out.add("video_tail", clipRanges(listOf(tail), originTns, endTns).singleOrNull()?.apply {
                addProperty("source_t_ns", shift(tail.text("source_t_ns"), originTns))
            })
        }
        out["timebase"]?.takeIf { it.isJsonObject }?.asJsonObject?.let { timebase ->
            if (originTns == null) {
                timebase.addProperty("known", false)
                timebase.addProperty("window_start_elapsed_ns", null as String?)
            }
            if (originTns == null || endTns == null) timebase.addProperty("window_end_uncertainty_ns", Long.MAX_VALUE.toString())
            originTns?.let { origin ->
                timebase["window_start_elapsed_ns"]?.takeIf { it.isJsonPrimitive }?.let {
                    timebase.addProperty("window_start_elapsed_ns", Math.addExact(it.asString.toLong(), origin).toString())
                }
            }
            timebase.addProperty("window_duration_ns", (if (originTns != null && endTns != null) Math.subtractExact(endTns, originTns)
                else Math.subtractExact(endUs, startUs) * 1000L).toString())
            timebase["samples"]?.takeIf { it.isJsonArray }?.asJsonArray?.forEach { sample ->
                val s = sample.asJsonObject
                s.addProperty("t_ns", shift(s.text("t_ns"), originTns))
            }
            timebase["boot_segments"]?.takeIf { it.isJsonArray }?.asJsonArray?.forEach { seg ->
                val s = seg.asJsonObject
                s.addProperty("first_sample_t_ns", shift(s.text("first_sample_t_ns"), originTns))
                s.addProperty("last_sample_t_ns", shift(s.text("last_sample_t_ns"), originTns))
            }
            timebase["clock_steps"]?.takeIf { it.isJsonArray }?.asJsonArray?.map { it.asJsonObject }?.let { steps ->
                val clipped = clipRanges(steps, originTns, endTns)
                clipped.forEach(::shiftSamples)
                timebase.add("clock_steps", gson.toJsonTree(clipped))
            }
        }
        out["coverage"]?.takeIf { it.isJsonObject }?.asJsonObject?.let { coverage ->
            val durationNs = if (originTns != null && endTns != null) endTns - originTns else (endUs - startUs) * 1000
            val videoTimes = outputRows.mapNotNull { shift(it.text("t_ns"), originTns)?.toLong() }
            val spans = logCounts.times + ("video" to (videoTimes.minOrNull()?.let { it..videoTimes.max() }))
            for (kind in listOf("video", "device_log", "app_log")) {
                val stream = coverage[kind]?.takeIf { it.isJsonObject }?.asJsonObject ?: continue
                val span = spans[kind]
                val observed = span?.let { (minOf(it.last, durationNs) - maxOf(it.first, 0L)).coerceAtLeast(0) } ?: 0L
                val missing = out["gaps"]?.asJsonArray?.sumOf { element ->
                    val gap = element.asJsonObject
                    val from = gap.longText("from_t_ns")
                    if (gap.text("kind") != kind || from == null) 0L
                    else (minOf(gap.longText("to_t_ns") ?: durationNs, durationNs) - maxOf(from, 0L)).coerceAtLeast(0)
                } ?: 0L
                stream.addProperty("availableSeconds", (observed - missing).coerceAtLeast(0) / 1e9)
            }
            coverage["audio"]?.takeIf { it.isJsonObject }?.asJsonObject?.let { audioMap ->
                val seconds = audio?.fold(0.0) { sum, s -> sum + s.getValue("duration_us").toString().toLong() / 1e6 } ?: 0.0
                audioMap.addProperty("availableSeconds", seconds)
                if (audio == null && saved.manifest["audio_track"] != null) {
                    audioMap.addProperty("reason", audioMap.text("reason") ?: "切り取り範囲に対応づけられる音声がありません")
                }
            }
        }
        val audioTrack = saved.manifest["audio_track"]?.takeIf { it.isJsonObject }?.asJsonObject
        out.add("audio_track", if (audio == null) null else audioTrack?.deepCopy()?.apply { add("segments", gson.toJsonTree(audio)) })
        out["watermarks"]?.takeIf { it.isJsonObject }?.asJsonObject?.let { watermarks ->
            watermarks.addProperty("video_pts_us", outputRows.lastOrNull()?.text("source_pts_us"))
            watermarks.addProperty("device_log_record_id", logCounts.lastDeviceId)
            watermarks.addProperty("app_log_record_id", logCounts.lastAppId)
        }
        out["touches"]?.takeIf { it.isJsonObject }?.asJsonObject?.let { touches ->
            touchCounts?.let { touches.addProperty("count", it.count); touches.addProperty("in_movie_count", it.inMovie) }
            touches["gaps"]?.takeIf { it.isJsonArray }?.asJsonArray?.map { it.asJsonObject }?.let { rows ->
                touches.add("gaps", gson.toJsonTree(clipRanges(rows, originTns, endTns)))
            }
            touches["injected"]?.takeIf { it.isJsonObject }?.asJsonObject?.let { injected ->
                touchCounts?.let { injected.addProperty("count", it.injected) }
                injected["gaps"]?.takeIf { it.isJsonArray }?.asJsonArray?.map { it.asJsonObject }?.let { rows ->
                    injected.add("gaps", gson.toJsonTree(clipRanges(rows, originTns, endTns)))
                }
            }
        }
        out.add("trim", gson.toJsonTree(mapOf(
            "method" to "leading_gop_reencode",
            "source_save_id" to saved.manifest["save_id"]?.asString,
            "source_folder_name" to saved.name,
            "source_replay_sha256" to saved.openFile(ReplayFiles.RECORD).use { it.hash() },
            "source_video_sha256" to saved.manifest["files_sha256"]?.asJsonObject?.get(ReplayFiles.VIDEO)?.asString,
            "from_movie_us" to fromUs.toString(), "to_movie_us" to toUs.toString(),
            "from_t_ns" to originTns?.toString(), "to_t_ns" to endTns?.toString(),
            "reencoded_leading_gop" to didReencode)))
        out.add("files_sha256", gson.toJsonTree(hashes))
        return out
    }

    private fun readme(saved: SavedReplay, name: String, createdAt: ZonedDateTime,
                     startUs: Long, endUs: Long, fromUs: Long, toUs: Long, frames: Int,
                     audio: List<Map<String, Any?>>?, logCounts: LogCounts, touchCounts: TouchCounts?): String {
        fun seconds(us: Long) = "%.3f".format(us / 1e6)
        return buildString {
            appendLine("# 再現記録（切り取り）: $name")
            appendLine()
            appendLine("## 概要")
            appendLine()
            appendLine("| 項目 | 内容 |")
            appendLine("| --- | --- |")
            appendLine("| 切り取り日時 | ${createdAt.toOffsetDateTime()} |")
            appendLine("| 元フォルダ | ${saved.name} |")
            appendLine("| 指定範囲 | ${seconds(fromUs)}秒 〜 ${seconds(toUs)}秒（動画内時刻） |")
            appendLine("| 残した範囲 | ${seconds(startUs)}秒 〜 ${seconds(endUs)}秒（${seconds(endUs - startUs)}秒） |")
            appendLine("| 動画frame数 | $frames |")
            appendLine("| 音声 | ${if (audio != null) "あり" else "なし"} |")
            appendLine("| ログ行数 | 全体 ${logCounts.device} / アプリ ${logCounts.app} |")
            touchCounts?.let { appendLine("| タップ | ${it.count}件（動画内 ${it.inMovie}件） |") }
            appendLine()
            appendLine("## ファイル")
            appendLine()
            appendLine("| ファイル | 内容 |")
            appendLine("| --- | --- |")
            appendLine("| video.mp4 | 切り取った動画（先頭GOPのみ再エンコード、以降は元packetをコピー） |")
            appendLine("| video-frames.jsonl | frameごとの動画内・動画外時刻の対応 |")
            if (saved.exists(ReplayFiles.VIDEO_SOURCE)) appendLine("| video-source.h264 | 残したsampleの元H.264 packet |")
            appendLine("| logcat-device.jsonl / logcat-app.jsonl | 切り取り範囲のログ（範囲が確定できない行も保持） |")
            if (saved.exists(ReplayFiles.TOUCHES)) appendLine("| touches.jsonl | 切り取り範囲のタップ（範囲が確定できないものも保持） |")
            appendLine("| replay.json | 保存内容・切り取り情報（schema 4） |")
            appendLine()
            appendLine("元フォルダは変更していません。files_sha256 は出力ファイルごとのSHA-256です。")
        }
    }
}
