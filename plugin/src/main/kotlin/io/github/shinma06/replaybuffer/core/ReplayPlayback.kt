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
internal class ReplayFrameCursor(private val saved: SavedReplay) : AutoCloseable {
    class Decoded(val image: BufferedImage, val startUs: Long, val endUs: Long, val index: Int)

    private val rows = saved.frames
    private val file = saved.openFile(ReplayFiles.VIDEO)
    private val channel: FileChannel = file.channel(EnumSet.of(StandardOpenOption.READ))
    private val track: AbstractMP4DemuxerTrack
    private val avcs: List<AvcCBox>
    private var nextIndex = 0
    private var normalizer: MixedVideoNormalizer? = null
    private var runEntry = -1

    init {
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

    private fun span(index: Int): Pair<Long, Long>? {
        val pts = rows[index].longText("movie_pts_us") ?: return null
        val end = rows[index].longText("movie_end_us") ?: return null
        return pts to end
    }

    private fun isKey(index: Int) = rows[index]["output_key"]?.takeIf { it.isJsonPrimitive }?.asBoolean == true

    /** Decodes from the reference IDR up to the frame displayed at [atUs] and returns it. */
    fun seek(atUs: Long, checkActive: () -> Unit = {}): Decoded {
        val target = rows.indices.firstOrNull { i -> span(i)?.let { atUs in it.first until it.second } == true }
            ?: throw IllegalArgumentException("指定時刻に表示されるframeがありません")
        val gopStart = (target downTo 0).firstOrNull {
            isKey(it) && rows[it]["segment_index"]?.asInt == rows[target]["segment_index"].asInt
        } ?: throw IllegalArgumentException("表示frameの参照IDRがありません")
        check(track.gotoFrame(gopStart.toLong())) { "動画の位置へ移動できません" }
        nextIndex = gopStart
        normalizer = null
        var last: Decoded? = null
        while (nextIndex <= target) { checkActive(); last = decodeNext() }
        return last ?: throw IllegalArgumentException("指定時刻のframeを復号できません")
    }

    /** The next frame with a movie span after the last decoded one, or null at the end of the movie. */
    fun next(checkActive: () -> Unit = {}): Decoded? {
        while (nextIndex <= rows.lastIndex) {
            checkActive()
            val decoded = decodeNext() ?: continue
            return decoded
        }
        return null
    }

    private fun decodeNext(): Decoded? {
        val index = nextIndex
        val packet = track.nextFrame() ?: throw IllegalStateException("動画のsampleが足りません")
        nextIndex = index + 1
        val entry = rows[index]["sample_entry"]?.asInt ?: throw IllegalArgumentException("frame記録を読めません")
        if (isKey(index)) {
            val config = annexBConfig(avcs[entry - 1])
            normalizer = try {
                MixedVideoNormalizer(listOf(config), rows[index].longText("source_duration_us")).apply { startRun(config) }
            } catch (e: IllegalArgumentException) {
                throw IllegalStateException("表示frameの復号に対応できません", e)
            }
            runEntry = entry
        }
        val decoder = normalizer ?: throw IllegalStateException("参照IDRより前から復号しようとしました")
        require(entry == runEntry) { "表示frameのsample構成が読めません" }
        val picture = try { decoder.decodeForDisplay(movSampleToAnnexB(packet.data, avcs[entry - 1])) }
            catch (e: Exception) { throw IllegalStateException("表示frameの復号に失敗しました", e) }
        val (start, end) = span(index) ?: return null
        return Decoded(ReplayFrameExport.rgbImage(picture), start, end, index)
    }

    override fun close() {
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
        fun open(saved: SavedReplay): ReplayAudioTrack? {
            val info = saved.manifest["audio_track"]?.takeIf { it.isJsonObject }?.asJsonObject ?: return null
            val sampleRate = info["sample_rate"]?.takeIf { it.isJsonPrimitive }?.asInt ?: return null
            val segments = info["segments"]?.takeIf { it.isJsonArray }?.asJsonArray?.map { it.asJsonObject }?.map {
                PlaybackAudioSegment(it.longText("movie_start_us") ?: return null, it.longText("duration_us") ?: return null,
                    it.longText("media_start_samples") ?: return null, it["samples"]?.asInt ?: return null)
            } ?: return null
            val file = saved.openFile(ReplayFiles.VIDEO)
            val channel = file.channel(EnumSet.of(StandardOpenOption.READ))
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
