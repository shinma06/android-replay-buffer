package io.github.shinma06.replaybuffer.core

import org.jcodec.codecs.mpeg4.mp4.EsdsBox
import org.jcodec.common.io.NIOUtils
import org.jcodec.containers.mp4.boxes.NodeBox
import org.jcodec.containers.mp4.demuxer.AbstractMP4DemuxerTrack
import org.jcodec.containers.mp4.demuxer.MP4Demuxer
import java.net.URLClassLoader
import java.nio.file.Path
import java.security.MessageDigest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class AacEscapePlaybackTest {
    @Test
    fun runtimeTablesAreRepairedAndKeepEveryUpstreamValue() {
        val name = "net/sourceforge/jaad/aac/syntax/IQTable.class"
        val copies = javaClass.classLoader.getResources(name).toList()
        assertEquals(1, copies.map { url ->
            url.openStream().use { MessageDigest.getInstance("SHA-256").digest(it.readBytes()).toList() }
        }.distinct().size, copies.toString())
        val field = Class.forName("net.sourceforge.jaad.aac.syntax.IQTable").getDeclaredField("IQ_TABLE")
        field.isAccessible = true
        val table = field.get(null) as FloatArray
        assertEquals(8192, table.size)
        val original = Path.of(System.getProperty("replay.jcodec.upstream")).toUri().toURL()
        URLClassLoader(arrayOf(original), ClassLoader.getPlatformClassLoader()).use { loader ->
            val oldField = loader.loadClass("net.sourceforge.jaad.aac.syntax.IQTable").getDeclaredField("IQ_TABLE")
            oldField.isAccessible = true
            val old = oldField.get(null) as FloatArray
            assertEquals(8191, old.size)
            for (i in old.indices) assertEquals(old[i], table[i], "index=$i")
        }
        assertEquals(165113.5f, table[8191])
    }

    @Test
    fun aacLcEscapeBoundaryDecodesWithoutDroppingPackets() {
        val source = Path.of(javaClass.getResource("/aac-lc-escape/beeps.m4a")!!.toURI())
        NIOUtils.readableChannel(source.toFile()).use { input ->
            val track = MP4Demuxer.createRawMP4Demuxer(input).audioTracks.single() as AbstractMP4DemuxerTrack
            val config = NodeBox.findFirst(track.sampleEntries[0], EsdsBox::class.java, EsdsBox.fourcc()).streamInfo
            val decoder = AacPcmDecoder(NIOUtils.toArray(config))
            assertEquals(48_000, decoder.sampleRate)
            assertEquals(1, decoder.channels)
            var packets = 0
            var peak = 0
            while (true) {
                val packet = track.nextFrame() ?: break
                val pcm = decoder.decode(NIOUtils.toArray(packet.data))
                assertEquals(1024 * 2, pcm.size, "packet=$packets")
                for (i in pcm.indices step 2) {
                    val value = (pcm[i].toInt() and 0xff) or (pcm[i + 1].toInt() shl 8)
                    peak = maxOf(peak, kotlin.math.abs(value))
                }
                packets++
            }
            assertEquals(376, packets)
            assertTrue(peak > 10_000, "The beeps must not be replaced by silence")
        }
    }
}
