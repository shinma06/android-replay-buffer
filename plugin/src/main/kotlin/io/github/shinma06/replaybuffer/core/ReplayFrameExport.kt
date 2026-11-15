package io.github.shinma06.replaybuffer.core

import com.google.gson.Gson
import org.jcodec.codecs.h264.H264Utils
import org.jcodec.common.model.ColorSpace
import org.jcodec.common.model.Picture
import org.jcodec.containers.mp4.boxes.VideoSampleEntry
import org.jcodec.containers.mp4.demuxer.MP4Demuxer
import org.jcodec.scale.Yuv420jToRgb
import java.awt.image.BufferedImage
import java.awt.image.ComponentColorModel
import java.awt.image.DataBufferByte
import java.awt.image.Raster
import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import java.nio.file.Path
import java.time.ZonedDateTime
import java.util.zip.CRC32
import javax.imageio.ImageIO

/**
 * Decodes the displayed frame at a movie position of a completed schema-4 save and writes it beside
 * screenshots as `frame-<time>-<id>.png` with tEXt time metadata. The source folder is read-only;
 * the image never overwrites an existing file. Runs outside EDT. [export]'s checkActive may abort only
 * while decoding, before any output exists.
 */
internal object ReplayFrameExport {

    private val gson = Gson()

    class Frame(val image: BufferedImage, val row: com.google.gson.JsonObject)

    /** Decodes the frame displayed at [atUs] movie time, or throws IllegalArgument with the Japanese reason. */
    fun decodeAt(saved: SavedReplay, atUs: Long, checkActive: () -> Unit = {}): Frame {
        val frames = saved.frames.mapIndexed { index, row -> index to row }
        val target = frames.firstOrNull { (_, row) ->
            val pts = row["movie_pts_us"]?.takeIf { it.isJsonPrimitive }?.asString?.toLongOrNull()
            val end = row["movie_end_us"]?.takeIf { it.isJsonPrimitive }?.asString?.toLongOrNull()
            pts != null && end != null && atUs in pts until end
        } ?: throw IllegalArgumentException("指定時刻に表示されるframeがありません")
        val targetIndex = target.first
        val rows = saved.frames
        val gopStart = (targetIndex downTo 0).firstOrNull {
            rows[it]["output_key"]?.takeIf { k -> k.isJsonPrimitive }?.asBoolean == true &&
                rows[it]["segment_index"]?.asInt == rows[targetIndex]["segment_index"].asInt
        } ?: throw IllegalArgumentException("表示frameの参照IDRがありません")
        saved.openFile(ReplayFiles.VIDEO).use { file ->
            file.channel(java.util.EnumSet.of(java.nio.file.StandardOpenOption.READ)).use { channel ->
            val demuxer = MP4Demuxer.createRawMP4Demuxer(org.jcodec.common.io.FileChannelWrapper(channel))
            val track = demuxer.videoTrack as org.jcodec.containers.mp4.demuxer.AbstractMP4DemuxerTrack
            val avcs = track.sampleEntries.map { entry ->
                H264Utils.parseAVCC(entry as VideoSampleEntry).also {
                    require(it.spsList.isNotEmpty() && it.ppsList.isNotEmpty()) { "動画のavcCが読めません" }
                }
            }
            val entry = rows[targetIndex]["sample_entry"]?.asInt
                ?: throw IllegalArgumentException("frame記録を読めません")
            val config = annexBConfig(avcs[entry - 1])
            val normalizer = try {
                MixedVideoNormalizer(listOf(config), rows[gopStart]["source_duration_us"]
                    ?.takeIf { it.isJsonPrimitive }?.asString?.toLongOrNull()).apply { startRun(config) }
            } catch (e: IllegalArgumentException) {
                throw IllegalStateException("表示frameの復号に対応できません", e)
            }
            var picture: Picture? = null
            var index = 0
            while (index <= targetIndex) {
                checkActive()
                val packet = track.nextFrame() ?: break
                if (index < gopStart) { index++; continue }
                require(rows[index]["sample_entry"].asInt == entry) { "表示frameのsample構成が読めません" }
                picture = try { normalizer.decodeOnly(movSampleToAnnexB(packet.data, avcs[entry - 1])) }
                catch (e: Exception) { throw IllegalStateException("表示frameの復号に失敗しました", e) }
                index++
            }
            val decoded = picture ?: throw IllegalArgumentException("指定時刻のframeを復号できません")
            return Frame(rgbImage(decoded), rows[targetIndex])
            }
        }
    }

    /** sRGB image of a decoded YUV420J picture at its own pixel size. */
    fun rgbImage(decoded: Picture): BufferedImage {
        val rgb = Picture.create(decoded.width, decoded.height, ColorSpace.RGB)
        Yuv420jToRgb().transform(decoded, rgb)
        val raster = Raster.createInterleavedRaster(
            DataBufferByte(rgb.getPlaneData(0), decoded.width * decoded.height * 3),
            decoded.width, decoded.height, decoded.width * 3, 3, intArrayOf(0, 1, 2), null)
        val colorModel = ComponentColorModel(java.awt.color.ColorSpace.getInstance(java.awt.color.ColorSpace.CS_sRGB),
            false, false, java.awt.Transparency.OPAQUE, java.awt.image.DataBuffer.TYPE_BYTE)
        return BufferedImage(colorModel, raster, false, null)
    }

    /** Encoded PNG bytes with tEXt time metadata inserted before IEND. */
    fun pngBytes(image: BufferedImage, movieUs: Long, row: com.google.gson.JsonObject, source: String): ByteArray {
        val png = ByteArrayOutputStream().also { check(ImageIO.write(image, "png", it)) { "PNGを作成できません" } }.toByteArray()
        require(png.size > 12 && png.takeLast(8).toByteArray().contentEquals(
            byteArrayOf(0x49, 0x45, 0x4e, 0x44, 0xae.toByte(), 0x42, 0x60, 0x82.toByte()))) { "PNGの末尾が不正です" }
        val out = ByteArrayOutputStream()
        out.write(png, 0, png.size - 12)
        fun text(keyword: String, value: String) {
            val data = "$keyword\u0000$value".toByteArray(Charsets.ISO_8859_1)
            val chunk = ByteBuffer.allocate(12 + data.size)
            chunk.putInt(data.size)
            chunk.put("tEXt".toByteArray(Charsets.US_ASCII))
            chunk.put(data)
            chunk.putInt(CRC32().apply { update("tEXt".toByteArray(Charsets.US_ASCII)); update(data) }.value.toInt())
            out.write(chunk.array())
        }
        text("arb:movie_us", movieUs.toString())
        row["t_ns"]?.takeIf { it.isJsonPrimitive }?.asString?.let { text("arb:t_ns", it) }
        row["display_t_ns"]?.takeIf { it.isJsonPrimitive }?.asString?.let { text("arb:display_t_ns", it) }
        row["movie_pts_us"]?.takeIf { it.isJsonPrimitive }?.asString?.let { text("arb:frame_movie_us", it) }
        text("arb:source_folder", source)
        out.write(png, png.size - 12, 12)
        return out.toByteArray()
    }

    /**
     * Saves the frame displayed at [atUs] to [destination] (the screenshot folder) and returns the PNG path.
     * Follows the screenshot save model: exclusive `.partial` output, ownership checks, no overwrite.
     */
    fun export(folder: Path, atUs: Long, destination: Path,
               createdAt: ZonedDateTime = ZonedDateTime.now(), id: String = nameId(),
               checkActive: () -> Unit = {}): Path {
        require(destination.isAbsolute) { "設定で保存先の絶対パスを指定してください。" }
        val bytes = SavedReplay.open(folder).use { saved ->
            val frame = decodeAt(saved, atUs, checkActive)
            pngBytes(frame.image, atUs, frame.row, saved.name)
        }
        validateScreenshotPng(bytes)
        val root = try { destination.toRealPath() } catch (_: Exception) {
            throw ScreenshotSaveFailure("保存先フォルダを確認できません。設定の保存先・アクセス権を確認してください。")
        }
        val name = frameFileName(createdAt, id)
        val partial = root.resolve(".${name.removeSuffix(".png")}.partial")
        val complete = root.resolve(name)
        var retained: Path? = null
        try {
            SaveOutputDirectory.open(root).use { parent ->
                parent.createFile(partial.fileName.toString()).use { output ->
                    retained = partial
                    output.requirePrivate()
                    output.channel().use { channel ->
                        val data = ByteBuffer.wrap(bytes)
                        while (data.hasRemaining()) channel.write(data)
                        channel.force(true)
                    }
                    fun verify(path: Path) {
                        SaveOutputDirectory.open(root).use { current ->
                            check(current.key == parent.key) { "保存先が変更されています。" }
                            current.verifyFile(path.fileName.toString(), output)
                        }
                        output.requirePrivate()
                    }
                    try {
                        verify(partial)
                        parent.moveFile(partial.fileName.toString(), parent, complete.fileName.toString())
                        retained = complete
                        verify(complete)
                        parent.flush()
                    } catch (failure: Exception) {
                        retained = listOf(partial, complete).firstOrNull { candidate -> runCatching {
                            SaveOutputDirectory.open(root).use { current ->
                                check(current.key == parent.key)
                                current.verifyFile(candidate.fileName.toString(), output)
                            }
                        }.isSuccess }
                        throw failure
                    }
                    return complete
                }
            }
        } catch (_: Exception) {
            throw ScreenshotSaveFailure("静止画の保存を完了できません。保存先・空き容量・アクセス権を確認してください。未完成の出力は保全します。", retained)
        }
    }
}
