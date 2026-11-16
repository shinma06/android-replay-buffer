package io.github.shinma06.replaybuffer.core

import com.google.gson.GsonBuilder
import com.google.gson.JsonParser
import org.jcodec.codecs.h264.H264Encoder
import org.jcodec.codecs.h264.H264Utils
import org.jcodec.common.model.ColorSpace
import org.jcodec.common.io.NIOUtils
import org.jcodec.common.model.Packet
import org.jcodec.common.model.Picture
import org.jcodec.containers.mp4.MP4Packet
import org.jcodec.containers.mp4.MP4TrackType
import org.jcodec.containers.mp4.demuxer.AbstractMP4DemuxerTrack
import org.jcodec.containers.mp4.demuxer.MP4Demuxer
import org.jcodec.containers.mp4.muxer.MP4Muxer
import org.jcodec.containers.mp4.muxer.MP4MuxerTrack
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.ByteBuffer
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardCopyOption.REPLACE_EXISTING
import java.security.MessageDigest
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

class ReplayTrimSpaceTest {
    @TempDir lateinit var root: Path
    private val gson = GsonBuilder().serializeNulls().disableHtmlEscaping().create()
    private val mib = 1024L * 1024

    private fun hash(bytes: ByteArray) = MessageDigest.getInstance("SHA-256").digest(bytes)
        .joinToString("") { "%02x".format(it) }

    private fun hashes(folder: Path) = Files.list(folder).use { paths ->
        paths.toList().associate { it.fileName.toString() to hash(Files.readAllBytes(it)) }
    }

    private fun fixture(width: Int = 32, height: Int = 32): Path {
        CaptureStore(root.resolve("ring"), minFree = 0).use { store ->
            store.generation(1)
            store.session(VideoPacket.Session(width, height), 1)
            val encoder = H264Encoder.createH264Encoder().apply { setKeyInterval(8) }
            repeat(40) { n ->
                val data = encoder.encodeFrame(Picture.create(width, height, ColorSpace.YUV420J).apply { fill(20 + n) },
                    ByteBuffer.allocate(width * height * 6)).data
                val bytes = ByteArray(data.remaining()).also(data::get)
                if (n == 0) {
                    val config = H264Utils.splitFrame(ByteBuffer.wrap(bytes)).filter { it.get(0).toInt() and 31 in setOf(7, 8) }
                        .fold(byteArrayOf()) { all, nal -> all + byteArrayOf(0, 0, 0, 1) + ByteArray(nal.remaining()).also(nal::get) }
                    store.frame(VideoPacket.Frame(0, true, false, config), 1)
                }
                assertTrue(store.frame(VideoPacket.Frame(n * 100_000L, false, n % 8 == 0, bytes), 1))
            }
            return SaveWriter().write(store.capture(ReplaySettings())!!, root, { false },
                { Files.move(it.partial, it.complete) }).directory
        }
    }

    private fun trim(source: Path, from: Long, to: Long, budget: Long): Path =
        ReplayTrim(availableSpace = { Math.addExact(ReplaySettings.MIN_FREE_BYTES, budget) })
            .trim(source, from, to, { false }, { Files.move(it.partial, it.complete) })

    private fun refreshHashes(source: Path) {
        val path = source.resolve(ReplayFiles.RECORD)
        val record = JsonParser.parseString(Files.readString(path)).asJsonObject
        val hashes = record["files_sha256"].asJsonObject
        for (name in hashes.keySet().toList()) {
            val file = source.resolve(name)
            if (Files.exists(file)) hashes.addProperty(name, hash(Files.readAllBytes(file))) else hashes.remove(name)
        }
        Files.writeString(path, gson.toJson(record))
    }

    private fun parityFixture(): Path {
        val source = Files.createTempDirectory(root, "source-")
        for (name in listOf("README.md", "replay.json", "video.mp4", "video-source.h264", "video-frames.jsonl",
                "logcat-device.jsonl", "logcat-app.jsonl", "touches.jsonl")) {
            javaClass.getResourceAsStream("/replay-trim-parity/replay-20240101-000000-manual-feedface/$name")!!.use {
                Files.copy(it, source.resolve(name))
            }
        }
        return source
    }

    private fun padVideo(source: Path, padding: Int, frame: Long? = null) {
        val temporary = Files.createTempFile(root, "padded-", ".mp4")
        NIOUtils.readableChannel(source.resolve(ReplayFiles.VIDEO).toFile()).use { input ->
            val original = MP4Demuxer.createRawMP4Demuxer(input).videoTrack as AbstractMP4DemuxerTrack
            NIOUtils.writableChannel(temporary.toFile()).use { output ->
                val mux = MP4Muxer.createMP4MuxerToChannel(output)
                val track = mux.addTrack(MP4MuxerTrack(mux.nextTrackId, MP4TrackType.VIDEO))
                original.sampleEntries.forEach(track::addSampleEntry)
                track.setEdits(original.edits)
                while (true) {
                    val packet = original.nextFrame() as? MP4Packet ?: break
                    val bytes = if (frame == null || frame == packet.frameNo) {
                        ByteBuffer.allocate(packet.data.remaining() + padding + 5).apply {
                            put(packet.data); putInt(padding + 1); put(12.toByte())
                            repeat(padding - 1) { put(255.toByte()) }; put(128.toByte()); flip()
                        }
                    } else packet.data
                    track.addFrameInternal(Packet.createPacket(bytes, packet.pts, packet.timescale, packet.duration,
                        packet.frameNo, packet.frameType, null), packet.entryNo + 1)
                }
                mux.finish()
            }
        }
        Files.move(temporary, source.resolve(ReplayFiles.VIDEO), REPLACE_EXISTING)
        refreshHashes(source)
    }

    private fun partialBytes(): Long = Files.list(root).use { folders ->
        folders.filter { it.fileName.toString().endsWith(".partial") }.mapToLong { folder ->
            Files.list(folder).use { files -> files.mapToLong(Files::size).sum() }
        }.sum()
    }

    private fun padAudio(source: Path, index: Int, padding: Int) {
        val temporary = Files.createTempFile(root, "audio-padded-", ".mp4")
        NIOUtils.readableChannel(source.resolve(ReplayFiles.VIDEO).toFile()).use { input ->
            val demuxer = MP4Demuxer.createRawMP4Demuxer(input)
            NIOUtils.writableChannel(temporary.toFile()).use { output ->
                val mux = MP4Muxer.createMP4MuxerToChannel(output)
                for (original in listOf(demuxer.videoTrack, demuxer.audioTracks.single())) {
                    original as AbstractMP4DemuxerTrack
                    val audio = original === demuxer.audioTracks.single()
                    val track = mux.addTrack(MP4MuxerTrack(mux.nextTrackId, if (audio) MP4TrackType.SOUND else MP4TrackType.VIDEO))
                    original.sampleEntries.forEach(track::addSampleEntry)
                    track.setEdits(original.edits)
                    while (true) {
                        val packet = original.nextFrame() as? MP4Packet ?: break
                        val bytes = if (audio && packet.frameNo == index.toLong()) {
                            ByteBuffer.allocate(packet.data.remaining() + padding).apply {
                                put(packet.data); position(capacity()); flip()
                            }
                        } else packet.data
                        track.addFrameInternal(Packet.createPacket(bytes, packet.pts, packet.timescale, packet.duration,
                            packet.frameNo, packet.frameType, null), packet.entryNo + 1)
                    }
                }
                mux.finish()
            }
        }
        Files.move(temporary, source.resolve(ReplayFiles.VIDEO), REPLACE_EXISTING)
        refreshHashes(source)
    }

    private fun estimate(source: Path, from: Long = 0, to: Long = 300_000): Long {
        val entries = Files.list(root).use { it.toList().toSet() }
        var low = 0L
        var high = 256 * mib
        while (low < high) {
            val mid = low + (high - low) / 2
            var checked = false
            val accepted = try {
                ReplayTrim(availableSpace = { checked = true; ReplaySettings.MIN_FREE_BYTES + mid })
                    .trim(source, from, to, { checked }, { error("unexpected publication") })
                error("unexpected completion")
            } catch (cancelled: SaveCancelled) {
                assertNull(cancelled.partial)
                true
            } catch (failed: SaveFailure) {
                assertNull(failed.partial)
                assertTrue(failed.message!!.contains("空き容量"), failed.message)
                false
            }
            if (accepted) high = mid else low = mid + 1
        }
        assertEquals(entries, Files.list(root).use { it.toList().toSet() })
        return low
    }

    @Test
    fun smallCopyFitsWithoutTheSourceWideSixteenMiBAllowance() {
        val source = fixture()
        val before = hashes(source)
        val output = trim(source, 0, 300_000, mib)
        SavedReplay.open(output).use { assertEquals(3, it.frames.size) }
        assertEquals(before, hashes(source))
    }

    @Test
    fun shortReencodeFitsButALongerLeadingGopRetainsItsBufferBound() {
        val source = fixture(640, 360)
        val before = hashes(source)
        trim(source, 1_100_000, 1_200_000, 4 * mib)
        val failure = assertFailsWith<SaveFailure> { trim(source, 1_100_000, 1_600_000, 4 * mib) }
        assertNull(failure.partial)
        assertTrue(failure.message!!.contains("空き容量"))
        assertEquals(before, hashes(source))
    }

    @Test
    fun cancellationAfterTheEstimateDoesNotCreateStaging() {
        val source = fixture()
        val before = hashes(source)
        val entries = Files.list(root).use { it.toList().toSet() }
        var cancelled = false
        var published = false
        val failure = assertFailsWith<SaveCancelled> {
            ReplayTrim(availableSpace = { cancelled = true; Long.MAX_VALUE }).trim(source, 0, 300_000,
                { cancelled }, { published = true })
        }
        assertFalse(published)
        assertNull(failure.partial)
        assertEquals(entries, Files.list(root).use { it.toList().toSet() })
        assertEquals(before, hashes(source))
    }

    @Test
    fun shortCopyDoesNotReserveUnselectedMp4Payload() {
        val source = fixture()
        assertFalse(Files.exists(source.resolve(ReplayFiles.VIDEO_SOURCE)))
        padVideo(source, mib.toInt())
        val before = hashes(source)
        assertTrue(Files.size(source.resolve(ReplayFiles.VIDEO)) > 40 * mib)
        val output = trim(source, 0, 300_000, 4 * mib)
        assertTrue(Files.size(output.resolve(ReplayFiles.VIDEO)) in 3 * mib until 4 * mib)
        assertEquals(before, hashes(source))
    }

    @Test
    fun longCopyReservesAllSelectedSamplesBeforeStaging() {
        val source = fixture()
        assertFalse(Files.exists(source.resolve(ReplayFiles.VIDEO_SOURCE)))
        padVideo(source, mib.toInt())
        val before = hashes(source)
        val end = JsonParser.parseString(Files.readString(source.resolve(ReplayFiles.RECORD))).asJsonObject["parts"]
            .asJsonArray.last().asJsonObject["movie_end_us"].asString.toLong()
        assertTrue(end > 3_000_000)
        var published = false
        val failure = assertFailsWith<SaveFailure> {
            ReplayTrim(availableSpace = { ReplaySettings.MIN_FREE_BYTES + 37 * mib - partialBytes() })
                .trim(source, 0, end, { false }, { published = true })
        }
        assertFalse(published)
        assertTrue(failure.message!!.contains("空き容量"), failure.message)
        assertNull(failure.partial)
        assertEquals(0L, partialBytes())
        assertEquals(before, hashes(source))
    }

    @Test
    fun mp4SampleSizesDriveCopyAndWarmupHasNoOutputBudget() {
        val source = fixture()
        val initial = estimate(source)
        padVideo(source, 4096, 0)
        assertEquals(initial + 4101, estimate(source))
        val middle = estimate(source, 1_100_000, 1_200_000)
        padVideo(source, 8192, 8)
        assertEquals(middle, estimate(source, 1_100_000, 1_200_000))
    }

    @Test
    fun rawBudgetCountsOnlyKeptSamplesAndOneConfigPerPart() {
        val source = parityFixture()
        val withRaw = estimate(source)
        val rows = Files.readAllLines(source.resolve(ReplayFiles.VIDEO_FRAMES)).map { JsonParser.parseString(it).asJsonObject }
        val expectedRaw = rows.first()["source_config_size"].asString.toLong() + rows.take(3).sumOf { it["source_size"].asString.toLong() }
        Files.delete(source.resolve(ReplayFiles.VIDEO_SOURCE))
        refreshHashes(source)
        // Removing the hash entry also removes its serialized key/value and JSON-growth reserve.
        val withoutRaw = estimate(source)
        assertTrue(withRaw - withoutRaw >= expectedRaw)
        assertTrue(withRaw - withoutRaw < expectedRaw + 1024)
    }

    @Test
    fun aacPrerollAndRetrimOutputsFitInsideTheirEstimates() {
        val source = parityFixture()
        val before = hashes(source)
        val budget = estimate(source, 200_001, 1_900_001)
        val output = ReplayTrim(availableSpace = { ReplaySettings.MIN_FREE_BYTES + budget - partialBytes() })
            .trim(source, 200_001, 1_900_001, { false }, { Files.move(it.partial, it.complete) })
        assertTrue(Files.list(output).use { it.mapToLong(Files::size).sum() } <= budget)
        val againBudget = estimate(output, 100_001, 1_500_001)
        val again = ReplayTrim(availableSpace = { ReplaySettings.MIN_FREE_BYTES + againBudget - partialBytes() })
            .trim(output, 100_001, 1_500_001, { false }, { Files.move(it.partial, it.complete) })
        assertTrue(Files.list(again).use { it.mapToLong(Files::size).sum() } <= againBudget)
        assertEquals(before, hashes(source))
    }

    @Test
    fun aacPrerollSizeIsCountedOnceAndUnselectedPacketsAreNotCounted() {
        val source = parityFixture()
        val start = ReplayTrim().preflight(source, 200_001, 300_001).startUs
        val preroll = ((start * 48_000 / 1_000_000) / 1024 - 1).toInt()
        val initial = estimate(source, 200_001, 300_001)
        padAudio(source, preroll, 4096)
        assertEquals(initial + 4096, estimate(source, 200_001, 300_001))
        padAudio(source, 79, 8192)
        assertEquals(initial + 4096, estimate(source, 200_001, 300_001))
        val output = trim(source, 200_001, 300_001, 16 * mib)
        val hidden = estimate(output, 0, 100_000)
        padAudio(output, 0, 8192)
        assertEquals(hidden + 8192, estimate(output, 0, 100_000))
    }

    @Test
    fun uncertainAndNullTimeUtf8RowsCountCopiesAndMovieClipping() {
        val source = parityFixture()
        for (name in listOf(ReplayFiles.LOGCAT_DEVICE, ReplayFiles.LOGCAT_APP, ReplayFiles.TOUCHES)) Files.writeString(source.resolve(name), "")
        refreshHashes(source)
        val empty = estimate(source)
        val text = "日本語😀".repeat(10_000)
        val uncertain = """{"record_id":"uncertain","t_ns":"9000000000","t_uncertainty_ns":"${Long.MAX_VALUE}","app_membership":true,"text":"$text"}"""
        val nullTime = """{"record_id":"null","t_ns":null,"t_uncertainty_ns":"0","text":"$text"}"""
        val outside = """{"record_id":"excluded","t_ns":"9000000000","t_uncertainty_ns":"0","text":"$text"}"""
        val touch = """{"t_ns":null,"t_uncertainty_ns":"0","movie_us":"150000","source":"injected","text":"$text"}"""
        val outsideMovie = """{"t_ns":null,"t_uncertainty_ns":"0","movie_us":"9000000","text":"$text"}"""
        Files.writeString(source.resolve(ReplayFiles.LOGCAT_DEVICE), listOf(uncertain, nullTime, outside).joinToString("\n", postfix = "\n"))
        Files.writeString(source.resolve(ReplayFiles.TOUCHES), "$touch\n$outsideMovie\n")
        refreshHashes(source)
        fun bytes(line: String) = gson.toJson(JsonParser.parseString(line)).toByteArray(Charsets.UTF_8).size.toLong() + 1
        val clipped = JsonParser.parseString(outsideMovie).asJsonObject.apply { addProperty("movie_us", null as String?) }
        val watermarks = listOf("null", "uncertain").sumOf { gson.toJson(it).toByteArray(Charsets.UTF_8).size - 4L }
        val expected = 2 * bytes(uncertain) + bytes(nullTime) + bytes(touch) + bytes(gson.toJson(clipped)) + watermarks
        assertEquals(empty + expected, estimate(source))
        val output = trim(source, 0, 300_000, 4 * mib)
        assertEquals(2, Files.readAllLines(output.resolve(ReplayFiles.LOGCAT_DEVICE)).size)
        assertEquals(1, Files.readAllLines(output.resolve(ReplayFiles.LOGCAT_APP)).size)
        assertEquals(2, Files.readAllLines(output.resolve(ReplayFiles.TOUCHES)).size)
    }

    @Test
    fun unknownClockCountsTheSameRetainedUtf8RowsAsWriting() {
        val source = parityFixture()
        val index = source.resolve(ReplayFiles.VIDEO_FRAMES)
        val rows = Files.readAllLines(index).map { JsonParser.parseString(it).asJsonObject }
        rows.first().apply { addProperty("t_ns", null as String?); addProperty("display_t_ns", null as String?) }
        Files.writeString(index, rows.joinToString("\n", postfix = "\n", transform = gson::toJson))
        Files.writeString(source.resolve(ReplayFiles.LOGCAT_DEVICE), """{"t_ns":null,"text":"${"未確定".repeat(50_000)}","app_membership":true}""" + "\n")
        refreshHashes(source)
        val budget = estimate(source)
        val output = trim(source, 0, 300_000, budget)
        val row = JsonParser.parseString(Files.readAllLines(output.resolve(ReplayFiles.LOGCAT_DEVICE)).single()).asJsonObject
        assertTrue(row["t_ns"].isJsonNull)
        assertEquals(Long.MAX_VALUE.toString(), row["t_uncertainty_ns"].asString)
        assertEquals(Files.readString(output.resolve(ReplayFiles.LOGCAT_DEVICE)), Files.readString(output.resolve(ReplayFiles.LOGCAT_APP)))
        assertTrue(Files.list(output).use { it.mapToLong(Files::size).sum() } <= budget)
    }

    @Test
    fun utf8DuplicatedLogsAreGuardedBeforeEitherWriteWhenSpaceDrops() {
        val source = parityFixture()
        val line = """{"t_ns":null,"app_membership":true,"text":"${"日".repeat(100_000)}"}"""
        Files.writeString(source.resolve(ReplayFiles.LOGCAT_DEVICE), line + "\n")
        refreshHashes(source)
        val before = hashes(source)
        val available = 2 * (line.toByteArray(Charsets.UTF_8).size.toLong() + 1) - 1
        val failure = assertFailsWith<SaveFailure> {
            ReplayTrim(availableSpace = {
                val writingLogs = Files.list(root).use { it.anyMatch { p -> p.fileName.toString().endsWith(".partial") && Files.exists(p.resolve(ReplayFiles.LOGCAT_DEVICE)) } }
                if (writingLogs) ReplaySettings.MIN_FREE_BYTES + available else Long.MAX_VALUE
            }).trim(source, 0, 300_000, { false }, { error("unexpected publication") })
        }
        val partial = assertNotNull(failure.partial)
        assertEquals(0L, Files.size(partial.resolve(ReplayFiles.LOGCAT_DEVICE)))
        assertEquals(0L, Files.size(partial.resolve(ReplayFiles.LOGCAT_APP)))
        assertFalse(Files.exists(partial.resolve(ReplayFiles.RECORD)))
        assertEquals(before, hashes(source))
    }

    @Test
    fun cancellationDuringMetadataCountingNeverCreatesStaging() {
        val source = fixture()
        val before = hashes(source)
        val entries = Files.list(root).use { it.toList().toSet() }
        var checks = 0
        val failure = assertFailsWith<SaveCancelled> {
            ReplayTrim().trim(source, 0, 300_000, { ++checks == 30 }, { error("unexpected publication") })
        }
        assertNull(failure.partial)
        assertEquals(entries, Files.list(root).use { it.toList().toSet() })
        assertEquals(before, hashes(source))
    }

    @Test
    fun rawSizeOverflowFailsBeforeStaging() {
        val source = parityFixture()
        val index = source.resolve(ReplayFiles.VIDEO_FRAMES)
        val rows = Files.readAllLines(index).map { JsonParser.parseString(it).asJsonObject }
        rows.first().addProperty("source_size", Long.MAX_VALUE.toString())
        Files.writeString(index, rows.joinToString("\n", postfix = "\n", transform = gson::toJson))
        refreshHashes(source)
        val before = hashes(source)
        val failure = assertFailsWith<SaveFailure> { trim(source, 0, 300_000, Long.MAX_VALUE - ReplaySettings.MIN_FREE_BYTES) }
        assertTrue(failure.cause is ArithmeticException)
        assertNull(failure.partial)
        assertEquals(before, hashes(source))
    }

    @Test
    fun logRecordIdsCopiedIntoManifestWatermarksAreAlsoReserved() {
        val source = parityFixture()
        val id = "日本😀\"\\".repeat(20_000)
        val row = com.google.gson.JsonObject().apply {
            addProperty("t_ns", null as String?)
            addProperty("record_id", id)
            addProperty("app_membership", true)
        }
        Files.writeString(source.resolve(ReplayFiles.LOGCAT_DEVICE), gson.toJson(row) + "\n")
        refreshHashes(source)
        val budget = estimate(source)
        val output = ReplayTrim(availableSpace = { ReplaySettings.MIN_FREE_BYTES + budget - partialBytes() })
            .trim(source, 0, 300_000, { false }, { Files.move(it.partial, it.complete) })
        assertTrue(Files.list(output).use { it.mapToLong(Files::size).sum() } <= budget)
        val record = JsonParser.parseString(Files.readString(output.resolve(ReplayFiles.RECORD))).asJsonObject
        assertEquals(id, record["watermarks"].asJsonObject["device_log_record_id"].asString)
        assertEquals(id, record["watermarks"].asJsonObject["app_log_record_id"].asString)
    }

    @Test
    fun utf8TouchesAreGuardedBeforeWritingWhenSpaceDrops() {
        val source = parityFixture()
        val line = """{"t_ns":null,"movie_us":null,"text":"${"日".repeat(100_000)}"}"""
        Files.writeString(source.resolve(ReplayFiles.TOUCHES), line + "\n")
        refreshHashes(source)
        val before = hashes(source)
        val available = line.toByteArray(Charsets.UTF_8).size.toLong()
        val failure = assertFailsWith<SaveFailure> {
            ReplayTrim(availableSpace = {
                val writingTouches = Files.list(root).use { it.anyMatch { p -> p.fileName.toString().endsWith(".partial") && Files.exists(p.resolve(ReplayFiles.TOUCHES)) } }
                if (writingTouches) ReplaySettings.MIN_FREE_BYTES + available else Long.MAX_VALUE
            }).trim(source, 0, 300_000, { false }, { error("unexpected publication") })
        }
        val partial = assertNotNull(failure.partial)
        assertEquals(0L, Files.size(partial.resolve(ReplayFiles.TOUCHES)))
        assertFalse(Files.exists(partial.resolve(ReplayFiles.RECORD)))
        assertEquals(before, hashes(source))
    }

    @Test
    fun manifestRemainsUnpublishedWhenSpaceFallsAfterSidecars() {
        val source = parityFixture()
        val before = hashes(source)
        val failure = assertFailsWith<SaveFailure> {
            ReplayTrim(availableSpace = {
                val wroteReadme = Files.list(root).use { it.anyMatch { p -> p.fileName.toString().endsWith(".partial") && Files.exists(p.resolve(ReplayFiles.SUMMARY)) } }
                if (wroteReadme) ReplaySettings.MIN_FREE_BYTES else Long.MAX_VALUE
            }).trim(source, 0, 300_000, { false }, { error("unexpected publication") })
        }
        val partial = assertNotNull(failure.partial)
        assertTrue(Files.exists(partial.resolve(ReplayFiles.SUMMARY)))
        assertFalse(Files.exists(partial.resolve(ReplayFiles.RECORD)))
        assertEquals(before, hashes(source))
    }
}
