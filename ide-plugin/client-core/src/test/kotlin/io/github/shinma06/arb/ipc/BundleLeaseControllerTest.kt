package io.github.shinma06.arb.ipc

import org.junit.jupiter.api.Test
import java.time.Duration
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class BundleLeaseControllerTest {
    @Test
    fun renewsHeartbeatWhileOpenAndReleasesViewportAndLeaseOnceOnClose() {
        val renewLatch = CountDownLatch(2)
        val renewCalls = AtomicInteger(0)
        val closeLeaseCalls = AtomicInteger(0)
        val closeViewportCalls = AtomicInteger(0)
        val events = mutableListOf<String>()

        val controller = BundleLeaseController(
            leaseId = "lease-42",
            renewAction = { id ->
                assertEquals("lease-42", id)
                renewCalls.incrementAndGet()
                renewLatch.countDown()
            },
            closeLeaseAction = { id ->
                assertEquals("lease-42", id)
                closeLeaseCalls.incrementAndGet()
                synchronized(events) { events.add("close_lease") }
            },
            closeViewportAction = {
                closeViewportCalls.incrementAndGet()
                synchronized(events) { events.add("close_viewport") }
            },
            heartbeatInterval = Duration.ofMillis(20),
        )

        assertTrue(renewLatch.await(5, TimeUnit.SECONDS), "expected at least 2 lease_renew heartbeats")
        // The latch fires inside renewAction, before the controller increments its success counter,
        // so wait for the counter itself instead of racing the heartbeat thread.
        val deadlineNs = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
        while (controller.successfulRenewals < 2 && System.nanoTime() < deadlineNs) {
            Thread.sleep(5)
        }
        assertTrue(controller.successfulRenewals >= 2, "expected at least 2 successful renewals")

        // Closing twice must be idempotent and release viewport before lease.
        controller.close()
        controller.close()

        assertTrue(controller.isClosed)
        assertEquals(1, closeViewportCalls.get())
        assertEquals(1, closeLeaseCalls.get())
        assertEquals(listOf("close_viewport", "close_lease"), events)

        val renewalsAtClose = renewCalls.get()
        Thread.sleep(60)
        assertEquals(renewalsAtClose, renewCalls.get(), "no heartbeats should fire after tab close")
    }

    @Test
    fun closeWaitsForInFlightHeartbeatBeforeReleasingLease() {
        val renewStarted = CountDownLatch(1)
        val releaseRenew = CountDownLatch(1)
        val events = mutableListOf<String>()
        val renewCalls = AtomicInteger(0)

        val controller = BundleLeaseController(
            leaseId = "lease-inflight",
            renewAction = {
                renewCalls.incrementAndGet()
                synchronized(events) { events.add("renew_start") }
                renewStarted.countDown()
                assertTrue(releaseRenew.await(5, TimeUnit.SECONDS), "test must unblock renew")
                synchronized(events) { events.add("renew_end") }
            },
            closeLeaseAction = {
                synchronized(events) { events.add("close_lease") }
            },
            closeViewportAction = {
                synchronized(events) { events.add("close_viewport") }
            },
            heartbeatInterval = Duration.ofMillis(10),
        )

        assertTrue(renewStarted.await(5, TimeUnit.SECONDS), "first heartbeat must enter renewAction")
        val closeThread = Thread { controller.close() }.apply { start() }
        // Give close() time to block on the in-flight heartbeat, then verify it has not closed yet.
        Thread.sleep(40)
        synchronized(events) {
            assertEquals(listOf("renew_start"), events, "close() must not release lease while renew is in flight")
        }

        releaseRenew.countDown()
        closeThread.join(5_000)
        assertTrue(!closeThread.isAlive, "close() must finish once in-flight heartbeat completes")

        synchronized(events) {
            assertEquals(
                listOf("renew_start", "renew_end", "close_viewport", "close_lease"),
                events,
                "close must serialize after the in-flight heartbeat and run viewport before lease",
            )
        }

        val renewalsAtClose = renewCalls.get()
        Thread.sleep(60)
        assertEquals(renewalsAtClose, renewCalls.get(), "no additional heartbeats may fire after close")
    }
}
