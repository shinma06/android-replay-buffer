package io.github.shinma06.replaybuffer.core

import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.TimeUnit
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue
import org.junit.jupiter.api.io.TempDir

class DeviceCaptureCommandTest {
    @TempDir
    lateinit var directory: Path

    @Test
    fun videoLaunchAlignsEncoderFrameRateWithCaptureLimit() {
        val fake = directory.resolve("adb")
        val ready = directory.resolve("ready")
        Files.writeString(fake, """#!/usr/bin/python3
import os, pathlib, sys, time
root = pathlib.Path(__file__).parent
args = sys.argv[1:]
if len(args) > 2 and args[2] == 'forward' and '--remove' not in args:
    print(1)
elif len(args) > 3 and args[2] == 'shell' and 'com.genymobile.scrcpy.Server' in args[3]:
    (root / 'command').write_text(args[3])
    (root / 'pid').write_text(str(os.getpid()))
    (root / 'ready').touch()
    time.sleep(20)
""")
        assertTrue(fake.toFile().setExecutable(true))
        val resources = CaptureResources(Files.createDirectory(directory.resolve("resources")))
        val store = CaptureStore(directory.resolve("ring"))
        val capture = DeviceCapture(fake, "fixture-1", resources, store, 1, ApplicationTarget())
        try {
            capture.start() // Only this executable receives the real launch arguments; no SDK/IDE/device.
            val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
            while (!Files.exists(ready) && System.nanoTime() < deadline) Thread.sleep(25)
            assertTrue(Files.exists(ready), "video command was not launched")
            val tokens = Files.readString(directory.resolve("command")).split(' ')
            assertEquals("max_fps=30", tokens.single { it.startsWith("max_fps=") })
            assertEquals("video_codec=h264", tokens.single { it.startsWith("video_codec=") })
            assertEquals("video_bit_rate=8000000", tokens.single { it.startsWith("video_bit_rate=") })
            val options = tokens.single { it.startsWith("video_codec_options=") }
                .substringAfter('=').split(',')
            assertEquals(
                setOf("max-bframes:int=0", "i-frame-interval:int=1", "frame-rate:int=30"),
                options.toSet(),
            )
            assertEquals(3, options.size)
        } finally {
            try {
                capture.close()
                if (Files.exists(ready)) {
                    val pid = Files.readString(directory.resolve("pid")).toLong()
                    assertFalse(ProcessHandle.of(pid).map { it.isAlive }.orElse(false))
                }
            } finally {
                store.close()
                resources.close()
            }
        }
    }
}
