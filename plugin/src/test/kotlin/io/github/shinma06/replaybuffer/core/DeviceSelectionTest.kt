package io.github.shinma06.replaybuffer.core

import io.github.shinma06.replaybuffer.testing.TestExecutables
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.TimeUnit
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue

class DeviceSelectionTest {
    private fun fixture(root: Path): Path = TestExecutables.python(root.resolve("adb"), """#!/usr/bin/python3
import sys,time,pathlib
root=pathlib.Path(__file__).parent
a=sys.argv[1:]
if a==['devices','-l']:
    print((root/'devices.txt').read_text(),end='')
elif len(a)>1 and a[0]=='-s':
    with (root/'serials.txt').open('a') as f: f.write(a[1]+'\n')
    if 'logcat' in a: time.sleep(30)
""")

    private fun devices(root: Path, vararg serials: String) = Files.writeString(root.resolve("devices.txt"),
        "List of devices attached\n" + serials.joinToString("") { "$it device model:M_$it\n" })

    private fun used(root: Path): Set<String> = root.resolve("serials.txt").let { if (Files.exists(it)) Files.readAllLines(it).toSet() else emptySet() }

    private fun ReplayCore.await(phase: String, predicate: (ReplaySnapshot) -> Boolean): ReplaySnapshot {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(15)
        while (System.nanoTime() < deadline) { snapshot().let { if (predicate(it)) return it }; Thread.sleep(25) }
        val state = snapshot()
        error("deadline ($phase): ${state.captureState}/device=${state.device}/devices=${state.devices}/error=${state.error}")
    }

    @Test
    fun chosenSerialIsAuthoritativeAndNeverReplacedSilently() {
        val root = Files.createTempDirectory("replay-device-select-")
        devices(root, "fixture-1", "fixture-2")
        val core = ReplayCore(ReplaySettings(fixture(root), captureMode = CaptureMode.LOG_ONLY, deviceSerial = "fixture-2"), root.resolve("workspace"))
        try {
            assertTrue(core.setEnabled(true).get(10, TimeUnit.SECONDS).accepted)
            val chosen = core.await("chosen") { it.device?.serial == "fixture-2" }
            assertFalse(chosen.captureState == CaptureState.MULTIPLE_DEVICES)
            assertEquals(listOf("fixture-1", "fixture-2"), chosen.devices.map { it.serial })
            core.await("chosen-used") { used(root).isNotEmpty() }
            devices(root, "fixture-1")
            val waiting = core.await("chosen-disconnected") { it.device?.connected == false }
            assertEquals("fixture-2", waiting.device?.serial)
            Thread.sleep(300)
            assertEquals("fixture-2", core.snapshot().device?.serial)
            assertEquals(setOf("fixture-2"), used(root))
            devices(root, "fixture-1", "fixture-2")
            core.await("chosen-reconnected") { it.device?.serial == "fixture-2" && it.device?.connected == true }
            val before = core.snapshot().generation
            assertTrue(core.applySettings(core.snapshot().settings.copy(deviceSerial = "fixture-1")).get(10, TimeUnit.SECONDS).accepted)
            val switched = core.await("switched") { it.device?.serial == "fixture-1" }
            assertTrue(switched.generation > before)
            assertTrue(core.setEnabled(false).get(15, TimeUnit.SECONDS).accepted)
            assertTrue(core.snapshot().devices.isEmpty())
        } finally { core.closeAsync().get(15, TimeUnit.SECONDS) }
    }

    @Test
    fun unchosenWaitsForAChoiceWithSeveralAndChosenWaitsWhenAbsent() {
        val root = Files.createTempDirectory("replay-device-select-")
        devices(root, "fixture-1", "fixture-2")
        val core = ReplayCore(ReplaySettings(fixture(root), captureMode = CaptureMode.LOG_ONLY), root.resolve("workspace"))
        try {
            assertTrue(core.setEnabled(true).get(10, TimeUnit.SECONDS).accepted)
            val ambiguous = core.await("ambiguous") { it.captureState == CaptureState.MULTIPLE_DEVICES }
            assertNull(ambiguous.device)
            assertEquals(MULTIPLE_DEVICES_REASON, ambiguous.error)
            Thread.sleep(300)
            assertTrue(used(root).isEmpty())
            assertTrue(core.applySettings(core.snapshot().settings.copy(deviceSerial = "fixture-9")).get(10, TimeUnit.SECONDS).accepted)
            val absent = core.await("absent") { it.captureState == CaptureState.WAITING }
            assertNull(absent.device)
            assertEquals("選択した端末（fixture-9）の接続を待っています。ほかの端末には切り替えません", absent.error)
            assertTrue(core.applySettings(core.snapshot().settings.copy(deviceSerial = null)).get(10, TimeUnit.SECONDS).accepted)
            devices(root, "fixture-1")
            core.await("single-auto") { it.device?.serial == "fixture-1" }
        } finally { core.closeAsync().get(15, TimeUnit.SECONDS) }
    }

    @Test
    fun invalidSerialIsRejected() {
        assertFailsWith<IllegalArgumentException> { ReplaySettings(deviceSerial = "-s x").validate() }
        assertEquals("emulator-5554", ReplaySettings(deviceSerial = "emulator-5554").validate().deviceSerial)
    }
}
