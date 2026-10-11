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
            SaveWriter().write(capture, root, { false }) { publication ->
                val partial = publication.partial
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
            val earlier = Files.list(root).use { it.toList().toSet() }
            var marker: Path? = null
            var identity: Any? = null
            val failure = assertFailsWith<SaveFailure> {
                SaveWriter().write(capture, root, {
                    if (marker == null) {
                        val partial = Files.list(root).use { paths -> paths
                            .filter { it !in earlier && it.fileName.toString().endsWith(".partial") && Files.exists(it.resolve("frames.jsonl")) }
                            .filter { !Files.exists(it.resolve(name), NOFOLLOW_LINKS) }.findFirst().orElse(null) }
                        if (partial != null) {
                            marker = partial.resolve(name)
                            Files.write(marker, markerBytes, CREATE_NEW, WRITE)
                            identity = key(marker!!)
                        }
                    }
                    false
                }) { _ -> error("collision must not publish") }
            }
            assertNotNull(marker)
            assertTrue(failure.cause is java.nio.file.FileAlreadyExistsException)
            assertNoComplete(root)
            assertFailsWith<IllegalStateException> { store.capture(ReplaySettings()) }
            assertTrue(Files.exists(marker, NOFOLLOW_LINKS), "cleanup must preserve the unowned $name")
            assertContentEquals(markerBytes, Files.readAllBytes(marker))
            assertEquals(identity, key(marker))
            assertEquals(marker.parent, failure.partial)
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
                    SaveWriter().write(capture, root, { false }) { publication ->
                        val partial = publication.partial
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
                assertTrue(Files.exists(retained.resolve("frames.jsonl"), NOFOLLOW_LINKS))
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
                SaveWriter().write(capture, root, { false }) { publication ->
                    val partial = publication.partial
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
    fun failureAndCancellationRetainAttemptOutputsRatherThanDeleteMutablePaths() = fixture { root, store, capture ->
        var attempted: Path? = null
        val failure = assertFailsWith<SaveFailure> {
            SaveWriter().write(capture, root, { false }) { publication ->
                val partial = publication.partial
                attempted = partial
                throw IOException("publication fixture failure")
            }
        }
        val retained = assertNotNull(failure.partial)
        assertEquals(attempted, retained)
        assertTrue(failure.message.orEmpty().contains("未完成"))
        assertTrue(failure.message.orEmpty().contains(retained.toString()))
        assertFalse(JsonParser.parseString(Files.readString(retained.resolve("session.json"))).asJsonObject["complete"].asBoolean)
        val earlier = Files.list(root).use { it.toList().toSet() }
        val cancelled = assertFailsWith<CancellationException> {
            SaveWriter().write(capture, root, {
                Files.list(root).use { paths -> paths.anyMatch { it !in earlier && it.fileName.toString().endsWith(".partial") } }
            }) { _ -> error("cancelled save must not publish") }
        }
        val partials = Files.list(root).use { paths -> paths.filter { it.fileName.toString().endsWith(".partial") }.toList() }
        assertEquals(2, partials.size)
        val cancelledOutput = partials.single { it != retained }
        assertTrue(Files.exists(cancelledOutput.resolve("frames.jsonl")))
        assertTrue(cancelled.message.orEmpty().contains(cancelledOutput.toString()))
        assertNoComplete(root)
        assertFailsWith<IllegalStateException> { store.capture(ReplaySettings()) }
        retry(capture, root)
    }

    @Test
    fun createdDescriptorSuppliesIdentityAndRejectsAnotherIntactFile() = fixture { root, _, _ ->
        val first = root.resolve("first.txt")
        val second = root.resolve("second.txt")
        SaveOutputDirectory.open(root).use { directory ->
            directory.createFile(first.fileName.toString()).use { created ->
                created.channel().use { it.write(ByteBuffer.wrap(markerBytes)) }
                val createdKey = created.key
                assertEquals((Files.getAttribute(first, "unix:dev") as Number).toLong() to
                    (Files.getAttribute(first, "unix:ino") as Number).toLong(), createdKey)
                assertEquals(sha256(first), created.hash())
                directory.verifyFile(first.fileName.toString(), created)
                directory.createFile(second.fileName.toString()).use { other ->
                    other.channel().use { it.write(ByteBuffer.wrap(markerBytes)) }
                    assertFalse(created.key == other.key)
                    assertFailsWith<IllegalStateException> { directory.verifyFile(second.fileName.toString(), created) }
                    assertContentEquals(markerBytes, Files.readAllBytes(first))
                    assertContentEquals(markerBytes, Files.readAllBytes(second))
                }
                assertEquals(createdKey, created.key)
                assertEquals(sha256(first), created.hash())
            }
        }
    }

    private fun retry(capture: FrozenCapture, root: Path): SaveOutput {
        val output = SaveWriter().write(capture, root, { false }) { publication -> Files.move(publication.partial, publication.complete) }
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
