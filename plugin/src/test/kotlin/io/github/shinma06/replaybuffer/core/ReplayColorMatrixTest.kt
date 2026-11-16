package io.github.shinma06.replaybuffer.core

import org.jcodec.codecs.h264.H264Utils
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.awt.image.BufferedImage
import java.nio.ByteBuffer
import java.nio.file.Files
import java.nio.file.Path
import java.security.MessageDigest
import javax.imageio.ImageIO
import kotlin.math.abs
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class ReplayColorMatrixTest {
    @TempDir lateinit var root: Path
    private val colors = listOf(0x000000, 0xffffff, 0x404040, 0x808080, 0xc0c0c0, 0xff0000, 0x00ff00, 0x0000ff)

    @Test
    fun bt709KnownColorsReachPngAndPlaybackForBothRanges() {
        for (range in listOf("bt709-limited", "bt709-full")) {
            val source = fixture(range)
            val before = hashes(source)
            SavedReplay.open(source).use { saved ->
                ReplayFrameCursor(saved).use { cursor ->
                    for (at in listOf(0L, 500_000L, 900_000L)) {
                        val exported = ImageIO.read(ReplayFrameExport.export(source, at, root).toFile())
                        val playback = cursor.seek(at).image
                        assertColors(exported)
                        assertColors(playback)
                        assertTrue(exported.getRGB(0, 0, 256, 32, null, 0, 256)
                            .contentEquals(playback.getRGB(0, 0, 256, 32, null, 0, 256)))
                        assertColors(cursor.next()!!.image)
                    }
                }
            }
            assertEquals(before, hashes(source))
        }
    }

    @Test
    fun interleavedPlayersAndRepeatedSeeksDoNotShareMatrixOrRange() {
        val bt709 = fixture("bt709-full")
        val bt601 = fixture("limited")
        val before709 = hashes(bt709)
        val before601 = hashes(bt601)
        SavedReplay.open(bt709).use { saved709 ->
            SavedReplay.open(bt601).use { saved601 ->
                ReplayFrameCursor(saved709).use { cursor709 ->
                    ReplayFrameCursor(saved601).use { cursor601 ->
                        for (at in listOf(500_000L, 0L, 900_000L, 400_000L)) {
                            assertColors(cursor709.seek(at).image)
                            assertColors(cursor601.seek(at).image)
                            assertColors(ReplayFrameExport.decodeAt(saved709, at).image)
                            assertColors(ReplayFrameExport.decodeAt(saved601, at).image)
                            assertColors(cursor709.next()!!.image)
                            assertColors(cursor601.next()!!.image)
                        }
                    }
                }
            }
        }
        assertEquals(before709, hashes(bt709))
        assertEquals(before601, hashes(bt601))
    }

    private fun fixture(range: String): Path {
        val bytes = javaClass.getResourceAsStream("/replay-color-range/" + range + ".h264")!!.use { it.readBytes() }
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
        val config = H264Utils.splitFrame(ByteBuffer.wrap(packets.first()))
            .filter { (it.get(0).toInt() and 31) in setOf(7, 8) }
            .fold(byteArrayOf()) { out, nal -> out + byteArrayOf(0, 0, 0, 1) + ByteArray(nal.remaining()).also { nal.get(it) } }
        val vui = H264Utils.readSPS(H264Utils.getRawSPS(ByteBuffer.wrap(config)).single()).vuiParams
        assertEquals(if (range.startsWith("bt709")) 1 else 6, vui.matrixCoefficients)
        assertEquals(range.endsWith("full"), vui.videoFullRangeFlag)
        val dir = Files.createDirectory(root.resolve(range))
        CaptureStore(dir.resolve("ring"), minFree = 0).use { store ->
            store.generation(1)
            store.session(VideoPacket.Session(256, 32), 1)
            store.frame(VideoPacket.Frame(0, true, false, config), 1)
            packets.forEachIndexed { n, data ->
                assertTrue(store.frame(VideoPacket.Frame(n * 100_000L, false,
                    H264Utils.isByteBufferIDRSlice(ByteBuffer.wrap(data)), data), 1))
            }
            return SaveWriter().write(store.capture(ReplaySettings())!!, dir, { false }) {
                Files.move(it.partial, it.complete)
            }.directory
        }
    }

    private fun assertColors(image: BufferedImage) {
        assertEquals(256, image.width); assertEquals(32, image.height)
        colors.forEachIndexed { n, expected ->
            val actual = image.getRGB(n * 32 + 16, 16)
            for (shift in listOf(16, 8, 0))
                assertTrue(abs(((expected shr shift) and 255) - ((actual shr shift) and 255)) <= 3,
                    "expected=" + expected.toString(16) + ", actual=" + (actual and 0xffffff).toString(16))
        }
    }

    private fun hashes(folder: Path): Map<String, String> = Files.walk(folder).use { paths ->
        paths.filter { Files.isRegularFile(it) }.toList().associate {
            folder.relativize(it).toString() to MessageDigest.getInstance("SHA-256")
                .digest(Files.readAllBytes(it)).joinToString("") { byte -> "%02x".format(byte.toInt() and 255) }
        }
    }
}
