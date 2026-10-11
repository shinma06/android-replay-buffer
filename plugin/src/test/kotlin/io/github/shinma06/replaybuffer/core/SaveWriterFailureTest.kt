package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonParser
import org.jcodec.codecs.h264.H264Encoder
import org.jcodec.codecs.h264.H264Utils
import org.jcodec.common.model.ColorSpace
import org.jcodec.common.model.Picture
import java.io.IOException
import java.nio.ByteBuffer
import java.nio.file.Files
import java.nio.file.LinkOption.NOFOLLOW_LINKS
import java.nio.file.Path
import java.nio.file.StandardOpenOption.CREATE_NEW
import java.nio.file.StandardOpenOption.WRITE
import java.nio.file.attribute.BasicFileAttributes
import java.util.concurrent.CancellationException
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

class SaveWriterFailureTest {
    private val markerBytes = "foreign fixture marker\n".toByteArray()

    @Test
    fun retainedPartialReportsIncompleteLocationAndFixedCaptureRetriesElsewhere() = fixture { root, store, capture ->
        var marker: Path? = null
        var identity: Any? = null
        val failure = assertFailsWith<SaveFailure> {
            SaveWriter().write(capture, root, { false }) { partial, _ ->
                marker = partial.resolve("foreign.txt")
                Files.write(marker, markerBytes, CREATE_NEW, WRITE)
                identity = key(marker!!)
                throw IOException("publication fixture failure")
            }
        }
        val retained = assertNotNull(failure.partial)
        assertEquals(retained, marker!!.parent)
        assertContentEquals(markerBytes, Files.readAllBytes(marker))
        assertEquals(identity, key(marker))
        assertNoComplete(root)
        assertFailsWith<IllegalStateException> { store.capture(ReplaySettings()) }
        val retry = retry(capture, root)
        assertFalse(retry.directory == retained)
        assertContentEquals(markerBytes, Files.readAllBytes(marker))
        assertEquals(identity, key(marker))
        assertTrue(failure.message.orEmpty().contains(retained.toString()), "retained location must reach Core's error message")
        assertTrue(failure.message.orEmpty().contains("未完成"))
    }

    @Test
    fun createNewCollisionsPreserveForeignBytesAndIdentity() = fixture { root, store, capture ->
        for (name in listOf("logcat-device.jsonl", "logcat-app.jsonl", "README.txt", "session.json")) {
            var marker: Path? = null
            var identity: Any? = null
            val failure = assertFailsWith<SaveFailure> {
                SaveWriter().write(capture, root, {
                    if (marker == null) {
                        val partial = Files.list(root).use { paths -> paths
                            .filter { it.fileName.toString().endsWith(".partial") && Files.exists(it.resolve("frames.jsonl")) }
                            .filter { !Files.exists(it.resolve(name), NOFOLLOW_LINKS) }.findFirst().orElse(null) }
                        if (partial != null) {
                            marker = partial.resolve(name)
                            Files.write(marker, markerBytes, CREATE_NEW, WRITE)
                            identity = key(marker!!)
                        }
                    }
                    false
                }) { _, _ -> error("collision must not publish") }
            }
            assertNotNull(marker)
            assertTrue(failure.cause is java.nio.file.FileAlreadyExistsException)
            assertNoComplete(root)
            assertFailsWith<IllegalStateException> { store.capture(ReplaySettings()) }
            assertTrue(Files.exists(marker, NOFOLLOW_LINKS), "cleanup must preserve the unowned $name")
            assertContentEquals(markerBytes, Files.readAllBytes(marker))
            assertEquals(identity, key(marker))
            assertEquals(marker.parent, failure.partial)
            Files.delete(marker)
            Files.delete(failure.partial)
        }
        retry(capture, root)
    }

    @Test
    fun replacedOwnedFilesAndSymlinksSurviveCleanupAndKeepCompletedEvidence() = fixture { root, store, capture ->
        val completed = retry(capture, root).directory.resolve("session.json")
        val completedBytes = Files.readAllBytes(completed)
        val completedKey = key(completed)
        for (name in listOf("frames.jsonl", "video-001.mp4", "logcat-device.jsonl", "logcat-app.jsonl", "README.txt", "session.json")) {
            for (symlink in listOf(false, true)) {
                var replacement: Path? = null
                var identity: Any? = null
                val original = root.resolve("original-$name-$symlink")
                val failure = assertFailsWith<SaveFailure> {
                    SaveWriter().write(capture, root, { false }) { partial, _ ->
                        replacement = partial.resolve(name)
                        Files.move(replacement, original)
                        if (symlink) Files.createSymbolicLink(replacement, original)
                        else Files.write(replacement, markerBytes, CREATE_NEW, WRITE)
                        identity = key(replacement!!)
                        throw IOException("replacement fixture failure")
                    }
                }
                val retained = assertNotNull(failure.partial)
                val path = assertNotNull(replacement)
                assertEquals(identity, key(path))
                if (symlink) assertEquals(original, Files.readSymbolicLink(path))
                else assertContentEquals(markerBytes, Files.readAllBytes(path))
                assertContentEquals(completedBytes, Files.readAllBytes(completed))
                assertEquals(completedKey, key(completed))
                assertFailsWith<IllegalStateException> { store.capture(ReplaySettings()) }
                Files.delete(path)
                Files.delete(retained)
            }
        }
        retry(capture, root)
    }

    @Test
    fun replacedPartialDirectoryIsNotDeletedOrReportedAsOurRetainedOutput() = fixture { root, _, capture ->
        for (symlink in listOf(false, true)) {
            var replacement: Path? = null
            var identity: Any? = null
            val original = root.resolve("original-partial-$symlink")
            val failure = assertFailsWith<SaveFailure> {
                SaveWriter().write(capture, root, { false }) { partial, _ ->
                    replacement = partial
                    Files.move(partial, original)
                    if (symlink) Files.createSymbolicLink(partial, original)
                    else Files.createDirectory(partial)
                    identity = key(partial)
                    throw IOException("directory replacement fixture failure")
                }
            }
            val path = assertNotNull(replacement)
            assertEquals(identity, key(path))
            assertTrue(Files.exists(original.resolve("session.json")))
            if (symlink) assertEquals(original, Files.readSymbolicLink(path))
            else assertEquals(0L, Files.list(path).use { it.count() })
            assertNull(failure.partial)
            assertFalse(failure.message.orEmpty().contains(path.toString()))
            Files.delete(path)
        }
    }

    @Test
    fun successfulCleanupReportsNoRetainedLocationAndCancellationKeepsPin() = fixture { root, store, capture ->
        var attempted: Path? = null
        val failure = assertFailsWith<SaveFailure> {
            SaveWriter().write(capture, root, { false }) { partial, _ ->
                attempted = partial
                throw IOException("publication fixture failure")
            }
        }
        assertNull(failure.partial)
        assertFalse(Files.exists(attempted, NOFOLLOW_LINKS))
        assertFalse(failure.message.orEmpty().contains("未完成"))
        assertFalse(failure.message.orEmpty().contains(attempted.toString()))
        assertFailsWith<CancellationException> {
            SaveWriter().write(capture, root, {
                Files.list(root).use { paths -> paths.anyMatch { it.fileName.toString().endsWith(".partial") } }
            }) { _, _ -> error("cancelled save must not publish") }
        }
        assertEquals(listOf(root.resolve("ring")), Files.list(root).use { it.toList() })
        assertFailsWith<IllegalStateException> { store.capture(ReplaySettings()) }
        retry(capture, root)
    }

    private fun retry(capture: FrozenCapture, root: Path): SaveOutput {
        val output = SaveWriter().write(capture, root, { false }) { a, b -> Files.move(a, b) }
        val manifest = JsonParser.parseString(Files.readString(output.directory.resolve("session.json"))).asJsonObject
        assertTrue(manifest["complete"].asBoolean)
        assertEquals(capture.id, manifest["save_id"].asString)
        return output
    }

    private fun assertNoComplete(root: Path) = Files.list(root).use { paths ->
        assertFalse(paths.anyMatch { it.fileName.toString().startsWith("replay-") })
    }

    private fun key(path: Path) = assertNotNull(Files.readAttributes(path, BasicFileAttributes::class.java, NOFOLLOW_LINKS).fileKey())

    private fun fixture(check: (Path, CaptureStore, FrozenCapture) -> Unit) {
        val root = Files.createTempDirectory("replay-save-failure-").toRealPath()
        val store = CaptureStore(root.resolve("ring"), minFree = 0)
        try {
            val host = System.nanoTime()
            store.generation(1)
            store.session(VideoPacket.Session(32, 32), 1)
            store.clock.add("fixture", listOf(1_000_000_000L, 1_000_000_000L, 1_700_000_001_000_000_000L, 1_000_000_000L), host, host)
            val encoded = H264Encoder.createH264Encoder().encodeFrame(
                Picture.create(32, 32, ColorSpace.YUV420J).apply { fill(20) }, ByteBuffer.allocate(65536),
            ).data
            val bytes = ByteArray(encoded.remaining()).also { encoded.get(it) }
            val config = H264Utils.splitFrame(ByteBuffer.wrap(bytes)).filter { it.get(0).toInt() and 31 in setOf(7, 8) }
                .fold(byteArrayOf()) { acc, nal -> acc + byteArrayOf(0, 0, 0, 1) + ByteArray(nal.remaining()).also { nal.get(it) } }
            assertFalse(store.frame(VideoPacket.Frame(0, true, false, config), 1, host))
            assertTrue(store.frame(VideoPacket.Frame(1_000_000, false, true, bytes), 1, host))
            store.clock.add("fixture", listOf(1_500_000_000L, 1_500_000_000L, 1_700_000_001_500_000_000L, 1_500_000_000L), host + 500_000_000, host + 500_000_000)
            store.freeze()
            check(root, store, assertNotNull(store.capture(ReplaySettings())))
        } finally {
            store.close()
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach(Files::deleteIfExists) }
        }
    }
}
