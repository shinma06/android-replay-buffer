package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonParser
import org.jcodec.codecs.h264.H264Decoder
import org.jcodec.codecs.h264.H264Encoder
import org.jcodec.codecs.h264.H264Utils
import org.jcodec.common.io.NIOUtils
import org.jcodec.common.model.ColorSpace
import org.jcodec.common.model.Picture
import org.jcodec.containers.mp4.MP4Packet
import org.jcodec.containers.mp4.boxes.VideoSampleEntry
import org.jcodec.containers.mp4.demuxer.AbstractMP4DemuxerTrack
import org.jcodec.containers.mp4.demuxer.MP4Demuxer
import java.nio.ByteBuffer
import java.nio.channels.FileChannel
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardOpenOption.WRITE
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

class ReplayTrimTest {

    private fun write(capture: FrozenCapture, root: Path) =
        SaveWriter().write(capture, root, { false }) { Files.move(it.partial, it.complete) }

    private fun trim(dir: Path, fromUs: Long, toUs: Long, cancelled: () -> Boolean = { false },
                     space: (Path) -> Long = { Files.getFileStore(it).usableSpace }) =
        ReplayTrim(space).trim(dir, fromUs, toUs, cancelled, { Files.move(it.partial, it.complete) })

    private fun manifest(dir: Path) = JsonParser.parseString(Files.readString(dir.resolve("replay.json"))).asJsonObject
    private fun rows(dir: Path) = Files.readAllLines(dir.resolve("video-frames.jsonl")).map { JsonParser.parseString(it).asJsonObject }
    private fun hashes(dir: Path) = Files.list(dir).use { it.toList().associate { p -> p.fileName.toString() to sha(Files.readAllBytes(p)) } }
    private fun sha(bytes: ByteArray) = java.security.MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }

    private fun packets(dir: Path): List<MP4Packet> {
        NIOUtils.readableChannel(dir.resolve("video.mp4").toFile()).use { input ->
            MP4Demuxer.createRawMP4Demuxer(input).use { demux ->
                val track = demux.videoTrack as AbstractMP4DemuxerTrack
                val list = mutableListOf<MP4Packet>()
                while (true) list += (track.nextFrame() as? MP4Packet ?: break)
                return list
            }
        }
    }

    private fun movBytes(packet: MP4Packet) = ByteArray(packet.data.remaining()).also { packet.data.duplicate().get(it) }

    /** Decodes every video sample; plane bytes at display size, one list entry per frame. */
    private fun decodeAll(dir: Path): List<List<ByteArray>> {
        NIOUtils.readableChannel(dir.resolve("video.mp4").toFile()).use { input ->
            MP4Demuxer.createRawMP4Demuxer(input).use { demux ->
                val track = demux.videoTrack as AbstractMP4DemuxerTrack
                var decoder = H264Decoder()
                val out = mutableListOf<List<ByteArray>>()
                while (true) {
                    val packet = track.nextFrame() as? MP4Packet ?: break
                    val entry = track.sampleEntries[packet.entryNo] as VideoSampleEntry
                    val config = H264Utils.parseAVCC(entry)
                    if (packet.isKeyFrame) decoder = H264Decoder().apply { addSps(config.spsList); addPps(config.ppsList) }
                    val planes = Picture.create((entry.width + 15) and -16, (entry.height + 15) and -16, ColorSpace.YUV420J).data
                    out += decoder.decodeFrame(H264Utils.decodeMOVPacket(packet.data, config), planes)
                        .cloneCropped().data.map { it.copyOf() }
                }
                return out
            }
        }
    }

    private fun meanSquareError(a: ByteArray, b: ByteArray): Double {
        require(a.size == b.size)
        var sum = 0.0
        for (i in a.indices) { val d = a[i].toInt() - b[i].toInt(); sum += d * d }
        return sum / a.size
    }

    private fun edits(dir: Path): List<org.jcodec.containers.mp4.boxes.Edit> {
        NIOUtils.readableChannel(dir.resolve("video.mp4").toFile()).use { input ->
            MP4Demuxer.createRawMP4Demuxer(input).use { demux ->
                return (demux.videoTrack as AbstractMP4DemuxerTrack).edits
            }
        }
    }

    /** Gapless one-run save: key interval 8 so every eighth sample starts a GOP. */
    @Test
    fun midGopStartReencodesTheLeadingGopAndCopiesLaterPackets() = withRuns(listOf(List(24) { it * 100_000L })) { root, _, capture, original, pixels ->
        val output = write(capture, root)
        val before = hashes(output.directory)
        val srcRows = rows(output.directory)
        val from = srcRows[11]["movie_pts_us"].asString.toLong() + 1
        val to = srcRows.last()["movie_end_us"].asString.toLong()
        val dir = trim(output.directory, from, to)
        assertTrue(dir.fileName.toString().matches(Regex("""replay-\d{8}-\d{6}-trim-[0-9a-z-]{8}""")))
        val outRows = rows(dir)
        assertEquals(13, outRows.size)
        // In lands on the frame shown at `from` (frame 11), never rounded back to the IDR at 8.
        assertEquals(srcRows[11]["source_pts_us"].asString, outRows[0]["source_pts_us"].asString)
        assertEquals(srcRows[23]["source_pts_us"].asString, outRows[12]["source_pts_us"].asString)
        assertTrue(outRows.take(5).all { it["reencoded"].asBoolean }) // GOP 8..15: 11..15 reencoded
        assertTrue(outRows.drop(5).all { !it["reencoded"].asBoolean }) // 16..23 copied
        assertTrue(outRows[0]["output_key"].asBoolean)
        assertEquals(0L, outRows[0]["movie_pts_us"].asString.toLong())
        assertEquals(srcRows.last()["movie_end_us"].asString.toLong() - srcRows[11]["movie_pts_us"].asString.toLong(),
            outRows.last()["movie_end_us"].asString.toLong())
        // Later original packets are copied unchanged.
        val srcPackets = packets(output.directory)
        val outPackets = packets(dir)
        (0 until 8).forEach { n -> assertContentEquals(movBytes(srcPackets[16 + n]), movBytes(outPackets[5 + n])) }
        // The decoded first output frame shows frame 11's image, so pre-start frames are not visible.
        val decoded = decodeAll(dir)
        assertEquals(13, decoded.size)
        assertTrue(meanSquareError(decoded[0][0], pixels[11][0]) < 4.0)
        assertTrue(meanSquareError(decoded[0][0], pixels[8][0]) > meanSquareError(decoded[0][0], pixels[11][0]))
        assertTrue(meanSquareError(decoded[5][0], pixels[16][0]) < 0.5)
        // Original folder is byte-identical after the operation.
        assertEquals(before, hashes(output.directory))
        // Output is a complete schema-4 save that the reader re-validates.
        SavedReplay.open(dir).use { assertTrue(it.manifest["complete"].asBoolean) }
        val trim = manifest(dir)["trim"].asJsonObject
        assertEquals(from.toString(), trim["from_movie_us"].asString)
        assertEquals(to.toString(), trim["to_movie_us"].asString)
        assertEquals(true, trim["reencoded_leading_gop"].asBoolean)
        assertEquals(output.directory.fileName.toString(), trim["source_folder_name"].asString)
        assertEquals(sha(Files.readAllBytes(output.directory.resolve("replay.json"))), trim["source_replay_sha256"].asString)
    }

    @Test
    fun fullStartDropsHiddenPrerollAndKeepsTheClippedDisplayDuration() = withRuns(
        listOf(List(16) { it * 100_000L }),
    ) { root, _, capture, _, pixels ->
        val video = capture.video.map { frame ->
            val t = frame.pts * 1000
            frame.copy(time = MappedTime(t, 0, 0, t))
        }
        val source = write(capture.copy(video = video, start = 150_000_000, end = 950_000_000,
            endUncertainty = 0, states = capture.states.mapValues { (_, value) -> value.copy(availableSeconds = 180.0) }), root)
        val sourceRows = rows(source.directory)
        assertFalse(sourceRows.first()["presented"].asBoolean)
        assertEquals("50000", sourceRows.first { it["presented"].asBoolean }["movie_end_us"].asString)
        val before = hashes(source.directory)
        val dir = trim(source.directory, 0, 50_000)
        assertEquals(1, rows(dir).size)
        assertEquals(1, packets(dir).size)
        assertEquals("100000", rows(dir).single()["source_pts_us"].asString)
        assertTrue(meanSquareError(decodeAll(dir).single()[0], pixels[1][0]) < 4.0)
        assertEquals(50_000L, edits(dir).single().duration)
        assertEquals(50_000L, edits(dir).single().mediaTime)
        val record = manifest(dir)
        assertEquals("50000000", record["timebase"].asJsonObject["window_duration_ns"].asString)
        assertEquals("50000000", record["trim"].asJsonObject["to_t_ns"].asString)
        assertTrue(record["coverage"].asJsonObject["video"].asJsonObject["availableSeconds"].asDouble <= 0.05)
        val repeated = trim(dir, 0, 50_000)
        assertEquals(50_000L, edits(repeated).single().mediaTime)
        assertEquals("50000000", manifest(repeated)["timebase"].asJsonObject["window_duration_ns"].asString)
        assertEquals(before, hashes(source.directory))
    }

    @Test
    fun videoCoverageUsesTheRebasedDeviceTimeline() = withRuns(listOf(List(16) { it * 100_000L })) { root, _, capture, _, _ ->
        val video = capture.video.map { frame ->
            val t = 10_000_000_000 + frame.pts * 1000
            frame.copy(time = MappedTime(t, 0, 0, t))
        }
        val source = write(capture.copy(video = video, start = 0, end = 12_000_000_000, endUncertainty = 0,
            states = capture.states.mapValues { (_, value) -> value.copy(availableSeconds = 180.0) }), root)
        val sourceRows = rows(source.directory)
        val dir = trim(source.directory, sourceRows[5]["movie_pts_us"].asString.toLong(), sourceRows[9]["movie_end_us"].asString.toLong())
        assertEquals(0.4, manifest(dir)["coverage"].asJsonObject["video"].asJsonObject["availableSeconds"].asDouble, 1e-9)
    }

    @Test
    fun schemaFourKindGapsReduceOnlyTheirStreamCoverage() {
        for (kind in listOf("video", "device_log", "app_log")) checkGapCoverage(kind,
            listOf(1_600_000_000L to 1_800_000_000L), listOf(200_000_000L to 400_000_000L), 0.2)
    }

    @Test
    fun gapsOutsideOrTouchingTheKeptRangeDoNotReduceCoverage() {
        for (kind in listOf("video", "device_log", "app_log")) checkGapCoverage(kind,
            listOf(1_100_000_000L to 1_400_000_000L, 2_200_000_000L to 2_400_000_000L), emptyList(), 0.0)
    }

    @Test
    fun gapsCrossingTheKeptBoundariesAreClippedBeforeSubtractingCoverage() {
        for (kind in listOf("video", "device_log", "app_log")) checkGapCoverage(kind,
            listOf(1_200_000_000L to 1_500_000_000L, 2_100_000_000L to 2_400_000_000L),
            listOf(0L to 100_000_000L, 700_000_000L to 800_000_000L), 0.2)
    }

    @Test
    fun openEndedGapCoverageStopsAtTheKeptWindowEnd() {
        for (kind in listOf("video", "device_log", "app_log")) checkGapCoverage(kind,
            listOf(1_900_000_000L to null), listOf(500_000_000L to null), 0.3)
    }

    private fun checkGapCoverage(kind: String, ranges: List<Pair<Long?, Long?>>, kept: List<Pair<Long?, Long?>>, missing: Double) =
        withRuns(listOf(List(16) { it * 100_000L })) { root, _, capture, _, _ ->
            val video = capture.video.map { frame ->
                val t = 1_000_000_000L + frame.pts * 1000
                frame.copy(time = MappedTime(t, 0, 0, t))
            }
            val logs = video.mapIndexed { n, frame ->
                LogEntry("$n", DeviceLog(1_700_000_000_000_000_000L + n * 100_000_000L, 12, 12, 10001, 0, 4,
                    "Fixture", "row $n", byteArrayOf(n.toByte())), frame.generation, frame.time, frame.retainedAt, true, frame.host)
            }
            val gaps = ranges.map { (from, to) -> CaptureGap(kind, from, to, "fixture gap", 0, 1, 0) } + listOf(
                CaptureGap("audio", 1_500_000_000L, 2_100_000_000L, "audio unavailable", 0, 1, 0),
                CaptureGap("clock", 1_500_000_000L, 1_600_000_000L, "clock unavailable", 0, 1, 0))
            val kinds = listOf("video", "device_log", "app_log")
            val source = write(capture.copy(video = video, logs = logs, start = 0, end = 3_000_000_000L,
                endUncertainty = 0, gaps = gaps, states = kinds.associateWith { StreamSnapshot(StreamState.CAPTURING, 1.5) }), root)
            val sourceRecord = manifest(source.directory)
            assertEquals(4, sourceRecord["schema"].asInt)
            val sourceGaps = sourceRecord["gaps"].asJsonArray.map { it.asJsonObject }
            assertEquals(ranges.size, sourceGaps.count { it["kind"].asString == kind })
            assertTrue(sourceGaps.all { it.has("kind") && !it.has("stream") })
            val before = hashes(source.directory)
            val sourceRows = rows(source.directory)
            val dir = trim(source.directory, sourceRows[4]["movie_pts_us"].asString.toLong(), sourceRows[11]["movie_end_us"].asString.toLong())
            val record = manifest(dir)
            assertEquals("800000000", record["timebase"].asJsonObject["window_duration_ns"].asString)
            for (stream in kinds) assertEquals((if (stream == "video") 0.7 else 0.8) - if (stream == kind) missing else 0.0,
                record["coverage"].asJsonObject[stream].asJsonObject["availableSeconds"].asDouble, 1e-9, "$kind / $stream")
            val keptGaps = record["gaps"].asJsonArray.map { it.asJsonObject }.filter { it["kind"].asString == kind }
            assertEquals(kept, keptGaps.map { gap ->
                gap["from_t_ns"].takeUnless { it.isJsonNull }?.asString?.toLong() to
                    gap["to_t_ns"].takeUnless { it.isJsonNull }?.asString?.toLong()
            })
            val clock = record["timebase"].asJsonObject["clock_steps"].asJsonArray.single().asJsonObject
            assertEquals("unmeasured", clock["kind"].asString)
            assertEquals("100000000", clock["from_t_ns"].asString)
            assertEquals("200000000", clock["to_t_ns"].asString)
            assertEquals(sourceRecord["timebase"].asJsonObject["known"], record["timebase"].asJsonObject["known"])
            assertEquals(0.0, record["coverage"].asJsonObject["audio"].asJsonObject["availableSeconds"].asDouble)
            val trimmedBefore = hashes(dir)
            val again = manifest(trim(dir, 0, 800_000))
            assertEquals(record["coverage"], again["coverage"])
            assertEquals(record["gaps"], again["gaps"])
            assertEquals(before, hashes(source.directory))
            assertEquals(trimmedBefore, hashes(dir))
        }

    @Test
    fun startOnAnIdrStillProducesAReadableTrimmedPart() = withRuns(listOf(List(16) { it * 100_000L })) { root, _, capture, _, _ ->
        val output = write(capture, root)
        val srcRows = rows(output.directory)
        val dir = trim(output.directory, srcRows[8]["movie_pts_us"].asString.toLong(),
            srcRows.last()["movie_end_us"].asString.toLong())
        assertEquals(8, rows(dir).size)
        assertEquals(srcRows[8]["source_pts_us"].asString, rows(dir)[0]["source_pts_us"].asString)
    }

    /** Two runs with a known capture gap: the movie keeps an empty edit and rebased times. */
    @Test
    fun keptPartsAcrossAKnownGapKeepTheEmptyEditAndShiftTimes() = withRuns(
        listOf(listOf(0, 100_000, 350_000), listOf(200_000, 410_000, 700_000)),
    ) { root, _, capture, _, _ ->
        val video = capture.video.mapIndexed { n, frame ->
            val sequence = if (n < 3) (frame.pts - 0) * 1000 else 5_000_000_000 + (frame.pts - 200_000) * 1000
            frame.copy(time = MappedTime(sequence, 0, 0, sequence))
        }
        val gap = CaptureGap("video", 2_000_000_000, 4_000_000_000, "disconnect", 0, 1, 0)
        val output = write(capture.copy(video = video, start = 0, end = 6_000_000_000, endUncertainty = 0,
            gaps = listOf(gap), states = emptyMap()), root)
        val srcRows = rows(output.directory)
        // Original: part1 [0, 500_001), gap, part2 -> movie [2_500_001, 3_000_002+...]; trim inside part1 to inside part2.
        val parts = manifest(output.directory)["parts"].asJsonArray
        assertEquals(2, parts.size())
        val from = srcRows[1]["movie_pts_us"].asString.toLong() + 1
        val to = srcRows[4]["movie_end_us"].asString.toLong()
        val dir = trim(output.directory, from, to)
        val outEdits = edits(dir)
        assertTrue(outEdits.any { it.mediaTime == -1L && it.duration == 2_000_000L })
        val outRows = rows(dir)
        val startUs = srcRows[1]["movie_pts_us"].asString.toLong()
        assertEquals(0L, outRows[0]["movie_pts_us"].asString.toLong())
        val firstPart = manifest(dir)["parts"].asJsonArray[0].asJsonObject
        assertEquals("0", firstPart["movie_start_us"].asString)
        assertTrue(outRows.all { it["t_ns"].isJsonNull || it["t_ns"].asString.toLong() >= 0 })
        // t_ns rebased: the first presented frame's display start is the origin.
        assertEquals(0L, outRows[0]["display_t_ns"].asString.toLong())
        assertEquals(1, manifest(dir)["video_run_boundaries"].asJsonArray.size())
    }

    /** The player's movie-time model: displayed spans, gap zones, taps and the clock flag. */
    @Test
    fun timelineMapsSpansGapsTapsAndClockFlag() = withRuns(
        listOf(listOf(0, 100_000, 350_000), listOf(200_000, 410_000, 700_000)),
    ) { root, _, capture, _, _ ->
        val video = capture.video.mapIndexed { n, frame ->
            val sequence = if (n < 3) frame.pts * 1000 else 5_000_000_000 + (frame.pts - 200_000) * 1000
            frame.copy(time = MappedTime(sequence, 0, 0, sequence))
        }
        val gap = CaptureGap("video", 2_000_000_000, 4_000_000_000, "disconnect", 0, 1, 0)
        val output = write(capture.copy(video = video, start = 0, end = 6_000_000_000, endUncertainty = 0,
            gaps = listOf(gap), states = emptyMap()), root)
        SavedReplay.open(output.directory).use { saved ->
            val timeline = saved.timeline()
            val movieEnd = manifest(output.directory)["parts"].asJsonArray.last().asJsonObject["movie_end_us"].asString.toLong()
            assertEquals(movieEnd, timeline.durationUs)
            assertEquals(rows(output.directory).count { it["presented"].asBoolean }, timeline.spans.size)
            assertEquals(0L, timeline.spans.first().startUs)
            // The capture gap stays an uncovered movie interval between the two runs' spans.
            assertEquals(1, timeline.gaps.size)
            val hole = timeline.gaps.single()
            assertTrue(timeline.spans.none { it.startUs < hole.endUs && it.endUs > hole.startUs })
            assertEquals(-1, timeline.spanAt((hole.startUs + hole.endUs) / 2))
            assertTrue(timeline.spanAt(0) >= 0)
            assertTrue(timeline.taps.isEmpty())
            assertTrue(timeline.clockKnown)
        }
    }

    /** Normalized save: the sidecar is regenerated for kept samples only. */
    @Test
    fun rawSidecarIsRegeneratedForKeptSamples() = withRuns(
        List(2) { run -> List(12) { n -> n * 100_000L + run * 2_000_000L } }, listOf(64 to 32, 32 to 64), knownColour = true,
    ) { root, _, capture, original, _ ->
        val video = capture.video.mapIndexed { n, frame ->
            val sequence = (if (n < 12) 0L else 5_000_000_000) + (n % 12) * 100_000_000L
            frame.copy(time = MappedTime(sequence, 0, 0, sequence))
        }
        val output = write(capture.copy(video = video, start = 0, end = 6_000_000_000, endUncertainty = 0, states = emptyMap()), root)
        assertTrue(Files.exists(output.directory.resolve("video-source.h264")))
        val srcRows = rows(output.directory)
        val from = srcRows[14]["movie_pts_us"].asString.toLong()
        val to = srcRows.last()["movie_end_us"].asString.toLong()
        val dir = trim(output.directory, from, to)
        val sidecar = Files.readAllBytes(dir.resolve("video-source.h264"))
        val outRows = rows(dir)
        assertTrue(outRows.all { it["source_file"].asString == "video-source.h264" })
        outRows.forEachIndexed { n, row ->
            val sourceIndex = srcRows.indexOfFirst { it["source_pts_us"].asString == row["source_pts_us"].asString }
            assertContentEquals(original[sourceIndex],
                sidecar.copyOfRange(row["source_offset"].asString.toInt(), row["source_offset"].asString.toInt() + row["source_size"].asString.toInt()))
        }
    }

    @Test
    fun preflightUsesSnappedDisplayedFramesRatherThanTheWholePartFlag() = withRuns(listOf(List(12) { it * 100_000L })) { root, _, capture, _, _ ->
        val video = capture.video.mapIndexed { index, frame -> frame.copy(time =
            if (index == 5) MappedTime(null, 0, Long.MAX_VALUE, null) else MappedTime(frame.pts * 1000, 0, 0)) }
        val source = write(capture.copy(video = video, start = 0, end = 1_200_000_000, endUncertainty = 0), root).directory
        assertFalse(manifest(source)["parts"].asJsonArray.single().asJsonObject["clock_alignment_known"].asBoolean)
        val before = hashes(source)
        val siblings = Files.list(root).use { it.toList().toSet() }
        val trim = ReplayTrim()
        for ((from, to) in listOf(300_001L to 499_999L, 600_000L to 700_000L)) {
            val plan = trim.preflight(source, from, to)
            assertFalse(plan.clockUnknown)
            assertNull(plan.warning)
        }
        val unknown = trim.preflight(source, 500_001, 599_999)
        assertEquals(500_000L, unknown.startUs)
        assertEquals(600_000L, unknown.endUs)
        assertTrue(unknown.clockUnknown)
        assertNotNull(unknown.warning)
        assertTrue(trim.preflight(source, 499_999, 500_001).clockUnknown)
        assertEquals(before, hashes(source))
        assertEquals(siblings, Files.list(root).use { it.toList().toSet() })
        val output = trim.trim(source, 500_001, 599_999, { false }, { Files.move(it.partial, it.complete) },
            expectedFingerprint = unknown.fingerprint)
        assertTrue(rows(output).all { !it["clock_alignment_known"].asBoolean && it["t_ns"].isJsonNull })
        assertEquals(before, hashes(source))
    }

    @Test
    fun preflightIgnoresUnknownDecodePrerollThatIsNotDisplayed() = withRuns(listOf(List(12) { it * 100_000L })) { root, _, capture, _, _ ->
        val video = capture.video.mapIndexed { index, frame -> frame.copy(time =
            if (index == 0) MappedTime(null, 0, Long.MAX_VALUE, null) else MappedTime(frame.pts * 1000, 0, 0)) }
        val source = write(capture.copy(video = video, start = 300_000_000, end = 1_200_000_000, endUncertainty = 0), root).directory
        assertTrue(rows(source).any { !it["presented"].asBoolean && !it["clock_alignment_known"].asBoolean })
        assertFalse(ReplayTrim().preflight(source, 1, 99_999).clockUnknown)
    }

    @Test
    fun preflightFingerprintRejectsChangedSourceAndRangeBeforeStaging() = withRuns(listOf(List(12) { it * 100_000L })) { root, _, capture, _, _ ->
        val source = write(capture, root).directory
        val trim = ReplayTrim()
        val plan = trim.preflight(source, 100_001, 500_000)
        fun rejected(from: Long, to: Long) {
            val siblings = Files.list(root).use { it.toList().toSet() }
            val failure = assertFailsWith<SaveFailure> {
                trim.trim(source, from, to, { false }, { error("stale preflight must not publish") }, expectedFingerprint = plan.fingerprint)
            }
            assertTrue(failure.message.orEmpty().contains("事前確認後"))
            assertEquals(siblings, Files.list(root).use { it.toList().toSet() })
        }
        rejected(200_000, 500_000)
        val record = manifest(source).apply { addProperty("created_at", "2024-02-01T00:00:00Z") }
        Files.writeString(source.resolve("replay.json"), record.toString())
        rejected(100_001, 500_000)
        assertTrue(plan.fingerprint != trim.preflight(source, 100_001, 500_000).fingerprint)
    }

    /** Unknown-clock save: every t_ns stays null and unmapped rows are never dropped. */
    @Test
    fun unknownClockKeepsNullTimesAndEveryLogRow() {
        val root = Files.createTempDirectory("replay-trim-unknown-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        try {
            store.generation(1)
            store.session(VideoPacket.Session(32, 32), 1)
            val encoder = H264Encoder.createH264Encoder().apply { setKeyInterval(8) }
            for (n in 0 until 10) {
                val encoded = encoder.encodeFrame(Picture.create(32, 32, ColorSpace.YUV420J).apply { fill(20 + n) },
                    ByteBuffer.allocate(32 * 32 * 3)).data
                val bytes = ByteArray(encoded.remaining()).also { encoded.get(it) }
                if (n == 0) {
                    val ps = H264Utils.splitFrame(ByteBuffer.wrap(bytes)).filter { it.get(0).toInt() and 31 in setOf(7, 8) }
                        .fold(byteArrayOf()) { r, nal -> r + byteArrayOf(0, 0, 0, 1) + ByteArray(nal.remaining()).also { nal.get(it) } }
                    store.frame(VideoPacket.Frame(0, true, false, ps), 1)
                }
                store.frame(VideoPacket.Frame(1_000_000L + n * 100_000, false, n == 0, bytes), 1)
            }
            store.status("device_log", StreamState.CAPTURING, null, 1)
            for (n in 0 until 5) store.log(DeviceLog(1_700_000_001_000_000_000 + n, 12, 12, 10001, 0, 4,
                "Fixture", "row $n", byteArrayOf(n.toByte())), 1)
            val output = write(store.capture(ReplaySettings())!!, root)
            val srcRows = rows(output.directory)
            val before = hashes(output.directory)
            val trim = ReplayTrim()
            val from = srcRows[3]["movie_pts_us"].asLong
            val to = srcRows[6]["movie_end_us"].asLong
            val plan = trim.preflight(output.directory, from, to)
            assertTrue(plan.clockUnknown)
            val dir = trim.trim(output.directory, from, to, { false }, { Files.move(it.partial, it.complete) }, expectedFingerprint = plan.fingerprint)
            assertTrue(rows(dir).all { it["t_ns"].isJsonNull && it["display_t_ns"].isJsonNull })
            assertEquals(4, rows(dir).size)
            assertEquals(Files.readAllLines(output.directory.resolve("logcat-device.jsonl")),
                Files.readAllLines(dir.resolve("logcat-device.jsonl")))
            assertFalse(manifest(dir)["timebase"].asJsonObject["known"].asBoolean)
            assertEquals(before, hashes(output.directory))
        } finally {
            store.close()
            Files.walk(root).use { files -> files.sorted(Comparator.reverseOrder()).forEach(Files::deleteIfExists) }
        }
    }

    @Test
    fun trimmedLogsAndTouchesAreFilteredByMappedTime() {
        val root = Files.createTempDirectory("replay-trim-logs-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        try {
            val host = System.nanoTime() + 1_000_000_000
            store.generation(1)
            sample(store, host, 1)
            store.session(VideoPacket.Session(32, 32), 1)
            val encoder = H264Encoder.createH264Encoder().apply { setKeyInterval(30) }
            for (n in 0 until 10) {
                val encoded = encoder.encodeFrame(Picture.create(32, 32, ColorSpace.YUV420J).apply { fill(20 + n) },
                    ByteBuffer.allocate(32 * 32 * 3)).data
                val bytes = ByteArray(encoded.remaining()).also { encoded.get(it) }
                if (n == 0) {
                    val ps = H264Utils.splitFrame(ByteBuffer.wrap(bytes)).filter { it.get(0).toInt() and 31 in setOf(7, 8) }
                        .fold(byteArrayOf()) { r, nal -> r + byteArrayOf(0, 0, 0, 1) + ByteArray(nal.remaining()).also { nal.get(it) } }
                    store.frame(VideoPacket.Frame(0, true, false, ps), 1)
                }
                store.frame(VideoPacket.Frame(1_000_000L + n * 100_000, false, n == 0, bytes), 1, host + n * 100_000_000L)
            }
            store.status("device_log", StreamState.CAPTURING, null, 1)
            for (n in 0 until 10) store.log(DeviceLog(1_700_000_000_000_000_000 + 1_000_000_000 + n * 100_000_000, 12, 12, 10001, 0, 4,
                "Fixture", "row $n", byteArrayOf(n.toByte())), 1, host + n * 100_000_000)
            store.touchStatus(StreamState.CAPTURING, null, 1)
            store.touch(TouchSample(1_050_000_000, "/dev/input/event2", 0, TouchAction.DOWN, 10, 20), 32, 32, 1, host + 50_000_000)
            store.touch(TouchSample(1_300_000_000, "/dev/input/event2", 0, TouchAction.MOVE, 10, 20), 32, 32, 1, host + 300_000_000)
            store.touch(TouchSample(1_550_000_000, "/dev/input/event2", 0, TouchAction.UP, 10, 20), 32, 32, 1, host + 550_000_000)
            sample(store, host, 2)
            val capture = store.capture(ReplaySettings(), null, host + 1_000_000_000)!!
            val output = write(capture, root)
            val srcRows = rows(output.directory)
            val from = srcRows[3]["movie_pts_us"].asString.toLong()
            val to = srcRows[7]["movie_end_us"].asString.toLong()
            val originTns = srcRows[3]["display_t_ns"].asString.toLong()
            val dir = trim(output.directory, from, to)
            val logs = Files.readAllLines(dir.resolve("logcat-device.jsonl"))
                .map { JsonParser.parseString(it).asJsonObject }
            assertTrue(logs.isNotEmpty())
            assertTrue(logs.any { it["t_ns"].asString == "0" })
            val srcLogs = Files.readAllLines(output.directory.resolve("logcat-device.jsonl")).size
            assertTrue(logs.size < srcLogs)
            val touches = Files.readAllLines(dir.resolve("touches.jsonl")).map { JsonParser.parseString(it).asJsonObject }
            val srcTouches = Files.readAllLines(output.directory.resolve("touches.jsonl"))
                .map { JsonParser.parseString(it).asJsonObject }.associateBy { it["device_mono_ns"].asString }
            assertEquals(2, touches.size) // The earlier touch is provably before the trim range.
            assertTrue(touches.any { it["t_ns"].asString == "0" })
            val touch = touches.single { it["t_ns"].asString != "0" }
            assertEquals(srcTouches.getValue(touch["device_mono_ns"].asString)["t_ns"].asString.toLong() - originTns,
                touch["t_ns"].asString.toLong())
            val srcMovieUs = srcTouches.getValue(touch["device_mono_ns"].asString)["movie_us"].asString.toLong()
            assertEquals(srcMovieUs - from, touch["movie_us"].asString.toLong())
            val manifest = manifest(dir)
            assertTrue(manifest["parts"].asJsonArray[0].asJsonObject["t_start_ns"].asString == "0")
            val available = manifest["coverage"].asJsonObject["device_log"].asJsonObject["availableSeconds"].asDouble
            assertTrue(available in 0.0..0.5)
            assertTrue(available < manifest(output.directory)["coverage"].asJsonObject["device_log"].asJsonObject["availableSeconds"].asDouble)
        } finally {
            store.close()
            Files.walk(root).use { files -> files.sorted(Comparator.reverseOrder()).forEach(Files::deleteIfExists) }
        }
    }

    @Test
    fun audioSegmentsAreClippedToTheKeptRange() = checkAudioTrim(false)

    @Test
    fun audioGapsSurviveTrimmingEvenWhenPacketNumbersAreConsecutive() = checkAudioTrim(true)

    @Test
    fun audioWritingStopsBeforeItUsesTheReservedSpace() = checkAudioTrim(false, checkAudioSpace = true)

    @Test
    fun audioSpaceGuardIgnoresCompletedReplaysThroughAnAliasedParent() =
        checkAudioTrim(false, checkAudioSpace = true, aliasedSpaceParent = true)

    private fun checkAudioTrim(withGap: Boolean, checkAudioSpace: Boolean = false, aliasedSpaceParent: Boolean = false) {
        val root = Files.createTempDirectory("replay-trim-audio-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0, audioRequested = true)
        try {
            val host = System.nanoTime() + 1_000_000_000
            store.generation(1)
            sample(store, host, 1)
            store.session(VideoPacket.Session(32, 32), 1)
            val encoder = H264Encoder.createH264Encoder().apply { setKeyInterval(30) }
            for (n in 0 until 10) {
                val encoded = encoder.encodeFrame(Picture.create(32, 32, ColorSpace.YUV420J).apply { fill(20 + n) },
                    ByteBuffer.allocate(32 * 32 * 3)).data
                val bytes = ByteArray(encoded.remaining()).also { encoded.get(it) }
                if (n == 0) {
                    val ps = H264Utils.splitFrame(ByteBuffer.wrap(bytes)).filter { it.get(0).toInt() and 31 in setOf(7, 8) }
                        .fold(byteArrayOf()) { r, nal -> r + byteArrayOf(0, 0, 0, 1) + ByteArray(nal.remaining()).also { nal.get(it) } }
                    store.frame(VideoPacket.Frame(0, true, false, ps), 1)
                }
                store.frame(VideoPacket.Frame(1_000_000L + n * 100_000, false, n == 0, bytes), 1, host + n * 100_000_000L)
            }
            store.audioSession(1)
            store.audio(AudioPacket(0, true, byteArrayOf(0x11, 0x90.toByte())), 1)
            val unit = 1024 * 1_000_000L / 48_000
            for (i in 0 until 40) {
                if (withGap && i in 27..32) continue
                store.audio(AudioPacket(1_000_000 + i * unit, false, byteArrayOf(0x21, i.toByte())), 1, host + i * unit * 1000)
            }
            sample(store, host, 2)
            val capture = store.capture(ReplaySettings(recordAudio = true), null, host + 1_000_000_000)!!
            val output = write(capture, root)
            assertFalse(manifest(output.directory)["audio_track"].isJsonNull)
            val srcRows = rows(output.directory)
            val from = srcRows[5]["movie_pts_us"].asString.toLong()
            val to = srcRows.last()["movie_end_us"].asString.toLong()
            val dir = trim(output.directory, from, to)
            val track = manifest(dir)["audio_track"].asJsonObject
            val segments = track["segments"].asJsonArray
            assertTrue(segments.size() > 0)
            assertEquals(segments.sumOf { it.asJsonObject["duration_us"].asString.toLong() } / 1e6,
                manifest(dir)["coverage"].asJsonObject["audio"].asJsonObject["availableSeconds"].asDouble, 1e-9)
            assertTrue(segments.all { it.asJsonObject["movie_start_us"].asString.toLong() >= 0 })
            val keptSamples = segments.sumOf { it.asJsonObject["samples"].asInt }
            if (checkAudioSpace) {
                val videoEnd = packets(dir).maxOf { it.fileOff + it.data.remaining() }
                val audioPacketBytes = 2L
                var insufficientAt: Long? = null
                val before = hashes(output.directory)
                val failure = assertFailsWith<SaveFailure> {
                    trim(output.directory, from, to, space = {
                        val partialVideo = Files.list(if (aliasedSpaceParent) root.resolve(".") else root).use { folders ->
                            folders.filter { !Files.isSameFile(it, output.directory) && !Files.isSameFile(it, dir) }
                                .map { it.resolve("video.mp4") }.filter { Files.isRegularFile(it) }.findFirst().orElse(null)
                        }
                        val bytes = partialVideo?.let(Files::size) ?: 0
                        if (bytes > videoEnd) {
                            if (insufficientAt == null) insufficientAt = bytes
                            ReplaySettings.MIN_FREE_BYTES
                        } else Long.MAX_VALUE
                    })
                }
                assertNotNull(insufficientAt)
                assertTrue(insufficientAt <= videoEnd + audioPacketBytes,
                    "容量下限の確認前に音声をまとめて書き込んだ: $insufficientAt > ${videoEnd + audioPacketBytes}")
                val partial = assertNotNull(failure.partial)
                assertTrue(Files.isDirectory(partial))
                assertTrue(Files.size(partial.resolve("video.mp4")) <= videoEnd + audioPacketBytes)
                assertEquals(before, hashes(output.directory))
                val expectedFolders = setOf(root.resolve("ring"), output.directory, dir, partial).map { it.toRealPath() }.toSet()
                val actualFolders = Files.list(root).use { folders -> folders.map { it.toRealPath() }.toList() }
                assertEquals(expectedFolders.size, actualFolders.size)
                assertEquals(expectedFolders, actualFolders.toSet())
            }
            NIOUtils.readableChannel(dir.resolve("video.mp4").toFile()).use { channel ->
                MP4Demuxer.createRawMP4Demuxer(channel).use { demuxer ->
                    val audio = demuxer.audioTracks.single() as AbstractMP4DemuxerTrack
                    assertEquals(keptSamples, audio.meta.totalFrames)
                    assertEquals(segments.map { it.asJsonObject.let { row ->
                        row["duration_us"].asString.toLong() to row["media_start_samples"].asString.toLong()
                    } }, audio.edits.filter { it.mediaTime >= 0 }.map { it.duration to it.mediaTime })
                    assertEquals(withGap, audio.edits.any { it.mediaTime == -1L })
                }
            }
            assertEquals("0", segments[0].asJsonObject["movie_start_us"].asString)
            assertEquals(448L, segments[0].asJsonObject["media_start_samples"].asString.toLong())
            val expected = manifest(output.directory)["audio_track"].asJsonObject["segments"].asJsonArray.mapNotNull {
                val segment = it.asJsonObject
                val begin = segment["movie_start_us"].asString.toLong()
                val end = begin + segment["duration_us"].asString.toLong()
                if (end <= from || begin >= to) null else (maxOf(begin, from) - from) to (minOf(end, to) - maxOf(begin, from))
            }
            assertEquals(if (withGap) 2 else 1, expected.size)
            assertEquals(expected, segments.map { it.asJsonObject.let { row ->
                row["movie_start_us"].asString.toLong() to row["duration_us"].asString.toLong()
            } })
            SavedReplay.open(dir).use { saved -> ReplayAudioTrack.open(saved)!!.use { audio ->
                assertEquals(448L, (audio.schedule(0).first() as AudioChunk.Packets).skipSamples)
                assertEquals(withGap, audio.schedule(0).any { it is AudioChunk.Silence })
            } }
            val again = trim(dir, 0, to - from)
            assertEquals(segments, manifest(again)["audio_track"].asJsonObject["segments"])
            assertEquals(manifest(dir)["coverage"].asJsonObject["audio"], manifest(again)["coverage"].asJsonObject["audio"])
        } finally {
            store.close()
            Files.walk(root).use { files -> files.sorted(Comparator.reverseOrder()).forEach(Files::deleteIfExists) }
        }
    }

    @Test
    fun cancelledAndFailedTrimsNeverPublishACompleteFolder() = withRuns(listOf(List(16) { it * 100_000L })) { root, _, capture, _, _ ->
        val output = write(capture, root)
        val end = rows(output.directory).last()["movie_end_us"].asString.toLong()
        var checks = 0
        assertFailsWith<java.util.concurrent.CancellationException> {
            trim(output.directory, 0, end, cancelled = { ++checks > 4 })
        }
        assertFailsWith<SaveFailure> {
            trim(output.directory, 0, end, space = { 0 })
        }
        Files.list(root.resolve(".")).use { paths ->
            assertFalse(paths.anyMatch { it.fileName.toString().startsWith("replay-") && !Files.isSameFile(it, output.directory) })
        }
        // A successful trim still works afterwards and the original stays readable.
        val dir = trim(output.directory, 0, rows(output.directory).last()["movie_end_us"].asString.toLong())
        SavedReplay.open(dir).use { assertEquals(16, it.frames.size) }
    }

    @Test
    fun parentAliasesWorkWithoutFollowingTheReplayFolderOrFiles() = withRuns(listOf(List(8) { it * 100_000L })) { root, _, capture, _, _ ->
        val output = write(capture, root)
        val alias = Files.createSymbolicLink(root.resolve("linked-parent"), root).resolve(output.directory.fileName)
        SavedReplay.open(alias).use { assertEquals(output.directory, it.directory) }
        val end = rows(output.directory).last()["movie_end_us"].asString.toLong()
        assertTrue(Files.isRegularFile(trim(alias, 0, end).resolve("replay.json")))
        assertTrue(Files.isRegularFile(ReplayFrameExport.export(alias, 0, root)))
        val linkedReplay = Files.createSymbolicLink(root.resolve("linked-replay"), output.directory)
        assertFailsWith<Exception> { SavedReplay.open(linkedReplay) }
        val video = output.directory.resolve("video.mp4")
        val outside = Files.move(video, root.resolve("outside.mp4"))
        Files.createSymbolicLink(video, outside)
        assertFailsWith<Exception> { SavedReplay.open(alias) }
    }

    @Test
    fun invalidInputsFailWithJapaneseReasons() = withRuns(listOf(List(8) { it * 100_000L })) { root, _, capture, _, _ ->
        val output = write(capture, root)
        val srcRows = rows(output.directory)
        val end = srcRows.last()["movie_end_us"].asString.toLong()
        assertFailsWith<Exception> { trim(output.directory.resolve("missing"), 0, end) }
        assertFailsWith<Exception> { trim(output.directory, -1, end) }
        assertFailsWith<Exception> { trim(output.directory, end, end) }
        assertFailsWith<Exception> { trim(output.directory, end + 1, end + 2) }
        // Tampered files are rejected by files_sha256 before any output is staged.
        val bytes = Files.readAllBytes(output.directory.resolve("video.mp4"))
        val flip = bytes.size / 2
        FileChannel.open(output.directory.resolve("video.mp4"), WRITE).use {
            it.write(ByteBuffer.wrap(byteArrayOf((bytes[flip].toInt() xor 1).toByte())), flip.toLong())
        }
        val failure = assertFailsWith<Exception> { trim(output.directory, 0, end) }
        assertTrue(failure.message!!.contains("変更") || failure.cause?.message?.contains("変更") == true)
        Files.write(output.directory.resolve("video.mp4"), bytes)
        val dir = trim(output.directory, 0, end)
        assertEquals(end, manifest(dir)["video_movie_duration_us"].asString.toLong())
    }

    @Test
    fun frameExportWritesPngWithTimeMetadataWithoutOverwrite() = withRuns(listOf(List(10) { it * 100_000L })) { root, _, capture, _, pixels ->
        val output = write(capture, root)
        val at = rows(output.directory)[4]["movie_pts_us"].asString.toLong() + 1
        val destination = Files.createDirectory(root.resolve("shots"))
        val file = ReplayFrameExport.export(output.directory, at, destination)
        assertTrue(file.fileName.toString().matches(Regex("""frame-\d{8}-\d{6}-[0-9a-z-]{8}\.png""")))
        val bytes = Files.readAllBytes(file)
        validateScreenshotPng(bytes)
        val text = String(bytes, Charsets.ISO_8859_1)
        assertTrue(text.contains("arb:movie_us"))
        assertTrue(text.contains("arb:source_folder"))
        val image = javax.imageio.ImageIO.read(file.toFile())
        assertEquals(32, image.width); assertEquals(32, image.height)
        // A second export must not overwrite the first.
        val second = ReplayFrameExport.export(output.directory, at, destination)
        assertTrue(second != file && Files.exists(file) && Files.exists(second))
        val fixed = java.time.ZonedDateTime.parse("2024-01-01T00:00:00Z")
        ReplayFrameExport.export(output.directory, at, destination, fixed, "deadbeef")
        assertFailsWith<Exception> { ReplayFrameExport.export(output.directory, at, destination, fixed, "deadbeef") }
        // Times in a movie gap or beyond the movie fail without creating output.
        assertFailsWith<Exception> { ReplayFrameExport.export(output.directory, 999_999_999_999, destination) }
    }

    @Test
    fun frameCursorPlaysForwardWithTheSamePixelsAsDecodeAt() = withRuns(listOf(List(20) { it * 100_000L })) { root, _, capture, _, _ ->
        val output = write(capture, root)
        val srcRows = rows(output.directory)
        SavedReplay.open(output.directory).use { saved ->
            fun pixels(image: java.awt.image.BufferedImage) = IntArray(image.width * image.height).also { image.getRGB(0, 0, image.width, image.height, it, 0, image.width) }
            ReplayFrameCursor(saved).use { cursor ->
                // Seek lands mid-GOP (frame 10 in GOP 8..15) and then plays forward to the end.
                val start = srcRows[10]["movie_pts_us"].asString.toLong()
                var frame: ReplayFrameCursor.Decoded? = cursor.seek(start + 1)
                var index = 10
                while (frame != null) {
                    assertEquals(index, frame.index)
                    assertEquals(srcRows[index]["movie_pts_us"].asString.toLong(), frame.startUs)
                    val expected = ReplayFrameExport.decodeAt(saved, frame.startUs).image
                    assertContentEquals(pixels(expected), pixels(frame.image))
                    frame = cursor.next()
                    index++
                }
                assertEquals(20, index)
                // Seeking backwards restarts from that frame's IDR.
                assertEquals(3, cursor.seek(srcRows[3]["movie_pts_us"].asString.toLong()).index)
            }
        }
    }

    @Test
    fun playbackReportsKnownGapsInsteadOfHoldingTheLastImage() = withRuns(
        listOf(listOf(1_000_000, 1_100_000, 1_350_000), listOf(200_000, 410_000, 700_000)),
    ) { root, _, capture, _, _ ->
        val video = capture.video.mapIndexed { n, frame ->
            val point = if (n < 3) (frame.pts - 1_000_000) * 1000 else 5_000_000_000 + (frame.pts - 200_000) * 1000
            frame.copy(time = MappedTime(point, 0, 0, point))
        }
        val output = write(capture.copy(video = video, start = 0, end = 6_000_000_000, endUncertainty = 0,
            gaps = listOf(CaptureGap("video", 2_000_000_000, 4_000_000_000, "disconnect", 0, 1, 0)), states = emptyMap()), root)
        for (from in listOf(1_900_000L, 2_100_000L)) {
            val gap = java.util.concurrent.CompletableFuture<Long>()
            val session = io.github.shinma06.replaybuffer.ui.ReplayPlaybackSession(output.directory, from, 4_100_000,
                onFrame = {}, onGap = { gap.complete(it) }, onFinished = { failure ->
                    gap.completeExceptionally(AssertionError(failure ?: "欠落が通知されませんでした"))
                }, onAudioUnavailable = {})
            session.start()
            try { assertTrue(gap.get(10, java.util.concurrent.TimeUnit.SECONDS) in maxOf(from, 2_000_000)..3_999_999) }
            finally { session.stop() }
        }
    }

    @Test
    fun playbackSessionShowsFramesInRealTimeAndStopsAtTheEnd() = withRuns(listOf(List(16) { it * 100_000L })) { root, _, capture, _, _ ->
        val output = write(capture, root)
        val srcRows = rows(output.directory)
        val from = srcRows[5]["movie_pts_us"].asString.toLong()
        val until = srcRows[12]["movie_pts_us"].asString.toLong()
        val shown = java.util.Collections.synchronizedList(mutableListOf<Int>())
        val finished = java.util.concurrent.CompletableFuture<String?>()
        val audio = java.util.concurrent.atomic.AtomicReference<String?>()
        val started = System.nanoTime()
        val session = io.github.shinma06.replaybuffer.ui.ReplayPlaybackSession(output.directory, from, until,
            onFrame = { shown += it.index }, onFinished = { finished.complete(it) }, onAudioUnavailable = { audio.set(it) })
        session.start()
        assertEquals(null, finished.get(10, java.util.concurrent.TimeUnit.SECONDS))
        val elapsedMs = (System.nanoTime() - started) / 1_000_000
        assertTrue(elapsedMs >= (until - from) / 1000 * 8 / 10, "played too fast: $elapsedMs ms")
        assertEquals(5, shown.first())
        assertTrue(shown.zipWithNext().all { (a, b) -> b > a }) // forward only, late frames may be skipped
        assertTrue(shown.last() in 9..11) // stops before Out
        assertEquals(null, audio.get()) // a save without audio plays video only, silently
        session.stop()
    }

    private fun sample(store: CaptureStore, host: Long, second: Long) {
        val elapsed = second * 1_000_000_000
        val received = host + (second - 1) * 1_000_000_000
        assertTrue(store.clock.add("1", listOf(elapsed, elapsed, 1_700_000_000_000_000_000 + elapsed, elapsed), received, received))
        store.clockStatus(true, 1)
    }

    private fun withRuns(pts: List<List<Long>>, sizes: List<Pair<Int, Int>> = pts.map { 32 to 32 }, knownColour: Boolean = false,
                         check: (Path, CaptureStore, FrozenCapture, List<ByteArray>, List<List<ByteArray>>) -> Unit) {
        val root = Files.createTempDirectory("replay-trim-runs-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        val original = mutableListOf<ByteArray>()
        val pixels = mutableListOf<List<ByteArray>>()
        try {
            pts.forEachIndexed { run, times ->
                store.generation(run + 1L)
                val (width, height) = sizes[run]
                store.session(VideoPacket.Session(width, height), run + 1L)
                val encoder = object : H264Encoder(org.jcodec.codecs.h264.encode.DumbRateControl()) {
                    override fun initSPS(size: org.jcodec.common.model.Size) = super.initSPS(size).apply {
                        if (knownColour) vuiParams = org.jcodec.codecs.h264.io.model.VUIParameters().apply {
                            aspectRatioInfoPresentFlag = true
                            aspectRatio = org.jcodec.codecs.h264.io.model.AspectRatio.fromValue(1)
                            videoSignalTypePresentFlag = true
                            videoFormat = 5
                            videoFullRangeFlag = true
                            colourDescriptionPresentFlag = true
                            colourPrimaries = 6; transferCharacteristics = 6; matrixCoefficients = 6
                        }
                    }
                }.apply { setKeyInterval(8) }
                val decoder = H264Decoder()
                times.forEachIndexed { n, time ->
                    val picture = Picture.create(width, height, ColorSpace.YUV420J).apply { fill(20 + run * 20 + n) }
                    val encoded = encoder.encodeFrame(picture, ByteBuffer.allocate(width * height * 3)).data
                    val bytes = ByteArray(encoded.remaining()).also { encoded.duplicate().get(it) } + if (knownColour) {
                        byteArrayOf(0, 0, 0, 1, 6, 5, 20) + "ReplayBufferTest!".toByteArray() + byteArrayOf(0, 0, 3, 1, 122, 0x80.toByte())
                    } else byteArrayOf()
                    if (n == 0) {
                        val config = H264Utils.splitFrame(ByteBuffer.wrap(bytes)).filter { it.get(0).toInt() and 31 in setOf(7, 8) }
                            .fold(byteArrayOf()) { result, nal -> result + byteArrayOf(0, 0, 0, 1) + ByteArray(nal.remaining()).also { nal.get(it) } }
                        store.frame(VideoPacket.Frame(0, true, false, config), run + 1L)
                    }
                    assertTrue(store.frame(VideoPacket.Frame(time, false, n == 0, bytes), run + 1L))
                    original += bytes
                    pixels += decoder.decodeFrame(ByteBuffer.wrap(bytes.copyOf()), Picture.create((width + 15) and -16, (height + 15) and -16, ColorSpace.YUV420J).data).cloneCropped().data.map { it.copyOf() }
                }
            }
            check(root, store, store.capture(ReplaySettings())!!, original, pixels)
        } finally {
            store.close()
            Files.walk(root).use { files -> files.sorted(Comparator.reverseOrder()).forEach(Files::deleteIfExists) }
        }
    }
}
