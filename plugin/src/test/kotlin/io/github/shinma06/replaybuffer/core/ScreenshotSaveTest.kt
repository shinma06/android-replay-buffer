package io.github.shinma06.replaybuffer.core

import io.github.shinma06.replaybuffer.testing.TestPermissions
import io.github.shinma06.replaybuffer.testing.TestExecutables
import java.awt.image.BufferedImage
import java.io.ByteArrayOutputStream
import java.nio.file.Files
import java.nio.file.Path
import javax.imageio.ImageIO
import kotlin.concurrent.thread
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFails
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class ScreenshotSaveTest {
    @Test
    fun physicalDisplayMustBeOneKnownScreen() {
        val display = "Display 4619827551948147201 (HWC display 0): port=0\n"
        assertEquals("4619827551948147201", screenshotDisplay(display))
        assertFails { screenshotDisplay(display + display.replace("7201", "7202")) }
        assertFails { screenshotDisplay("unknown vendor format") }
        assertFails { screenshotDisplay(display.replace("4619827551948147201", "18446744073709551616")) }
    }

    @Test
    fun currentScreenIsSavedWhateverAppOrLockScreenIsInFront() {
        fixture { root, adb ->
            val destination = Files.createDirectory(root.resolve("output"))
            val saved = saveScreenshot(adb, destination, "usb123")
            assertContentEquals(Files.readAllBytes(root.resolve("image.png")), Files.readAllBytes(saved.file))
            assertEquals("usb123", saved.serial)
            assertEquals(listOf("devices -l", "-s usb123 shell dumpsys SurfaceFlinger --display-id",
                "-s usb123 exec-out screencap -p -d 4619827551948147201"), Files.readAllLines(root.resolve("commands")))
        }
    }

    @Test
    fun pngMustBeCompleteAndUnmodifiedIncludingEveryChunk() {
        val bytes = png()
        validateScreenshotPng(bytes)
        assertFails { validateScreenshotPng(bytes.copyOf(bytes.size - 1)) }
        assertFails { validateScreenshotPng(bytes + byteArrayOf(0)) }
        assertFails { validateScreenshotPng(bytes.copyOfRange(0, 33) + bytes.copyOfRange(8, bytes.size)) }
        assertFails { validateScreenshotPng(bytes.copyOf().also { it[it.size / 2] = (it[it.size / 2].toInt() xor 1).toByte() }) }
        assertFails { validateScreenshotPng("not a png".toByteArray()) }
        val oversized = bytes.copyOf()
        java.nio.ByteBuffer.wrap(oversized, 16, 4).putInt(16385)
        val crc = java.util.zip.CRC32().apply { update(oversized, 12, 17) }
        java.nio.ByteBuffer.wrap(oversized, 29, 4).putInt(crc.value.toInt())
        assertFails { validateScreenshotPng(oversized) }
        // A blank image is valid; pixels cannot identify FLAG_SECURE or prove app content.
        val blank = ByteArrayOutputStream().also { ImageIO.write(BufferedImage(2, 2, BufferedImage.TYPE_INT_RGB), "png", it) }.toByteArray()
        validateScreenshotPng(blank)
    }

    @Test
    fun missingDestinationIsNotCreated() {
        fixture { root, adb ->
            val missing = root.resolve("missing")
            assertFails { saveScreenshot(adb, missing) }
            assertFalse(Files.exists(missing))
        }
    }

    @Test
    fun configuredLinkedDestinationResolvesButLaterLinkChangesCannotRedirectDeletion() {
        fixture { root, adb ->
            val other = Files.createDirectory(root.resolve("other"))
            val link = Files.createSymbolicLink(root.resolve("linked-output"), other)
            val saved = saveScreenshot(adb, link)
            assertEquals(other, saved.file.parent)
            assertContentEquals(Files.readAllBytes(root.resolve("image.png")), Files.readAllBytes(saved.file))
            val replacement = Files.createDirectory(root.resolve("replacement"))
            Files.writeString(replacement.resolve("keep"), "unrelated")
            Files.delete(link)
            Files.createSymbolicLink(link, replacement)
            saved.delete()
            assertFalse(Files.exists(saved.file))
            assertTrue(Files.isDirectory(other))
            assertTrue(Files.isSymbolicLink(link))
            assertEquals("unrelated", Files.readString(replacement.resolve("keep")))
        }
    }

    @Test
    fun singleDeviceStreamsBinaryWithoutRecordingAndPublishesExactImageThenDeletesOnlyThatImage() {
        fixture { root, adb ->
            val destination = Files.createDirectory(root.resolve("output"))
            Files.writeString(destination.resolve("keep"), "unrelated")
            val saved = saveScreenshot(adb, destination)
            assertContentEquals(Files.readAllBytes(root.resolve("image.png")), Files.readAllBytes(saved.file))
            assertEquals(destination, saved.file.parent)
            assertEquals("usb123", saved.serial)
            assertTrue(Regex("screenshot-[0-9]{8}-[0-9]{6}-[0-9]{3}-[0-9a-f-]{36}\\.png").matches(saved.file.fileName.toString()))
            assertEquals(setOf("keep", saved.file.fileName.toString()), Files.list(destination).use { it.map { path -> path.fileName.toString() }.toList().toSet() })
            assertEquals(TestPermissions.mode("rw-------"), TestPermissions.get(saved.file))
            assertEquals(listOf("devices -l", "-s usb123 shell dumpsys SurfaceFlinger --display-id",
                "-s usb123 exec-out screencap -p -d 4619827551948147201"),
                Files.readAllLines(root.resolve("commands")))
            val second = saveScreenshot(adb, destination)
            assertTrue(saved.file != second.file)
            saved.delete()
            saved.delete()
            assertFalse(Files.exists(saved.file))
            assertTrue(Files.isDirectory(saved.file.parent))
            assertTrue(Files.exists(second.file))
            assertEquals("unrelated", Files.readString(destination.resolve("keep")))
        }
    }

    @Test
    fun concurrentDirectImagesAreUniqueAndExclusiveFileCollisionPreservesBothInputs() {
        fixture { root, adb ->
            val destination = Files.createDirectory(root.resolve("output"))
            java.util.concurrent.Executors.newFixedThreadPool(2).use { executor ->
                val start = java.util.concurrent.CyclicBarrier(2)
                val requests = List(2) { executor.submit<SavedScreenshot> {
                    start.await()
                    saveScreenshot(adb, destination)
                } }
                val saved = requests.map { it.get(10, java.util.concurrent.TimeUnit.SECONDS) }
                assertEquals(2, saved.map { it.file }.toSet().size)
                assertEquals(saved.map { it.file }.toSet(), Files.list(destination).use { it.toList().toSet() })
                saved.forEach { assertEquals(destination, it.file.parent); validateScreenshotPng(Files.readAllBytes(it.file)) }
            }
            SaveOutputDirectory.open(destination).use { parent ->
                parent.createFile(".collision.partial").use { partial ->
                    partial.channel().use { it.write(java.nio.ByteBuffer.wrap("preserved".toByteArray())); it.force(true) }
                    Files.writeString(destination.resolve("existing.png"), "unrelated")
                    assertFails { parent.moveFile(".collision.partial", parent, "existing.png") }
                    parent.verifyFile(".collision.partial", partial)
                    assertEquals("preserved", Files.readString(destination.resolve(".collision.partial")))
                    assertEquals("unrelated", Files.readString(destination.resolve("existing.png")))
                }
            }
        }
    }

    @Test
    fun recordingTargetWinsOverOtherDeviceAndNeverFallsBackAfterDisconnect() {
        fixture { root, adb ->
            val destination = Files.createDirectory(root.resolve("output"))
            val target = "emulator-5554 device model:Recording"
            val other = "usb123 device model:Other"
            Files.writeString(root.resolve("devices"), "List of devices attached\n$target\n$other\n")
            val saved = saveScreenshot(adb, destination, "emulator-5554")
            assertContentEquals(Files.readAllBytes(root.resolve("image.png")), Files.readAllBytes(saved.file))
            assertEquals(listOf("devices -l", "-s emulator-5554 shell dumpsys SurfaceFlinger --display-id",
                "-s emulator-5554 exec-out screencap -p -d 4619827551948147201"), Files.readAllLines(root.resolve("commands")))
            for (missing in listOf("", "emulator-5554 offline\n", "emulator-5554 unauthorized\n")) {
                Files.writeString(root.resolve("commands"), "")
                Files.writeString(root.resolve("devices"), "List of devices attached\n$missing$other\n")
                val failure = kotlin.test.assertFailsWith<ScreenshotSaveFailure> {
                    saveScreenshot(adb, destination, "emulator-5554")
                }
                assertTrue(failure.message!!.contains("録画対象"))
                assertEquals(listOf("devices -l"), Files.readAllLines(root.resolve("commands")))
                assertEquals(1, Files.list(destination).use { it.count() })
            }
        }
    }

    @Test
    fun withoutRecordingHistoryUsesSameUniqueDeviceRuleAndRejectsAmbiguity() {
        fixture { root, adb ->
            val destination = Files.createDirectory(root.resolve("output"))
            Files.writeString(root.resolve("devices"), "List of devices attached\nemulator-5554 device model:Only\n")
            val saved = saveScreenshot(adb, destination)
            assertTrue(Files.exists(saved.file))
            assertTrue(Files.readAllLines(root.resolve("commands")).drop(1).all { it.startsWith("-s emulator-5554 ") })
            for (devices in listOf("", "usb123 offline\n", "usb123 unauthorized\n",
                "emulator-5554 device\nusb123 device\n")) {
                Files.writeString(root.resolve("commands"), "")
                Files.writeString(root.resolve("devices"), "List of devices attached\n$devices")
                assertFails { saveScreenshot(adb, destination) }
                assertEquals(listOf("devices -l"), Files.readAllLines(root.resolve("commands")))
                assertEquals(1, Files.list(destination).use { it.count() })
            }
        }
    }

    @Test
    fun deletedReplacedChangedAndSymlinkedFilesCannotDeleteOtherData() {
        fixture { root, adb ->
            val destination = Files.createDirectory(root.resolve("output"))
            for (kind in listOf("removed", "replaced", "changed", "symlink", "directory")) {
                val saved = saveScreenshot(adb, destination)
                val keep = root.resolve("keep-$kind")
                Files.writeString(keep, "untouched")
                when (kind) {
                    "changed" -> Files.writeString(saved.file, "edited")
                    "directory" -> {
                        Files.move(saved.file.parent, root.resolve("old-directory"))
                        Files.createDirectory(saved.file.parent)
                        Files.writeString(saved.file, "replacement")
                    }
                    else -> {
                        Files.delete(saved.file)
                        if (kind == "replaced") Files.writeString(saved.file, "replacement")
                        if (kind == "symlink") Files.createSymbolicLink(saved.file, keep)
                    }
                }
                assertFails { saved.delete() }
                assertEquals("untouched", Files.readString(keep))
                if (kind in listOf("changed", "replaced", "directory")) assertTrue(Files.exists(saved.file))
                if (kind == "symlink") assertTrue(Files.isSymbolicLink(saved.file))
            }
        }
    }

    @Test
    fun disconnectedAmbiguousOrInvalidOutputNeverPublishesAnImage() {
        fixture { root, adb ->
            val destination = Files.createDirectory(root.resolve("output"))
            for (mode in listOf("disconnected", "ambiguous-display", "broken")) {
                Files.writeString(root.resolve("mode"), mode)
                assertFails { saveScreenshot(adb, destination) }
                assertEquals(0, Files.list(destination).use { it.count() })
            }
        }
    }

    @Test
    fun interruptStopsOnlyItsBinaryClientAndLeavesNoOutput() {
        fixture { root, adb ->
            val destination = Files.createDirectory(root.resolve("output"))
            Files.writeString(root.resolve("mode"), "wait")
            val failure = java.util.concurrent.atomic.AtomicReference<Throwable?>()
            val worker = thread {
                try { saveScreenshot(adb, destination) }
                catch (caught: Throwable) { failure.set(caught) }
            }
            try {
                val limit = System.nanoTime() + 5_000_000_000
                while (!Files.exists(root.resolve("pid")) && System.nanoTime() < limit) Thread.sleep(10)
                assertTrue(Files.exists(root.resolve("pid")))
                val pid = Files.readString(root.resolve("pid")).toLong()
                worker.interrupt()
                worker.join(5000)
                assertFalse(worker.isAlive)
                assertTrue(failure.get() != null)
                assertFalse(ProcessHandle.of(pid).map { it.isAlive }.orElse(false))
                assertEquals(0, Files.list(destination).use { it.count() })
            } finally { worker.interrupt(); worker.join(5000) }
        }
    }

    private fun png(): ByteArray {
        val image = BufferedImage(11, 7, BufferedImage.TYPE_INT_RGB)
        for (y in 0 until image.height) for (x in 0 until image.width) image.setRGB(x, y, (x * 123456 + y * 4567) and 0xffffff)
        return ByteArrayOutputStream().also { ImageIO.write(image, "png", it) }.toByteArray()
    }

    private fun fixture(body: (Path, Path) -> Unit) {
        val root = Files.createTempDirectory("screenshot-fixture-").toRealPath()
        try {
            Files.write(root.resolve("image.png"), png())
            Files.writeString(root.resolve("devices"), "List of devices attached\nusb123 device model:Fixture\n")
            var adb = root.resolve("adb")
            adb = TestExecutables.python(adb, """#!/usr/bin/env python3
import os,pathlib,sys,time
root=pathlib.Path(__file__).parent
mode=(root/'mode').read_text() if (root/'mode').exists() else ''
command=' '.join(sys.argv[1:])
with (root/'commands').open('a') as log: log.write(command+'\n')
if command=='devices -l':
 if mode=='disconnected': sys.exit(1)
 (root/'phase').write_text('before')
 print((root/'devices').read_text(),end='')
elif sys.argv[1]=='-s' and sys.argv[2] in ('usb123','emulator-5554'):
 command=' '.join(sys.argv[3:])
else: sys.exit(2)
if command=='shell dumpsys SurfaceFlinger --display-id':
 print('Display 4619827551948147201 (HWC display 0): port=0')
 if mode=='ambiguous-display': print('Display 4619827551948147202 (HWC display 1): port=1')
elif command=='exec-out screencap -p -d 4619827551948147201':
 (root/'phase').write_text('after')
 if mode=='wait':
  (root/'pid').write_text(str(os.getpid()))
  time.sleep(30)
 sys.stdout.buffer.write(b'broken' if mode=='broken' else (root/'image.png').read_bytes())
elif command!='devices -l': sys.exit(2)
""")
            body(root, adb)
        } finally { root.toFile().deleteRecursively() }
    }
}
