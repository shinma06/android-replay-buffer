package io.github.shinma06.replaybuffer.core

import io.github.shinma06.replaybuffer.testing.TestExecutables
import java.lang.reflect.InvocationTargetException
import java.nio.file.Files
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.concurrent.thread
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class InjectedTouchRecoveryTest {
    @Test
    fun `an unread Android version returns to the recovery loop instead of waiting until capture is turned off`() {
        val error = assertFailsWith<InvocationTargetException> { run(sdk = null) }
        assertEquals("Androidのversionを確認できません", error.targetException.message)
    }

    @Test
    fun `a confirmed old Android version waits without retrying and never changes the log property`() {
        // Close's own cleanup (ps, rm) is not part of the reader.
        assertEquals(listOf("getprop ro.build.version.sdk"), run(sdk = "28").filter { it.contains("prop") })
    }

    /** Runs the injected-tap reader once, then turns capture off, and returns the device shell commands it ran. */
    private fun run(sdk: String?): List<String> {
        val root = Files.createTempDirectory("replay-injected-touch-").toRealPath()
        try {
            val adb = TestExecutables.python(root.resolve("adb"), """import pathlib,sys
a=sys.argv[1:]
if 'shell' in a:
    s=a[a.index('shell')+1:]
    with open(pathlib.Path(__file__).parent/'shell.log','a') as f: f.write(' '.join(s)+'\n')
    if s[:2]==['getprop','ro.build.version.sdk']:
        ${if (sdk == null) "sys.exit(1)" else "print('$sdk')"}
sys.exit(0)
""")
            CaptureResources(Files.createDirectory(root.resolve("resources"))).use { resources ->
                CaptureStore(root.resolve("ring")).use { store ->
                    val capture = DeviceCapture(adb, "fixture-1", resources, store, 1, ApplicationTarget())
                    val reader = DeviceCapture::class.java.getDeclaredMethod("injectedTouches").apply { isAccessible = true }
                    var failure: Throwable? = null
                    val worker = thread { try { reader.invoke(capture) } catch (e: Throwable) { failure = e } }
                    // An unsupported device keeps waiting until capture is turned off; an unread version must return by itself.
                    worker.join(if (sdk == null) 10_000 else 1_500)
                    if (sdk == null) assertFalse(worker.isAlive, "an unread version must return to the recovery loop")
                    else assertTrue(worker.isAlive, "a confirmed old version waits until capture is turned off")
                    runCatching { capture.close() }
                    worker.join(10_000)
                    assertFalse(worker.isAlive)
                    failure?.let { throw it }
                }
            }
            val log = root.resolve("shell.log")
            return if (Files.exists(log)) Files.readAllLines(log).map { it.trim() } else emptyList()
        } finally {
            Files.walk(root).use { it.sorted(Comparator.reverseOrder()).forEach(Files::deleteIfExists) }
        }
    }
}
