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
            assertEquals(1, successes.get())
            assertEquals(1, listOf(partial, other).count { Files.exists(it) })
            assertFalse(Files.readString(target.resolve("proof")).isEmpty())
        } finally { Files.walk(root).use { it.sorted(java.util.Comparator.reverseOrder()).forEach(Files::deleteIfExists) } }
    }
}
