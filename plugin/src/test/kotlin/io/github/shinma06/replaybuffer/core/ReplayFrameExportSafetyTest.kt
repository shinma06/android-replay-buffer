package io.github.shinma06.replaybuffer.core

import io.github.shinma06.replaybuffer.testing.TestPermissions
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import org.junit.jupiter.params.ParameterizedTest
import org.junit.jupiter.params.provider.ValueSource
import java.nio.file.Files
import java.nio.file.LinkOption.NOFOLLOW_LINKS
import java.nio.file.Path
import java.security.MessageDigest
import java.time.ZonedDateTime
import java.util.concurrent.CancellationException
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertSame
import kotlin.test.assertTrue

class ReplayFrameExportSafetyTest {
    @TempDir lateinit var root: Path
    private val createdAt = ZonedDateTime.parse("2024-01-02T03:04:05Z")
    private val id = "deadbeef"

    private fun fixture(): Path {
        val parent = Files.createDirectory(root.resolve("保存元"))
        val source = Files.createDirectory(parent.resolve("replay-20240101-000000-manual-feedface"))
        for (name in listOf("README.md", "replay.json", "video.mp4", "video-source.h264", "video-frames.jsonl",
            "logcat-device.jsonl", "logcat-app.jsonl", "touches.jsonl")) {
            assertNotNull(javaClass.getResourceAsStream("/replay-trim-parity/${source.fileName}/$name")).use {
                Files.copy(it, source.resolve(name))
            }
        }
        return source
    }

    private data class Tree(val entries: Map<String, String>, val bytes: Map<String, ByteArray>, val hashes: Map<String, String>)

    private fun tree(source: Path): Tree = Files.walk(source).use { walk ->
        val paths = walk.toList()
        val bytes = paths.filter { Files.isRegularFile(it, NOFOLLOW_LINKS) }
            .associate { source.relativize(it).toString() to Files.readAllBytes(it) }
        Tree(paths.associate { source.relativize(it).toString() to when {
            Files.isSymbolicLink(it) -> "link:${Files.readSymbolicLink(it)}"
            Files.isDirectory(it, NOFOLLOW_LINKS) -> "directory"
            else -> "file"
        } }, bytes, bytes.mapValues { (_, value) -> java.util.HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256").digest(value)) })
    }

    private fun unchanged(source: Path, before: Tree) {
        val after = tree(source)
        assertEquals(before.entries, after.entries, "source directory entries")
        assertEquals(before.bytes.keys, after.bytes.keys)
        before.bytes.forEach { (name, bytes) -> assertContentEquals(bytes, after.bytes[name], name) }
        assertEquals(before.hashes, after.hashes)
    }

    private fun entries(directory: Path) = Files.list(directory).use { it.toList().map { path -> path.fileName.toString() }.toSet() }

    @ParameterizedTest
    @ValueSource(strings = ["last-decode", "encoded", "before-create"])
    fun cancellationBeforeOutputLeavesNeitherPngNorPartial(stage: String) {
        val source = fixture()
        val before = tree(source)
        for (cancel in listOf(SaveCancelled(null), InterruptedException("cancelled"), CancellationException("cancelled"))) {
            val destination = Files.createDirectory(root.resolve("shots-${cancel.javaClass.simpleName}"))
            val cancelled = AtomicBoolean()
            var encoded = false
            var opened = false
            val io = ReplayFrameExport.Io(
                encode = { frame, at, name, saveId ->
                    ReplayFrameExport.pngBytes(frame.image, at, frame.row, name, saveId).also {
                        encoded = true
                        if (stage == "encoded") cancelled.set(true)
                    }
                },
                openDirectory = { path -> SaveOutputDirectory.open(path).also {
                    opened = true
                    if (stage == "before-create") cancelled.set(true)
                } },
            )
            val failure = assertFailsWith<Exception> {
                ReplayFrameExport.export(source, 0, destination, io = io, checkActive = {
                    if (cancelled.get()) throw cancel
                    // At movie 0 there is one decode iteration. Model cancellation arriving after its last poll.
                    if (stage == "last-decode") cancelled.set(true)
                })
            }
            assertSame(cancel, failure, "cancellation must not become ScreenshotSaveFailure")
            assertEquals(stage != "last-decode", encoded)
            assertEquals(stage == "before-create", opened)
            assertEquals(emptySet(), entries(destination), "no PNG or partial before writing")
            unchanged(source, before)
        }
    }

    @ParameterizedTest
    @ValueSource(strings = ["self", "child", "self-alias", "child-alias", "parent-alias", "input-parent-alias"])
    fun sourceAndDescendantDestinationsAreRejectedWithoutAnyMutation(mode: String) {
        val source = fixture()
        val child = Files.createDirectories(source.resolve("子directory/nested"))
        Files.writeString(child.resolve("keep.txt"), "unchanged child")
        val input = if (mode == "input-parent-alias")
            Files.createSymbolicLink(root.resolve("input-parent"), source.parent).resolve(source.fileName) else source
        val destination = when (mode) {
            "self" -> source
            "child" -> child
            "self-alias" -> Files.createSymbolicLink(root.resolve("source-alias"), source)
            "child-alias" -> Files.createSymbolicLink(root.resolve("child-alias"), child)
            "input-parent-alias" -> child
            else -> Files.createSymbolicLink(root.resolve("parent-alias"), source.parent).resolve(source.fileName).resolve("子directory")
        }
        val before = tree(source)
        val failure = assertFailsWith<IllegalArgumentException> { ReplayFrameExport.export(input, 0, destination) }
        assertTrue(failure.message.orEmpty().contains("元フォルダ"))
        unchanged(source, before)
    }

    @ParameterizedTest
    @ValueSource(booleans = [false, true])
    fun aSourceMovedAfterPathResolutionStillCannotBecomeTheDestination(descendant: Boolean) {
        val source = fixture()
        if (descendant) Files.createDirectory(source.resolve("shots"))
        val before = tree(source)
        val moved = Files.createDirectory(root.resolve("replacement"))
        val destination = if (descendant) Files.createDirectory(moved.resolve("shots")) else moved
        val failure = assertFailsWith<IllegalArgumentException> {
            ReplayFrameExport.export(source, 0, destination, io = ReplayFrameExport.Io(openDirectory = { path ->
                // Source readers have closed, so this controlled rename works on every supported OS.
                if (descendant) Files.delete(destination)
                Files.delete(moved)
                Files.move(source, moved)
                SaveOutputDirectory.open(path)
            }))
        }
        assertTrue(failure.message.orEmpty().contains("元フォルダ"))
        unchanged(moved, before)
    }

    @ParameterizedTest
    @ValueSource(strings = ["external", "non-ascii", "sibling", "external-alias", "source-parent-alias"])
    fun externalDestinationsAndLegalAliasesStillWork(mode: String) {
        val source = fixture()
        var input = source
        val destination = when (mode) {
            "sibling" -> Files.createDirectory(source.resolveSibling("${source.fileName}-shots"))
            "non-ascii" -> Files.createDirectory(root.resolve("静止画_日本語😀"))
            "external-alias" -> Files.createSymbolicLink(root.resolve("shots-link"), Files.createDirectory(root.resolve("actual-shots")))
            "source-parent-alias" -> {
                input = Files.createSymbolicLink(root.resolve("source-parent"), source.parent).resolve(source.fileName)
                Files.createDirectory(root.resolve("shots"))
            }
            else -> Files.createDirectory(root.resolve("shots"))
        }
        val before = tree(source)
        val file = ReplayFrameExport.export(input, 0, destination)
        assertEquals(destination.toRealPath(), file.parent)
        assertEquals(setOf(file.fileName.toString()), entries(file.parent))
        validateScreenshotPng(Files.readAllBytes(file))
        val image = javax.imageio.ImageIO.read(file.toFile())
        assertTrue(image.width > 0 && image.height > 0)
        unchanged(source, before)
    }

    @Test fun missingDestinationIsNotCreated() {
        val source = fixture()
        val before = tree(source)
        val missing = root.resolve("missing")
        val failure = assertFailsWith<ScreenshotSaveFailure> { ReplayFrameExport.export(source, 0, missing) }
        assertEquals(null, failure.retained)
        assertFalse(Files.exists(missing))
        unchanged(source, before)
    }

    @ParameterizedTest
    @ValueSource(booleans = [false, true])
    fun cancellationAfterOutputStartsWaitsForCompletionAndPreservesRealIoFailure(collision: Boolean) {
        val source = fixture()
        val before = tree(source)
        // Model a TempDir with a linked ancestor (e.g. /var -> /private/var) on every OS.
        val physicalParent = Files.createDirectory(root.resolve("physical-output"))
        val aliasParent = Files.createSymbolicLink(root.resolve("output-parent"), physicalParent)
        val destination = Files.createDirectory(aliasParent.resolve("shots")).toRealPath()
        val complete = destination.resolve(frameFileName(createdAt, id))
        val partial = destination.resolve(".${complete.fileName.toString().removeSuffix(".png")}.partial")
        val cancelled = AtomicBoolean()
        lateinit var expected: ByteArray
        val io = ReplayFrameExport.Io(
            encode = { frame, at, name, saveId -> ReplayFrameExport.pngBytes(frame.image, at, frame.row, name, saveId).also { expected = it } },
            createFile = { parent, name -> parent.createFile(name).also {
                if (collision) Files.writeString(complete, "existing output")
                cancelled.set(true)
            } },
        )
        fun save() = ReplayFrameExport.export(source, 0, destination, createdAt, id, io) {
            if (cancelled.get()) throw SaveCancelled(null)
        }
        if (collision) {
            val failure = assertFailsWith<ScreenshotSaveFailure> { save() }
            assertEquals(partial, failure.retained)
            assertEquals("existing output", Files.readString(complete))
            assertContentEquals(expected, Files.readAllBytes(partial))
            assertEquals("rw-------", TestPermissions.get(partial))
            assertEquals(setOf(partial.fileName.toString(), complete.fileName.toString()), entries(destination))
        } else {
            assertEquals(complete, save())
            assertContentEquals(expected, Files.readAllBytes(complete))
            assertEquals("rw-------", TestPermissions.get(complete))
            assertEquals(setOf(complete.fileName.toString()), entries(destination))
        }
        assertTrue(cancelled.get())
        unchanged(source, before)
    }

    @Test fun aChangedDestinationAliasCannotRedirectTheHeldOutputIntoTheSource() {
        val source = fixture()
        val before = tree(source)
        val actual = Files.createDirectory(root.resolve("actual-shots"))
        val alias = Files.createSymbolicLink(root.resolve("shots-alias"), actual)
        val file = ReplayFrameExport.export(source, 0, alias, io = ReplayFrameExport.Io(openDirectory = { path ->
            SaveOutputDirectory.open(path).also {
                Files.delete(alias)
                Files.createSymbolicLink(alias, source)
            }
        }))
        assertEquals(actual.toRealPath(), file.parent)
        assertEquals(setOf(file.fileName.toString()), entries(actual))
        validateScreenshotPng(Files.readAllBytes(file))
        unchanged(source, before)
    }
}
