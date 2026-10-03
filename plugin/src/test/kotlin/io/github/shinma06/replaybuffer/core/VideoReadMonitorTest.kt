package io.github.shinma06.replaybuffer.core

import java.io.DataInputStream
import java.net.InetAddress
import java.net.ServerSocket
import java.net.Socket
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.concurrent.thread
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class VideoReadMonitorTest {
    @Test
    fun partialFirstPacketDoesNotExtendTheWatchdogAndThreeSecondsDoesNotCloseTheSocket() {
        val listener = ServerSocket(0, 8, InetAddress.getLoopbackAddress())
        val client = Socket(InetAddress.getLoopbackAddress(), listener.localPort)
        val peer = listener.accept()
        val process = ProcessBuilder("/bin/sleep", "20").start()
        val timer = Executors.newSingleThreadScheduledExecutor()
        val monitor = VideoReadMonitor(client, process, timer)
        val interruptedRead = AtomicBoolean()
        val reader = thread { runCatching { readVideo(DataInputStream(client.getInputStream())) }.onFailure { interruptedRead.set(true) } }
        val trickle = thread {
            runCatching { repeat(6) { peer.getOutputStream().write(0); peer.getOutputStream().flush(); Thread.sleep(2000) } }
        }
        try {
            Thread.sleep(3400)
            assertTrue(monitor.arrivalUnconfirmed)
            assertFalse(client.isClosed)
            reader.join(8000)
            assertTrue(client.isClosed && interruptedRead.get() && !reader.isAlive)
        } finally {
            monitor.close(); client.close(); peer.close(); listener.close(); trickle.interrupt(); trickle.join(2000); reader.join(2000)
            process.destroyForcibly(); process.waitFor(2, TimeUnit.SECONDS); timer.shutdownNow(); timer.awaitTermination(2, TimeUnit.SECONDS)
        }
    }

    @Test
    fun aBlockedClockStopCannotQueueVideoSocketCloseBehindIt() {
        val listener = ServerSocket(0, 8, InetAddress.getLoopbackAddress())
        val client = Socket(InetAddress.getLoopbackAddress(), listener.localPort)
        val peer = listener.accept()
        val process = ProcessBuilder("/bin/sleep", "20").start()
        val timer = captureWatchdog()
        val stoppingClock = java.util.concurrent.CountDownLatch(1)
        val releaseClock = java.util.concurrent.CountDownLatch(1)
        timer.execute { stoppingClock.countDown(); releaseClock.await() }
        val monitor = VideoReadMonitor(client, process, timer)
        try {
            assertTrue(stoppingClock.await(2, TimeUnit.SECONDS))
            process.destroy(); assertTrue(process.waitFor(2, TimeUnit.SECONDS))
            val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(2)
            while (!client.isClosed && System.nanoTime() < deadline) Thread.sleep(25)
            assertTrue(client.isClosed)
            assertEquals(1L, releaseClock.count)
        } finally {
            monitor.close(); releaseClock.countDown(); client.close(); peer.close(); listener.close()
            process.destroyForcibly(); process.waitFor(2, TimeUnit.SECONDS); timer.shutdownNow(); timer.awaitTermination(2, TimeUnit.SECONDS)
        }
    }

    @Test
    fun cancelledWatchCannotCloseItsOldSocketWhenTheNextAttemptObservesServerExit() {
        val listener = ServerSocket(0, 8, InetAddress.getLoopbackAddress())
        val old = Socket(InetAddress.getLoopbackAddress(), listener.localPort)
        val oldPeer = listener.accept()
        val next = Socket(InetAddress.getLoopbackAddress(), listener.localPort)
        val nextPeer = listener.accept()
        val process = ProcessBuilder("/bin/sleep", "20").start()
        val timer = Executors.newSingleThreadScheduledExecutor()
        val first = VideoReadMonitor(old, process, timer)
        first.close()
        val second = VideoReadMonitor(next, process, timer)
        try {
            process.destroy(); assertTrue(process.waitFor(2, TimeUnit.SECONDS))
            val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(2)
            while (!next.isClosed && System.nanoTime() < deadline) Thread.sleep(25)
            assertTrue(next.isClosed)
            assertFalse(old.isClosed)
            assertFalse(first.arrivalUnconfirmed)
        } finally {
            first.close(); second.close(); old.close(); next.close(); oldPeer.close(); nextPeer.close(); listener.close()
            process.destroyForcibly(); process.waitFor(2, TimeUnit.SECONDS); timer.shutdownNow(); timer.awaitTermination(2, TimeUnit.SECONDS)
        }
    }
}
