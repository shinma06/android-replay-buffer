package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonParser
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.TimeUnit
import kotlin.concurrent.thread
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class ForwardAllocationCleanupTest {
    @Test
    fun lostAllocationResponseIsDurableAndNextOffCoreRecoversOnlyItsOwnEndpoint() {
        Fixture().use { f ->
            Files.writeString(f.root.resolve("offline"), "offline")
            val core = ReplayCore(ReplaySettings(), f.root.resolve("workspace"), f.root.resolve("journal"))
            assertTrue(core.applySettings(ReplaySettings()).get(5, TimeUnit.SECONDS).accepted)
            ReplayCore::class.java.getDeclaredField("backend").apply { isAccessible = true }.set(core, f.capture)
            f.allocate()
            val endpoint = f.endpoint()
            core.closeAsync().get(10, TimeUnit.SECONDS)
            assertTrue(f.capture.cleanupPending)
            val record = f.capture.cleanupRecord()
            assertEquals(setOf(endpoint), record.pendingEndpoints)
            assertTrue(record.forwards.isEmpty())
            val path = f.root.resolve("journal/remote-cleanup-${record.token}.json")
            val descriptor = JsonParser.parseString(Files.readString(path)).asJsonObject
            assertEquals(2, descriptor["schema"].asInt)
            assertEquals(endpoint, descriptor["pending_endpoints"].asJsonArray.single().asString)
            assertEquals(record.serial, descriptor["serial"].asString)
            assertEquals(record.token, descriptor["token"].asString)
            f.foreignMappings(endpoint)
            Files.delete(f.root.resolve("offline"))
            val next = ReplayCore(ReplaySettings(f.adb), f.root.resolve("next-workspace"), path.parent)
            try {
                assertTrue(next.applySettings(ReplaySettings(f.adb)).get(10, TimeUnit.SECONDS).accepted)
                assertEquals(0, next.snapshot().cleanupPendingCount)
                assertFalse(next.snapshot().enabled)
                assertFalse(Files.exists(path))
                assertEquals(listOf("fixture-2 tcp:23457 $endpoint", "fixture-1 tcp:23458 localabstract:foreign"), f.mappings())
                assertEquals(listOf("tcp:23456"), Files.readAllLines(f.root.resolve("removed")))
                assertFalse(Files.exists(f.root.resolve("launched-server")))
            } finally { next.closeAsync().get(10, TimeUnit.SECONDS) }
        }
    }

    @Test
    fun closeDuringAllocationReconcilesItsMappingWithoutWaitingForStdout() {
        Fixture().use { f ->
            f.allocate()
            val endpoint = f.endpoint()
            f.foreignMappings(endpoint)
            f.capture.close()
            assertFalse(f.capture.cleanupPending)
            assertEquals(listOf("fixture-2 tcp:23457 $endpoint", "fixture-1 tcp:23458 localabstract:foreign"), f.mappings())
            assertEquals(listOf("tcp:23456"), Files.readAllLines(f.root.resolve("removed")))
            assertFalse(Files.exists(f.root.resolve("launched-server")))
        }
    }

    @Test
    fun ambiguousPendingIdentityIsRetainedAndRecycledOrForeignMappingsAreNotRemoved() {
        Fixture().use { f ->
            val endpoint = "localabstract:scrcpy_00000001"
            val record = f.capture.cleanupRecord().copy(pendingEndpoints = setOf(endpoint))
            Files.writeString(f.root.resolve("mappings"), "fixture-1 tcp:12345 $endpoint\nfixture-1 tcp:12346 $endpoint\n")
            RemoteCleanupJournal(f.root.resolve("journal")).use { journal ->
                journal.retain(record)
                journal.clean(f.adb, setOf("fixture-1"))
                assertEquals(1, journal.pendingCount)
                assertFalse(Files.exists(f.root.resolve("removed")))
                Files.writeString(f.root.resolve("mappings"), "fixture-1 tcp:12345 localabstract:foreign\nfixture-2 tcp:12346 $endpoint\n")
                journal.clean(f.adb, setOf("fixture-1"))
                assertEquals(0, journal.pendingCount)
                assertEquals(listOf("fixture-1 tcp:12345 localabstract:foreign", "fixture-2 tcp:12346 $endpoint"), f.mappings())
                assertFalse(Files.exists(f.root.resolve("removed")))
            }
        }
    }

    @Test
    fun oldDescriptorIsStillRecoverableAndInvalidPendingEndpointDescriptorsArePreserved() {
        Fixture().use { f ->
            val stable = Files.createDirectory(f.root.resolve("journal"))
            val old = stable.resolve("remote-cleanup-${"a".repeat(32)}.json")
            Files.writeString(old, """{"schema":1,"serial":"fixture-1","token":"${"a".repeat(32)}","forwards":{}}""")
            val invalid = listOf(
                "[\"localabstract:foreign\"]",
                "[\"localabstract:scrcpy_00000001\",\"localabstract:scrcpy_00000001\"]",
                (0..64).joinToString(prefix = "[", postfix = "]") { "\"localabstract:scrcpy_${it.toString(16).padStart(8, '0')}\"" },
            ).mapIndexed { index, endpoints ->
                val token = ('b' + index).toString().repeat(32)
                val path = stable.resolve("remote-cleanup-$token.json")
                val bytes = """{"schema":2,"serial":"fixture-1","token":"$token","forwards":{},"pending_endpoints":$endpoints}"""
                Files.writeString(path, bytes)
                path to bytes
            }
            RemoteCleanupJournal(stable).use { journal ->
                journal.recover()
                assertEquals(4, journal.pendingCount)
                assertTrue(journal.hasRecoverable)
                journal.clean(f.adb, setOf("fixture-1"))
                assertFalse(Files.exists(old))
                assertEquals(3, journal.pendingCount)
                invalid.forEach { (path, bytes) -> assertEquals(bytes, Files.readString(path)) }
            }
        }
    }

    private class Fixture : AutoCloseable {
        val root = Files.createTempDirectory("replay-forward-response-loss-").toRealPath()
        val adb = root.resolve("adb")
        private val resources = CaptureResources(Files.createDirectory(root.resolve("resources")))
        private val store = CaptureStore(root.resolve("ring"))
        val capture: DeviceCapture
        private var worker: Thread? = null

        init {
            Files.writeString(adb, """#!/usr/bin/python3
import sys,time,pathlib
r=pathlib.Path(__file__).parent;a=sys.argv[1:]
p=r/'mappings'
if a==['devices','-l']:
    print('List of devices attached\nfixture-1 device model:Fixture')
elif a==['forward','--list']:
    if (r/'offline').exists(): sys.exit(1)
    print(p.read_text() if p.exists() else '',end='')
elif a[:3]==['-s','fixture-1','forward'] and a[3]=='tcp:0':
    p.write_text('fixture-1 tcp:23456 '+a[4]+'\n')
    (r/'allocated').touch()
    time.sleep(30)
    print('23456')
elif a[:4]==['-s','fixture-1','forward','--remove']:
    rows=p.read_text().splitlines()
    p.write_text(''.join(x+'\n' for x in rows if not x.startswith('fixture-1 '+a[4]+' ')))
    with (r/'removed').open('a') as f: f.write(a[4]+'\n')
elif a[:3]==['-s','fixture-1','push']: pass
elif a[:4]==['-s','fixture-1','shell','ps']:
    if (r/'offline').exists(): sys.exit(1)
    print('PID ARGS')
elif a[:4]==['-s','fixture-1','shell','rm']: pass
elif a[:3]==['-s','fixture-1','shell']:
    (r/'launched-server').touch();sys.exit(1)
else: sys.exit(1)
""")
            assertTrue(adb.toFile().setExecutable(true))
            capture = DeviceCapture(adb, "fixture-1", resources, store, 1, ApplicationTarget())
        }

        fun allocate() {
            val video = DeviceCapture::class.java.getDeclaredMethod("video").apply { isAccessible = true }
            val thread = thread(name = "replay-forward-response-loss", start = false, isDaemon = true) {
                runCatching { video.invoke(capture) }
            }
            @Suppress("UNCHECKED_CAST")
            val workers = DeviceCapture::class.java.getDeclaredField("workers").apply { isAccessible = true }.get(capture) as MutableList<Thread>
            workers += thread
            worker = thread
            thread.start()
            val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
            while (!Files.exists(root.resolve("allocated")) && System.nanoTime() < deadline) Thread.sleep(10)
            assertTrue(Files.exists(root.resolve("allocated")))
        }

        fun mappings(): List<String> = root.resolve("mappings").takeIf(Files::exists)?.let(Files::readAllLines) ?: emptyList()
        fun endpoint(): String = mappings().single().substringAfterLast(' ')
        fun foreignMappings(endpoint: String) {
            Files.writeString(root.resolve("mappings"), "fixture-1 tcp:23456 $endpoint\nfixture-2 tcp:23457 $endpoint\nfixture-1 tcp:23458 localabstract:foreign\n")
        }

        override fun close() {
            Files.deleteIfExists(root.resolve("offline"))
            capture.close()
            worker?.let { it.join(5000); check(!it.isAlive) }
            store.close()
            resources.close()
            Files.walk(root).use { it.sorted(java.util.Comparator.reverseOrder()).forEach(Files::deleteIfExists) }
        }
    }
}
