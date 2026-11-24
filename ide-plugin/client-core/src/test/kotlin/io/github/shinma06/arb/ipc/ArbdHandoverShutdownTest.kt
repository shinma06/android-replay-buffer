package io.github.shinma06.arb.ipc

import io.github.shinma06.arb.model.ArbdRpcException
import org.junit.jupiter.api.Test
import java.nio.file.Path
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/**
 * P0-05 / CE-05a: replacing a connected daemon (identity upgrade or takeover) must go through
 * `system.shutdown_for_handover`, which `arbd` refuses atomically while a save is in flight. The
 * raw `system.shutdown` is only the fallback for daemons that predate that RPC.
 */
class ArbdHandoverShutdownTest {
    private val manager = ArbdProcessManager(
        paths = ArbPaths(Path.of("d"), Path.of("c"), Path.of("r"), os = HostOs.LINUX),
        os = HostOs.LINUX,
        parentBound = true,
    )

    private fun status(activeSaveJobs: Long) =
        """{"pid":1,"uptime_ms":1,"capture":{"active":false,"connected":false,"storage_pressure_stopped":false,""" +
            """"video_frames":0,"audio_frames":0,"log_records":0,"tap_records":0,"discontinuities":0,"static_hold_events":0},""" +
            """"ring_chunk_count":0,"ring_total_bytes":0,"ring_pinned_chunk_count":0,"active_viewports":0,""" +
            """"active_save_jobs":$activeSaveJobs,"recovery":{}}"""

    private fun run(identityUpgrade: Boolean, respond: (String) -> Any): Pair<Boolean, List<String>> {
        val transport = ScriptedTransport(respond)
        val accepted = ArbdRpcClient(transport).use { manager.requestHandoverShutdown(it, identityUpgrade) }
        return accepted to transport.sent.toList()
    }

    @Test
    fun identityUpgradeOfABusyDaemonIsDeferredWithoutARawShutdown() {
        for (identityUpgrade in listOf(true, false)) {
            val (accepted, sent) = run(identityUpgrade) { method ->
                when (method) {
                    "system.shutdown_for_handover" -> """{"ok":false}"""
                    else -> """{"ok":true}"""
                }
            }
            assertFalse(accepted, "a daemon refusing the handover (save in flight) keeps running")
            assertEquals(listOf("system.shutdown_for_handover"), sent, "never a raw system.shutdown")
        }
    }

    @Test
    fun idleDaemonAcceptsTheHandover() {
        val (accepted, sent) = run(identityUpgrade = true) { """{"ok":true}""" }
        assertTrue(accepted)
        assertEquals(listOf("system.shutdown_for_handover"), sent)
    }

    @Test
    fun onlyAPreHandoverDaemonFallsBackToTheStatusCheckedShutdown() {
        val notFound = ScriptedTransport.Error(ArbdRpcException.RPC_ERR_METHOD_NOT_FOUND, "method not found")
        fun legacy(activeSaves: Long): (String) -> Any = { method ->
            when (method) {
                "system.shutdown_for_handover" -> notFound
                "system.status" -> status(activeSaves)
                else -> """{"ok":true}"""
            }
        }
        val idle = run(identityUpgrade = true, respond = legacy(0))
        assertTrue(idle.first)
        assertEquals(listOf("system.shutdown_for_handover", "system.status", "system.shutdown"), idle.second)

        val busy = run(identityUpgrade = true, respond = legacy(1))
        assertFalse(busy.first, "a legacy daemon with an active save is not stopped")
        assertEquals(listOf("system.shutdown_for_handover", "system.status"), busy.second)

        val takeover = run(identityUpgrade = false, respond = legacy(0))
        assertFalse(takeover.first, "a takeover never force-stops a legacy daemon")
        assertEquals(listOf("system.shutdown_for_handover"), takeover.second)
    }
}
