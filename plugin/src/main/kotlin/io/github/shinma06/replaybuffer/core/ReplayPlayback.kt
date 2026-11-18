package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonObject
import net.sourceforge.jaad.aac.Decoder
import net.sourceforge.jaad.aac.SampleBuffer
import org.jcodec.codecs.h264.H264Utils
import org.jcodec.codecs.h264.mp4.AvcCBox
import org.jcodec.codecs.mpeg4.mp4.EsdsBox
import org.jcodec.common.io.FileChannelWrapper
import org.jcodec.containers.mp4.boxes.AudioSampleEntry
import org.jcodec.containers.mp4.boxes.NodeBox
import org.jcodec.containers.mp4.boxes.VideoSampleEntry
import org.jcodec.containers.mp4.demuxer.AbstractMP4DemuxerTrack
import org.jcodec.containers.mp4.demuxer.MP4Demuxer
import java.awt.image.BufferedImage
import java.nio.channels.FileChannel
import java.nio.file.StandardOpenOption
import java.util.EnumSet

private fun JsonObject.longText(name: String): Long? =
    get(name)?.takeIf { it.isJsonPrimitive }?.asString?.toLongOrNull()

/**
 * Forward decoder of a saved replay's video for the player's real-time playback and filmstrip:
 * [seek] decodes from the GOP start like [ReplayFrameExport.decodeAt] (same pixels), then [next]
 * continues packet by packet instead of re-decoding the GOP for every frame. Read-only; outside EDT.
 */
internal class ReplayFrameCursor(
    private val saved: SavedReplay,
    openChannel: (SaveOutputFile) -> FileChannel = { it.channel(EnumSet.of(StandardOpenOption.READ)) },
    private val cache: ReplayFrameCache = ReplayFrameCache(),
    private val packetReadObserver: ((Int) -> Unit)? = null,
    private val gopRestartObserver: ((Int) -> Unit)? = null,
) : ReplayVideoDecoder {
    class Decoded(val image: BufferedImage, val startUs: Long, val endUs: Long, val index: Int)

    private data class FrameRow(
        val startUs: Long?,
        val endUs: Long?,
        val outputKey: Boolean,
        val segmentIndex: Int?,
        val sampleEntry: Int?,
        val sourceDurationUs: Long?,
    )
    private data class DisplayRow(val startUs: Long, val endUs: Long, val rowIndex: Int)
    private data class GopKey(val segmentIndex: Int?, val sampleEntry: Int?)

    private val rows = saved.frames
    private val frameRows = rows.map { row ->
        FrameRow(row.longText("movie_pts_us"), row.longText("movie_end_us"),
            runCatching { row["output_key"]?.takeIf { it.isJsonPrimitive }?.asBoolean == true }.getOrDefault(false),
            runCatching { row["segment_index"]?.takeIf { it.isJsonPrimitive }?.asInt }.getOrNull(),
            runCatching { row["sample_entry"]?.takeIf { it.isJsonPrimitive }?.asInt }.getOrNull(),
            row.longText("source_duration_us"))
    }
    private val displayRows = frameRows.mapIndexedNotNull { index, row ->
        val start = row.startUs
        val end = row.endUs
        if (start != null && end != null) DisplayRow(start, end, index) else null
    }.sortedWith(compareBy<DisplayRow> { it.startUs }.thenByDescending { it.rowIndex })
    private val displayStarts = LongArray(displayRows.size) { displayRows[it].startUs }
    private val keyRowsByGop = frameRows.indices.filter { frameRows[it].outputKey }
        .groupBy { GopKey(frameRows[it].segmentIndex, frameRows[it].sampleEntry) }
        .mapValues { (_, indices) -> indices.toIntArray() }
    private val nextDisplayRow = IntArray(frameRows.size).also { nextRows ->
        var next = -1
        for (index in frameRows.indices.reversed()) {
            if (frameRows[index].startUs != null && frameRows[index].endUs != null) next = index
            nextRows[index] = next
        }
    }
    private val file = saved.openFile(ReplayFiles.VIDEO)
    private val channel: FileChannel
    private val track: AbstractMP4DemuxerTrack
    private val avcs: List<AvcCBox>
    private var logicalNextIndex = 0
    private var physicalNextIndex = 0
    private var physicalGopStart: Int? = null
    private var sourcePinsNeedRecheck = false
    private var normalizer: MixedVideoNormalizer? = null
    private var runEntry = -1
    private var closed = false
    private val cacheSource = "${saved.recordSha256}:${file.key}:full-bgra-v1"

    init {
        channel = try { openChannel(file) }
        catch (failure: Throwable) { file.close(); throw failure }
        try {
            val demuxer = MP4Demuxer.createRawMP4Demuxer(FileChannelWrapper(channel))
            track = demuxer.videoTrack as AbstractMP4DemuxerTrack
            avcs = track.sampleEntries.map { entry ->
                H264Utils.parseAVCC(entry as VideoSampleEntry).also {
                    require(it.spsList.isNotEmpty() && it.ppsList.isNotEmpty()) { "動画のavcCが読めません" }
                }
            }
        } catch (failure: Throwable) {
            close(); throw failure
        }
    }

    private fun targetIndex(atUs: Long): Int? {
        val position = upperBound(displayStarts, atUs) - 1
        if (position < 0) return null
        return displayRows[position].takeIf { atUs < it.endUs }?.rowIndex
    }

    private fun gopStart(target: Int): Int? {
        val row = frameRows[target]
        val indices = keyRowsByGop[GopKey(row.segmentIndex, row.sampleEntry)] ?: return null
        return indices.getOrNull(upperBound(indices, target) - 1)
    }

    private fun canResume(target: Int, gopStart: Int): Boolean =
        normalizer != null && physicalGopStart == gopStart &&
            physicalNextIndex in gopStart..target && runEntry == frameRows[target].sampleEntry

    private fun verifySourcePins() {
        saved.openFile(ReplayFiles.VIDEO).close()
        saved.openFile(ReplayFiles.RECORD).close()
        saved.openFile(ReplayFiles.VIDEO_FRAMES).close()
    }

    /** Decodes from the reference IDR up to the frame displayed at [atUs] and returns it. */
    override fun seek(atUs: Long, checkActive: () -> Unit): Decoded = seek(atUs, checkActive, useCached = true)

    override fun seekForPlayback(atUs: Long, checkActive: () -> Unit): Decoded = seek(atUs, checkActive, useCached = false)

    private fun seek(atUs: Long, checkActive: () -> Unit, useCached: Boolean): Decoded {
        check(!closed) { "表示デコーダは終了しています" }
        checkActive()
        // Recheck live identity pins even when no MP4 read is needed for a cache hit.
        verifySourcePins()
        val target = targetIndex(atUs)
            ?: throw IllegalArgumentException("指定時刻に表示されるframeがありません")
        if (useCached) cache.get(cacheSource, target)?.let {
            logicalNextIndex = target + 1
            sourcePinsNeedRecheck = true
            return it
        }
        val gopStart = gopStart(target) ?: throw IllegalArgumentException("表示frameの参照IDRがありません")
        if (!canResume(target, gopStart)) {
            normalizer = null
            physicalGopStart = null
            runEntry = -1
            check(track.gotoFrame(gopStart.toLong())) { "動画の位置へ移動できません" }
            physicalNextIndex = gopStart
            gopRestartObserver?.invoke(gopStart)
        }
        var last: Decoded? = null
        // Keep the two preceding frames, without RGB allocations for the rest of the GOP.
        while (physicalNextIndex <= target) { checkActive(); last = decodeNext(physicalNextIndex >= target - 2) }
        checkActive()
        val decoded = last ?: throw IllegalArgumentException("指定時刻のframeを復号できません")
        logicalNextIndex = target + 1
        sourcePinsNeedRecheck = false
        return decoded
    }

    /** The next frame with a movie span after the last returned one, or null at the end of the movie. */
    override fun next(checkActive: () -> Unit): Decoded? {
        check(!closed) { "表示デコーダは終了しています" }
        if (logicalNextIndex >= rows.size) return null
        val target = nextDisplayRow[logicalNextIndex]
        if (target < 0) {
            logicalNextIndex = rows.size
            return null
        }
        val atUs = frameRows[target].startUs
            ?: throw IllegalStateException("次の表示frameの時刻がありません")
        val keyframe = gopStart(target)
        if (keyframe != null && canResume(target, keyframe)) {
            if (sourcePinsNeedRecheck) verifySourcePins()
            var last: Decoded? = null
            while (physicalNextIndex <= target) {
                checkActive()
                last = decodeNext(physicalNextIndex >= target - 2)
            }
            checkActive()
            val decoded = last ?: throw IllegalArgumentException("次の表示frameを復号できません")
            logicalNextIndex = target + 1
            sourcePinsNeedRecheck = false
            return decoded
        }
        return seek(atUs, checkActive)
    }

    private fun decodeNext(display: Boolean = true): Decoded? {
        val index = physicalNextIndex
        val packet = track.nextFrame() ?: throw IllegalStateException("動画のsampleが足りません")
        physicalNextIndex = index + 1
        packetReadObserver?.invoke(index)
        val row = frameRows[index]
        val entry = row.sampleEntry ?: throw IllegalArgumentException("frame記録を読めません")
        if (row.outputKey) {
            normalizer = null
            physicalGopStart = null
            runEntry = -1
            val config = annexBConfig(avcs[entry - 1])
            normalizer = try {
                MixedVideoNormalizer(listOf(config), row.sourceDurationUs).apply { startRun(config) }
            } catch (e: IllegalArgumentException) {
                throw IllegalStateException("表示frameの復号に対応できません", e)
            }
            runEntry = entry
            physicalGopStart = index
        }
        val decoder = normalizer ?: throw IllegalStateException("参照IDRより前から復号しようとしました")
        require(entry == runEntry) { "表示frameのsample構成が読めません" }
        val picture = try {
            val bytes = movSampleToAnnexB(packet.data, avcs[entry - 1])
            if (display) decoder.decodeForDisplay(bytes) else { decoder.decodeOnly(bytes); null }
        }
            catch (e: Exception) {
                normalizer = null
                physicalGopStart = null
                runEntry = -1
                throw IllegalStateException("表示frameの復号に失敗しました", e)
            }
        if (picture == null) return null
        val start = row.startUs ?: return null
        val end = row.endUs ?: return null
        return Decoded(ReplayPreviewImage.convert(picture), start, end, index).also { cache.put(cacheSource, it) }
    }

    private fun upperBound(values: LongArray, target: Long): Int {
        var low = 0
        var high = values.size
        while (low < high) {
            val mid = (low + high) ushr 1
            if (values[mid] <= target) low = mid + 1 else high = mid
        }
        return low
    }

    private fun upperBound(values: IntArray, target: Int): Int {
        var low = 0
        var high = values.size
        while (low < high) {
            val mid = (low + high) ushr 1
            if (values[mid] <= target) low = mid + 1 else high = mid
        }
        return low
    }

    override fun close() {
        if (closed) return
        closed = true
        runCatching { channel.close() }
        runCatching { file.close() }
    }
}

/** One movie-ordered piece of the player's audio: silence, or a run of AAC packets. */
internal sealed class AudioChunk {
    data class Silence(val samples: Long) : AudioChunk()
    /** Packets [first, first+count) decoded in order; drop [skipSamples] leading and keep at most [keepSamples]. */
    data class Packets(val first: Int, val count: Int, val skipSamples: Long, val keepSamples: Long) : AudioChunk()
}

/** [samples] AAC packets covering [mediaStart] (which may be inside the first packet) at [movieStartUs]. */
internal data class PlaybackAudioSegment(val movieStartUs: Long, val durationUs: Long, val mediaStart: Long, val samples: Int)

/**
 * Movie-ordered audio from [startUs] to the end: silence where no segment covers the movie and the
 * packets of each segment otherwise, clipped to the segment duration. Pure; for the player clock.
 */
internal fun audioSchedule(segments: List<PlaybackAudioSegment>, sampleRate: Int, startUs: Long): List<AudioChunk> {
    fun samplesOf(us: Long) = Math.floorDiv(us * sampleRate, 1_000_000L)
    val out = mutableListOf<AudioChunk>()
    var cursorUs = startUs
    segments.sortedBy { it.movieStartUs }.forEach { segment ->
        val segmentEnd = segment.movieStartUs + segment.durationUs
        if (segmentEnd <= cursorUs || segment.samples <= 0) return@forEach
        if (segment.movieStartUs > cursorUs) {
            out += AudioChunk.Silence(samplesOf(segment.movieStartUs - cursorUs))
            cursorUs = segment.movieStartUs
        }
        val leading = segment.mediaStart % AAC_PACKET_SAMPLES
        val elapsed = samplesOf(cursorUs - segment.movieStartUs)
        val skip = leading + elapsed
        val keep = (samplesOf(segmentEnd - segment.movieStartUs) - elapsed)
            .coerceAtMost(segment.samples * AAC_PACKET_SAMPLES - skip)
        if (keep <= 0) return@forEach
        val firstPacket = (skip / AAC_PACKET_SAMPLES).toInt()
        val lastPacket = ((skip + keep - 1) / AAC_PACKET_SAMPLES).toInt()
        out += AudioChunk.Packets((segment.mediaStart / AAC_PACKET_SAMPLES).toInt() + firstPacket,
            lastPacket - firstPacket + 1, skip - firstPacket * AAC_PACKET_SAMPLES, keep)
        cursorUs = segmentEnd
    }
    return out
}

internal const val AAC_PACKET_SAMPLES = 1024L

/** Decodes AAC-LC packets to interleaved 16-bit little-endian PCM with the JAAD decoder bundled in JCodec. */
internal class AacPcmDecoder(audioSpecificConfig: ByteArray) {
    private val decoder = Decoder(audioSpecificConfig)
    private val buffer = SampleBuffer().apply { isBigEndian = false }

    val sampleRate: Int get() = decoder.config.sampleFrequency.frequency
    val channels: Int get() = decoder.config.channelConfiguration.channelCount

    fun decode(packet: ByteArray): ByteArray {
        decoder.decodeFrame(packet, buffer)
        require(buffer.bitsPerSample == 16 && buffer.sampleRate == sampleRate && buffer.channels == channels) {
            "音声の形式に対応できません"
        }
        return buffer.data.copyOf()
    }
}

/**
 * The saved AAC track for playback: per-packet bytes read on demand through the demuxer, plus the
 * segment map from `audio_track`. Null when the save has no audio. Read-only; outside EDT.
 */
internal class ReplayAudioTrack private constructor(
    private val file: SaveOutputFile,
    private val channel: FileChannel,
    private val track: AbstractMP4DemuxerTrack,
    val config: ByteArray,
    val sampleRate: Int,
    val channels: Int,
    val segments: List<PlaybackAudioSegment>,
) : AutoCloseable {

    fun schedule(startUs: Long): List<AudioChunk> = audioSchedule(segments, sampleRate, startUs)

    /** Positions the demuxer so the next [packet] call returns packet [index]. */
    fun goTo(index: Int) = check(track.gotoFrame(index.toLong())) { "音声の位置へ移動できません" }

    fun packet(): ByteArray? = track.nextFrame()?.data?.let { data -> ByteArray(data.remaining()).also { data.duplicate().get(it) } }

    override fun close() {
        runCatching { channel.close() }
        runCatching { file.close() }
    }

    companion object {
        fun open(saved: SavedReplay,
                 openChannel: (SaveOutputFile) -> FileChannel = { it.channel(EnumSet.of(StandardOpenOption.READ)) }): ReplayAudioTrack? {
            val info = saved.manifest["audio_track"]?.takeIf { it.isJsonObject }?.asJsonObject ?: return null
            val sampleRate = info["sample_rate"]?.takeIf { it.isJsonPrimitive }?.asInt ?: return null
            val segments = info["segments"]?.takeIf { it.isJsonArray }?.asJsonArray?.map { it.asJsonObject }?.map {
                PlaybackAudioSegment(it.longText("movie_start_us") ?: return null, it.longText("duration_us") ?: return null,
                    it.longText("media_start_samples") ?: return null, it["samples"]?.asInt ?: return null)
            } ?: return null
            val file = saved.openFile(ReplayFiles.VIDEO)
            val channel = try { openChannel(file) }
            catch (failure: Throwable) { file.close(); throw failure }
            try {
                val demuxer = MP4Demuxer.createRawMP4Demuxer(FileChannelWrapper(channel))
                val track = demuxer.audioTracks.firstOrNull() as? AbstractMP4DemuxerTrack
                    ?: throw IllegalArgumentException("音声trackが見つかりません")
                val entry = track.sampleEntries.filterIsInstance<AudioSampleEntry>().firstOrNull()
                    ?: throw IllegalArgumentException("音声sample entryが見つかりません")
                val esds = NodeBox.findFirst(entry, EsdsBox::class.java, EsdsBox.fourcc())
                    ?: throw IllegalArgumentException("音声の設定（esds）が見つかりません")
                val info2 = esds.streamInfo.duplicate()
                val config = ByteArray(info2.remaining()).also { info2.get(it) }
                return ReplayAudioTrack(file, channel, track, config, sampleRate, entry.channelCount.toInt(), segments)
            } catch (failure: Throwable) {
                runCatching { channel.close() }; runCatching { file.close() }
                throw failure
            }
        }
    }
}
