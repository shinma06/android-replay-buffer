package io.github.shinma06.replaybuffer.core

import com.google.gson.Gson
import com.google.gson.JsonObject
import org.junit.jupiter.api.io.TempDir
import java.io.IOException
import java.nio.ByteBuffer
import java.nio.channels.FileChannel
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardOpenOption.READ
import java.security.MessageDigest
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFails
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNotSame
import kotlin.test.assertSame
import kotlin.test.assertTrue

class SavedReplayIdentityTest {
    @TempDir lateinit var root: Path

    private fun fixture(hashes: Boolean = true): Path {
        val dir = Files.createDirectory(root.resolve("source"))
        val contents = linkedMapOf(
            ReplayFiles.VIDEO to "0123456789",
            ReplayFiles.VIDEO_FRAMES to "{\"presented\":true,\"movie_pts_us\":\"0\",\"movie_end_us\":\"10\"}\n",
            ReplayFiles.LOGCAT_DEVICE to "original log\n",
            ReplayFiles.TOUCHES to "{\"movie_us\":\"1\"}\n",
            ReplayFiles.VIDEO_SOURCE to "original raw",
        )
        contents.forEach { (name, text) -> Files.writeString(dir.resolve(name), text) }
        val record = JsonObject().apply {
            addProperty("schema", 4); addProperty("complete", true)
            add("files", JsonObject().apply {
                addProperty("video", ReplayFiles.VIDEO); addProperty("video_frames", ReplayFiles.VIDEO_FRAMES)
            })
            add("files_sha256", JsonObject().apply {
                if (hashes) contents.forEach { (name, text) -> addProperty(name, sha(text.toByteArray())) }
            })
        }
        Files.writeString(dir.resolve(ReplayFiles.RECORD), Gson().toJson(record))
        return dir
    }

    private fun sha(bytes: ByteArray) = MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }
    private fun snapshot(dir: Path) = Files.list(dir).use { paths -> paths.toList().associate { it.fileName.toString() to Files.readAllBytes(it) } }
    private fun assertSame(before: Map<String, ByteArray>, dir: Path) {
        val after = snapshot(dir)
        assertEquals(before.keys, after.keys)
        before.forEach { (name, bytes) -> assertContentEquals(bytes, after.getValue(name), name) }
    }

    private fun swap(dir: Path, name: String) {
        Files.move(dir.resolve(name), dir.resolve("held-$name"))
        Files.writeString(dir.resolve(name), "replacement\n")
    }

    @Test fun everyReaderRejectsAReplacementAndRecordAndRawArePinnedToo() {
        val dir = fixture()
        SavedReplay.open(dir).use { saved ->
            val names = listOf(ReplayFiles.VIDEO, ReplayFiles.VIDEO_FRAMES, ReplayFiles.LOGCAT_DEVICE,
                ReplayFiles.TOUCHES, ReplayFiles.RECORD, ReplayFiles.VIDEO_SOURCE)
            names.forEach { swap(dir, it) }
            val replaced = snapshot(dir)
            names.forEach { name ->
                assertFailsWith<IllegalStateException>(name) { saved.openFile(name).use {} }
                assertFailsWith<IllegalStateException>(name) { saved.channel(name).use {} }
                assertFailsWith<IllegalStateException>(name) { saved.forEachLine(name) { error("replacement read") } }
            }
            assertSame(replaced, dir)
        }
    }

    @Test fun theFramesParserCannotReadAFileSwappedAfterItsHash() {
        val dir = fixture()
        assertFailsWith<IllegalStateException> {
            SavedReplay.open(dir) { name, _ ->
                if (name == ReplayFiles.VIDEO_FRAMES) {
                    Files.move(dir.resolve(name), dir.resolve("held-$name"))
                    Files.writeString(dir.resolve(name), "{\"movie_pts_us\":\"999\"}\n")
                }
            }.close()
        }
    }

    @Test fun missingHashEntriesKeepTheirAcceptanceButNotLateIdentitySelection() {
        val dir = fixture(hashes = false)
        SavedReplay.open(dir).use { saved ->
            assertEquals("0123456789", saved.text(ReplayFiles.VIDEO))
            assertTrue(saved.manifest["files_sha256"].asJsonObject.entrySet().isEmpty())
            swap(dir, ReplayFiles.TOUCHES)
            assertFailsWith<IllegalStateException> { saved.timeline() }
            swap(dir, ReplayFiles.VIDEO)
            assertFailsWith<IllegalStateException> { saved.openFile(ReplayFiles.VIDEO).use {} }
        }
    }

    @Test fun readersHaveIndependentPositionsAndClosingOneDoesNotCloseThePin() {
        val dir = fixture()
        val before = snapshot(dir)
        val pins = mutableListOf<SaveOutputFile>()
        val saved = SavedReplay.open(dir) { _, pin -> pins += pin }
        try {
            assertTrue(pins.all { !it.isClosed })
            saved.channel(ReplayFiles.VIDEO).use { first ->
                saved.openFile(ReplayFiles.VIDEO).use { file -> file.channel(setOf(READ)).use { second ->
                    fun next(channel: FileChannel) = ByteBuffer.allocate(1).also { assertEquals(1, channel.read(it)) }.array()[0].toInt().toChar()
                    assertEquals('0', next(first)); first.position(6)
                    assertEquals('0', next(second)); second.position(2)
                    assertEquals('6', next(first)); first.close()
                    assertEquals('2', next(second))
                } }
            }
            assertEquals("0123456789", saved.text(ReplayFiles.VIDEO))
            assertFailsWith<IllegalArgumentException> { saved.forEachLine(ReplayFiles.LOGCAT_DEVICE) { throw IllegalArgumentException("consumer") } }
            assertEquals(listOf("original log"), saved.lines(ReplayFiles.LOGCAT_DEVICE))
            assertTrue(pins.all { !it.isClosed })
        } finally { saved.close(); saved.close() }
        assertTrue(pins.all { it.isClosed })
        assertFailsWith<IllegalStateException> { saved.openFile(ReplayFiles.VIDEO) }
        assertSame(before, dir)
    }

    @Test fun aHashFailureOrObserverFailureClosesAllAcquiredPins() {
        val dir = fixture()
        val pins = mutableListOf<SaveOutputFile>()
        assertFailsWith<IllegalArgumentException> {
            SavedReplay.open(dir) { _, pin -> pins += pin; throw IllegalArgumentException("stop") }
        }
        assertTrue(pins.isNotEmpty()); assertTrue(pins.all { it.isClosed })
        Files.writeString(dir.resolve(ReplayFiles.VIDEO), "changed in place")
        assertFailsWith<IllegalArgumentException> { SavedReplay.open(dir) }
    }

    @Test fun parserFailureClosesTheStillLivePins() {
        val dir = fixture()
        val pins = mutableListOf<SaveOutputFile>()
        assertFails {
            SavedReplay.open(dir) { name, pin ->
                pins += pin
                if (name == ReplayFiles.VIDEO_FRAMES) Files.writeString(dir.resolve(name), "{invalid\n")
            }
        }
        assertTrue(pins.isNotEmpty()); assertTrue(pins.all { it.isClosed })
    }

    @Test fun failedPlaybackParserInitializationReleasesItsFreshFile() {
        val dir = fixture()
        SavedReplay.open(dir).use { saved ->
            assertFails { ReplayFrameCursor(saved).close() }
            saved.manifest.add("audio_track", JsonObject().apply {
                addProperty("sample_rate", 48000); add("segments", com.google.gson.JsonArray())
            })
            assertFails { ReplayAudioTrack.open(saved)?.close() }
            val field = SavedReplay::class.java.getDeclaredField("openFiles").apply { isAccessible = true }
            @Suppress("UNCHECKED_CAST")
            val readers = field.get(saved) as List<SaveOutputFile>
            assertTrue(readers.all { it.isClosed })
            assertFalse(saved.lines(ReplayFiles.LOGCAT_DEVICE).isEmpty())
        }
    }

    @Test fun frameChannelAcquisitionFailureClosesItsFreshFile() = channelAcquisitionFailure(audio = false)

    @Test fun audioChannelAcquisitionFailureClosesItsFreshFile() = channelAcquisitionFailure(audio = true)

    private fun channelAcquisitionFailure(audio: Boolean) {
        val dir = fixture()
        val before = snapshot(dir)
        val pins = linkedMapOf<String, SaveOutputFile>()
        SavedReplay.open(dir) { name, pin -> pins[name] = pin }.use { saved ->
            if (audio) saved.manifest.add("audio_track", JsonObject().apply {
                addProperty("sample_rate", 48000); add("segments", com.google.gson.JsonArray())
            })
            val failure = IOException("channel acquisition boundary")
            lateinit var fresh: SaveOutputFile
            var calls = 0
            val acquire: (SaveOutputFile) -> FileChannel = { file ->
                fresh = file; calls++
                assertFalse(file.isClosed)
                throw failure
            }
            val thrown = assertFailsWith<IOException> {
                if (audio) ReplayAudioTrack.open(saved, acquire)?.close()
                else ReplayFrameCursor(saved, acquire).close()
            }
            assertSame(failure, thrown) // The channel acquisition itself threw; no parser was reached.
            assertEquals(1, calls)
            assertNotSame(pins.getValue(ReplayFiles.VIDEO), fresh)
            assertEquals(pins.getValue(ReplayFiles.VIDEO).key, fresh.key)
            assertTrue(fresh.isClosed, "fresh file must close before SavedReplay.close")
            assertTrue(pins.values.all { !it.isClosed })
            assertEquals("0123456789", saved.text(ReplayFiles.VIDEO))
            assertSame(before, dir)
        }
        assertTrue(pins.values.all { it.isClosed })
    }
}
