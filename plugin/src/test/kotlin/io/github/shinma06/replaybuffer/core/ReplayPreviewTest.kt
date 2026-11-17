package io.github.shinma06.replaybuffer.core

import org.jcodec.common.model.ColorSpace
import org.jcodec.common.model.Picture
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.awt.image.BufferedImage
import java.nio.file.Files
import java.nio.file.Path
import kotlin.random.Random
import kotlin.test.*

class ReplayPreviewTest {
    @TempDir lateinit var root: Path

    @Test fun packedPreviewEqualsExportForRangesMatricesRoundingAndClipping() {
        val random = Random(373)
        for (color in listOf(ColorSpace.YUV420, ColorSpace.YUV420J)) for (matrix in listOf(null, 1, 6)) {
            repeat(12) {
                val p = Picture.create(66, 34, color)
                p.data.forEach { random.nextBytes(it) }
                val frame = MixedVideoNormalizer.DisplayFrame(p, matrix)
                val expected = ReplayFrameExport.rgbImage(frame)
                val actual = ReplayPreviewImage.convert(frame)
                assertContentEquals(expected.getRGB(0, 0, 66, 34, null, 0, 66), actual.getRGB(0, 0, 66, 34, null, 0, 66))
                assertTrue((3 until actual.pixels.size step 4).all { actual.pixels[it] == (-1).toByte() })
            }
        }
    }

    @Test fun frameCacheEvictsByBytesAndSeparatesPinnedSourceAndResolution() {
        val cache = ReplayFrameCache(128)
        fun frame(i: Int, width: Int = 4) = ReplayFrameCursor.Decoded(BufferedImage(width, 4, BufferedImage.TYPE_INT_RGB), i.toLong(), i + 1L, i)
        cache.put("source-a:full-v1", frame(0)); cache.put("source-a:full-v1", frame(1))
        assertNotNull(cache.get("source-a:full-v1", 0))
        cache.put("source-b:full-v1", frame(0))
        assertNull(cache.get("source-a:full-v1", 1))
        assertNotNull(cache.get("source-a:full-v1", 0))
        assertNotNull(cache.get("source-b:full-v1", 0))
        cache.put("source-b:full-v1", frame(0, 8))
        assertEquals(128L, cache.sizeBytes())
        assertEquals(8, cache.get("source-b:full-v1", 0)!!.image.width)
        cache.put("oversize", frame(3, 100))
        assertTrue(cache.sizeBytes() <= 128)
        assertNull(cache.get("oversize", 3))
        cache.clear(); assertEquals(0L, cache.sizeBytes())
    }

    @Test fun diskCacheRoundTripsPrivatelyBoundsSizeAndRejectsTruncation() {
        val cache = ReplayThumbnailDiskCache(root, maxBytes = 1024)
        val image = BufferedImage(8, 4, BufferedImage.TYPE_INT_RGB)
        for (y in 0..3) for (x in 0..7) image.setRGB(x, y, (x * 31 shl 16) or (y * 64 shl 8) or 127)
        cache.write("a", 0, image)
        val read = assertNotNull(cache.read("a", 0))
        assertContentEquals(image.getRGB(0, 0, 8, 4, null, 0, 8), read.getRGB(0, 0, 8, 4, null, 0, 8))
        assertNull(cache.read("b", 0)); assertNull(cache.read("a", 1))
        val folder = root.resolve("arb-preview-v1")
        assertTrue(PrivateFiles.isPrivate(folder, true))
        val entry = Files.list(folder).use { paths -> paths.filter { it.toString().endsWith(".thumb") }.findFirst().get() }
        assertTrue(PrivateFiles.isPrivate(entry, false))
        Files.write(entry, byteArrayOf(1, 2))
        assertNull(cache.read("a", 0))
        repeat(20) { cache.write("a", it + 1L, image) }
        assertTrue(Files.list(folder).use { paths -> paths.filter { it.toString().endsWith(".thumb") }.mapToLong { Files.size(it) }.sum() } <= 1024)
    }

    @Test fun thumbnailOwnerLockExcludesAnotherReaderAndReleasesWithoutDeletingItsIdentity() {
        SaveOutputDirectory.open(root).use { dir ->
            dir.createFile("cache.lock").use { first ->
                val owner = assertNotNull(first.tryOwnerLock())
                owner.use { dir.openFile("cache.lock").use { second -> assertNull(second.tryOwnerLock()) } }
                dir.openFile("cache.lock").use { second ->
                    assertEquals(first.key, second.key)
                    assertNotNull(second.tryOwnerLock()).close()
                }
            }
        }
    }

    @Test fun cursorCacheNeighborsForwardRecoveryCancellationAndSourceReplacementPreserveExactPixels() {
        val folder = fixture()
        SavedReplay.open(folder).use { saved ->
            val cache = ReplayFrameCache()
            ReplayFrameCursor(saved, cache = cache).use { cursor ->
                val starts = saved.frames.mapNotNull { it["movie_pts_us"]?.takeIf { it.isJsonPrimitive }?.asLong }
                for (at in listOf(starts[8], starts[7], starts[8], starts[6], starts.last(), starts[1])) {
                    val expected = ReplayFrameExport.decodeAt(saved, at).image
                    val actual = cursor.seek(at).image
                    assertContentEquals(expected.getRGB(0, 0, expected.width, expected.height, null, 0, expected.width),
                        actual.getRGB(0, 0, actual.width, actual.height, null, 0, actual.width))
                    cursor.next()?.let { next ->
                        val reference = ReplayFrameExport.decodeAt(saved, next.startUs).image
                        assertContentEquals(reference.getRGB(0, 0, reference.width, reference.height, null, 0, reference.width),
                            next.image.getRGB(0, 0, reference.width, reference.height, null, 0, reference.width))
                    }
                }
                assertFailsWith<InterruptedException> { cursor.seek(starts.last()) { throw InterruptedException("superseded") } }
                assertEquals(starts.first(), cursor.seek(starts.first()).startUs)
                // A cached frame cannot bypass the original source identity pins.
                val video = folder.resolve(ReplayFiles.VIDEO)
                val moved = folder.resolve("old-video.mp4")
                Files.move(video, moved); Files.copy(moved, video)
                assertFailsWith<IllegalStateException> { cursor.seek(starts.first()) }
            }
        }
    }

    @Test fun keyframeIndexDoesNotCrossGapsSegmentsEntriesOrUnpresentedPreroll() {
        fun row(start: Long?, key: Boolean, segment: Int = 0, entry: Int = 1) = com.google.gson.JsonObject().apply {
            addProperty("movie_pts_us", start?.toString()); addProperty("movie_end_us", start?.plus(10)?.toString())
            addProperty("output_key", key); addProperty("segment_index", segment); addProperty("sample_entry", entry)
        }
        val index = ReplayPreviewIndex(listOf(row(null, true), row(10, false), row(20, true), row(30, false),
            row(50, false, 1), row(60, true, 1), row(70, false, 1, 2)))
        assertNull(index.at(10)); assertNull(index.at(45)); assertNull(index.at(50)); assertNull(index.at(70))
        assertEquals(20L, index.at(35)); assertEquals(60L, index.at(69))
    }

    @Test fun cachedSeekFollowedByUnpresentedRowsRepositionsTheDecoderBeforeNext() {
        SavedReplay.open(fixture()).use { saved ->
            val before = saved.frames[3]["movie_pts_us"].asLong
            val after = saved.frames[5]["movie_pts_us"].asLong
            saved.frames[4].apply {
                add("movie_pts_us", com.google.gson.JsonNull.INSTANCE)
                add("movie_end_us", com.google.gson.JsonNull.INSTANCE)
            }
            val cursor = ReplayFrameCursor(saved)
            cursor.use {
                cursor.seek(before)
                cursor.seek(after)
                cursor.seek(before) // Cache hit: the MP4 cursor is deliberately elsewhere.
                val next = assertNotNull(cursor.next())
                assertEquals(after, next.startUs)
                val reference = ReplayFrameExport.decodeAt(saved, after).image
                assertContentEquals(reference.getRGB(0, 0, reference.width, reference.height, null, 0, reference.width),
                    next.image.getRGB(0, 0, reference.width, reference.height, null, 0, reference.width))
            }
            assertFailsWith<IllegalStateException> { cursor.seek(before) }
        }
    }

    private fun fixture(): Path {
        val name = "replay-20240101-000000-manual-feedface"
        val folder = Files.createDirectory(root.resolve(name))
        for (file in listOf("README.md", "replay.json", "video.mp4", "video-source.h264", "video-frames.jsonl",
            "logcat-device.jsonl", "logcat-app.jsonl", "touches.jsonl")) {
            javaClass.getResourceAsStream("/replay-trim-parity/$name/$file")!!.use { Files.copy(it, folder.resolve(file)) }
        }
        return folder
    }
}
