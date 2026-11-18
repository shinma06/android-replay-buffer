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

    @Test fun resizedThumbnailStripKeepsItsVisibleCacheHitsWhileMissingSlotsArrive() {
        val cache = io.github.shinma06.replaybuffer.ui.ThumbnailCache<String>(3, 6) { it.length.toLong() }
        for (key in 1L..3L) cache.put(key, "ok")
        assertEquals(listOf(4L), cache.missing(listOf(1L, 2L, 4L)))
        cache.put(4, "ok")
        assertTrue(cache.missing(listOf(1L, 2L, 4L)).isEmpty())
        assertNull(cache[3])
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
        // Match the cache caller: TempDir may contain Darwin's /var -> /private/var symlink.
        SaveOutputDirectory.open(root.toRealPath()).use { dir ->
            dir.createFile("cache.lock").use { first ->
                val owner = assertNotNull(first.tryOwnerLock())
                owner.use {
                    repeat(2) {
                        dir.verifyFile("cache.lock", first)
                        dir.openFile("cache.lock").use { second -> assertNull(second.tryOwnerLock()) }
                    }
                }
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

    @Test fun cachedBackwardSeekKeepsNextOutputPositionSeparateFromPhysicalDecoder() {
        SavedReplay.open(fixture()).use { saved ->
            val starts = saved.frames.map { it["movie_pts_us"].asString.toLong() }
            ReplayFrameCursor(saved).use { cursor ->
                assertEquals(8, cursor.seek(starts[8]).index)
                assertEquals(6, cursor.seek(starts[6]).index)
                val next = assertNotNull(cursor.next())
                assertEquals(7, next.index)
                val expected = ReplayFrameExport.decodeAt(saved, starts[7]).image
                assertContentEquals(expected.getRGB(0, 0, expected.width, expected.height, null, 0, expected.width),
                    next.image.getRGB(0, 0, next.image.width, next.image.height, null, 0, next.image.width))
            }
        }
    }

    @Test fun cursorDecodesAnUnpresentedPrerollIdrForTheExactDisplayedFrame() {
        SavedReplay.open(fixture()).use { saved ->
            val target = saved.frames[1]["movie_pts_us"].asString.toLong()
            saved.frames[0].apply {
                add("movie_pts_us", com.google.gson.JsonNull.INSTANCE)
                add("movie_end_us", com.google.gson.JsonNull.INSTANCE)
                addProperty("presented", false)
                addProperty("preroll", true)
            }
            ReplayFrameCursor(saved).use { cursor ->
                val actual = cursor.seek(target)
                assertEquals(1, actual.index)
                val expected = ReplayFrameExport.decodeAt(saved, target).image
                assertContentEquals(expected.getRGB(0, 0, expected.width, expected.height, null, 0, expected.width),
                    actual.image.getRGB(0, 0, actual.image.width, actual.image.height, null, 0, actual.image.width))
            }
        }
    }

    @Test fun cursorIndexesRejectGapsAndGopsAcrossSegmentOrSampleEntryChanges() {
        SavedReplay.open(fixture()).use { saved ->
            val missing = saved.frames[4]
            val gapUs = missing["movie_pts_us"].asString.toLong()
            val originalStart = missing["movie_pts_us"]
            val originalEnd = missing["movie_end_us"]
            missing.add("movie_pts_us", com.google.gson.JsonNull.INSTANCE)
            missing.add("movie_end_us", com.google.gson.JsonNull.INSTANCE)
            ReplayFrameCursor(saved).use { cursor ->
                assertFailsWith<IllegalArgumentException> { cursor.seek(gapUs) }
                val nextUs = saved.frames[5]["movie_pts_us"].asString.toLong()
                assertEquals(5, cursor.seek(nextUs).index)
            }
            missing.add("movie_pts_us", originalStart)
            missing.add("movie_end_us", originalEnd)
            for (field in listOf("segment_index", "sample_entry")) {
                val targetIndex = 5
                val row = saved.frames[targetIndex]
                val target = row["movie_pts_us"].asString.toLong()
                val original = row[field]
                row.addProperty(field, original.asInt + 1)
                ReplayFrameCursor(saved).use { cursor ->
                    assertFailsWith<IllegalArgumentException> { cursor.seek(target) }
                }
                row.add(field, original)
            }
        }
    }

    @Test fun playbackPreparesReferenceStateBeforeClockInsteadOfDeferringGopReplayUntilCacheMiss() {
        SavedReplay.open(fixture()).use { saved ->
            val shared = ReplayFrameCache()
            val at = saved.frames[5]["movie_pts_us"].asLong
            ReplayFrameCursor(saved, cache = shared).use { it.seek(at) }
            val packets = mutableListOf<Int>()
            ReplayFrameCursor(saved, cache = shared, packetReadObserver = { packets += it }).use { playback ->
                assertEquals(at, playback.seekForPlayback(at).startUs)
                assertEquals((0..5).toList(), packets)
                val next = assertNotNull(playback.next())
                assertEquals(saved.frames[6]["movie_pts_us"].asLong, next.startUs)
                assertEquals((0..6).toList(), packets)
            }
        }
    }

    @Test fun nextAcrossCachedBackwardSeekRestartsAtSegmentAndSampleEntryBoundary() {
        SavedReplay.open(fixture()).use { saved ->
            val beforeUs = saved.frames[11]["movie_pts_us"].asString.toLong()
            val boundaryUs = saved.frames[12]["movie_pts_us"].asString.toLong()
            val nextUs = saved.frames[13]["movie_pts_us"].asString.toLong()
            assertEquals(0, saved.frames[11]["segment_index"].asInt)
            assertEquals(1, saved.frames[11]["sample_entry"].asInt)
            assertEquals(1, saved.frames[12]["segment_index"].asInt)
            assertEquals(2, saved.frames[12]["sample_entry"].asInt)

            val packets = mutableListOf<Int>()
            val restarts = mutableListOf<Int>()
            ReplayFrameCursor(saved, packetReadObserver = { packets += it },
                gopRestartObserver = { restarts += it }).use { cursor ->
                assertEquals(11, cursor.seek(beforeUs).index)
                packets.clear()
                restarts.clear()
                assertEquals(11, cursor.seek(beforeUs).index)

                val boundary = assertNotNull(cursor.next())
                assertEquals(12, boundary.index)
                assertEquals(listOf(12), packets)
                assertEquals(listOf(12), restarts)
                val expectedBoundary = ReplayFrameExport.decodeAt(saved, boundaryUs).image
                assertContentEquals(expectedBoundary.getRGB(0, 0, expectedBoundary.width, expectedBoundary.height, null, 0, expectedBoundary.width),
                    boundary.image.getRGB(0, 0, boundary.image.width, boundary.image.height, null, 0, boundary.image.width))

                val next = assertNotNull(cursor.next())
                assertEquals(13, next.index)
                assertEquals(listOf(12, 13), packets)
                assertEquals(listOf(12), restarts)
                val expectedNext = ReplayFrameExport.decodeAt(saved, nextUs).image
                assertContentEquals(expectedNext.getRGB(0, 0, expectedNext.width, expectedNext.height, null, 0, expectedNext.width),
                    next.image.getRGB(0, 0, next.image.width, next.image.height, null, 0, next.image.width))
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

                val lastUs = saved.frames.last()["movie_pts_us"].asString.toLong()
                assertEquals(saved.frames.lastIndex, cursor.seek(lastUs).index)
                assertNull(cursor.next())
                assertNull(cursor.next())
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
