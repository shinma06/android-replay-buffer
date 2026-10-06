package io.github.shinma06.replaybuffer.core

import java.nio.file.Files
import kotlin.test.Test
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class OwnedAdbTest {
    @Test
    fun shortFailuresRecoverImmediatelyOnceAndHealthyAcquisitionResetsBackoff() {
        val recovery = RecoveryDelay()
        kotlin.test.assertEquals(listOf(0L, 1000L, 2000L, 4000L, 5000L, 5000L), (1..6).map { recovery.next(false) })
        kotlin.test.assertEquals(0, recovery.next(true))
        kotlin.test.assertEquals(1000, recovery.next(false))
    }

    @Test
    fun cancellationTerminatesOnlyOwnedClientsAndRejectsLateCreation() {
        val directory = Files.createTempDirectory("replay-adb-fixture-")
        val fake = directory.resolve("adb")
        Files.writeString(fake, """#!/usr/bin/python3
import signal,time,pathlib,os
signal.signal(signal.SIGTERM,signal.SIG_IGN)
pathlib.Path(__file__).with_name(str(os.getpid())+'.ready').touch()
time.sleep(30)
""")
        assertTrue(fake.toFile().setExecutable(true))
        val unrelated = ProcessBuilder("/bin/sleep", "30").start()
        val adb = OwnedAdb(fake)
        try {
            val owned = java.util.concurrent.CopyOnWriteArrayList((1..3).map { adb.start("devices", "-l") })
            val ready = System.nanoTime() + 2_000_000_000
            while (owned.any { !Files.exists(directory.resolve("${it.pid()}.ready")) } && System.nanoTime() < ready) Thread.sleep(10)
            assertTrue(owned.all { Files.exists(directory.resolve("${it.pid()}.ready")) })
            val racing = kotlin.concurrent.thread {
                repeat(2) { runCatching { adb.start("devices", "-l") }.onSuccess { owned += it } }
            }
            val started = System.nanoTime()
            adb.close()
            racing.join(1000)
            assertFalse(racing.isAlive)
            assertTrue(System.nanoTime() - started < 4_000_000_000, "Batch shutdown must not wait a grace period per client")
            assertTrue(owned.none { it.isAlive })
            adb.close()
            assertTrue(unrelated.isAlive)
            assertFailsWith<IllegalStateException> { adb.start("devices") }
            assertFailsWith<IllegalArgumentException> { serialArgument("emulator-5554;kill") }
        } finally { adb.close(); unrelated.destroyForcibly().waitFor(); directory.toFile().deleteRecursively() }
    }
}
