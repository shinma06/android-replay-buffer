package io.github.shinma06.replaybuffer.core

import java.nio.file.Files
import java.util.jar.JarFile
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class CaptureResourcesTest {
    @Test
    fun packagedHelpersMatchIdentityAndClockJarContainsOnlyDex() {
        val root = Files.createTempDirectory("replay-resources-fixture-")
        try {
            CaptureResources(root).use { resources ->
                assertEquals(CaptureResources.SERVER_HASH, sha256(resources.server))
                assertEquals(CaptureResources.identity().getValue("clock"), sha256(resources.clock))
                JarFile(resources.clock.toFile()).use { jar ->
                    val dex = jar.getInputStream(jar.getJarEntry("classes.dex")).readAllBytes()
                    assertTrue(dex.copyOfRange(0, 4).contentEquals(byteArrayOf(100, 101, 120, 10)))
                    assertTrue(dex.toString(Charsets.ISO_8859_1).contains("ClockProbe"))
                }
            }
            assertEquals(0, Files.list(root).use { it.count() })
        } finally { Files.walk(root).use { p -> p.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } } }
    }
}
