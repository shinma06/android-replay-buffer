package io.github.shinma06.replaybuffer.core

import io.github.shinma06.replaybuffer.testing.TestExecutables
import java.nio.file.Files
import kotlin.test.Test
import kotlin.test.assertEquals

class ShowTouchesRestoreTest {
    @Test
    fun `show_touches is turned on for the capture and restored to the previous value on close`() {
        assertEquals(listOf("get", "put 1", "put 0"), run(showTouches = true, original = "0"))
        assertEquals(listOf("get", "put 1", "delete"), run(showTouches = true, original = "null"))
        assertEquals(listOf("get"), run(showTouches = true, original = "1"))
        assertEquals(listOf("get"), run(showTouches = false, original = "0"))
        assertEquals(TouchDots.OFF, dots)
    }

    @Test
    fun `with the setting off the device value is read but never changed`() {
        // Dots turned on in the device's developer options are in the video even though the plugin did not set them.
        assertEquals(listOf("get"), run(showTouches = false, original = "1"))
        assertEquals(TouchDots.ALREADY_ON, dots)
    }

    @Test
    fun `a restore missed while disconnected is retried by the next capture instead of reading our own value`() {
        val restore = ShowTouchesRestore()
        assertEquals(listOf("get", "put 1"), run(showTouches = true, original = "0", restore = restore, offlineOnClose = true))
        // The device now reports the plugin's "1"; the remembered original must still win.
        assertEquals(listOf("put 1", "put 0"), run(showTouches = true, original = "1", restore = restore))
        assertEquals(listOf("get"), run(showTouches = true, original = "1", restore = restore))
    }

    private var dots: TouchDots? = null

    private fun run(showTouches: Boolean, original: String, restore: ShowTouchesRestore = ShowTouchesRestore(),
                    offlineOnClose: Boolean = false): List<String> {
        val root = Files.createTempDirectory("replay-show-touches-").toRealPath()
        try {
            val adb = TestExecutables.python(root.resolve("adb"), """import pathlib,sys
a=sys.argv[1:]
if (pathlib.Path(__file__).parent/'offline').exists(): sys.exit(1)
if 'settings' in a:
    s=a[a.index('settings')+1:]
    if s[0]=='get': print('$original')
    with open(pathlib.Path(__file__).parent/'settings.log','a') as f: f.write(' '.join([s[0]]+s[3:])+'\n')
sys.exit(0)
""")
            CaptureResources(Files.createDirectory(root.resolve("resources"))).use { resources ->
                CaptureStore(root.resolve("ring")).use { store ->
                    val capture = DeviceCapture(adb, "fixture-1", resources, store, 1, ApplicationTarget(), showTouches = showTouches, touchRestore = restore)
                    dots = DeviceCapture::class.java.getDeclaredMethod("applyShowTouches").apply { isAccessible = true }.invoke(capture) as TouchDots?
                    if (offlineOnClose) Files.createFile(root.resolve("offline"))
                    runCatching { capture.close() }
                }
            }
            val log = root.resolve("settings.log")
            return if (Files.exists(log)) Files.readAllLines(log).map { it.trim() } else emptyList()
        } finally {
            Files.walk(root).use { it.sorted(Comparator.reverseOrder()).forEach(Files::deleteIfExists) }
        }
    }
}
