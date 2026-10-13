package io.github.shinma06.replaybuffer.core

import java.nio.file.Files
import java.util.concurrent.TimeUnit
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class RemoteCleanupTest {
    @Test
    fun nextCoreWithANewWorkspaceRecoversOnlyStoppedOwnerAndMatchingRemoteIdentities() {
        val root = Files.createTempDirectory("replay-cleanup-fixture-").toRealPath()
        val stable = root.resolve("journal")
        val fake = root.resolve("adb")
        Files.writeString(root.resolve("connected"), "no")
        Files.writeString(fake, """#!/usr/bin/python3
import sys,pathlib,time,json
r=pathlib.Path(__file__).parent;a=sys.argv[1:]
with (r/'calls').open('a') as f: f.write(json.dumps(a)+'\n')
if a==['devices','-l']:
    print('List of devices attached')
    if (r/'connected').read_text()=='yes': print('fixture-1 device model:Fixture')
elif a==['forward','--list']:
    print('fixture-1 tcp:12345 localabstract:scrcpy_00000001')
    print('fixture-2 tcp:12346 localabstract:someone_else')
elif a[:2]==['-s','fixture-1']:
    if (r/'connected').read_text()!='yes': sys.exit(1)
    if a[2]=='forward' and '--remove' not in a: print(1)
    elif a[2]=='shell' and a[3]=='ps':
        print('PID ARGS')
        if (r/'alive').exists(): print('77 '+(r/'owned-name').read_text())
    elif a[2]=='shell' and a[3]=='kill': (r/'alive').unlink(missing_ok=True)
    elif a[2]=='shell' and a[3]=='settings': print(1)
    elif a[2]=='shell' and 'scrcpy.Server' in a[3]: time.sleep(20)
""")
        assertTrue(fake.toFile().setExecutable(true))
        val resources = CaptureResources(Files.createDirectory(root.resolve("resources")))
        val store = CaptureStore(root.resolve("ring"))
        val capture = DeviceCapture(fake, "fixture-1", resources, store, 1, ApplicationTarget())
        capture.close() // Genuine offline close yields a pending owned descriptor; no actual adb/device.
        assertTrue(capture.cleanupPending)
        val record = capture.cleanupRecord().copy(forwards = mapOf("12345" to "localabstract:scrcpy_00000001", "12346" to "localabstract:scrcpy_00000002"))
        Files.writeString(root.resolve("owned-name"), "replay-${record.token}-video")
        Files.writeString(root.resolve("alive"), "yes")
        val firstOwner = RemoteCleanupJournal(stable)
        firstOwner.retain(record)
        val core = ReplayCore(ReplaySettings(fake), root.resolve("new-workspace"), stable)
        fun await(ready: () -> Boolean) {
            val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(10)
            while (!ready() && System.nanoTime() < deadline) Thread.sleep(25)
            assertTrue(ready(), core.snapshot().toString())
        }
        try {
            await { core.snapshot().cleanupPendingCount == 1 }
            assertFalse(core.snapshot().enabled)
            Files.writeString(root.resolve("connected"), "yes")
            assertTrue(core.applySettings(ReplaySettings(fake)).get(10, TimeUnit.SECONDS).accepted)
            assertTrue(Files.exists(root.resolve("alive"))) // Live journal owner still excludes recovery.
            firstOwner.close()
            await { core.snapshot().cleanupPendingCount == 0 && !Files.exists(root.resolve("alive")) }
            assertEquals(0, Files.list(stable).use { it.count() })
            val calls = Files.readAllLines(root.resolve("calls"))
            assertTrue(calls.any { it.contains("kill") && it.contains("77") })
            assertTrue(calls.any { it.contains("--remove") && it.contains("tcp:12345") })
            assertFalse(calls.any { it.contains("--remove") && it.contains("tcp:12346") })
            assertTrue(calls.any { it.contains("replay-${record.token}-server.jar") && it.contains("rm") })
            assertFalse(calls.any { it.contains("kill-server") || it.contains("pkill") || it.contains("-s\", \"fixture-2") })
            core.closeAsync().get(15, TimeUnit.SECONDS)
            assertFalse(core.snapshot().enabled)
            assertFalse(calls.any { it.contains("ClockProbe") || it.contains("scrcpy.Server") || it.contains("logcat") || it.contains("push") })
            assertFalse(Files.exists(root.resolve("new-workspace")))
        } finally {
            firstOwner.close(); core.closeAsync().get(15, TimeUnit.SECONDS); store.close(); resources.close()
            Files.walk(root).use { it.sorted(java.util.Comparator.reverseOrder()).forEach(Files::deleteIfExists) }
        }
    }

    @Test
    fun offWithNoVerifiedPendingRecordNeverInvokesSdkAdb() {
        val root = Files.createTempDirectory("replay-off-cleanup-").toRealPath()
        val fake = root.resolve("adb")
        Files.writeString(fake, "#!/bin/sh\nprintf called >> \"${root.resolve("calls")}\"\nexit 1\n")
        assertTrue(fake.toFile().setExecutable(true))
        val stable = Files.createDirectory(root.resolve("journal"))
        val core = ReplayCore(ReplaySettings(fake), root.resolve("unused-workspace"), stable)
        try {
            assertTrue(core.applySettings(ReplaySettings(fake)).get(10, TimeUnit.SECONDS).accepted)
            assertEquals(0, core.snapshot().cleanupPendingCount)
            assertFalse(Files.exists(root.resolve("calls")))
            val path = stable.resolve("remote-cleanup-${"b".repeat(32)}.json")
            Files.writeString(path, "{\"schema\":999}")
            assertTrue(core.applySettings(ReplaySettings(fake)).get(10, TimeUnit.SECONDS).accepted)
            assertEquals(1, core.snapshot().cleanupPendingCount)
            assertFalse(Files.exists(root.resolve("calls")))
            assertFalse(core.snapshot().enabled)
            assertFalse(Files.exists(root.resolve("unused-workspace")))
            assertEquals("{\"schema\":999}", Files.readString(path))
        } finally {
            core.closeAsync().get(10, TimeUnit.SECONDS)
            Files.walk(root).use { it.sorted(java.util.Comparator.reverseOrder()).forEach(Files::deleteIfExists) }
        }
    }

    @Test
    fun malformedFutureOversizedAndSymlinkRecordsArePreservedWithoutRemoteCalls() {
        val root = Files.createTempDirectory("replay-invalid-cleanup-").toRealPath()
        val originals = linkedMapOf<java.nio.file.Path, ByteArray>()
        listOf("{", "{\"schema\":999}", "x".repeat(65537)).forEachIndexed { index, bytes ->
            val path = root.resolve("remote-cleanup-${index.toString().padStart(32, '0')}.json")
            originals[path] = bytes.toByteArray(); Files.write(path, originals.getValue(path))
        }
        val outside = Files.createTempFile("replay-unrelated-cleanup-", ".txt")
        Files.writeString(outside, "keep")
        val link = root.resolve("remote-cleanup-${"a".repeat(32)}.json")
        Files.createSymbolicLink(link, outside)
        try {
            RemoteCleanupJournal(root).use { journal ->
                journal.recover(); assertEquals(4, journal.pendingCount)
                journal.clean(root.resolve("never-executed-adb"), setOf("fixture-1"))
            }
            originals.forEach { (path, bytes) -> assertTrue(bytes.contentEquals(Files.readAllBytes(path))) }
            assertTrue(Files.isSymbolicLink(link)); assertEquals("keep", Files.readString(outside))
        } finally { Files.walk(root).use { it.sorted(java.util.Comparator.reverseOrder()).forEach(Files::deleteIfExists) }; Files.delete(outside) }
    }
}
