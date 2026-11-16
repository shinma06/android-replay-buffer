package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonParser
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
import java.nio.file.Files
import java.nio.file.Path
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

    private fun fixture(): Path {
        CaptureStore(root.resolve("ring"), minFree = 0).use { store ->
            store.generation(1)
            store.session(VideoPacket.Session(32, 32), 1)
            val encoder = H264Encoder.createH264Encoder().apply { setKeyInterval(8) }
            repeat(40) { n ->
                val encoded = encoder.encodeFrame(Picture.create(32, 32, ColorSpace.YUV420J).apply { fill(20 + n) },
                    ByteBuffer.allocate(32 * 32 * 6)).data
                val bytes = ByteArray(encoded.remaining()).also(encoded::get)
                if (n == 0) {
                    val config = H264Utils.splitFrame(ByteBuffer.wrap(bytes)).filter { it.get(0).toInt() and 31 in setOf(7, 8) }
                        .fold(byteArrayOf()) { all, nal -> all + byteArrayOf(0, 0, 0, 1) + ByteArray(nal.remaining()).also(nal::get) }
                    store.frame(VideoPacket.Frame(0, true, false, config), 1)
                }
                assertTrue(store.frame(VideoPacket.Frame(n * 100_000L, false, n % 8 == 0, bytes), 1))
            }
            return SaveWriter().write(store.capture(ReplaySettings())!!, root, { false },
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
}
