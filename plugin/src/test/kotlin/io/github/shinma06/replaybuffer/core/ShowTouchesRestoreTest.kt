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
        assertEquals(emptyList(), run(showTouches = false, original = "0"))
    }

    private fun run(showTouches: Boolean, original: String): List<String> {
        val root = Files.createTempDirectory("replay-show-touches-").toRealPath()
        try {
            val adb = TestExecutables.python(root.resolve("adb"), """import pathlib,sys
a=sys.argv[1:]
if 'settings' in a:
    s=a[a.index('settings')+1:]
    if s[0]=='get': print('$original')
    with open(pathlib.Path(__file__).parent/'settings.log','a') as f: f.write(' '.join([s[0]]+s[3:])+'\n')
sys.exit(0)
""")
            CaptureResources(Files.createDirectory(root.resolve("resources"))).use { resources ->
                CaptureStore(root.resolve("ring")).use { store ->
                    val capture = DeviceCapture(adb, "fixture-1", resources, store, 1, ApplicationTarget(), showTouches = showTouches)
                    DeviceCapture::class.java.getDeclaredMethod("applyShowTouches").apply { isAccessible = true }.invoke(capture)
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
