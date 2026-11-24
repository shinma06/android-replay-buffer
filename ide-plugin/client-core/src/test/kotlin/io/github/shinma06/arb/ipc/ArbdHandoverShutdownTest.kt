package io.github.shinma06.arb.ipc

import io.github.shinma06.arb.model.ArbdRpcException
import org.junit.jupiter.api.Test
import java.nio.file.Path
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/**
 * P0-05 / CE-05a: replacing a connected daemon (identity upgrade or takeover) must go through
 * `system.shutdown_for_handover`, which `arbd` refuses atomically while a save is in flight. Legacy daemons defer.
 */
class ArbdHandoverShutdownTest {
    private val manager = ArbdProcessManager(
        paths = ArbPaths(Path.of("d"), Path.of("c"), Path.of("r"), os = HostOs.LINUX),
        os = HostOs.LINUX,
        parentBound = true,
    )

    private fun run(respond: (String) -> Any): Pair<Boolean, List<String>> {
        val transport = ScriptedTransport(respond)
        val accepted = ArbdRpcClient(transport).use { manager.requestHandoverShutdown(it) }
        return accepted to transport.sent.toList()
    }

    @Test
    fun identityUpgradeOfABusyDaemonIsDeferredWithoutARawShutdown() {
        val (accepted, sent) = run { method ->
            when (method) {
                "system.shutdown_for_handover" -> """{"ok":false}"""
                else -> """{"ok":true}"""
            }
        }
        assertFalse(accepted, "a daemon refusing the handover (save in flight) keeps running")
        assertEquals(listOf("system.shutdown_for_handover"), sent, "never a raw system.shutdown")
    }

    @Test
    fun idleDaemonAcceptsTheHandover() {
        val (accepted, sent) = run { """{"ok":true}""" }
        assertTrue(accepted)
        assertEquals(listOf("system.shutdown_for_handover"), sent)
    }

    @Test
    fun legacyDaemonDefersBecauseStatusCannotReserveTheNextSave() {
        val notFound = ScriptedTransport.Error(ArbdRpcException.RPC_ERR_METHOD_NOT_FOUND, "method not found")
        val (accepted, sent) = run { method ->
            if (method == "system.shutdown_for_handover") notFound else """{"active_save_jobs":0,"ok":true}"""
        }
        assertFalse(accepted)
        assertEquals(listOf("system.shutdown_for_handover"), sent)
    }
}
