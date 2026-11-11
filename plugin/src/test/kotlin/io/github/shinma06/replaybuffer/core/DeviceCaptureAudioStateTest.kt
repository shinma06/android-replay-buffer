package io.github.shinma06.replaybuffer.core

import io.github.shinma06.replaybuffer.testing.TestExecutables
import java.nio.file.Files
import java.nio.file.Path
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import kotlin.test.assertEquals
import kotlin.test.assertFalse

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

    /** A failed audio JAR push is audio's own failure: audio becomes unavailable and video keeps the official server. */
    @Test
    fun audioPreparationFailureLeavesVideoOnTheOfficialServer() {
        val adb = TestExecutables.python(root.resolve("adb"), """import sys
a=sys.argv[1:]
if 'push' in a: sys.exit(1)
if 'getprop' in a: print('36')
sys.exit(0)
""")
        CaptureResources(Files.createDirectory(root.resolve("resources"))).use { resources ->
            CaptureStore(root.resolve("ring"), minFree = 0, audioRequested = true).use { store ->
                store.generation(1)
                val capture = DeviceCapture(adb, "fixture-1", resources, store, 1, ApplicationTarget(), recordAudio = true)
                val prepared = DeviceCapture::class.java.getDeclaredMethod("prepareAudio").apply { isAccessible = true }
                assertFalse(prepared.invoke(capture) as Boolean)
                val reason = "音声用のserverを端末へ準備できませんでした（動画のみ記録しています）"
                assertEquals(reason, DeviceCapture::class.java.getDeclaredField("audioStopped").apply { isAccessible = true }.get(capture))
                val state = CaptureStore::class.java.getDeclaredField("audioState").apply { isAccessible = true }.get(store) as StreamSnapshot
                assertEquals(StreamState.UNAVAILABLE, state.state)
                assertEquals(reason, state.reason)
                // Later video retries stay on the official server without trying audio again.
                assertFalse(prepared.invoke(capture) as Boolean)
            }
        }
    }
}
