package io.github.shinma06.replaybuffer.menubar

import com.intellij.openapi.Disposable
import com.intellij.openapi.util.Disposer
import java.util.concurrent.CompletableFuture
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import javax.swing.SwingUtilities
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertSame
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class MenuBarPostEventListenerTest {
    @Test
    fun `disposal before SDK cleanup registration rolls back the hook`() = disposalWins(beforeCleanup = true)

    @Test
    fun `disposal after SDK acquisition but before parent attachment rolls back the hook`() = disposalWins(beforeCleanup = false)

    private fun disposalWins(beforeCleanup: Boolean) {
        Hook().use { hook ->
            val entered = CountDownLatch(1)
            val resume = CountDownLatch(1)
            val registration = onEdt {
                registerMenuBarPostEventListener(hook.parent) { owner ->
                    hook.acquire(owner) { cleanupRegistered ->
                        if (cleanupRegistered != beforeCleanup) {
                            entered.countDown()
                            check(resume.await(10, TimeUnit.SECONDS)) { "Background disposal did not finish" }
                        }
                    }
                }
            }
            try {
                assertTrue(entered.await(10, TimeUnit.SECONDS), "EDT did not reach SDK acquisition")
                assertFalse(SwingUtilities.isEventDispatchThread())
                Disposer.dispose(hook.parent)
                assertEquals(1, hook.listeners.size, "The unpublished hook must survive until acquisition completes")
            } finally {
                resume.countDown()
            }
            var failure: Throwable? = null
            val attached = try { registration.get(10, TimeUnit.SECONDS) } catch (error: Exception) {
                failure = error
                null
            }
            assertTrue(hook.listeners.isEmpty(), "Losing registration must release the SDK hook")
            assertEquals(1, hook.removals.get(), "Cleanup must run exactly once")
            assertTrue(Disposer.isDisposed(checkNotNull(hook.owner)))
            assertNull(failure, "Losing registration must not raise a disposed-parent error")
            assertEquals(false, attached)
        }
    }

    @Test
    fun `parent attachment before background disposal retains then releases the hook once`() {
        Hook().use { hook ->
            assertTrue(onEdt { registerMenuBarPostEventListener(hook.parent) { hook.acquire(it) } }.get(10, TimeUnit.SECONDS))
            assertEquals(1, hook.listeners.size)
            assertFalse(Disposer.isDisposed(checkNotNull(hook.owner)))
            assertEquals(0, hook.removals.get())
            Disposer.dispose(hook.parent)
            assertTrue(hook.listeners.isEmpty())
            assertTrue(Disposer.isDisposed(checkNotNull(hook.owner)))
            Disposer.dispose(hook.parent)
            assertEquals(1, hook.removals.get())
        }
    }

    @Test
    fun `failed acquisition releases its cleanup without disposing the service`() {
        Hook().use { hook ->
            val expected = IllegalStateException("acquisition failed")
            onEdt {
                assertSame(expected, assertThrows(IllegalStateException::class.java) {
                    registerMenuBarPostEventListener(hook.parent) {
                        hook.acquire(it)
                        throw expected
                    }
                })
            }.get(10, TimeUnit.SECONDS)
            assertTrue(hook.listeners.isEmpty())
            assertEquals(1, hook.removals.get())
            assertTrue(Disposer.isDisposed(checkNotNull(hook.owner)))
            assertFalse(Disposer.isDisposed(hook.parent))
        }
    }

    private fun <T> onEdt(action: () -> T): CompletableFuture<T> = CompletableFuture<T>().also { result ->
        SwingUtilities.invokeLater {
            try { result.complete(action()) } catch (error: Throwable) { result.completeExceptionally(error) }
        }
    }

    private class Hook : AutoCloseable {
        val parent = Disposer.newDisposable("menu bar hook test")
        val listeners = CopyOnWriteArrayList<Any>()
        val removals = AtomicInteger()
        var owner: Disposable? = null

        fun acquire(owner: Disposable, step: (cleanupRegistered: Boolean) -> Unit = {}) {
            assertTrue(SwingUtilities.isEventDispatchThread(), "SDK acquisition must stay on the EDT")
            this.owner = owner
            // AI-262.10968.63.2622.16730706 IdeEventQueue.addPostEventListener bytecode:
            // List.add(listener), then Disposer.register(owner, { List.remove(listener) }).
            val listener = Any()
            listeners.add(listener)
            step(false)
            Disposer.register(owner, Disposable {
                assertTrue(listeners.remove(listener))
                removals.incrementAndGet()
            })
            step(true)
        }

        override fun close() {
            Disposer.dispose(parent)
            owner?.let(Disposer::dispose)
        }
    }
}
