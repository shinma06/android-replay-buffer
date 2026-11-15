package io.github.shinma06.replaybuffer.menubar

import com.android.tools.idea.gradle.project.sync.GradleSyncState
import com.intellij.mock.MockApplication
import com.intellij.mock.MockProject
import com.intellij.notification.NotificationDisplayType
import com.intellij.notification.NotificationGroup
import com.intellij.notification.NotificationGroupManager
import com.intellij.openapi.Disposable
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.application.CoroutineSupport
import com.intellij.openapi.application.ModalityState
import com.intellij.openapi.application.impl.PlatformCoroutineSupport
import com.intellij.openapi.extensions.PluginId
import com.intellij.openapi.project.ProjectManager
import com.intellij.openapi.util.Disposer
import com.intellij.util.messages.MessageBusConnection
import io.github.shinma06.replaybuffer.ide.ReplayProjectService
import io.github.shinma06.replaybuffer.settings.ReplaySettingsStore
import io.github.shinma06.replaybuffer.settings.ReplaySettingsConfigurable
import java.lang.reflect.Proxy
import java.util.concurrent.CompletableFuture
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import javax.swing.SwingUtilities
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.SupervisorJob
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class ReplayMenuBarServiceThreadingTest {
    @Test
    fun `project subscriptions end on project untracking or service unload`() {
        listOf(false, true).forEach { unload ->
            Fixture().use { fixture ->
                val project = fixture.project("open")
                val bus = project.messageBus
                fun connections(): Set<*> = (bus.javaClass.getField("subscribers").get(bus) as Collection<*>).toSet()
                val before = connections()
                fixture.track(project)
                val connection = (connections() - before).single() as MessageBusConnection
                assertFalse(Disposer.isDisposed(connection))
                SwingUtilities.invokeAndWait {
                    if (unload) Disposer.dispose(fixture.service)
                    else fixture.application.messageBus.syncPublisher(ProjectManager.TOPIC).projectClosing(project)
                }
                assertFalse(project.isDisposed, "The project must still be open")
                assertTrue(Disposer.isDisposed(connection), "The menu must release its project subscriptions")
            }
        }
    }

    @Test
    fun `open settings receive asynchronous registration results and ignore them after UI disposal`() {
        Fixture().use { fixture ->
            val project = fixture.project("settings")
            val configurable = ReplaySettingsConfigurable(project)
            fixture.application.registerService(ReplayMenuBarService::class.java, fixture.service)
            SwingUtilities.invokeAndWait {
                val form = configurable.createForm()
                form.menuBarReason = "古い表示"
                fixture.service.projectOpened(project)
                fixture.drainRefreshes()
                assertEquals(fixture.service.availabilityReason(), form.menuBarReason, "Rendering must publish its availability")
                fixture.application.messageBus.syncPublisher(ReplayMenuBarService.AVAILABILITY_CHANGED).changed("登録失敗")
                fixture.drainRefreshes()
                assertEquals("登録失敗", form.menuBarReason)
                fixture.application.messageBus.syncPublisher(ReplayMenuBarService.AVAILABILITY_CHANGED).changed(null)
                fixture.drainRefreshes()
                assertNull(form.menuBarReason, "Successful registration clears the reason without reopening settings")
                fixture.application.messageBus.syncPublisher(ReplayMenuBarService.AVAILABILITY_CHANGED).changed("遅れた結果")
                configurable.disposeUIResources()
                fixture.drainRefreshes()
                assertNull(form.menuBarReason, "A queued result must not mutate disposed UI")
            }
        }
    }

    @Test
    fun `queued registration rechecks project disposal on the EDT`() {
        Fixture().use { fixture ->
            val closing = fixture.project("closing")
            SwingUtilities.invokeAndWait {
                CompletableFuture.runAsync { fixture.service.projectOpened(closing) }.get(10, TimeUnit.SECONDS)
                Disposer.dispose(closing)
            }
            SwingUtilities.invokeAndWait {}
            assertTrue(fixture.tracked().isEmpty())
            assertTrue(fixture.callbacks.isEmpty())
        }
    }

    @Test
    fun `background project registration cannot change the map during EDT iteration`() {
        Fixture().use { fixture ->
            val first = fixture.project("first")
            val second = fixture.project("second")
            val added = fixture.project("added")
            fixture.track(first, second)
            var whileRendering = emptyList<Any>()
            fixture.duringRender(first) {
                CompletableFuture.runAsync {
                    fixture.service.projectOpened(added)
                    fixture.service.projectOpened(added)
                }.get(10, TimeUnit.SECONDS)
                whileRendering = fixture.tracked()
            }
            assertEquals(listOf(first, second), whileRendering)
            assertEquals(listOf(first, second, added), fixture.tracked())
        }
    }

    @Test
    fun `project closing cannot remove entries during EDT iteration`() {
        Fixture().use { fixture ->
            val first = fixture.project("first")
            val closing = fixture.project("closing")
            fixture.track(first, closing)
            var whileRendering = emptyList<Any>()
            fixture.duringRender(first) {
                CompletableFuture.runAsync {
                    fixture.application.messageBus.syncPublisher(ProjectManager.TOPIC).projectClosing(closing)
                }.get(10, TimeUnit.SECONDS)
                whileRendering = fixture.tracked()
            }
            assertEquals(listOf(first, closing), whileRendering)
            assertEquals(listOf(first), fixture.tracked())
        }
    }

    @Test
    fun `disposal waits for EDT iteration and cancels pending registration and refresh`() {
        Fixture().use { fixture ->
            val first = fixture.project("first")
            val second = fixture.project("second")
            val added = fixture.project("added")
            fixture.track(first, second)
            var whileRendering = emptyList<Any>()
            fixture.duringRender(first) {
                CompletableFuture.runAsync {
                    fixture.service.projectOpened(added)
                    fixture.service.refresh()
                    fixture.service.dispose()
                    fixture.service.projectOpened(added)
                }.get(10, TimeUnit.SECONDS)
                whileRendering = fixture.tracked()
            }
            assertEquals(listOf(first, second), whileRendering)
            assertTrue(fixture.tracked().isEmpty())
            fixture.drainRefreshes()
            fixture.service.refresh()
            assertTrue(fixture.callbacks.isEmpty())
            assertTrue(fixture.tracked().isEmpty())
        }
    }

    private class ProbeProject(private val label: String, lifetime: Disposable) : MockProject(null, lifetime) {
        @Volatile var disposedRead: Runnable? = null
        override fun getName(): String = label
        override fun isDisposed(): Boolean {
            disposedRead?.run()
            return super.isDisposed()
        }
    }

    private class Fixture : AutoCloseable {
        private val lifetime = Disposer.newDisposable("menu bar threading test")
        private val job = SupervisorJob()
        val callbacks = ConcurrentLinkedQueue<Runnable>()
        val application = object : MockApplication(lifetime) {
            override fun invokeLater(runnable: Runnable, state: ModalityState) { callbacks.add(runnable) }
        }
        val service: ReplayMenuBarService

        init {
            ApplicationManager.setApplication(application, lifetime)
            application.registerService(CoroutineScope::class.java, CoroutineScope(job))
            application.registerService(CoroutineSupport::class.java, PlatformCoroutineSupport())
            application.registerService(ReplayMenuBarStore::class.java, ReplayMenuBarStore())
            application.registerService(NotificationGroupManager::class.java, object : NotificationGroupManager {
                override fun getNotificationGroup(id: String): NotificationGroup = NotificationGroup.create(
                    id, NotificationDisplayType.NONE, false, null, null, PluginId.getId("com.intellij"))
                override fun isGroupRegistered(id: String): Boolean = false
                override fun getRegisteredNotificationGroups(): Collection<NotificationGroup> = emptyList()
                override fun isRegisteredNotificationId(id: String): Boolean = false
            })
            service = ReplayMenuBarService()
            Disposer.register(lifetime, service)
        }

        fun project(name: String): ProbeProject {
            val project = ProbeProject(name, lifetime)
            project.registerService(ReplaySettingsStore::class.java, ReplaySettingsStore())
            project.registerService(GradleSyncState::class.java, Proxy.newProxyInstance(
                GradleSyncState::class.java.classLoader, arrayOf(GradleSyncState::class.java),
            ) { _, method, args ->
                check(method.name == "subscribe") { "Unexpected Gradle operation: ${method.name}" }
                project.messageBus.connect(args[2] as Disposable)
            } as GradleSyncState)
            // Real service and inputs, but no SDK resolution, capture worker or CLI endpoint.
            val scope = CoroutineScope(SupervisorJob().apply { cancel() })
            val replay = ReplayProjectService(project, scope)
            project.registerService(ReplayProjectService::class.java, replay)
            Disposer.register(lifetime, replay)
            return project
        }

        fun track(vararg projects: ProbeProject) {
            SwingUtilities.invokeAndWait { projects.forEach(service::projectOpened) }
            assertEquals(1, callbacks.size, "Refreshes must coalesce until the EDT renders")
        }

        fun tracked(): List<Any> {
            val field = ReplayMenuBarService::class.java.getDeclaredField("projects").apply { isAccessible = true }
            return (field.get(service) as Map<*, *>).keys.filterNotNull()
        }

        fun duringRender(project: ProbeProject, whileBlocked: () -> Unit) {
            val entered = CountDownLatch(1)
            val resume = CountDownLatch(1)
            val finished = CountDownLatch(1)
            val once = AtomicBoolean(false)
            val failure = AtomicReference<Throwable>()
            project.disposedRead = Runnable {
                if (SwingUtilities.isEventDispatchThread() && once.compareAndSet(false, true)) {
                    entered.countDown()
                    check(resume.await(10, TimeUnit.SECONDS)) { "Project mutation did not finish" }
                }
            }
            val render = checkNotNull(callbacks.poll())
            SwingUtilities.invokeLater {
                try { render.run() } catch (error: Throwable) { failure.set(error) } finally { finished.countDown() }
            }
            try {
                assertTrue(entered.await(10, TimeUnit.SECONDS), "EDT did not start project iteration")
                whileBlocked()
            } finally {
                resume.countDown()
                assertTrue(finished.await(10, TimeUnit.SECONDS), "EDT render did not finish")
                project.disposedRead = null
                SwingUtilities.invokeAndWait {}
            }
            assertNull(failure.get(), "Background lifecycle events must not invalidate EDT iteration")
        }

        fun drainRefreshes() {
            val drain = { while (true) (callbacks.poll() ?: break).run() }
            if (SwingUtilities.isEventDispatchThread()) drain() else SwingUtilities.invokeAndWait(drain)
        }

        override fun close() {
            SwingUtilities.invokeAndWait { Disposer.dispose(lifetime) }
            job.cancel()
        }
    }
}
