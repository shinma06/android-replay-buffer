package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonParser
import io.github.shinma06.replaybuffer.settings.ReplaySettings
import org.jcodec.codecs.h264.H264Encoder
import org.jcodec.codecs.h264.H264Utils
import org.jcodec.common.model.ColorSpace
import org.jcodec.common.model.Picture
import java.nio.ByteBuffer
import java.nio.file.Files
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

/** Rotation: the same scrcpy connection reports a new screen size (session) and keeps streaming in one generation. */
class RotationFollowTest {
    private fun encode(encoder: H264Encoder, width: Int, height: Int, fill: Int): Pair<ByteArray, ByteArray> {
        val encoded = encoder.encodeFrame(Picture.create(width, height, ColorSpace.YUV420J).apply { fill(fill) },
            ByteBuffer.allocate(65536)).data
        val bytes = ByteArray(encoded.remaining()).also { encoded.get(it) }
        val config = H264Utils.splitFrame(ByteBuffer.wrap(bytes)).filter { it.get(0).toInt() and 31 in setOf(7, 8) }
            .fold(byteArrayOf()) { result, nal -> result + byteArrayOf(0, 0, 0, 1) + ByteArray(nal.remaining()).also { nal.get(it) } }
        return bytes to config
    }

    @Test
    fun `a screen size change keeps one aligned timeline with a known gap until the first new frame`() {
        val root = Files.createTempDirectory("replay-rotation-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        // Clock reads run up to "now", so the session packet and later frames arrive on the real host clock.
        val base = System.nanoTime() - 1_500_000_000
        fun anchor(elapsed: Long, host: Long) =
            store.clock.add("1", listOf(elapsed, elapsed, 1_700_000_000_000_000_000 + elapsed, elapsed), host, host)
        try {
            store.generation(1)
            for (t in 0L..1_500_000_000L step 100_000_000L) anchor(10_000_000_000 + t, base + t)
            val portrait = H264Encoder.createH264Encoder().apply { setKeyInterval(100) }
            store.session(VideoPacket.Session(16, 32), 1)
            for (n in 0..9) {
                val (bytes, config) = encode(portrait, 16, 32, 20 + n)
                if (n == 0) store.frame(VideoPacket.Frame(0, true, false, config), 1)
                val pts = 10_100_000L + n * 100_000L
                assertTrue(store.frame(VideoPacket.Frame(pts, false, n == 0, bytes), 1, base + (pts * 1000 - 10_000_000_000)))
            }
            val offset = store.clock.video(10_000_000, System.nanoTime()).sequence!! - 10_000_000_000
            // Rotation: the encoder restarts at the new size; its first frame is captured after the session packet
            // but received later (encoder start-up), so the gap must end at that frame's own time, not at its arrival.
            store.session(VideoPacket.Session(32, 16), 1)
            val switched = assertNotNull(store.clock.now())
            val firstPts = (switched + 20_000_000 - offset) / 1000
            Thread.sleep(80)
            val landscape = H264Encoder.createH264Encoder().apply { setKeyInterval(100) }
            for (n in 0..3) {
                val (bytes, config) = encode(landscape, 32, 16, 120 + n)
                if (n == 0) store.frame(VideoPacket.Frame(0, true, false, config), 1)
                assertTrue(store.frame(VideoPacket.Frame(firstPts + n * 10_000L, false, n == 0, bytes), 1))
            }
            Thread.sleep(50)
            val now = System.nanoTime()
            anchor(10_000_000_000 + (now - base), now)
            val capture = store.capture(ReplaySettings(replaySeconds = 180), host = now)!!
            val rotated = capture.video.first { it.width == 32 }
            val gap = capture.gaps.single { it.stream == "video" }
            assertTrue(gap.fromNs!! < rotated.time.sequence!!, "$gap ${rotated.time}")
            assertEquals(rotated.time.sequence, gap.toNs)

            val output = SaveWriter().write(capture, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
            val manifest = JsonParser.parseString(Files.readString(output.directory.resolve("session.json"))).asJsonObject
            val parts = manifest.getAsJsonArray("parts").map { it.asJsonObject }
            assertEquals(2, parts.size)
            assertTrue(parts.all { it["clock_alignment_known"].asBoolean }, parts.toString())
            assertTrue(manifest["video_reencoded"].asBoolean)
            assertEquals(32, manifest.getAsJsonObject("video_canvas")["width"].asInt)
            val boundary = manifest.getAsJsonArray("video_run_boundaries").single().asJsonObject
            assertEquals((gap.toNs!! - gap.fromNs!!) / 1000, boundary["actual_gap_duration_us"].asLong)
            // One movie timeline: movie time and window time differ by the same constant before and after the rotation.
            val rows = Files.readAllLines(output.directory.resolve("frames.jsonl")).map { JsonParser.parseString(it).asJsonObject }
                .filter { it["presented"].asBoolean }
            val shifts = rows.map { it["movie_pts_us"].asLong - it["display_window_start_ns"].asLong / 1000 }
            assertTrue(shifts.max() - shifts.min() <= 2, shifts.toString())
            // The last portrait image is held until the rotation gap starts, then the movie shows the gap until the new frame.
            val held = rows.last { it["output_width"].asInt == 32 && it["source_rect"].asJsonObject["width"].asInt == 16 }
            assertEquals((gap.fromNs!! - capture.start) / 1000, held["movie_end_us"].asLong - shifts.first(), held.toString())
            store.release(capture.id)
        } finally {
            store.close()
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }
}
