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
        Files.writeString(fake, "#!/bin/sh\nexec /bin/sleep 30\n")
        assertTrue(fake.toFile().setExecutable(true))
        val unrelated = ProcessBuilder("/bin/sleep", "30").start()
        val adb = OwnedAdb(fake)
        try {
            val owned = adb.start("devices", "-l")
            adb.close()
            assertFalse(owned.isAlive)
            assertTrue(unrelated.isAlive)
            assertFailsWith<IllegalStateException> { adb.start("devices") }
            assertFailsWith<IllegalArgumentException> { serialArgument("emulator-5554;kill") }
        } finally { adb.close(); unrelated.destroyForcibly().waitFor(); Files.delete(fake); Files.delete(directory) }
    }
}
