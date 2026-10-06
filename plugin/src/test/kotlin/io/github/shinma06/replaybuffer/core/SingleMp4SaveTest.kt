package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonParser
import org.jcodec.codecs.h264.H264Encoder
import org.jcodec.codecs.h264.H264Decoder
import org.jcodec.codecs.h264.H264Utils
import org.jcodec.common.model.ColorSpace
import org.jcodec.common.model.Picture
import org.jcodec.common.io.NIOUtils
import org.jcodec.containers.mp4.MP4Packet
import org.jcodec.containers.mp4.demuxer.AbstractMP4DemuxerTrack
import org.jcodec.containers.mp4.demuxer.MP4Demuxer
import org.jcodec.containers.mp4.boxes.VideoSampleEntry
import java.nio.ByteBuffer
import java.nio.file.Files
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertContentEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class SingleMp4SaveTest {
    @Test
    fun sourceAdjacencyRequiresPositiveAcquisitionFactsAndEveryPacket() = withRuns(listOf(listOf(1_000_000, 1_100_000, 1_350_000))) { _, _, capture, _, _ ->
        val first = capture.video[0]
        val next = capture.video[1]
        assertTrue(first.playbackContinuousTo(next))
        assertFalse(first.continuousTo(next, emptyList())) // Retention still rejects null/MAX.
        assertTrue(first.copy(time = MappedTime(1, 99, 0, 1)).playbackContinuousTo(next))
        for (broken in listOf(next.copy(generation = 0), next.copy(session = 0), next.copy(decodeRun = 0),
            next.copy(packetOrdinal = 0), next.copy(packetOrdinal = next.packetOrdinal + 1),
            next.copy(decodeRun = next.decodeRun + 1), next.copy(pts = first.pts),
            next.copy(width = 64), next.copy(config = byteArrayOf(1)), next.copy(offset = next.offset + 1))) {
            assertFalse(first.playbackContinuousTo(broken))
        }
        assertFalse(first.copy(packetOrdinal = Long.MAX_VALUE).playbackContinuousTo(next))
        assertFalse(first.copy(offset = Long.MAX_VALUE).playbackContinuousTo(next))
    }

    @Test
    fun reconnectsAndConfigChangesKeepOneTrackAllPacketsAndResetPtsMappings() = withRuns(
        listOf(listOf(1_000_000, 1_100_000, 1_350_000), listOf(200_000, 410_000, 700_000), listOf(0, 100_000, 400_000)),
    ) { root, store, capture, original, pixels ->
        val output = write(capture, root)
        val record = manifest(output)
        assertEquals(2, record["schema"].asInt)
        assertEquals(3, record["parts"].asJsonArray.size())
        assertEquals(setOf("video-001.mp4"), record["parts"].asJsonArray.map { it.asJsonObject["file"].asString }.toSet())
        val rows = rows(output)
        assertEquals(capture.video.map { it.pts }, rows.map { it["source_pts_us"].asLong })
        assertEquals(listOf(0L, 100_000L, 350_000L, 350_001L, 560_001L, 850_001L, 850_002L, 950_002L, 1_250_002L),
            rows.map { it["media_pts_us"].asLong })
        assertEquals(3, rows.count { it["source_duration_us"].isJsonNull })
        assertTrue(rows.all { it["presented"].asBoolean && it["window_ns"].isJsonNull })
        val boundaries = record["video_run_boundaries"].asJsonArray
        assertTrue(boundaries.all { it.asJsonObject["actual_gap_duration_us"].isJsonNull && it.asJsonObject["authored_gap_duration_us"].asLong == 0L })
        assertPackets(output, original, pixels, expectedEntries = 2)
        store.generation(4)
        val retry = write(capture, root)
        assertEquals(record["parts"], manifest(retry)["parts"])
        assertEquals(Files.readAllLines(output.directory.resolve("frames.jsonl")), Files.readAllLines(retry.directory.resolve("frames.jsonl")))
    }

    @Test
    fun knownVideoGapUsesEmptyEditWhileUnknownActualDurationStaysNull() = withRuns(
        listOf(listOf(1_000_000, 1_100_000, 1_350_000), listOf(200_000, 410_000, 700_000)),
    ) { root, _, capture, original, pixels ->
        val video = capture.video.mapIndexed { n, frame ->
            val sequence = if (n < 3) (frame.pts - 1_000_000) * 1000 else 5_000_000_000 + (frame.pts - 200_000) * 1000
            frame.copy(time = MappedTime(sequence, 0, 0, sequence))
        }
        val gap = CaptureGap("video", 2_000_000_000, 4_000_000_000, "disconnect", 0, 1, 0)
        val known = capture.copy(video = video, gaps = listOf(gap), start = 0, end = 6_000_000_000,
            endUncertainty = 0, states = emptyMap())
        val output = write(known, root)
        val boundary = manifest(output)["video_run_boundaries"].asJsonArray.single().asJsonObject
        assertEquals(2_000_000, boundary["actual_gap_duration_us"].asLong)
        NIOUtils.readableChannel(output.directory.resolve("video-001.mp4").toFile()).use { input ->
            MP4Demuxer.createRawMP4Demuxer(input).use { demux ->
                val track = demux.videoTrack as AbstractMP4DemuxerTrack
                assertEquals(listOf(350_001L, 2_000_000L, 500_001L), track.edits.map { it.duration })
                assertEquals(-1, track.edits[1].mediaTime)
                assertEquals(2_850_002, demux.movie.duration)
            }
        }
        assertPackets(output, original, pixels, expectedEntries = 2)
        val unknown = write(known.copy(gaps = listOf(gap.copy(boundaryUncertaintyNs = Long.MAX_VALUE))), root)
        assertTrue(manifest(unknown)["video_run_boundaries"].asJsonArray.single().asJsonObject["actual_gap_duration_us"].isJsonNull)
        assertEquals(6, rows(unknown).size)
        assertPackets(unknown, original, pixels, expectedEntries = 2)
    }

    @Test
    fun sourceBreakWithoutIdrFailsAndKeepsThePinnedRequest() = withRuns(listOf(listOf(1_000_000, 1_100_000, 1_350_000))) { root, store, capture, _, _ ->
        for (broken in listOf(capture.video.drop(1), capture.video.mapIndexed { n, frame ->
            if (n > 0) frame.copy(packetOrdinal = frame.packetOrdinal + 1) else frame
        })) {
            assertFailsWith<SaveFailure> { write(capture.copy(video = broken), root) }
            assertFailsWith<IllegalStateException> { store.capture(ReplaySettings()) }
            assertTrue(capture.video.all { Files.exists(it.file) })
        }
        assertEquals(3, rows(write(capture, root)).size)
    }

    @Test
    fun laterSourceReadFailureAndCancellationNeverPublishAPartialMovie() = withRuns(
        listOf(listOf(1_000_000, 1_100_000, 1_350_000), listOf(200_000, 410_000, 700_000)),
    ) { root, store, capture, _, _ ->
        val broken = capture.copy(video = capture.video.mapIndexed { n, frame ->
            if (n == capture.video.lastIndex) frame.copy(size = frame.size + 1) else frame
        })
        assertFailsWith<SaveFailure> { write(broken, root) }
        assertFailsWith<IllegalStateException> { store.capture(ReplaySettings()) }
        var checks = 0
        assertFailsWith<java.util.concurrent.CancellationException> {
            SaveWriter().write(capture, root, { ++checks > 5 }) { _, _ -> error("must not publish") }
        }
        Files.list(root).use { paths -> assertFalse(paths.anyMatch { it.fileName.toString().startsWith("replay-") }) }
        assertEquals(6, rows(write(capture, root)).size)
    }

    @Test
    fun mixedGeometryPreservesEveryOriginalPacketAndConfigWithOneCanvasAndFixedRetry() = withRuns(
        List(4) { List(35) { n -> n * 100_000L + if (n >= 2) 150_000L else 0 } }, List(4) { if (it % 2 == 0) 64 to 32 else 32 to 64 },
    ) { root, store, capture, original, pixels ->
        val output = write(capture, root)
        val record = manifest(output)
        assertTrue(record["video_reencoded"].asBoolean)
        assertFalse(record["video_canvas"].asJsonObject["colour_known"].asBoolean)
        val saved = Files.readAllBytes(output.directory.resolve("video-source.bin"))
        val rows = rows(output)
        assertEquals(140, rows.size)
        assertTrue(rows.all { it["window_ns"].isJsonNull && it["uncertainty_ns"].asLong == Long.MAX_VALUE })
        assertEquals(capture.video.map { it.pts }, rows.map { it["source_pts_us"].asLong })
        rows.forEachIndexed { n, row ->
            assertContentEquals(original[n], saved.copyOfRange(row["source_offset"].asInt, row["source_offset"].asInt + row["source_size"].asInt))
            assertContentEquals(capture.video[n].config, saved.copyOfRange(row["source_config_offset"].asInt,
                row["source_config_offset"].asInt + row["source_config_size"].asInt))
            assertEquals(hash(original[n]), row["source_sha256"].asString)
            assertEquals(hash(capture.video[n].config), row["source_config_sha256"].asString)
        }
        assertMixedPackets(output, pixels, colourKnown = false)
        store.generation(9)
        val retry = write(capture, root)
        assertContentEquals(saved, Files.readAllBytes(retry.directory.resolve("video-source.bin")))
        assertMixedPackets(retry, pixels, colourKnown = false) // Container creation timestamps may differ.
        assertEquals(rows, rows(retry))
    }

    @Test
    fun mixedGeometryKeepsPrerollVfrKnownGapAndOriginalColourSemantics() = withRuns(
        List(2) { listOf(0, 100_000, 350_000) }, listOf(64 to 32, 32 to 64), knownColour = true,
    ) { root, _, capture, _, pixels ->
        val video = capture.video.mapIndexed { n, frame -> frame.copy(time = MappedTime(
            (if (n < 3) 0L else 5_000_000_000) + frame.pts * 1000, 0, 0,
            (if (n < 3) 0L else 5_000_000_000) + frame.pts * 1000)) }
        val output = write(capture.copy(video = video, start = 150_000_000, end = 6_000_000_000, endUncertainty = 0,
            gaps = listOf(CaptureGap("video", 2_000_000_000, 4_000_000_000, "disconnect", 0, 1, 0)), states = emptyMap()), root)
        val rows = rows(output)
        assertEquals(6, rows.size)
        assertTrue(rows[0]["preroll"].asBoolean)
        assertTrue(rows[0]["movie_pts_us"].isJsonNull)
        assertEquals(150_000, rows[1]["source_display_start_us"].asLong)
        assertEquals(250_000, rows[1]["source_duration_us"].asLong)
        assertEquals(2_000_000, manifest(output)["video_run_boundaries"].asJsonArray.single().asJsonObject["actual_gap_duration_us"].asLong)
        assertMixedPackets(output, pixels, colourKnown = true)
    }

    @Test
    fun mixedLaterCodecFailureDiskShortageAndCodecBoundaryCancelKeepPinAndNoPartialSuccess() = withRuns(
        List(2) { listOf(0, 100_000, 350_000) }, listOf(64 to 32, 32 to 64),
    ) { root, store, capture, _, _ ->
        val last = capture.video.last()
        val bytes = Files.readAllBytes(last.file)
        java.nio.channels.FileChannel.open(last.file, java.nio.file.StandardOpenOption.WRITE).use { channel ->
            channel.write(ByteBuffer.wrap(ByteArray(last.size)), last.offset)
        }
        assertFailsWith<SaveFailure> { write(capture, root) }
        Files.write(last.file, bytes)
        var checks = 0
        assertFailsWith<SaveFailure> {
            SaveWriter { if (++checks > 8) 0 else Long.MAX_VALUE }.write(capture, root, { false }) { _, _ -> error("partial publish") }
        }
        var cancelledAt = 0L
        val started = System.nanoTime()
        assertFailsWith<java.util.concurrent.CancellationException> {
            SaveWriter().write(capture, root, {
                val trigger = Files.list(root).use { paths -> paths.filter { it.fileName.toString().endsWith(".partial") }
                    .anyMatch { dir -> val source = dir.resolve("video-source.bin"); Files.exists(source) && Files.size(source) > capture.video.first().size } }
                if (trigger && cancelledAt == 0L) cancelledAt = System.nanoTime()
                trigger
            }) { _, _ -> error("partial publish") }
        }
        assertTrue(cancelledAt >= started && System.nanoTime() - cancelledAt < 10_000_000_000)
        assertFailsWith<IllegalStateException> { store.capture(ReplaySettings()) }
        Files.list(root).use { paths -> assertFalse(paths.anyMatch { it.fileName.toString().startsWith("replay-") || it.fileName.toString().endsWith(".partial") }) }
        assertEquals(6, rows(write(capture, root)).size)
    }

    @Test
    fun mixedUnsupportedSarFailsButSameGeometryRemuxStillWorksWithoutRawSidecar() = withRuns(
        List(2) { listOf(0, 100_000, 350_000) }, listOf(64 to 32, 32 to 64), knownColour = true,
    ) { root, _, capture, _, _ ->
        fun changedConfig(change: (org.jcodec.codecs.h264.io.model.SeqParameterSet) -> Unit): ByteArray {
            val config = capture.video.last().config
            val sps = H264Utils.readSPS(H264Utils.getRawSPS(ByteBuffer.wrap(config)).first().duplicate()).apply(change)
            val out = ByteBuffer.allocate(65536)
            out.put(byteArrayOf(0, 0, 0, 1, 0x67))
            out.put(H264Utils.writeSPS(sps, 65536))
            H264Utils.getRawPPS(ByteBuffer.wrap(config)).forEach { out.put(byteArrayOf(0, 0, 0, 1, 0x68)); out.put(it.duplicate()) }
            out.flip()
            return ByteArray(out.remaining()).also { out.get(it) }
        }
        val colourChange = changedConfig { it.vuiParams.matrixCoefficients = 1 }
        val incompatible = assertFailsWith<SaveFailure> { write(capture.copy(video = capture.video.map {
            if (it.generation == 2L) it.copy(config = colourChange) else it
        }), root) }
        assertTrue(incompatible.message!!.contains("色指定"))
        val changed = changedConfig {
            it.vuiParams.aspectRatio = org.jcodec.codecs.h264.io.model.AspectRatio.fromValue(255)
            it.vuiParams.sarWidth = 2; it.vuiParams.sarHeight = 1
        }
        val failure = assertFailsWith<SaveFailure> { write(capture.copy(video = capture.video.map { if (it.generation == 2L) it.copy(config = changed) else it }), root) }
        assertTrue(failure.message!!.contains("aspect"))
        val normal = write(capture.copy(video = capture.video.take(3)), root)
        assertFalse(manifest(normal)["video_reencoded"].asBoolean)
        assertFalse(Files.exists(normal.directory.resolve("video-source.bin")))
    }

    @Test
    fun maximumCaptureAxesAndCroppedCodedPlanesSaveAllFramesAndCancelAfterInFlightCodec() = withRuns(
        List(2) { listOf(0, 100_000) }, listOf(1920 to 1080, 1080 to 1920), knownColour = true,
    ) { root, store, capture, original, pixels ->
        val requested = java.util.concurrent.atomic.AtomicLong()
        var signal: Thread? = null
        try {
            assertFailsWith<java.util.concurrent.CancellationException> {
                SaveWriter().write(capture, root, {
                    if (signal == null && Files.list(root).use { paths -> paths.filter { it.fileName.toString().endsWith(".partial") }
                        .anyMatch { dir -> val source = dir.resolve("video-source.bin"); Files.exists(source) && Files.size(source) >= capture.video.first().size + capture.video.first().config.size } }) {
                        signal = Thread {
                            Thread.sleep(5)
                            requested.set(System.nanoTime())
                        }.apply { start() }
                    }
                    requested.get() != 0L
                }) { _, _ -> error("cancelled publish") }
            }
            signal!!.join()
            assertTrue(System.nanoTime() - requested.get() < 10_000_000_000)
            assertFailsWith<IllegalStateException> { store.capture(ReplaySettings()) }
            val output = write(capture, root)
            val savedRows = rows(output)
            assertEquals(4, savedRows.size)
            val savedBytes = Files.readAllBytes(output.directory.resolve("video-source.bin"))
            assertTrue(original.any { bytes -> (0 until bytes.size - 2).any { n -> bytes[n] == 0.toByte() && bytes[n + 1] == 0.toByte() && bytes[n + 2] == 3.toByte() } })
            savedRows.forEachIndexed { n, row ->
                assertContentEquals(original[n], savedBytes.copyOfRange(row["source_offset"].asInt, row["source_offset"].asInt + row["source_size"].asInt))
                assertEquals(hash(original[n]), row["source_sha256"].asString)
            }
            assertEquals(1920, manifest(output)["video_canvas"].asJsonObject["width"].asInt)
            assertEquals(1920, manifest(output)["video_canvas"].asJsonObject["height"].asInt)
            assertMixedPackets(output, pixels, colourKnown = true)
        } finally { signal?.join() }
    }

    @Test
    fun hostileParameterArraySizesFailBeforeJcodecAllocation() {
        fun spsCycle(): ByteBuffer {
            val bytes = ByteBuffer.allocate(32)
            val writer = org.jcodec.common.io.BitWriter(bytes)
            writer.writeNBit(66, 8); writer.writeNBit(0, 8); writer.writeNBit(40, 8)
            fun ue(n: Int) = org.jcodec.codecs.h264.io.write.CAVLCWriter.writeUE(writer, n)
            ue(0); ue(0); ue(1); writer.write1Bit(0); ue(0); ue(0); ue(30_000)
            writer.flush(); bytes.flip()
            return bytes
        }
        assertFailsWith<IllegalArgumentException> { boundedSps(spsCycle()) }
        val bytes = ByteBuffer.allocate(32)
        val writer = org.jcodec.common.io.BitWriter(bytes)
        org.jcodec.codecs.h264.io.write.CAVLCWriter.writeUE(writer, 0)
        org.jcodec.codecs.h264.io.write.CAVLCWriter.writeUE(writer, 0)
        writer.writeNBit(0, 2)
        org.jcodec.codecs.h264.io.write.CAVLCWriter.writeUE(writer, 30_000)
        writer.flush(); bytes.flip()
        assertFailsWith<IllegalArgumentException> { boundedPps(bytes) }
    }

    private fun hash(bytes: ByteArray) = java.security.MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }

    private fun assertMixedPackets(output: SaveOutput, pixels: List<List<ByteArray>>, colourKnown: Boolean) {
        val rows = rows(output)
        val canvas = manifest(output)["video_canvas"].asJsonObject
        val canvasWidth = canvas["width"].asInt; val canvasHeight = canvas["height"].asInt
        NIOUtils.readableChannel(output.directory.resolve("video-001.mp4").toFile()).use { input ->
            MP4Demuxer.createRawMP4Demuxer(input).use { demux ->
                assertEquals(1, demux.videoTracks.size)
                val track = demux.videoTrack as AbstractMP4DemuxerTrack
                assertEquals(track.edits.sumOf { it.duration }, demux.movie.duration)
                var decoder = H264Decoder()
                rows.forEachIndexed { n, row ->
                    val packet = track.nextFrame() as MP4Packet
                    val entry = track.sampleEntries[packet.entryNo] as VideoSampleEntry
                    assertEquals(canvasWidth, entry.width); assertEquals(canvasHeight, entry.height)
                    val config = H264Utils.parseAVCC(entry)
                    val sps = H264Utils.readSPS(config.spsList.first().duplicate())
                    assertEquals(51, sps.levelIdc); assertEquals(1, sps.numRefFrames)
                    if (colourKnown) {
                        assertTrue(sps.vuiParams.videoFullRangeFlag)
                        assertEquals(6, sps.vuiParams.matrixCoefficients)
                    } else assertTrue(sps.vuiParams == null || !sps.vuiParams.videoSignalTypePresentFlag)
                    if (packet.isKeyFrame) decoder = H264Decoder().apply { addSps(config.spsList); addPps(config.ppsList) }
                    val annex = H264Utils.decodeMOVPacket(packet.data, config)
                    val decoded = decoder.decodeFrame(annex, Picture.create(canvasWidth, canvasHeight, ColorSpace.YUV420J).data)
                    val rect = row["source_rect"].asJsonObject
                    val w = rect["width"].asInt; val h = rect["height"].asInt
                    val x = rect["x"].asInt; val y = rect["y"].asInt
                    for (c in 0..2) {
                        val shift = if (c == 0) 0 else 1
                        val cw = w shr shift; val ch = h shr shift
                        var squareError = 0L
                        for (a in 0 until ch) for (b in 0 until cw) {
                            val error = pixels[n][c][a * cw + b].toInt() - decoded.getPlaneData(c)[(a + (y shr shift)) * (canvasWidth shr shift) + b + (x shr shift)].toInt()
                            squareError += error.toLong() * error
                        }
                        assertTrue(squareError.toDouble() / (cw * ch) < 4, "frame $n plane $c source rectangle quality")
                    }
                    assertEquals(row["media_pts_us"].asLong, packet.mediaPts)
                    assertEquals(row["display_duration_us"].asLong, packet.duration)
                    assertEquals(row["sample_entry"].asInt - 1, packet.entryNo)
                }
                assertEquals(null, track.nextFrame())
            }
        }
        Files.list(output.directory).use { files -> assertEquals(1L, files.filter { it.fileName.toString().endsWith(".mp4") }.count()) }
    }

    private fun write(capture: FrozenCapture, root: java.nio.file.Path) = SaveWriter().write(capture, root, { false }) { a, b -> Files.move(a, b) }
    private fun manifest(output: SaveOutput) = JsonParser.parseString(Files.readString(output.directory.resolve("session.json"))).asJsonObject
    private fun rows(output: SaveOutput) = Files.readAllLines(output.directory.resolve("frames.jsonl")).map { JsonParser.parseString(it).asJsonObject }

    private fun assertPackets(output: SaveOutput, original: List<ByteArray>, pixels: List<List<ByteArray>>, expectedEntries: Int) {
        val rows = rows(output)
        NIOUtils.readableChannel(output.directory.resolve("video-001.mp4").toFile()).use { input ->
            MP4Demuxer.createRawMP4Demuxer(input).use { demux ->
                assertEquals(1, demux.videoTracks.size)
                val track = demux.videoTrack as AbstractMP4DemuxerTrack
                assertEquals(expectedEntries, track.sampleEntries.size)
                assertEquals(track.edits.sumOf { it.duration }, demux.movie.duration)
                assertEquals(demux.movie.duration, track.box.duration)
                var decoder = H264Decoder()
                rows.forEachIndexed { n, row ->
                    val packet = track.nextFrame() as MP4Packet
                    val config = H264Utils.parseAVCC(track.sampleEntries[packet.entryNo] as VideoSampleEntry)
                    val payload = ByteArray(packet.data.remaining()).also { packet.data.duplicate().get(it) }
                    val annex = H264Utils.decodeMOVPacket(ByteBuffer.wrap(payload), config)
                    fun vcl(data: ByteBuffer) = H264Utils.splitFrame(data).filter { it.get(0).toInt() and 31 !in setOf(7, 8) }
                        .map { nal -> ByteArray(nal.remaining()).also { nal.duplicate().get(it) } }
                    val expectedNals = vcl(ByteBuffer.wrap(original[n]))
                    val actualNals = vcl(annex.duplicate())
                    assertEquals(expectedNals.size, actualNals.size)
                    expectedNals.zip(actualNals).forEach { (a, b) -> assertContentEquals(a, b) }
                    if (row["sample_index"].asInt == 0 || packet.isKeyFrame) decoder = H264Decoder().apply { addSps(config.spsList); addPps(config.ppsList) }
                    val decoded = decoder.decodeFrame(annex, Picture.create(32, 32, ColorSpace.YUV420J).data)
                    pixels[n].zip(decoded.data).forEach { (a, b) -> assertContentEquals(a, b) }
                    assertEquals(row["media_pts_us"].asLong, packet.mediaPts)
                    assertEquals(row["display_duration_us"].asLong, packet.duration)
                }
                assertEquals(null, track.nextFrame())
            }
        }
        Files.list(output.directory).use { paths -> assertEquals(1, paths.filter { it.toString().endsWith(".mp4") }.count().toInt()) }
    }

    private fun withRuns(pts: List<List<Long>>, sizes: List<Pair<Int, Int>> = pts.map { 32 to 32 }, knownColour: Boolean = false, check: (java.nio.file.Path, CaptureStore, FrozenCapture, List<ByteArray>, List<List<ByteArray>>) -> Unit) {
        val root = Files.createTempDirectory("replay-mp4-runs-")
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
                }.apply { setKeyInterval(if (run % 2 == 0) 8 else 16) }
                val decoder = H264Decoder()
                times.forEachIndexed { n, time ->
                    val encoded = encoder.encodeFrame(Picture.create(width, height, ColorSpace.YUV420J).apply { fill(20 + run * 20 + n) }, ByteBuffer.allocate(width * height * 3)).data
                    val bytes = ByteArray(encoded.remaining()).also { encoded.duplicate().get(it) } + if (knownColour) {
                        // Valid user_data_unregistered SEI with a real emulation-prevention byte.
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
    @Test
    fun unknownClockDoesNotCreateMultipleMp4Files() = unknownClockSave { output ->
        Files.list(output.directory).use { files ->
            assertEquals(1L, files.filter { it.fileName.toString().endsWith(".mp4") }.count())
        }
    }

    @Test
    fun unknownClockKeepsObservedVfrIntervalsInsteadOfOneTickPerFrame() = unknownClockSave { output ->
        val visible = Files.readAllLines(output.directory.resolve("frames.jsonl"))
            .map { JsonParser.parseString(it).asJsonObject }.filter { it["presented"].asBoolean }
        assertEquals(listOf(1_000_000L, 1_100_000L, 1_350_000L), visible.map { it["source_pts_us"].asLong })
        assertTrue(visible.all { it["window_ns"].isJsonNull && it["uncertainty_ns"].asLong == Long.MAX_VALUE })
        assertEquals(350_001L, visible.sumOf { it["display_duration_us"].asLong })
    }

    private fun unknownClockSave(check: (SaveOutput) -> Unit) {
        val root = Files.createTempDirectory("replay-single-mp4-")
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        try {
            store.generation(1)
            store.session(VideoPacket.Session(32, 32), 1)
            val encoder = H264Encoder.createH264Encoder().apply { setKeyInterval(100) }
            listOf(1_000_000L, 1_100_000L, 1_350_000L).forEachIndexed { n, pts ->
                val encoded = encoder.encodeFrame(Picture.create(32, 32, ColorSpace.YUV420J).apply { fill(20 + n) },
                    ByteBuffer.allocate(65536)).data
                val bytes = ByteArray(encoded.remaining()).also { encoded.get(it) }
                if (n == 0) {
                    val config = H264Utils.splitFrame(ByteBuffer.wrap(bytes)).filter { it.get(0).toInt() and 31 in setOf(7, 8) }
                        .fold(byteArrayOf()) { result, nal -> result + byteArrayOf(0, 0, 0, 1) +
                            ByteArray(nal.remaining()).also { nal.get(it) } }
                    store.frame(VideoPacket.Frame(0, true, false, config), 1)
                }
                assertTrue(store.frame(VideoPacket.Frame(pts, false, n == 0, bytes), 1))
            }
            val capture = store.capture(ReplaySettings())!!
            assertEquals(3, capture.video.size)
            assertTrue(capture.video.all { it.time.sequence == null && it.time.uncertainty == Long.MAX_VALUE })
            check(SaveWriter().write(capture, root, { false }) { a, b -> Files.move(a, b) })
        } finally {
            store.close()
            Files.walk(root).use { files -> files.sorted(Comparator.reverseOrder()).forEach(Files::deleteIfExists) }
        }
    }
}
