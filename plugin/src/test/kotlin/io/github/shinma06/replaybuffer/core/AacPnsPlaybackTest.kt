package io.github.shinma06.replaybuffer.core

import org.jcodec.codecs.mpeg4.mp4.EsdsBox
import org.jcodec.common.io.NIOUtils
import org.jcodec.containers.mp4.boxes.NodeBox
import org.jcodec.containers.mp4.demuxer.AbstractMP4DemuxerTrack
import org.jcodec.containers.mp4.demuxer.MP4Demuxer
import java.nio.file.Path
import kotlin.math.PI
import kotlin.math.cos
import kotlin.math.sin
import kotlin.math.sqrt
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class AacPnsPlaybackTest {
    private data class Input(val config: ByteArray, val packets: List<ByteArray>)

    private fun input(name: String): Input {
        val path = Path.of(javaClass.getResource("/aac-pns/$name.m4a")!!.toURI())
        return NIOUtils.readableChannel(path.toFile()).use { channel ->
            val track = MP4Demuxer.createRawMP4Demuxer(channel).audioTracks.single() as AbstractMP4DemuxerTrack
            val config = NodeBox.findFirst(track.sampleEntries[0], EsdsBox::class.java, EsdsBox.fourcc()).streamInfo
            val packets = buildList {
                while (true) add(NIOUtils.toArray((track.nextFrame() ?: break).data))
            }
            Input(NIOUtils.toArray(config), packets)
        }
    }

    private fun decode(input: Input): List<Int> {
        val decoder = AacPcmDecoder(input.config)
        assertEquals(48_000, decoder.sampleRate)
        assertEquals(1, decoder.channels)
        assertEquals(13, input.packets.size)
        return input.packets.flatMapIndexed { index, packet ->
            val bytes = decoder.decode(packet)
            assertEquals(2048, bytes.size, "packet=$index")
            (bytes.indices step 2).map { (bytes[it].toInt() and 0xff) or (bytes[it + 1].toInt() shl 8) }
        }.also { assertEquals(13 * 1024, it.size) }
    }

    private fun assertSteadyTone(name: String) {
        val pcm = decode(input(name))
        // Raw axis: 1024 priming + 1920 source onset + 480 edge exclusion, never a fitted shift.
        val central = pcm.subList(3424, 3424 + 3840)
        for ((index, window) in central.chunked(48).withIndex()) {
            val rms = sqrt(window.sumOf { it.toDouble() * it } / window.size)
            assertTrue(rms > 4000, "$name: central 1ms window=$index RMS=$rms")
            assertTrue(rms < 10_000, "$name: central 1ms window=$index RMS=$rms")
        }
        val rms = sqrt(central.sumOf { it.toDouble() * it } / central.size)
        assertTrue(rms in 8000.0..9000.0, "$name RMS=$rms")
        assertTrue(pcm.all { it in -20_000..20_000 }, "$name: clipping/overshoot")
        fun power(hz: Int): Double {
            val real = central.withIndex().sumOf { (i, v) -> v * cos(2 * PI * hz * i / 48_000) }
            val imag = central.withIndex().sumOf { (i, v) -> v * sin(2 * PI * hz * i / 48_000) }
            return real * real + imag * imag
        }
        val peakHz = (900..1100 step 10).maxBy(::power)
        assertEquals(1000, peakHz, name)
        val tonePower = 2 * power(1000) / (central.size.toDouble() * central.size)
        assertTrue(tonePower / (rms * rms) > 0.99, "$name: 1kHz energy fraction")
    }

    @Test
    fun pnsToneKeepsEveryMillisecondOfEnergy() = assertSteadyTone("pns")

    @Test
    fun nonPnsControlKeepsItsTone() = assertSteadyTone("no-pns")

    @Test
    fun pnsProcessingRemainsFiniteAcrossDecoderInstances() {
        val input = input("pns")
        repeat(3) { run ->
            val decoder = AacPcmDecoder(input.config)
            val delegate = field(decoder, "decoder")
            val syntax = field(delegate, "syntacticElements")
            input.packets.forEachIndexed { index, packet ->
                decoder.decode(packet)
                @Suppress("UNCHECKED_CAST")
                val data = field(syntax, "data") as Array<FloatArray>
                assertTrue(data.all { row -> row.all { it.isFinite() } }, "run=$run packet=$index")
            }
        }
    }

    private fun field(value: Any, name: String): Any = value.javaClass.getDeclaredField(name).run {
        isAccessible = true
        get(value)
    }
}
