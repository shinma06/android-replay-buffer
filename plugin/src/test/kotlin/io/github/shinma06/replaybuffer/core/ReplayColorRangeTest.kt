package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonParser
import org.jcodec.codecs.h264.H264Encoder
import org.jcodec.codecs.h264.H264Utils
import org.jcodec.codecs.h264.encode.DumbRateControl
import org.jcodec.codecs.h264.io.model.SeqParameterSet
import org.jcodec.codecs.h264.io.model.VUIParameters
import org.jcodec.common.io.NIOUtils
import org.jcodec.common.model.ColorSpace
import org.jcodec.common.model.Picture
import org.jcodec.common.model.Size
import org.jcodec.containers.mp4.boxes.VideoSampleEntry
import org.jcodec.containers.mp4.demuxer.AbstractMP4DemuxerTrack
import org.jcodec.containers.mp4.demuxer.MP4Demuxer
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.awt.image.BufferedImage
import java.nio.ByteBuffer
import java.nio.file.Files
import java.nio.file.Path
import javax.imageio.ImageIO
import kotlin.math.abs
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class ReplayColorRangeTest {
    @TempDir lateinit var root: Path
    private val colors = listOf(0x000000, 0xffffff, 0x404040, 0x808080, 0xc0c0c0, 0xff0000, 0x00ff00, 0x0000ff)

    @Test
    fun checkedInTrimParityKeepsItsExplicitFullRangeSps() {
        val dir = Files.createDirectory(root.resolve("parity"))
        javaClass.getResourceAsStream("/replay-trim-parity/replay-20240101-000000-manual-feedface/video.mp4")!!.use {
            Files.copy(it, dir.resolve("video.mp4"))
        }
        assertRange(dir, "full")
    }

    @Test
    fun explicitAndDefaultRangesShowKnownColorsInPngAndPreview() {
        for (range in listOf("limited", "full", "default")) {
            val source = saveFixture(range)
            assertRange(source, range)
            assertPngAndPreview(source, 0)
            assertPngAndPreview(source, 500_000)
        }
    }

    @Test
    fun leadingGopReencodeAndRepeatedTrimPreserveRangeAndKnownColors() {
        for (range in listOf("limited", "full", "default")) {
            val source = saveFixture(range)
            val trimmed = trim(source, 100_001, 900_000)
            val rows = Files.readAllLines(trimmed.resolve("video-frames.jsonl")).map { JsonParser.parseString(it).asJsonObject }
            assertTrue(rows.first()["reencoded"].asBoolean)
            assertTrue(rows.any { !it["reencoded"].asBoolean })
            assertRange(trimmed, range)
            assertPngAndPreview(trimmed, 0)
            assertPngAndPreview(trimmed, 400_000)
            val repeated = trim(trimmed, 100_001, 700_000)
            assertRange(repeated, range)
            assertPngAndPreview(repeated, 0)
            assertPngAndPreview(repeated, 400_000)
        }
    }

    @Test
    fun jcodecPlaneLabelDoesNotOverrideAbsentOrExplicitSpsRange() {
        for (full in listOf(null, false, true)) {
            val encoder = object : H264Encoder(DumbRateControl()) {
                override fun initSPS(size: Size): SeqParameterSet = super.initSPS(size).apply {
                    if (full != null) vuiParams = VUIParameters().apply {
                        videoSignalTypePresentFlag = true; videoFormat = 5; videoFullRangeFlag = full
                    }
                }
            }
            val picture = Picture.create(96, 32, ColorSpace.YUV420J)
            val lumas = if (full == true) listOf(0, 128, 255) else listOf(16, 126, 235)
            for (y in 0 until 32) for (x in 0 until 96)
                picture.getPlaneData(0)[y * 96 + x] = (lumas[x / 32] - 128).toByte()
            val encoded = encoder.encodeFrame(picture, ByteBuffer.allocate(96 * 32 * 3)).data
            val bytes = ByteArray(encoded.remaining()).also { encoded.get(it) }
            val config = config(bytes)
            val sps = H264Utils.readSPS(H264Utils.getRawSPS(ByteBuffer.wrap(config)).single())
            if (full == null) assertEquals(null, sps.vuiParams)
            val decoder = MixedVideoNormalizer(listOf(config), 100_000).apply { startRun(config) }
            val decoded = decoder.decodeOnly(bytes)
            assertEquals(if (full == true) ColorSpace.YUV420J else ColorSpace.YUV420, decoded.color)
            val image = ReplayFrameExport.rgbImage(decoded)
            listOf(0x000000, 0x808080, 0xffffff).forEachIndexed { n, rgb -> assertColor(rgb, image.getRGB(n * 32 + 16, 16), 2) }
        }
    }

    private fun config(bytes: ByteArray): ByteArray = H264Utils.splitFrame(ByteBuffer.wrap(bytes))
        .filter { (it.get(0).toInt() and 31) in setOf(7, 8) }
        .fold(byteArrayOf()) { out, nal -> out + byteArrayOf(0, 0, 0, 1) + ByteArray(nal.remaining()).also { nal.get(it) } }

    private fun saveFixture(range: String): Path {
        val bytes = javaClass.getResourceAsStream("/replay-color-range/$range.h264")!!.use { it.readBytes() }
        val packets = mutableListOf<ByteArray>()
        var packet = byteArrayOf()
        for (nal in H264Utils.splitFrame(ByteBuffer.wrap(bytes))) {
            if ((nal.get(0).toInt() and 31) == 9 && packet.isNotEmpty()) {
                packets += packet; packet = byteArrayOf()
            }
            packet += byteArrayOf(0, 0, 0, 1) + ByteArray(nal.remaining()).also { nal.get(it) }
        }
        if (packet.isNotEmpty()) packets += packet
        assertEquals(12, packets.size)
        val dir = Files.createDirectory(root.resolve(range))
        CaptureStore(dir.resolve("ring"), minFree = 0).use { store ->
            store.generation(1)
            store.session(VideoPacket.Session(256, 32), 1)
            store.frame(VideoPacket.Frame(0, true, false, config(packets.first())), 1)
            packets.forEachIndexed { n, data ->
                assertTrue(store.frame(VideoPacket.Frame(n * 100_000L, false, H264Utils.isByteBufferIDRSlice(ByteBuffer.wrap(data)), data), 1))
            }
            return SaveWriter().write(store.capture(ReplaySettings())!!, dir, { false }) {
                Files.move(it.partial, it.complete)
            }.directory
        }
    }

    private fun trim(source: Path, from: Long, to: Long): Path = ReplayTrim().trim(source, from, to, { false }, {
        Files.move(it.partial, it.complete)
    })

    private fun assertRange(dir: Path, range: String) {
        NIOUtils.readableChannel(dir.resolve("video.mp4").toFile()).use { channel ->
            MP4Demuxer.createRawMP4Demuxer(channel).use { demux ->
                for (entry in (demux.videoTrack as AbstractMP4DemuxerTrack).sampleEntries) {
                    for (raw in H264Utils.parseAVCC(entry as VideoSampleEntry).spsList) {
                        val vui = H264Utils.readSPS(raw.duplicate()).vuiParams
                        assertEquals(range != "default", vui?.videoSignalTypePresentFlag == true)
                        assertEquals(range == "full", vui?.videoSignalTypePresentFlag == true && vui.videoFullRangeFlag)
                    }
                }
            }
        }
    }

    private fun assertPngAndPreview(dir: Path, atUs: Long) {
        val png = ReplayFrameExport.export(dir, atUs, root)
        val exported = ImageIO.read(png.toFile())
        SavedReplay.open(dir).use { saved ->
            ReplayFrameCursor(saved).use { cursor ->
                val preview = cursor.seek(atUs).image
                assertImageColors(exported)
                assertImageColors(preview)
                assertTrue(exported.getRGB(0, 0, 256, 32, null, 0, 256)
                    .contentEquals(preview.getRGB(0, 0, 256, 32, null, 0, 256)))
                assertImageColors(cursor.next()!!.image)
            }
        }
    }

    private fun assertImageColors(image: BufferedImage) {
        assertEquals(256, image.width); assertEquals(32, image.height)
        assertEquals(0x000000, image.getRGB(16, 16) and 0xffffff)
        assertEquals(0xffffff, image.getRGB(48, 16) and 0xffffff)
        colors.forEachIndexed { n, rgb -> assertColor(rgb, image.getRGB(n * 32 + 16, 16), 3) }
    }

    private fun assertColor(expected: Int, actual: Int, tolerance: Int) {
        for (shift in listOf(16, 8, 0)) assertTrue(abs(((expected shr shift) and 255) - ((actual shr shift) and 255)) <= tolerance,
            "expected=${expected.toString(16)}, actual=${(actual and 0xffffff).toString(16)}, channel=$shift")
    }
}
