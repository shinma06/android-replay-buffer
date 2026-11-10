package io.github.shinma06.replaybuffer.ide

import io.github.shinma06.replaybuffer.core.PrivateFiles
import io.github.shinma06.replaybuffer.testing.TestPermissions
import java.nio.file.Files
import java.nio.file.Path
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir

class ReplayCleanupDirectoryTest {
    @TempDir lateinit var directory: Path

    @Test
    fun `canonical project identity is stable private and separate from another project`() {
        val config = Files.createDirectory(directory.resolve("config"))
        val project = Files.createDirectory(directory.resolve("project"))
        val alias = Files.createSymbolicLink(directory.resolve("alias"), project)
        val other = Files.createDirectory(directory.resolve("other"))
        val stable = prepareCleanupDirectory(config, project.toString())
        assertTrue(stable.fileName.toString().matches(Regex("[0-9a-f]{64}")))
        assertEquals(stable, prepareCleanupDirectory(config, alias.toString()))
        assertEquals(stable, prepareCleanupDirectory(config, project.toString()))
        assertFalse(stable == prepareCleanupDirectory(config, other.toString()))
        for (path in listOf(stable, stable.parent, stable.parent.parent)) {
            assertEquals(TestPermissions.mode("rwx------"), TestPermissions.get(path))
        }
        assertEquals(config.toRealPath(), stable.parent.parent.parent)
    }

    @Test
    fun `imported directories are private again without touching other projects or capture contents`() {
        val config = Files.createDirectory(directory.resolve("config"))
        val project = Files.createDirectory(directory.resolve("project"))
        val stable = prepareCleanupDirectory(config, project.toString())
        val imported = TestPermissions.mode("rwxr-xr-x")
        val child = Files.createDirectory(stable.resolve("replay-retained"))
        val sibling = Files.createDirectory(stable.parent.resolve("other-project"))
        for (path in listOf(stable.parent.parent, stable.parent, stable, child, sibling))
            TestPermissions.set(path, imported)
        assertEquals(stable, prepareCleanupDirectory(config, project.toString()))
        for (path in listOf(stable.parent.parent, stable.parent, stable))
            assertEquals(TestPermissions.mode("rwx------"), TestPermissions.get(path))
        for (path in listOf(child, sibling)) assertEquals(imported, TestPermissions.get(path))
    }

    @Test
    fun `an owner mismatch never normalizes an existing directory`() {
        val existing = Files.createDirectory(directory.resolve("cleanup"))
        val mode = TestPermissions.mode("rwxr-xr-x")
        TestPermissions.set(existing, mode)
        val failure = assertThrows(CleanupDirectoryFailure::class.java) {
            preparePrivateDirectory(existing, Path.of("cleanup")) { "different-owner" }
        }
        assertTrue(failure.message!!.contains("所有者が異な"))
        assertTrue(failure.message!!.contains(PrivateFiles.describe(existing)))
        assertEquals(mode, TestPermissions.get(existing))
    }

    @Test
    fun `untrusted namespace entries and missing project are preserved and rejected`() {
        val project = Files.createDirectory(directory.resolve("project"))
        val outside = Files.createDirectory(directory.resolve("outside"))
        Files.writeString(outside.resolve("keep"), "preserve")
        for (kind in listOf("file", "symlink", "permissions", "ownerAccess")) {
            val config = Files.createDirectory(directory.resolve(kind))
            val namespace = config.resolve("android-replay-buffer")
            when (kind) {
                "file" -> Files.writeString(namespace, "preserve")
                "symlink" -> Files.createSymbolicLink(namespace, outside)
                else -> {
                    Files.createDirectory(namespace)
                    TestPermissions.set(namespace, TestPermissions.mode(if (kind == "permissions") "rwxrwxr-x" else "r-x------"))
                }
            }
            assertThrows(IllegalArgumentException::class.java) { prepareCleanupDirectory(config, project.toString()) }
            assertTrue(Files.exists(namespace))
            if (kind == "file") assertEquals("preserve", Files.readString(namespace))
            if (kind == "symlink") assertTrue(Files.isSymbolicLink(namespace))
            if (kind == "permissions") assertEquals(TestPermissions.mode("rwxrwxr-x"), TestPermissions.get(namespace))
            if (kind == "ownerAccess") assertEquals(TestPermissions.mode("r-x------"), TestPermissions.get(namespace))
        }
        assertEquals("preserve", Files.readString(outside.resolve("keep")))
        val config = Files.createDirectory(directory.resolve("empty-config"))
        assertThrows(IllegalArgumentException::class.java) { prepareCleanupDirectory(config, null) }
        assertThrows(IllegalArgumentException::class.java) { prepareCleanupDirectory(config, "relative") }
        assertFalse(Files.exists(config.resolve("android-replay-buffer")))
    }
}
