package io.github.shinma06.arb.integration

import io.github.shinma06.arb.ipc.ArbPaths
import io.github.shinma06.arb.ipc.ArbdProcessManager
import io.github.shinma06.arb.ipc.HostOs
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.io.IOException
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths
import java.time.Duration
import java.util.concurrent.TimeUnit
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNotNull
import kotlin.test.assertSame
import kotlin.test.assertTrue

/**
 * C3 / CE-18 against the real `arbd`: the IDE must never stop the healthy managed daemon it
 * holds. Closing a managed daemon's stdin is reserved for an explicit stop / restart.
 */
class ArbdManagedDaemonSurvivalTest {
    private fun repoRoot(): Path {
        System.getProperty("arb.repo.root")?.takeIf { it.isNotBlank() }?.let {
            return Paths.get(it).toAbsolutePath().normalize()
        }
        var cur: Path? = Paths.get("").toAbsolutePath().normalize()
        repeat(6) {
            val d = cur ?: error("Could not locate repository root")
            if (Files.isDirectory(d.resolve("core")) && Files.isDirectory(d.resolve("fixtures"))) return d
            cur = d.parent
        }
        error("Could not locate repository root containing core/ and fixtures/")
    }

    /**
     * A managed `arbd` that does not answer the probe (slow start, Windows `ERROR_PIPE_BUSY`)
     * reads as `NotRunning`. Modelled by an endpoint override the daemon never listens on: the
     * second `connectOrSpawn` must wait on the live daemon instead of spawning a replacement
     * (which used to close the first daemon's stdin and stop it).
     */
    @Test
    fun notRunningProbeWaitsOnTheLiveManagedDaemonInsteadOfReplacingIt(@TempDir tmp: Path) {
        val unreachable = if (HostOs.detect() == HostOs.WINDOWS) {
            """\\.\pipe\arb-never-listening-${ProcessHandle.current().pid()}"""
        } else {
            tmp.resolve("never-listening.sock").toString()
        }
        val paths = ArbPaths(tmp.resolve("data"), tmp.resolve("cache"), tmp.resolve("run"), explicitEndpoint = unreachable)
        val ide = ArbdProcessManager(paths = paths, searchRoot = repoRoot(), parentBound = true)
        try {
            assertFailsWith<IOException> { ide.connectOrSpawn(Duration.ofMillis(800)) }
            val first = assertNotNull(ide.managedDaemonProcess(), "the first attempt spawned the managed arbd")

            repeat(2) {
                assertFailsWith<IOException> { ide.connectOrSpawn(Duration.ofMillis(800)) }
                assertSame(first, ide.managedDaemonProcess(), "no replacement daemon may be spawned")
                assertTrue(first.isAlive, "the slow-to-answer managed arbd must keep running")
            }
        } finally {
            ide.stopManagedDaemon()
        }
    }

    @Test
    fun foreignRestartLeavesTheOwnerRunning(@TempDir tmp: Path) {
        val paths = ArbPaths(tmp.resolve("data"), tmp.resolve("cache"), tmp.resolve("run"))
        val owner = ArbdProcessManager(paths = paths, searchRoot = repoRoot(), parentBound = true)
        val other = ArbdProcessManager(paths = paths, searchRoot = repoRoot(), parentBound = true)
        try {
            val original = owner.connectOrSpawn()
            original.client.use { rpc ->
                assertFailsWith<IOException> { other.restartDaemon().client.close() }
                assertEquals(original.hello.pid, rpc.systemHello(false).pid)
            }
        } finally {
            other.stopManagedDaemon()
            owner.stopManagedDaemon()
        }
    }

    /** A replacement that fails to start must not take the running managed daemon down with it. */
    @Test
    fun failedSpawnKeepsTheRunningManagedDaemon(@TempDir tmp: Path) {
        val paths = ArbPaths(tmp.resolve("data"), tmp.resolve("cache"), tmp.resolve("run"))
        val ide = ArbdProcessManager(paths = paths, searchRoot = repoRoot(), parentBound = true)
        val connected = ide.connectOrSpawn(Duration.ofSeconds(15))
        try {
            val running = assertNotNull(ide.managedDaemonProcess())
            assertFailsWith<IOException> {
                ide.spawnDaemon(tmp.resolve("missing").resolve(HostOs.detect().executableName("arbd")))
            }
            assertSame(running, ide.managedDaemonProcess(), "the managed daemon stays the tracked one")
            assertTrue(!running.waitFor(500, TimeUnit.MILLISECONDS), "its stdin must stay open")
            assertEquals(running.pid(), connected.client.systemHello(false).pid, "and it keeps serving")
        } finally {
            connected.client.close()
            ide.stopManagedDaemon()
        }
    }
}
