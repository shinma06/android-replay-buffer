package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonParser
import org.jcodec.codecs.h264.H264Decoder
import org.jcodec.codecs.h264.H264Encoder
import org.jcodec.codecs.h264.H264Utils
import org.jcodec.common.io.FileChannelWrapper
import org.jcodec.common.io.NIOUtils
import org.jcodec.common.io.SeekableByteChannel
import org.jcodec.common.model.ColorSpace
import org.jcodec.common.model.Picture
import org.jcodec.containers.mp4.MP4Packet
import org.jcodec.containers.mp4.boxes.VideoSampleEntry
import org.jcodec.containers.mp4.demuxer.AbstractMP4DemuxerTrack
import org.jcodec.containers.mp4.demuxer.MP4Demuxer
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.ByteBuffer
import java.nio.channels.FileChannel
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardOpenOption.WRITE
import java.security.MessageDigest
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class ReplayTrimVideoReadTest {
    @TempDir lateinit var root: Path

    private data class Sample(val frame: Long, val offset: Long, val bytes: ByteArray, val idr: Boolean)

    private class Reads(private val input: SeekableByteChannel, private val samples: List<Sample>) : SeekableByteChannel by input {
        val frames = mutableListOf<Long>()
        var bytes = 0L
        private var active: Sample? = null
        override fun read(dst: ByteBuffer): Int {
            samples.firstOrNull { it.offset == input.position() && it.bytes.size == dst.remaining() }?.let {
                active = it
                frames += it.frame
            }
            val count = input.read(dst)
            active?.let { sample ->
                bytes += count
                if (input.position() == sample.offset + sample.bytes.size) active = null
            }
            return count
        }
    }

    private fun samples(source: Path): List<Sample> = NIOUtils.readableChannel(source.resolve(ReplayFiles.VIDEO).toFile()).use { input ->
        val track = MP4Demuxer.createRawMP4Demuxer(input).videoTrack as AbstractMP4DemuxerTrack
        buildList {
            while (true) {
                val packet = track.nextFrame() as? MP4Packet ?: break
                val avcc = H264Utils.parseAVCC(track.sampleEntries[packet.entryNo] as VideoSampleEntry)
                val idr = H264Utils.isByteBufferIDRSlice(ByteBuffer.wrap(movSampleToAnnexB(packet.data, avcc)))
                add(Sample(packet.frameNo, packet.fileOff, ByteArray(packet.data.remaining()).also(packet.data::get), idr))
            }
        }
    }

    private fun fixture(normalized: Boolean = false): Path {
        CaptureStore(root.resolve("ring"), minFree = 0).use { store ->
            val sizes = if (normalized) listOf(64 to 32, 32 to 64) else listOf(32 to 32)
            sizes.forEachIndexed { run, (width, height) ->
                val generation = run + 1L
                store.generation(generation)
                store.session(VideoPacket.Session(width, height), generation)
                val encoder = H264Encoder.createH264Encoder().apply { setKeyInterval(8) }
                repeat(40) { n ->
                    val encoded = encoder.encodeFrame(Picture.create(width, height, ColorSpace.YUV420J).apply { fill(20 + n) },
                        ByteBuffer.allocate(width * height * 6)).data
                    val bytes = ByteArray(encoded.remaining()).also(encoded::get)
                    if (n == 0) {
                        val config = H264Utils.splitFrame(ByteBuffer.wrap(bytes)).filter { it.get(0).toInt() and 31 in setOf(7, 8) }
                            .fold(byteArrayOf()) { all, nal -> all + byteArrayOf(0, 0, 0, 1) + ByteArray(nal.remaining()).also(nal::get) }
                        store.frame(VideoPacket.Frame(0, true, false, config), generation)
                    }
                    assertTrue(store.frame(VideoPacket.Frame(run * 5_000_000L + n * 100_000L, false, n % 8 == 0, bytes), generation))
                }
            }
            val capture = store.capture(ReplaySettings())!!
            val input = if (normalized) capture.copy(start = 0, end = 9_000_000_000, endUncertainty = 0,
                states = emptyMap(), video = capture.video.map { frame ->
                    val t = frame.pts * 1000
                    frame.copy(time = MappedTime(t, 0, 0, t))
                }) else capture
            return SaveWriter().write(input, root, { false },
                { Files.move(it.partial, it.complete) }).directory
        }
    }

    private fun hashes(source: Path): Map<String, String> = Files.list(source).use { files ->
        files.toList().associate { path -> path.fileName.toString() to
            MessageDigest.getInstance("SHA-256").digest(Files.readAllBytes(path)).joinToString("") { "%02x".format(it) } }
    }

    private fun trim(source: Path, from: Int, until: Int): Pair<Path, Reads> {
        val original = samples(source)
        lateinit var reads: Reads
        val output = ReplayTrim(mp4Input = { Reads(FileChannelWrapper(it), original).also { reads = it } }).trim(
            source, from * 100_000L, until * 100_000L, { false },
            { Files.move(it.partial, it.complete) })
        return output to reads
    }

    @Test
    fun shortHeadTrimDoesNotReadTheUnusedTail() {
        val source = fixture()
        val before = hashes(source)
        val original = samples(source)
        val (output, reads) = trim(source, 0, 3)
        assertEquals(listOf(0L, 1L, 2L), reads.frames)
        assertEquals(original.take(3).sumOf { it.bytes.size.toLong() }, reads.bytes)
        samples(output).zip(original).forEach { (actual, expected) -> assertContentEquals(expected.bytes, actual.bytes) }
        assertEquals(before, hashes(source))
    }

    @Test
    fun middleTrimSeeksToItsLeadingGopThenCopiesLaterPacketsWithoutReadingEitherTail() {
        val source = fixture()
        val before = hashes(source)
        val original = samples(source)
        val (output, reads) = trim(source, 11, 21)
        assertEquals((8L..20L).toList(), reads.frames)
        assertEquals(original.subList(8, 21).sumOf { it.bytes.size.toLong() }, reads.bytes)
        val result = samples(output)
        assertEquals(10, result.size)
        assertTrue(result.first().idr)
        for (n in 16..20) assertContentEquals(original[n].bytes, result[n - 11].bytes)
        val rows = Files.readAllLines(output.resolve(ReplayFiles.VIDEO_FRAMES)).map { JsonParser.parseString(it).asJsonObject }
        assertEquals((0L..9L).toList(), rows.map { it["sample_index"].asLong })
        assertEquals((0L..9L).map { it * 100_000 }, rows.map { it["movie_pts_us"].asLong })
        assertEquals(List(5) { true } + List(5) { false }, rows.map { it["reencoded"].asBoolean })
        SavedReplay.open(output).use { assertEquals(4, it.manifest["schema"].asInt) }
        assertEquals(before, hashes(source))
    }

    private data class Video(val pts: List<Long>, val durations: List<Long>, val keys: List<Boolean>,
        val configs: List<ByteArray>, val pixels: List<List<ByteArray>>, val edits: List<Pair<Long, Long>>)

    /** Cold decode from each actual IDR using only its stored sample entry, without source preroll. */
    private fun video(source: Path): Video = NIOUtils.readableChannel(source.resolve(ReplayFiles.VIDEO).toFile()).use { input ->
        val track = MP4Demuxer.createRawMP4Demuxer(input).videoTrack as AbstractMP4DemuxerTrack
        val pts = mutableListOf<Long>()
        val durations = mutableListOf<Long>()
        val keys = mutableListOf<Boolean>()
        val configs = mutableListOf<ByteArray>()
        val pixels = mutableListOf<List<ByteArray>>()
        var decoder: H264Decoder? = null
        while (true) {
            val packet = track.nextFrame() as? MP4Packet ?: break
            val entry = track.sampleEntries[packet.entryNo] as VideoSampleEntry
            val avcc = H264Utils.parseAVCC(entry)
            assertTrue(avcc.spsList.isNotEmpty() && avcc.ppsList.isNotEmpty())
            val config = ByteBuffer.allocate(avcc.estimateSize()).also(avcc::write).apply { flip() }
            configs += ByteArray(config.remaining()).also(config::get)
            if (packet.isKeyFrame) decoder = H264Decoder().apply { addSps(avcc.spsList); addPps(avcc.ppsList) }
            val planes = Picture.create((entry.width + 15) and -16, (entry.height + 15) and -16, ColorSpace.YUV420J).data
            pixels += requireNotNull(decoder).decodeFrame(ByteBuffer.wrap(movSampleToAnnexB(packet.data, avcc)), planes)
                .cloneCropped().data.map { it.copyOf() }
            assertEquals(1_000_000, packet.timescale)
            pts += packet.pts
            durations += packet.duration
            keys += packet.isKeyFrame
        }
        Video(pts, durations, keys, configs, pixels, track.edits.map { it.duration to it.mediaTime })
    }

    private fun rows(source: Path) = Files.readAllLines(source.resolve(ReplayFiles.VIDEO_FRAMES))
        .map { JsonParser.parseString(it).asJsonObject }

    private fun assertIdrCopy(source: Path, first: Int, end: Int, insideUs: Long = 0): Path {
        val before = hashes(source)
        val original = samples(source)
        val originalVideo = video(source)
        val selected = rows(source).subList(first, end)
        assertTrue(original[first].idr)
        assertTrue(selected.first()["output_key"].asBoolean)
        assertTrue(selected.all { it["segment_index"] == selected.first()["segment_index"] })
        val fromUs = selected.first()["movie_pts_us"].asLong
        val untilUs = selected.last()["movie_end_us"].asLong
        lateinit var reads: Reads
        val output = ReplayTrim(mp4Input = { Reads(FileChannelWrapper(it), original).also { reads = it } }).trim(
            source, fromUs + insideUs, untilUs, { false }, { Files.move(it.partial, it.complete) })
        SavedReplay.open(output).use { saved ->
            assertEquals(4, saved.manifest["schema"].asInt)
            assertTrue(saved.manifest["complete"].asBoolean)
            assertFalse(saved.manifest["trim"].asJsonObject["reencoded_leading_gop"].asBoolean)
            val part = saved.manifest["parts"].asJsonArray.single().asJsonObject
            assertEquals(0, part["preroll_samples"].asInt)
            assertEquals("0", part["movie_start_us"].asString)
            assertEquals((untilUs - fromUs).toString(), part["duration_us"].asString)
            if (!selected.first()["t_ns"].isJsonNull) assertEquals("0", part["t_start_ns"].asString)
        }
        assertEquals((first.toLong() until end.toLong()).toList(), reads.frames)
        assertEquals(original.subList(first, end).sumOf { it.bytes.size.toLong() }, reads.bytes)
        val actual = samples(output)
        assertEquals(end - first, actual.size)
        assertTrue(actual.first().idr)
        actual.forEachIndexed { i, sample -> assertContentEquals(original[first + i].bytes, sample.bytes) }
        val result = video(output)
        assertEquals(originalVideo.pts.subList(first, end).map { it - originalVideo.pts[first] }, result.pts)
        assertEquals(originalVideo.durations.subList(first, end), result.durations)
        assertEquals(originalVideo.keys.subList(first, end), result.keys)
        assertEquals(listOf((untilUs - fromUs) to 0L), result.edits)
        selected.indices.forEach { i ->
            assertContentEquals(originalVideo.configs[first + i], result.configs[i]) // Full avcC: SPS/PPS, profile, level, NAL length.
            originalVideo.pixels[first + i].indices.forEach { plane ->
                assertContentEquals(originalVideo.pixels[first + i][plane], result.pixels[i][plane])
            }
        }
        val outputRows = rows(output)
        val stable = listOf("source_pts_us", "source_duration_us", "display_duration_us", "packet_ordinal", "decode_run",
            "source_key", "output_key", "reencoded", "source_rect", "output_width", "output_height", "source_colour_known")
        outputRows.forEachIndexed { i, row ->
            stable.forEach { field -> assertEquals(selected[i][field], row[field], field) }
            assertEquals((selected[i]["movie_pts_us"].asLong - fromUs).toString(), row["movie_pts_us"].asString)
            assertEquals((selected[i]["movie_end_us"].asLong - fromUs).toString(), row["movie_end_us"].asString)
            assertEquals(i.toLong(), row["sample_index"].asLong)
        }
        if (Files.exists(source.resolve(ReplayFiles.VIDEO_SOURCE))) {
            val oldRaw = Files.readAllBytes(source.resolve(ReplayFiles.VIDEO_SOURCE))
            val newRaw = Files.readAllBytes(output.resolve(ReplayFiles.VIDEO_SOURCE))
            outputRows.forEachIndexed { i, row ->
                for ((offset, size) in listOf("source_offset" to "source_size", "source_config_offset" to "source_config_size")) {
                    val start = selected[i][offset].asInt
                    val actualStart = row[offset].asInt
                    assertContentEquals(oldRaw.copyOfRange(start, start + selected[i][size].asInt),
                        newRaw.copyOfRange(actualStart, actualStart + row[size].asInt))
                }
            }
        }
        assertEquals(before, hashes(source))
        return output
    }

    @Test
    fun secondIdrCopiesPacketsConfigAndColdPixelsWithoutReencoding() {
        val source = fixture()
        assertTrue(rows(source).all { !it["reencoded"].asBoolean })
        assertEquals(listOf(0L, 8L, 16L, 24L, 32L), samples(source).filter { it.idr }.map { it.frame })
        for (insideUs in listOf(0L, 1L)) {
            val output = assertIdrCopy(source, 8, 21, insideUs)
            assertIdrCopy(output, 8, 13, insideUs)
        }
    }

    @Test
    fun thirdIdrCopiesPacketsConfigAndColdPixelsWithoutReencoding() {
        val source = fixture()
        assertTrue(rows(source).all { !it["reencoded"].asBoolean })
        for (insideUs in listOf(0L, 1L)) assertIdrCopy(source, 16, 21, insideUs)
    }

    @Test
    fun laterNormalizedIdrKeepsItsRawPlacementAndReencodingHistory() {
        val source = fixture(normalized = true)
        val sourceRows = rows(source)
        assertTrue(sourceRows.all { it["reencoded"].asBoolean })
        val first = sourceRows.indexOfFirst { it["sample_index"].asInt > 0 && it["segment_index"].asInt == 0 && it["output_key"].asBoolean }
        assertTrue(first > 0)
        assertTrue(sourceRows[first]["source_rect"].isJsonObject)
        assertEquals(64, sourceRows[first]["output_width"].asInt)
        assertEquals(64, sourceRows[first]["output_height"].asInt)
        val output = assertIdrCopy(source, first, first + 5, 1)
        assertIdrCopy(output, 0, 5)
    }

    @Test
    fun copyHeadRejectsAKeyFlagWhoseNalIsNotAnIdr() {
        val source = fixture()
        val selected = samples(source)[8]
        assertTrue(selected.idr)
        assertTrue(rows(source)[8]["output_key"].asBoolean)
        val bytes = selected.bytes.copyOf()
        NIOUtils.readableChannel(source.resolve(ReplayFiles.VIDEO).toFile()).use { input ->
            val track = MP4Demuxer.createRawMP4Demuxer(input).videoTrack as AbstractMP4DemuxerTrack
            assertTrue(track.gotoFrame(8))
            val packet = track.nextFrame() as MP4Packet
            assertTrue(packet.isKeyFrame) // Keep both the MP4 sync flag and output_key=true.
            val avcc = H264Utils.parseAVCC(track.sampleEntries[packet.entryNo] as VideoSampleEntry)
            assertEquals(4, avcc.nalLengthSize)
            assertEquals(5, H264Utils.splitMOVPacket(ByteBuffer.wrap(bytes), avcc).single().run { get(position()).toInt() and 31 })
            bytes[4] = ((bytes[4].toInt() and 0xe0) or 1).toByte()
            assertFalse(H264Utils.isByteBufferIDRSlice(ByteBuffer.wrap(movSampleToAnnexB(ByteBuffer.wrap(bytes), avcc))))
        }
        FileChannel.open(source.resolve(ReplayFiles.VIDEO), WRITE).use { channel ->
            channel.position(selected.offset)
            val data = ByteBuffer.wrap(bytes)
            while (data.hasRemaining()) channel.write(data)
        }
        // This is a deliberately malformed synthetic save with valid file hashes, not a source race.
        val record = JsonParser.parseString(Files.readString(source.resolve(ReplayFiles.RECORD))).asJsonObject
        record["files_sha256"].asJsonObject.addProperty(ReplayFiles.VIDEO,
            MessageDigest.getInstance("SHA-256").digest(Files.readAllBytes(source.resolve(ReplayFiles.VIDEO)))
                .joinToString("") { "%02x".format(it) })
        Files.writeString(source.resolve(ReplayFiles.RECORD), record.toString())
        val before = hashes(source)
        var published = false
        val error = assertFailsWith<SaveFailure> {
            ReplayTrim().trim(source, 800_000, 2_100_000, { false }, { published = true })
        }
        assertEquals("切り取り先頭のcopy sampleがIDRではありません", error.message)
        assertFalse(published)
        assertEquals(before, hashes(source))
        Files.list(root).use { folders ->
            assertEquals(listOf(source), folders.filter { Files.isDirectory(it) && it.fileName.toString().startsWith("replay-") }.toList())
        }
    }

    private fun cancel(from: Int, until: Int, expected: List<Long>) {
        val source = fixture()
        val before = hashes(source)
        val original = samples(source)
        var observed: Reads? = null
        var published = false
        assertFailsWith<SaveCancelled> {
            ReplayTrim(mp4Input = { Reads(FileChannelWrapper(it), original).also { observed = it } }).trim(
                source, from * 100_000L, until * 100_000L,
                { (observed?.frames?.size ?: 0) >= 3 }, { published = true })
        }
        val reads = requireNotNull(observed)
        assertEquals(expected, reads.frames)
        assertEquals(original.filter { it.frame in expected }.sumOf { it.bytes.size.toLong() }, reads.bytes)
        assertFalse(published)
        assertEquals(before, hashes(source))
    }

    @Test
    fun cancellationDuringLeadingGopStopsBeforeAnotherPacketRead() = cancel(11, 21, listOf(8, 9, 10))

    @Test
    fun cancellationAtTheLastKeptPacketDoesNotDrainTheRestOfTheSource() = cancel(0, 3, listOf(0, 1, 2))

    @Test
    fun cancellationDuringLaterIdrCopyDoesNotPublishOrReadAnotherPacket() = cancel(8, 21, listOf(8, 9, 10))
}
