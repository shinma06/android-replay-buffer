package io.github.shinma06.replaybuffer.core

import org.junit.jupiter.api.Assumptions.assumeTrue
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.attribute.BasicFileAttributes
import java.util.concurrent.CountDownLatch
import java.util.concurrent.atomic.AtomicInteger
import kotlin.concurrent.thread
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse

class SavePublicationTest {
    @Test
    fun heldDirectoryAcquisitionAndPublicationUseExactEntries() {
        val root = Files.createTempDirectory("replay-held-publish-").toRealPath()
        try { SaveOutputDirectory.open(root).use { parent ->
            val partial = root.resolve(".partial")
            parent.createDirectory(partial.fileName.toString()).use { staging ->
                staging.requireEntries(emptyMap())
                assertEquals(emptySet(), staging.entries())
                for (invalid in listOf("", ".", "..", "../outside", "/outside", "bad\u0000name"))
                    assertFailsWith<IllegalArgumentException> { staging.createFile(invalid) }
                staging.createFile("proof").use { output ->
                    output.channel().use { it.write(java.nio.ByteBuffer.wrap("proof".toByteArray())) }
                    assertEquals(setOf("proof"), staging.entries())
                    staging.requireEntries(mapOf("proof" to output))
                    assertFailsWith<IllegalStateException> { staging.requireEntries(emptyMap()) }
                    assertFailsWith<java.nio.file.FileAlreadyExistsException> { staging.createFile("proof") }
                    staging.createFile("other").use { other ->
                        assertFailsWith<IllegalStateException> { staging.requireEntries(mapOf("proof" to output)) }
                        val outputs = mapOf("proof" to output, "other" to other)
                        staging.requireEntries(outputs)
                        assertEquals(setOf("proof", "other"), staging.entries())
                        if (System.getProperty("os.name") == "Mac OS X") {
                            val complete = root.resolve("completed")
                            SavePublication(partial, complete, parent, staging) { staging.requireEntries(outputs) }.publish()
                            assertEquals("proof", Files.readString(complete.resolve("proof")))
                            assertFalse(Files.exists(partial))
                            assertEquals(staging.key, parent.openChild("completed").use { it.key })
                        }
                    }
                }
            }
        } } finally { Files.walk(root).use { it.sorted(java.util.Comparator.reverseOrder()).forEach(Files::deleteIfExists) } }
    }

    @Test
    fun macNativePublicationRejectsEveryExistingTargetAndConcurrentPublisher() {
        assumeTrue(System.getProperty("os.name") == "Mac OS X", "macOS native syscall is not executed on Linux CI")
        val root = Files.createTempDirectory("replay-publish-fixture-").toRealPath()
        try {
            val partial = Files.createDirectory(root.resolve(".partial"))
            Files.writeString(partial.resolve("proof"), "complete evidence")
            for (kind in listOf("empty", "nonempty", "file", "symlink")) {
                val target = root.resolve(kind)
                when (kind) {
                    "empty" -> Files.createDirectory(target)
                    "nonempty" -> { Files.createDirectory(target); Files.writeString(target.resolve("keep"), "keep") }
                    "file" -> Files.writeString(target, "keep")
                    else -> Files.createSymbolicLink(target, Path.of("does-not-exist"))
                }
                val key = Files.readAttributes(target, BasicFileAttributes::class.java, java.nio.file.LinkOption.NOFOLLOW_LINKS).fileKey()
                assertFailsWith<IllegalStateException> { publishCapture(partial, target) }
                assertEquals(key, Files.readAttributes(target, BasicFileAttributes::class.java, java.nio.file.LinkOption.NOFOLLOW_LINKS).fileKey())
                assertEquals("complete evidence", Files.readString(partial.resolve("proof")))
                if (kind == "nonempty") assertEquals("keep", Files.readString(target.resolve("keep")))
                if (kind == "file") assertEquals("keep", Files.readString(target))
                if (kind == "symlink") assertEquals(Path.of("does-not-exist"), Files.readSymbolicLink(target))
            }
            val other = Files.createDirectory(root.resolve(".other"))
            Files.writeString(other.resolve("proof"), "other evidence")
            val start = CountDownLatch(1)
            val successes = AtomicInteger()
            val target = root.resolve("completed")
            val publishers = listOf(partial, other).map { source -> thread {
                start.await()
                runCatching { publishCapture(source, target) }.onSuccess { successes.incrementAndGet() }
            } }
            start.countDown(); publishers.forEach { it.join(2000) }
            assertFalse(publishers.any { it.isAlive })
            assertEquals(1, successes.get())
            assertEquals(1, listOf(partial, other).count { Files.exists(it) })
            assertFalse(Files.readString(target.resolve("proof")).isEmpty())
        } finally { Files.walk(root).use { it.sorted(java.util.Comparator.reverseOrder()).forEach(Files::deleteIfExists) } }
    }
    private fun publishCapture(partial: Path, complete: Path) {
        SaveOutputDirectory.open(partial.parent).use { parent ->
            parent.openChild(partial.fileName.toString()).use { staging ->
                SavePublication(partial, complete, parent, staging) {}.publish()
            }
        }
    }

}
