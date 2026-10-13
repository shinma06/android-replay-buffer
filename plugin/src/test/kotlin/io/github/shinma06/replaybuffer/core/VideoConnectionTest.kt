package io.github.shinma06.replaybuffer.core

import java.net.InetAddress
import java.net.ServerSocket
import java.net.Socket
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.concurrent.thread
import kotlin.test.Test
import kotlin.test.assertFailsWith
import kotlin.test.assertTrue

class VideoConnectionTest {
    @Test
    fun preparationHasADeadlineAndRealExitAndCancellationReleaseSockets() {
        val server = ServerSocket(0, 8, InetAddress.getLoopbackAddress())
        val held = ConcurrentHashMap.newKeySet<Socket>()
        val sockets = ConcurrentHashMap.newKeySet<Socket>()
        val accept = thread(isDaemon = true) { runCatching { while (!server.isClosed) held += server.accept() } }
        // Owned fixture process only, not adb or another agent.
        val process = ProcessBuilder("/bin/sleep", "20").start()
        try {
            val started = System.nanoTime()
            assertFailsWith<IllegalStateException> { connectVideo(server.localPort, process, AtomicBoolean(), sockets, 250) }
            assertTrue(System.nanoTime() - started < TimeUnit.SECONDS.toNanos(2))
            assertTrue(sockets.isEmpty())
            val stopping = AtomicBoolean()
            val unexpected = java.util.concurrent.atomic.AtomicReference<Throwable>()
            val worker = thread {
                try { connectVideo(server.localPort, process, stopping, sockets); unexpected.set(AssertionError("Cancellation accepted a socket")) }
                catch (_: IllegalStateException) { } catch (e: Throwable) { unexpected.set(e) }
            }
            val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(2)
            while (sockets.isEmpty() && System.nanoTime() < deadline) Thread.sleep(5)
            assertTrue(sockets.isNotEmpty())
            stopping.set(true); sockets.forEach { it.close() }
            worker.join(2000)
            assertTrue(!worker.isAlive && sockets.isEmpty())
            kotlin.test.assertNull(unexpected.get())
            process.destroy(); assertTrue(process.waitFor(2, TimeUnit.SECONDS))
            assertFailsWith<IllegalStateException> { connectVideo(server.localPort, process, AtomicBoolean(), sockets) }
            assertTrue(sockets.isEmpty())
        } finally {
            process.destroyForcibly(); process.waitFor(2, TimeUnit.SECONDS)
            server.close(); held.forEach { it.close() }; accept.join(2000)
        }
    }
}
