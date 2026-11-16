package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonObject
import com.google.gson.JsonParser
import org.jcodec.codecs.h264.H264Encoder
import org.jcodec.codecs.h264.H264Utils
import org.jcodec.common.model.ColorSpace
import org.jcodec.common.model.Picture
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.ByteBuffer
import java.nio.file.Files
import java.nio.file.Path
import java.security.MessageDigest
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

class ReplayTrimAutoTriggerTest {
    @TempDir lateinit var root: Path
    private val evidenceNames = listOf("java_evidence", "platform_evidence")
    private val relativeNames = listOf("arming_t_ns", "desired_pre_report_start_t_ns", "desired_pre_report_end_t_ns")
    private fun record(path: Path) = JsonParser.parseString(Files.readString(path.resolve(ReplayFiles.RECORD))).asJsonObject
    private fun rows(path: Path, file: String) = Files.readAllLines(path.resolve(file)).map { JsonParser.parseString(it).asJsonObject }
    private fun bytes(path: Path) = Files.list(path).use { files -> files.toList().associate { it.fileName.toString() to Files.readAllBytes(it) } }
    private fun unchanged(path: Path, expected: Map<String, ByteArray>) {
        val actual = bytes(path)
        assertEquals(expected.keys, actual.keys)
        expected.forEach { (name, value) ->
            assertContentEquals(value, actual.getValue(name), name)
            assertContentEquals(MessageDigest.getInstance("SHA-256").digest(value),
                MessageDigest.getInstance("SHA-256").digest(actual.getValue(name)), name)
        }
    }

    private fun source(unknown: Set<Int> = emptySet()): Path = CrashFixture(
        Files.createDirectory(root.resolve("capture")), CaptureMode.VIDEO_AND_LOGS
    ).use { fixture ->
        val encoded = H264Encoder.createH264Encoder().encodeFrame(
            Picture.create(32, 32, ColorSpace.YUV420J).apply { fill(15) }, ByteBuffer.allocate(65536)).data
        val packet = ByteArray(encoded.remaining()).also { encoded.get(it) }
        val config = H264Utils.splitFrame(ByteBuffer.wrap(packet)).filter { it.get(0).toInt() and 31 in setOf(7, 8) }
            .fold(byteArrayOf()) { bytes, nal -> bytes + byteArrayOf(0, 0, 0, 1) + ByteArray(nal.remaining()).also { nal.get(it) } }
        fixture.store.session(VideoPacket.Session(32, 32), 1)
        fixture.store.frame(VideoPacket.Frame(0, true, false, config), 1, fixture.at(9_900_000_000))
        for (n in 0 until 12) {
            val time = 9_900_000_000L + n * 10_000_000L
            assertTrue(fixture.store.frame(VideoPacket.Frame((100_000_000_000L + time) / 1000, false, true, packet),
                1, fixture.at(time + 1_000_000)))
        }
        fixture.standard()
        val capture = assertNotNull(fixture.store.capture(fixture.settings, fixture.pair(), fixture.at(10_020_000_000)))
        SaveWriter().write(capture.copy(video = capture.video.mapIndexed { n, frame ->
            if (n in unknown) frame.copy(time = MappedTime(null, frame.time.region, Long.MAX_VALUE)) else frame
        }), fixture.root, { false }) { Files.move(it.partial, it.complete) }.directory
    }

    private fun trim(path: Path, first: Int, end: Int): Path {
        val frames = rows(path, ReplayFiles.VIDEO_FRAMES).filter { it["presented"].asBoolean }
        return ReplayTrim().trim(path, frames[first]["movie_pts_us"].asString.toLong(),
            frames[end]["movie_end_us"].asString.toLong(), { false }, { Files.move(it.partial, it.complete) })
    }

    private fun checkEvidence(source: Path, output: Path, origin: Long?) {
        val old = record(source)["auto_trigger"].asJsonObject
        val actual = record(output)["auto_trigger"].asJsonObject
        val logs = rows(output, ReplayFiles.LOGCAT_DEVICE)
        val originalRows = rows(source, ReplayFiles.LOGCAT_DEVICE).associateBy { it["record_id"].asString }
        for (name in evidenceNames) {
            val before = old[name].asJsonObject
            val evidence = actual[name].asJsonObject
            val row = logs.singleOrNull { it["record_id"] == evidence["record_id"] }
            assertEquals(row != null, evidence["in_output"].asBoolean, name)
            val expected = row?.get("t_ns")?.takeUnless { it.isJsonNull }?.asString
                ?: before["frozen_t_ns"].takeUnless { it.isJsonNull }?.asString?.toLong()
                    ?.let { time -> origin?.let { (time - it).toString() } }
            assertEquals(expected, evidence["frozen_t_ns"].takeUnless { it.isJsonNull }?.asString, name)
            if (row != null) {
                assertEquals(row["t_ns"], evidence["frozen_t_ns"])
                assertEquals(row["t_uncertainty_ns"], evidence["t_uncertainty_ns"])
                val original = originalRows.getValue(row["record_id"].asString)
                for (key in listOf("payload_base64", "epoch_ns", "elapsed_ns", "pid", "uid", "lid")) assertEquals(original[key], row[key], key)
            }
            val facts = before.deepCopy().apply { remove("frozen_t_ns"); remove("in_output"); remove("t_uncertainty_ns") }
            assertEquals(facts, evidence.deepCopy().apply { remove("frozen_t_ns"); remove("in_output"); remove("t_uncertainty_ns") })
        }
        for (name in relativeNames) {
            val expected = old[name].takeUnless { it.isJsonNull }?.asString?.toLong()?.let { time -> origin?.let { (time - it).toString() } }
            assertEquals(expected, actual[name].takeUnless { it.isJsonNull }?.asString, name)
        }
        fun facts(trigger: JsonObject) = trigger.deepCopy().apply { (evidenceNames + relativeNames).forEach(::remove) }
        assertEquals(facts(old), facts(actual))
    }

    @Test
    fun retainedEvidenceTracksTheWrittenRowsAndNewOriginAcrossRetrim() {
        val source = source()
        val before = bytes(source)
        val origin = rows(source, ReplayFiles.VIDEO_FRAMES)[3]["display_t_ns"].asString.toLong()
        val output = trim(source, 3, 11)
        checkEvidence(source, output, origin)
        val outputBefore = bytes(output)
        val repeatedOrigin = rows(output, ReplayFiles.VIDEO_FRAMES)[2]["display_t_ns"].asString.toLong()
        checkEvidence(output, trim(output, 2, 8), repeatedOrigin)
        unchanged(source, before)
        unchanged(output, outputBefore)
    }

    @Test
    fun excludedEvidenceDoesNotClaimAnUnrelatedRowIsTheEvidence() {
        val source = source()
        val before = bytes(source)
        val output = trim(source, 0, 7)
        assertTrue(rows(output, ReplayFiles.LOGCAT_DEVICE).isNotEmpty())
        evidenceNames.forEach { assertFalse(record(output)["auto_trigger"].asJsonObject[it].asJsonObject["in_output"].asBoolean) }
        checkEvidence(source, output, 0)
        val outputBefore = bytes(output)
        checkEvidence(output, trim(output, 1, 6), 10_000_000)
        unchanged(source, before)
        unchanged(output, outputBefore)
    }

    @Test
    fun unknownOriginKeepsEvidenceRowsWithoutInventingTheirRelativeTimes() {
        val source = source(setOf(3))
        val before = bytes(source)
        val output = trim(source, 3, 11)
        assertFalse(record(output)["timebase"].asJsonObject["known"].asBoolean)
        checkEvidence(source, output, null)
        val outputBefore = bytes(output)
        val repeated = trim(output, 0, 8)
        assertFalse(record(repeated)["timebase"].asJsonObject["known"].asBoolean)
        checkEvidence(output, repeated, null)
        unchanged(source, before)
        unchanged(output, outputBefore)
    }
}
