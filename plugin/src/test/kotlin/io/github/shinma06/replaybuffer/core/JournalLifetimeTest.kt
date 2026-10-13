package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonParser
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.CountDownLatch
import java.util.concurrent.ScheduledThreadPoolExecutor
import java.util.concurrent.TimeUnit
import kotlin.concurrent.thread
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertSame
import kotlin.test.assertTrue

class JournalLifetimeTest {
    @Test
    fun unresolvedLifetimeIsExcludedFromBothPollBranchesAndSettingsChanges() {
        for (disabled in listOf(false, true)) Fixture().use { f ->
            f.failStop()
            if (disabled) set(f.core, "enabled", false)
            f.poll()
            assertTrue(Files.exists(f.descriptor))
            assertSame(f.capture, get(f.core, "backend"))
            assertTrue(f.reader.isAlive)
            assertFalse(Files.exists(f.root.resolve("removed")))
            val changed = f.core.snapshot().settings.copy(adbPath = f.root.resolve("other-adb"))
            assertFalse(f.core.applySettings(changed).get(10, TimeUnit.SECONDS).accepted)
            assertTrue(Files.exists(f.descriptor))
            assertEquals(3, JsonParser.parseString(Files.readString(f.descriptor)).asJsonObject["schema"].asInt)
        }
    }

    @Test
    fun failedDisposalCannotMakeUnresolvedRecordRecoverableByNextCoreEvenAfterReaderExit() {
        Fixture().use { f ->
            f.failStop()
            assertTrue(runCatching { f.core.closeAsync().get(10, TimeUnit.SECONDS) }.isFailure)
            f.nextCore().use { next ->
                assertEquals(1, next.snapshot().cleanupPendingCount)
                assertTrue(Files.exists(f.descriptor))
                assertFalse(Files.exists(f.root.resolve("removed")))
                f.releaseReader()
                // Exit alone cannot establish that the disposed owner's client/request lifetime was settled.
                assertTrue(next.applySettings(next.snapshot().settings).get(10, TimeUnit.SECONDS).accepted)
                assertEquals(1, next.snapshot().cleanupPendingCount)
                assertTrue(Files.exists(f.descriptor))
            }
        }
    }

    @Test
    fun confirmedRetryRetiresOnlyExactOwnedRecordAndClearsPendingWhileOffline() {
        Fixture().use { f ->
            f.failStop()
            f.releaseReader()
            assertTrue(f.core.setEnabled(false).get(10, TimeUnit.SECONDS).accepted)
            assertFalse(f.capture.cleanupPending)
            assertFalse(Files.exists(f.descriptor))
            assertEquals(0, f.core.snapshot().cleanupPendingCount)
            Files.writeString(f.root.resolve("offline"), "offline")
            f.poll()
            assertEquals(0, f.core.snapshot().cleanupPendingCount)
        }
    }

    @Test
    fun confirmedHostStopWithOfflineRemoteCleanupMakesSameRecordRecoverable() {
        Fixture().use { f ->
            f.failStop()
            f.releaseReader()
            Files.writeString(f.root.resolve("offline"), "offline")
            assertTrue(f.core.setEnabled(false).get(10, TimeUnit.SECONDS).accepted)
            assertTrue(f.capture.cleanupPending)
            assertEquals(2, JsonParser.parseString(Files.readString(f.descriptor)).asJsonObject["schema"].asInt)
            f.core.closeAsync().get(10, TimeUnit.SECONDS)
            Files.delete(f.root.resolve("offline"))
            f.nextCore().use { next ->
                assertEquals(0, next.snapshot().cleanupPendingCount)
                assertFalse(Files.exists(f.descriptor))
            }
        }
    }

    @Test
    fun unconfirmedOwnedClientDrainAlsoBlocksJournalUntilSuccessfulCloseRetry() {
        Fixture().use { f ->
            val adb = get(f.capture, "adb") as OwnedAdb
            val process = adb.start("devices", "-l")
            assertTrue(process.waitFor(5, TimeUnit.SECONDS))
            @Suppress("UNCHECKED_CAST")
            val drains = get(adb, "drains") as MutableMap<Process, Thread>
            drains.getValue(process).join(5000)
            drains[process] = f.reader
            f.failStop()
            f.poll()
            assertTrue(Files.exists(f.descriptor))
            assertTrue(f.reader.isAlive)
            f.releaseReader()
            assertTrue(f.core.setEnabled(false).get(10, TimeUnit.SECONDS).accepted)
            assertFalse(Files.exists(f.descriptor))
            assertEquals(0, f.core.snapshot().cleanupPendingCount)
        }
    }

    @Test
    fun successfulCleanupCannotRetireReplacedInodeOrInvalidLock() {
        Fixture().use { f ->
            f.failStop()
            f.releaseReader()
            Files.move(f.descriptor, f.root.resolve("owned-record"))
            Files.writeString(f.descriptor, "foreign")
            assertFalse(f.core.setEnabled(false).get(10, TimeUnit.SECONDS).accepted)
            assertEquals("foreign", Files.readString(f.descriptor))
            assertSame(f.capture, get(f.core, "backend"))
            Files.delete(f.descriptor)
            Files.move(f.root.resolve("owned-record"), f.descriptor)
            val journal = get(f.core, "cleanup") as RemoteCleanupJournal
            val entry = (get(journal, "owned") as Map<*, *>).values.single()!!
            (get(entry, "channel") as java.nio.channels.FileChannel).close()
            assertFalse(f.core.setEnabled(false).get(10, TimeUnit.SECONDS).accepted)
            assertTrue(Files.exists(f.descriptor))
        }
    }

    private class Fixture : AutoCloseable {
        val root = Files.createTempDirectory("replay-journal-lifetime-").toRealPath()
        val adb = root.resolve("adb")
        private val resources = CaptureResources(Files.createDirectory(root.resolve("resources")))
        private val store = CaptureStore(root.resolve("ring"))
        val capture: DeviceCapture
        private val release = CountDownLatch(1)
        val reader: Thread
        val core: ReplayCore
        private val control: ScheduledThreadPoolExecutor
        val descriptor: Path

        init {
            Files.writeString(adb, """#!/usr/bin/python3
import pathlib,sys
r=pathlib.Path(__file__).parent;a=sys.argv[1:]
if a==['devices','-l']:
 print('List of devices attached')
 if not (r/'offline').exists(): print('fixture-1 device model:Fixture')
elif (r/'offline').exists(): sys.exit(1)
elif a==['forward','--list']: pass
elif a[:4]==['-s','fixture-1','shell','ps']: print('PID ARGS')
elif a[:4]==['-s','fixture-1','shell','rm']: pass
elif a[:4]==['-s','fixture-1','forward','--remove']: (r/'removed').touch()
else: sys.exit(1)
""")
            check(adb.toFile().setExecutable(true))
            capture = DeviceCapture(adb, "fixture-1", resources, store, 1, ApplicationTarget())
            @Suppress("UNCHECKED_CAST")
            (get(capture, "pendingEndpoints") as MutableSet<String>) += "localabstract:scrcpy_00000001"
            val entered = CountDownLatch(1)
            reader = thread(isDaemon = true) {
                entered.countDown()
                while (release.count > 0) try { release.await() } catch (_: InterruptedException) { }
            }
            assertTrue(entered.await(5, TimeUnit.SECONDS))
            @Suppress("UNCHECKED_CAST")
            (get(capture, "workers") as MutableList<Thread>) += reader
            val settings = ReplaySettings(adb)
            core = ReplayCore(settings, root.resolve("workspace"), root.resolve("journal"))
            assertTrue(core.applySettings(settings).get(5, TimeUnit.SECONDS).accepted)
            control = get(get(core, "control")!!, "e") as ScheduledThreadPoolExecutor
            control.queue.clear() // Step the production poll deterministically on its actual executor.
            set(core, "backend", capture); set(core, "enabled", true)
            set(core, "store", store); set(core, "resources", resources); set(core, "probe", OwnedAdb(adb))
            descriptor = root.resolve("journal/remote-cleanup-${capture.cleanupRecord().token}.json")
        }

        fun failStop() {
            assertFalse(core.setEnabled(false).get(10, TimeUnit.SECONDS).accepted)
            assertTrue(Files.exists(descriptor))
        }
        fun poll() {
            val method = ReplayCore::class.java.getDeclaredMethod("poll").apply { isAccessible = true }
            control.submit { method.invoke(core) }.get(10, TimeUnit.SECONDS)
        }
        fun releaseReader() { release.countDown(); reader.join(5000); check(!reader.isAlive) }
        fun nextCore(): ReplayCore = ReplayCore(ReplaySettings(adb), root.resolve("next"), descriptor.parent).also {
            assertTrue(it.applySettings(ReplaySettings(adb)).get(10, TimeUnit.SECONDS).accepted)
        }
        override fun close() {
            releaseReader()
            runCatching { core.closeAsync().get(10, TimeUnit.SECONDS) }
            capture.close(); store.close(); resources.close()
            Files.walk(root).use { it.sorted(java.util.Comparator.reverseOrder()).forEach(Files::deleteIfExists) }
        }
    }

    companion object {
        private fun get(owner: Any, name: String): Any? = owner.javaClass.getDeclaredField(name).apply { isAccessible = true }.get(owner)
        private fun set(owner: Any, name: String, value: Any) { owner.javaClass.getDeclaredField(name).apply { isAccessible = true }.set(owner, value) }
    }
}
