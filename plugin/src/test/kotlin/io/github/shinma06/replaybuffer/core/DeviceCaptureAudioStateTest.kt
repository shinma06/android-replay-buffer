package io.github.shinma06.replaybuffer.core

import java.nio.file.Files
import java.nio.file.Path
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import kotlin.test.assertEquals

class DeviceCaptureAudioStateTest {
    @TempDir lateinit var root: Path

    /** A video worker may run before start() returns, so audio-off must already hold at construction. */
    @Test
    fun audioOffIsDecidedBeforeAnyWorkerStarts() {
        CaptureResources(Files.createDirectory(root.resolve("resources"))).use { resources ->
            for ((mode, record, expected) in listOf(Triple(CaptureMode.VIDEO_AND_LOGS, false, ""),
                Triple(CaptureMode.LOG_ONLY, true, ""), Triple(CaptureMode.VIDEO_AND_LOGS, true, null))) {
                CaptureStore(root.resolve("ring-$mode-$record"), minFree = 0, captureMode = mode, audioRequested = record).use { store ->
                    val capture = DeviceCapture(root.resolve("adb"), "fixture-1", resources, store, 1, ApplicationTarget(),
                        recordAudio = record)
                    val field = DeviceCapture::class.java.getDeclaredField("audioStopped").apply { isAccessible = true }
                    assertEquals(expected, field.get(capture), "$mode/$record")
                }
            }
        }
    }
}
