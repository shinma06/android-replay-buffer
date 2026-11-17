package io.github.shinma06.replaybuffer.core

import org.jcodec.common.model.ColorSpace
import org.jcodec.common.model.Picture
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class ReplayPlaybackTest {

    @Test
    fun rgbImagePreservesBlackWhiteAndNeutralGrays() {
        for (gray in listOf(0, 64, 128, 192, 255)) {
            val picture = Picture.create(2, 2, ColorSpace.YUV420J)
            picture.getPlaneData(0).fill((gray - 128).toByte())
            val image = ReplayFrameExport.rgbImage(MixedVideoNormalizer.DisplayFrame(picture, null))
            for (y in 0 until 2) for (x in 0 until 2)
                assertEquals(gray * 0x010101, image.getRGB(x, y) and 0xffffff, "gray=$gray")
        }
    }

    @Test
    fun audioScheduleFillsGapsWithSilenceAndClipsSegments() {
        val rate = 48_000
        val segments = listOf(
            PlaybackAudioSegment(movieStartUs = 0, durationUs = 64_000, mediaStart = 0, samples = 3),
            PlaybackAudioSegment(movieStartUs = 500_000, durationUs = 21_333, mediaStart = 3 * 1024, samples = 1),
        )
        // From 0: the first segment (64 ms = 3072 samples), 436 ms of silence, then one packet clipped to its duration.
        assertEquals(listOf(
            AudioChunk.Packets(0, 3, 0, 3072),
            AudioChunk.Silence(20_928),
            AudioChunk.Packets(3, 1, 0, 1023),
        ), audioSchedule(segments, rate, 0))
        // From inside the first segment: skip whole packets and the remainder within the first kept one.
        assertEquals(AudioChunk.Packets(1, 2, 176, 3072 - 1200), audioSchedule(segments, rate, 25_000).first())
        // Past every segment: nothing.
        assertTrue(audioSchedule(segments, rate, 600_000).isEmpty())
    }

    @Test
    fun audioScheduleHonoursAnEditStartingInsideAnAacPacket() {
        val segment = PlaybackAudioSegment(0, 30_000, 2 * 1024 + 480, 2)
        assertEquals(listOf(AudioChunk.Packets(2, 2, 480, 1440)), audioSchedule(listOf(segment), 48_000, 0))
        assertEquals(listOf(AudioChunk.Packets(3, 1, 416, 480)), audioSchedule(listOf(segment), 48_000, 20_000))
    }

    @Test
    fun bundledJaadDecodesAacLcToPcm() {
        // 0.1 s of a 440 Hz sine, mono 48 kHz AAC-LC (AudioSpecificConfig + raw access units).
        val config = hex("1188")
        val frames = listOf(
            "de02004c61766336312e31392e313031000268a359a1288d35874a995f6f8a711e697b92243c2249fffeae481374c37362b4f5b36d748babb83b668efc37b8b056ca9ad3252255cdb519ccb44a44a4489929128181819103031b366c1819122446cd9b368912296596cdc012a2e237044aa9c8d64d8d708cf6d15748ce5d056c8cd392aa1c069929c9a26b9296ae",
            "013494dac95d964bab25d414e9bfdfceadb5a97ff6f1fbf9d6b8d5ebffe2f5fbf9d6b8d5ebffec5fe3ceb5ad6ac6d4ecdbae79e60d65b2cf70a2f7fb28183740fa50afa503e9f4d6603e9f4fa5007d3e9f4d607d3e9f4d741801f4fa7d3599b006b9eb458b549fcdb748843133e5133e5133e5f288983e5f2e31307cbe5c4983e5f2f94511003e5f2f9718a2005973d67024242424242424242424242424e7",
            "0136f18afac64fe3fa7ebd71c79d4e26a69abb91bf725aad2a07fe1b4bee7c7c666ddf7f703e33019f40193e00cfee07c6630cfee01fc58070",
            "0104318a425d639effe3ffe2ffcbfdffcdd5cd5571cfc77198efd7771b8954228a25af418d145145145dc22881451211544fd6b7420a8ea270e0",
            "0144318afac339ff8ff3f7d2eef52e4892470c5a49120bb7f8488abc3ba3dbd87bb7f8491edecc2edfe1247b7b3028fc126baa39477a4d7547e0",
            "0152318afa521694adf1b9c6b4d5cb9245a38648f69a910677f9fdf27c1b464dd0b79dfe7d191de6cd9df8346474b3677cd0b8e936d3e685c4936eccdb4f82cce9db69f497",
        ).map(::hex)
        val decoder = AacPcmDecoder(config)
        assertEquals(48_000, decoder.sampleRate)
        assertEquals(1, decoder.channels)
        val pcm = frames.map(decoder::decode)
        assertTrue(pcm.all { it.size == 1024 * 2 })
        val samples = pcm.drop(2).flatMap { bytes -> (bytes.indices step 2).map { (bytes[it].toInt() and 0xff) or (bytes[it + 1].toInt() shl 8) } }
        assertTrue(samples.maxOf { kotlin.math.abs(it) } > 1000) // the tone is audible after the encoder delay
    }

    private fun hex(text: String) = ByteArray(text.length / 2) { text.substring(it * 2, it * 2 + 2).toInt(16).toByte() }
}
