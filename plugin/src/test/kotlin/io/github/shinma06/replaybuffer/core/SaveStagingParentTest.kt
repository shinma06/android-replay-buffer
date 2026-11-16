package io.github.shinma06.replaybuffer.core

import org.junit.jupiter.api.io.TempDir
import java.nio.ByteBuffer
import java.nio.file.Files
import java.nio.file.Path
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFails
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class SaveStagingParentTest {
    @TempDir lateinit var root: Path

    @Test fun aDifferentHeldParentIsRejectedBeforeCreatingAnything() {
        val first = Files.createDirectory(root.resolve("first"))
        val other = Files.createDirectory(root.resolve("other"))
        SaveOutputDirectory.open(first).use { held ->
            assertFails { SaveStaging.create(other, "output", held).close() }
            assertEquals(emptySet(), held.entries())
            assertEquals(emptyList(), Files.list(other).use { it.toList() })
        }
    }

    @Test fun borrowingKeepsTheParentAliveThroughPublicationAndFailure() {
        SaveOutputDirectory.open(root.toRealPath()).use { parent ->
            SaveStaging.create(root.toRealPath(), "complete", parent).use { staging ->
                staging.createFile(staging.file("proof")).use { it.write(ByteBuffer.wrap("held".toByteArray())) }
                staging.closeForRename()
                staging.publication().publish()
                assertTrue(staging.ownsDirectory(staging.complete))
            }
            assertEquals(setOf("complete"), parent.entries())
            SaveStaging.create(root.toRealPath(), "failed", parent).use { staging ->
                staging.createFile(staging.file("proof")).use { it.write(ByteBuffer.wrap("partial".toByteArray())) }
                assertTrue(staging.ownsDirectory(staging.partial))
            }
            assertEquals(setOf("complete", ".failed.partial"), parent.entries())
            assertFalse(Files.exists(root.resolve("failed")))
            assertFails { SaveStaging.create(root.toRealPath(), "failed", parent).close() }
            assertFails { SaveStaging.create(root.toRealPath(), "complete", parent).use { it.publication().publish() } }
            // Even a create/rename failure must not close the caller-owned descriptor.
            assertTrue("complete" in parent.entries())
        }
    }
}
