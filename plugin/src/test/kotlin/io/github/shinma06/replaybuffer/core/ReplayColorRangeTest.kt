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
import org.jcodec.scale.Yuv420jToRgb
import org.jcodec.scale.Yuv420pToRgb
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.awt.image.BufferedImage
import java.nio.ByteBuffer
import java.nio.file.Files
import java.nio.file.Path
import javax.imageio.ImageIO
import kotlin.math.abs
import kotlin.math.roundToInt
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class ReplayColorRangeTest {
    @TempDir lateinit var root: Path
    private val colors = listOf(0x000000, 0xffffff, 0x404040, 0x808080, 0xc0c0c0, 0xff0000, 0x00ff00, 0x0000ff)

    @Test
    fun explicitBt709BothRangesMatchIndependentColourVectors() {
        for (full in listOf(false, true)) {
            val picture = Picture.create(colors.size * 2, 2, if (full) ColorSpace.YUV420J else ColorSpace.YUV420)
            colors.forEachIndexed { n, rgb ->
                val r = ((rgb shr 16) and 255).toDouble()
                val g = ((rgb shr 8) and 255).toDouble()
                val b = (rgb and 255).toDouble()
                val level = 0.2126 * r + 0.7152 * g + 0.0722 * b
                val y = if (full) level else 16 + level * 219 / 255
                val scale = if (full) 1.0 else 224.0 / 255
                val cb = 128 + (b - level) / (2 * (1 - 0.0722)) * scale
                val cr = 128 + (r - level) / (2 * (1 - 0.2126)) * scale
                for (row in 0..1) for (col in 0..1)
                    picture.getPlaneData(0)[row * picture.width + n * 2 + col] = (y.roundToInt().coerceIn(0, 255) - 128).toByte()
                picture.getPlaneData(1)[n] = (cb.roundToInt().coerceIn(0, 255) - 128).toByte()
                picture.getPlaneData(2)[n] = (cr.roundToInt().coerceIn(0, 255) - 128).toByte()
            }
            val image = ReplayFrameExport.rgbImage(MixedVideoNormalizer.DisplayFrame(picture, 1))
            colors.forEachIndexed { n, expected -> assertColor(expected, image.getRGB(n * 2, 0), 2) }
        }
    }

    @Test
    fun unspecifiedAndOtherMatricesKeepLegacyConversionBytes() {
        for (full in listOf(false, true)) {
            val picture = Picture.create(4, 4, if (full) ColorSpace.YUV420J else ColorSpace.YUV420)
            picture.data.forEachIndexed { plane, bytes -> bytes.indices.forEach { bytes[it] = (it * 53 + plane * 31).toByte() } }
            val expected = Picture.create(4, 4, ColorSpace.RGB)
            if (full) Yuv420jToRgb().transform(picture, expected) else Yuv420pToRgb().transform(picture, expected)
            val unsigned = expected.getPlaneData(0).map { (it.toInt() + 128).toByte() }.toByteArray()
            for (matrix in listOf(null, 2, 4, 5, 6, 7, 8, 9, 10)) {
                val image = ReplayFrameExport.rgbImage(MixedVideoNormalizer.DisplayFrame(picture, matrix))
                assertContentEquals(unsigned, (image.raster.dataBuffer as java.awt.image.DataBufferByte).data)
            }
        }
    }

    @Test
    fun bt709ClipsExtremeSamplesUsingTheDeclaredRange() {
        for (full in listOf(false, true)) for (y in listOf(0, 16, 128, 235, 255))
            for (cb in listOf(0, 128, 255)) for (cr in listOf(0, 128, 255)) {
                val picture = Picture.create(2, 2, if (full) ColorSpace.YUV420J else ColorSpace.YUV420)
                picture.getPlaneData(0).fill((y - 128).toByte())
                picture.getPlaneData(1).fill((cb - 128).toByte())
                picture.getPlaneData(2).fill((cr - 128).toByte())
                val level = if (full) y.toDouble() else (y - 16) * 255.0 / 219
                val u = (cb - 128) * if (full) 1.0 else 255.0 / 224
                val v = (cr - 128) * if (full) 1.0 else 255.0 / 224
                val r = level + 2 * (1 - 0.2126) * v
                val b = level + 2 * (1 - 0.0722) * u
                val g = (level - 0.2126 * r - 0.0722 * b) / 0.7152
                val expected = (r.roundToInt().coerceIn(0, 255) shl 16) or
                    (g.roundToInt().coerceIn(0, 255) shl 8) or b.roundToInt().coerceIn(0, 255)
                val image = ReplayFrameExport.rgbImage(MixedVideoNormalizer.DisplayFrame(picture, 1))
                for (row in 0..1) for (col in 0..1) assertColor(expected, image.getRGB(col, row), 1)
            }
    }

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
            val decoded = decoder.decodeForDisplay(bytes)
            assertEquals(if (full == true) ColorSpace.YUV420J else ColorSpace.YUV420, decoded.picture.color)
            assertEquals(null, decoded.matrixCoefficients)
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
