package io.github.shinma06.replaybuffer.ui

import com.intellij.mock.MockApplication
import com.intellij.mock.MockProject
import com.intellij.openapi.actionSystem.ActionGroup
import com.intellij.openapi.actionSystem.ActionManager
import com.intellij.openapi.actionSystem.ActionPopupMenu
import com.intellij.openapi.actionSystem.ActionToolbar
import com.intellij.openapi.actionSystem.AnAction
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.actionSystem.DefaultActionGroup
import com.intellij.openapi.actionSystem.KeyboardShortcut
import com.intellij.openapi.actionSystem.TimerListener
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.application.ModalityState
import com.intellij.openapi.components.Service
import com.intellij.openapi.extensions.PluginId
import com.intellij.openapi.util.ActionCallback
import com.intellij.openapi.util.Disposer
import com.intellij.openapi.wm.IdeFocusManager
import com.intellij.openapi.wm.RegisterToolWindowTask
import com.intellij.openapi.wm.ToolWindow
import com.intellij.openapi.wm.ToolWindowBalloonShowOptions
import com.intellij.openapi.wm.ToolWindowManager
import com.intellij.openapi.wm.ex.ToolWindowEx
import com.intellij.openapi.ui.popup.Balloon
import com.intellij.ui.content.Content
import com.intellij.ui.content.ContentManager
import com.intellij.ui.content.ContentManagerEvent
import com.intellij.ui.content.ContentManagerListener
import java.awt.Component
import java.awt.Container
import java.awt.event.ContainerEvent
import java.awt.event.ContainerAdapter
import java.awt.event.InputEvent
import java.lang.reflect.Proxy
import java.util.IdentityHashMap
import java.util.concurrent.CompletableFuture
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import javax.swing.SwingUtilities
import javax.swing.JPanel
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertSame
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class LogcatTitleActionsTest {
    @Test
    fun `an already registered Logcat gets one action and preserves existing plugins`() {
        Fixture().use { f ->
            val other = action("other plugin")
            f.header!!.add(other)
            repeat(3) { f.binding.refresh() }
            f.drain()
            assertEquals(listOf(f.save, other), f.children())
            f.binding.refresh()
            f.drain()
            assertEquals(listOf(f.save, other), f.children())
            assertEquals(1, f.toolbar().updates)
            Disposer.dispose(f.owner)
            assertTrue(f.projectOpen.get(), "Unloading must detach even while the project stays open")
            assertEquals(listOf(other), f.children())
        }
    }

    @Test
    fun `late Logcat registration and replacement headers are handled without retaining old actions`() {
        Fixture().use { f ->
            val old = f.header!!
            f.header = null
            f.drain()
            assertTrue(f.children(old).isEmpty())
            f.header = old
            f.binding.refresh()
            f.drain()
            assertEquals(listOf(f.save), f.children(old))
            val replacement = DefaultActionGroup()
            val other = action("replacement owner")
            replacement.add(other)
            f.header = replacement
            f.binding.refresh()
            f.drain()
            assertTrue(f.children(old).isEmpty())
            assertEquals(listOf(f.save, other), f.children(replacement))
            Disposer.dispose(f.owner)
            assertEquals(listOf(other), f.children(replacement))
        }
    }

    @Test
    fun `unload removes the installed old instance not the new action with the same ID`() {
        Fixture().use { f ->
            val other = action("other plugin")
            f.header!!.add(other)
            f.drain()
            val old = f.save
            f.manager.unregisterAction(LOGCAT_SAVE_LOGS_ACTION_ID)
            val replacement = action("replacement with same ID")
            f.manager.registerAction(LOGCAT_SAVE_LOGS_ACTION_ID, replacement)
            f.header!!.add(replacement)
            val lookups = f.manager.lookups
            val idLookups = f.manager.idLookups
            Disposer.dispose(f.owner)
            assertEquals(lookups, f.manager.lookups, "Cleanup must not resolve the now-reused action ID")
            assertEquals(idLookups, f.manager.idLookups, "Cleanup must not ask for the old action's ID either")
            assertEquals(listOf(other, replacement), f.children())
            assertSame(replacement, f.manager.getAction(LOGCAT_SAVE_LOGS_ACTION_ID))
            assertFalse(f.children().any { it === old })
        }
    }

    @Test
    fun `unregister before unload and repeated disposal preserve later foreign edits`() {
        Fixture().use { f ->
            f.drain()
            val other = action("added after installation")
            f.header!!.add(other)
            f.manager.unregisterAction(LOGCAT_SAVE_LOGS_ACTION_ID)
            Disposer.dispose(f.owner)
            Disposer.dispose(f.owner)
            assertEquals(listOf(other), f.children())
        }
    }

    @Test
    fun `the own action already copied into the header is adopted for cleanup`() {
        Fixture().use { f ->
            f.header!!.add(f.save)
            f.drain()
            Disposer.dispose(f.owner)
            assertTrue(f.children().isEmpty())
            assertEquals(1, f.toolbar().resets)
        }
    }

    @Test
    fun `queued and later registration callbacks cannot reinstall after unload or project close`() {
        listOf(false, true).forEach { closeProject ->
            Fixture().use { f ->
                f.binding.refresh()
                if (closeProject) f.projectOpen.set(false)
                Disposer.dispose(f.owner)
                f.binding.refresh()
                f.drain()
                assertTrue(f.children().isEmpty())
                assertEquals(0, f.manager.lookups, "Disposed callbacks must not even load the action")
            }
        }
    }

    @Test
    fun `disposal during resolution is rechecked before mutating the header`() {
        Fixture().use { f ->
            f.onFindGroup = { Disposer.dispose(f.owner) }
            f.drain()
            assertTrue(f.children().isEmpty())
        }
    }

    @Test
    fun `concurrent dispose waits for installation and removes it before returning`() {
        Fixture().use { f ->
            val entered = CountDownLatch(1)
            val resume = CountDownLatch(1)
            val disposing = CountDownLatch(1)
            val rendered = CompletableFuture<Void>()
            f.onFindGroup = {
                entered.countDown()
                check(resume.await(10, TimeUnit.SECONDS))
            }
            SwingUtilities.invokeLater {
                try { f.drain(); rendered.complete(null) } catch (failure: Throwable) { rendered.completeExceptionally(failure) }
            }
            assertTrue(entered.await(10, TimeUnit.SECONDS))
            val disposed = CompletableFuture.runAsync {
                disposing.countDown()
                Disposer.dispose(f.owner)
            }
            try {
                assertTrue(disposing.await(10, TimeUnit.SECONDS))
            } finally { resume.countDown() }
            rendered.get(10, TimeUnit.SECONDS)
            disposed.get(10, TimeUnit.SECONDS)
            assertTrue(f.children().isEmpty(), "No async removal may remain after dispose returns")
            f.binding.refresh()
            f.drain()
            assertTrue(f.children().isEmpty())
        }
    }

    @Test
    fun `service uses plugin project lifetime and its pending startup callback expires on disposal`() {
        Fixture().use { f ->
            val serviceAnnotation = LogcatTitleActionService::class.java.getAnnotation(Service::class.java)
            assertEquals(listOf(Service.Level.PROJECT), serviceAnnotation.value.toList())
            val project = MockProject(null, f.lifetime)
            val manager = TestToolWindowManager(f.callbacks)
            project.registerService(ToolWindowManager::class.java, manager)
            val before = f.callbacks.size
            val service = LogcatTitleActionService(project)
            project.registerService(LogcatTitleActionService::class.java, service)
            Disposer.register(f.lifetime, service)
            assertEquals(before + 1, f.callbacks.size, "Startup must inspect already registered tool windows")
            val listener = LogcatTitleActions(project)
            listener.toolWindowsRegistered(listOf("unrelated"), manager)
            assertEquals(before + 1, f.callbacks.size)
            listener.toolWindowsRegistered(listOf(LOGCAT_TOOL_WINDOW_ID), manager)
            assertEquals(before + 1, f.callbacks.size)
            Disposer.dispose(service)
            assertFalse(project.isDisposed)
            listener.toolWindowsRegistered(listOf(LOGCAT_TOOL_WINDOW_ID), manager)
            assertEquals(before + 1, f.callbacks.size, "Delayed delivery must not enqueue work for a disposed service")
            f.drain()
        }
    }

    @Test
    fun `unload and close synchronously clear existing toolbar caches without looking up a new header`() {
        listOf(false, true).forEach { closeProject ->
            Fixture().use { f ->
                val other = action("other plugin")
                f.header!!.add(other)
                f.drain()
                val toolbar = f.toolbar()
                toolbar.visible = f.children()
                assertTrue(toolbar.visible.any { it === f.save })
                f.onFindGroup = { error("Disposal must never initialize or look up a header") }
                if (closeProject) f.projectOpen.set(false)
                Disposer.dispose(f.owner)
                assertEquals(1, toolbar.resets)
                assertTrue(toolbar.visible.isEmpty(), "Do not wait for async refresh to release old action references")
                assertEquals(listOf(other), toolbar.pending)
                assertEquals(listOf(other), f.children())
                Disposer.dispose(f.owner)
                assertEquals(1, toolbar.resets)
            }
        }
    }

    @Test
    fun `unsupported missing or disposed SDK decorators never replace other title actions`() {
        val manager = Proxy.newProxyInstance(ContentManager::class.java.classLoader, arrayOf(ContentManager::class.java)) { _, _, _ ->
            error("Only the non-lazy ToolWindow accessor may be used")
        } as ContentManager
        fun window(api: Class<out ToolWindow>, disposed: Boolean = false, created: Boolean = true, initialize: () -> Unit = {}, decorator: () -> Any? = { null }): ToolWindow =
            Proxy.newProxyInstance(api.classLoader, arrayOf(api)) { _, method, _ ->
                when (method.name) {
                    "isDisposed" -> disposed
                    "getContentManagerIfCreated" -> if (created) manager else null
                    "getComponent" -> { initialize(); JPanel() }
                    "getDecorator" -> decorator()
                    else -> error("Unsupported SDK must not call ${method.name}")
                }
            } as ToolWindow
        assertTrue(logcatHeaderTargets(null).isEmpty())
        assertTrue(logcatHeaderTargets(window(ToolWindow::class.java)).isEmpty())
        assertTrue(logcatHeaderTargets(window(ToolWindowEx::class.java)).isEmpty())
        assertTrue(logcatHeaderTargets(window(ToolWindowEx::class.java, disposed = true) { error("Disposed decorator accessed") }).isEmpty())
        assertTrue(logcatHeaderTargets(window(ToolWindowEx::class.java, created = false,
            initialize = { error("Uncreated UI must not be initialized") }) { error("Uncreated decorator accessed") }).isEmpty())
        assertTrue(logcatHeaderTargets(window(ToolWindowEx::class.java) { throw NoSuchMethodError("SDK boundary changed") }).isEmpty())
        var initialized = false
        assertTrue(logcatHeaderTargets(window(ToolWindowEx::class.java, initialize = { initialized = true }) {
            check(initialized) { "Fixed SDK decorator getter requires initialization first" }
            null
        }).isEmpty())
        assertTrue(initialized)
    }

    @Test
    fun `uncreated service refresh and early shown never initialize UI or add pending listeners`() {
        ServiceFixture(false).use { f ->
            f.observer.toolWindowsRegistered(listOf("unrelated"), f.toolWindows)
            repeat(3) { f.registered(); f.shown() }
            assertEquals(1, f.callbacks.size)
            f.drain()
            assertFalse(f.created)
            assertEquals(0, f.componentReads)
            assertEquals(0, f.decoratorReads)
            assertEquals(0, f.manager.adds)
            assertEquals(0, f.pendingListeners)
            assertEquals(listOf(f.manager.foreign), f.manager.listeners)
        }
    }

    @Test
    fun `shown schedules the real service after an uncreated startup without new registration`() {
        ServiceFixture(false).use { f ->
            f.drain()
            assertEquals(0, f.componentReads)
            f.created = true
            repeat(3) { f.shown() }
            assertEquals(1, f.callbacks.size)
            f.drain()
            assertEquals(1, f.componentReads)
            assertEquals(1, f.decoratorReads)
            assertEquals(1, f.manager.adds)
            assertEquals(0, f.pendingListeners)
        }
    }

    @Test
    fun `existing manager events coalesce and replacement removes only the owned listener`() {
        ServiceFixture(true).use { f ->
            f.drain()
            val old = f.manager
            assertEquals(1, old.adds)
            assertEquals(2, old.listeners.size)
            old.added(); old.removed()
            assertEquals(1, f.callbacks.size)
            f.drain()
            assertEquals(1, old.adds)
            f.manager = ManagerSpy()
            f.service.refresh(); f.drain()
            assertEquals(listOf(old.foreign), old.listeners)
            assertEquals(1, old.removes)
            assertEquals(1, f.manager.adds)
            assertEquals(2, f.manager.listeners.size)
            assertEquals(0, f.pendingListeners)
        }
    }

    @Test
    fun `service disposal expires startup and later observer callbacks without touching UI`() {
        listOf(false, true).forEach { created ->
            ServiceFixture(created).use { f ->
                Disposer.dispose(f.service)
                assertFalse(f.project.isDisposed)
                val queued = f.callbacks.size
                f.registered(); f.shown(); f.service.refresh()
                assertEquals(queued, f.callbacks.size)
                f.drain()
                assertEquals(0, f.componentReads)
                assertEquals(0, f.decoratorReads)
                assertEquals(0, f.manager.adds)
            }
        }
    }

    @Test
    fun `off EDT service disposal releases manager ownership before returning and stale callbacks expire`() {
        ServiceFixture(true).use { f ->
            f.drain()
            val manager = f.manager
            val saved = manager.listeners.filter { it !== manager.foreign }
            assertEquals(1, saved.size)
            val event = manager.event()
            f.service.refresh()
            val reads = f.componentReads
            assertFalse(SwingUtilities.isEventDispatchThread())
            Disposer.dispose(f.service)
            assertEquals(listOf(manager.foreign), manager.listeners)
            assertEquals(1, manager.removes)
            val queued = f.callbacks.size
            saved.forEach { it.contentAdded(event); it.contentRemoved(event) }
            f.registered(); f.shown(); f.service.refresh()
            assertEquals(queued, f.callbacks.size)
            f.drain()
            assertEquals(reads, f.componentReads)
            Disposer.dispose(f.service)
            assertEquals(1, manager.removes)
        }
    }

    @Test
    fun `disposed and missing windows release the created manager without eager lookups`() {
        ServiceFixture(true).use { f ->
            f.drain()
            assertEquals(1, f.manager.adds)
            val reads = f.componentReads
            f.windowDisposed = true
            f.service.refresh(); f.drain()
            assertEquals(reads, f.componentReads)
            assertEquals(listOf(f.manager.foreign), f.manager.listeners)
            assertEquals(1, f.manager.removes)
            f.windowDisposed = false
            f.toolWindows.window = null
            f.service.refresh(); f.drain()
            assertEquals(reads, f.componentReads)
            assertEquals(1, f.manager.removes)
        }
    }

    @Test
    fun `late root has one action and disposal preserves foreign container listeners with queued work`() {
        Fixture().use { f ->
            f.header = null
            f.drain()
            val foreign = action("foreign root action")
            f.header = DefaultActionGroup().apply { add(foreign) }
            f.binding.refresh(); f.drain()
            assertEquals(listOf(f.save, foreign), f.children())
            val toolbar = f.toolbar()
            val foreignListener = object : ContainerAdapter() {}
            toolbar.container.addContainerListener(foreignListener)
            val saved = toolbar.container.containerListeners.toList()
            toolbar.visible = f.children()
            f.binding.refresh(); f.drain()
            assertEquals(listOf(f.save, foreign), f.children())
            f.binding.refresh()
            Disposer.dispose(f.owner)
            assertEquals(listOf(foreignListener), toolbar.container.containerListeners.toList())
            assertEquals(listOf(foreign), f.children())
            assertTrue(toolbar.visible.isEmpty())
            assertEquals(1, toolbar.resets)
            val queued = f.callbacks.size
            saved.forEach { it.componentAdded(ContainerEvent(toolbar.container, ContainerEvent.COMPONENT_ADDED, JPanel())) }
            assertEquals(queued, f.callbacks.size)
            f.drain()
            assertEquals(listOf(foreign), f.children())
        }
    }

    @Test
    fun `existing and nested split headers preserve foreign order and same ID replacements on unload`() {
        Fixture().use { f ->
            val groups = List(5) { DefaultActionGroup() }
            val all = listOf(f.header!!) + groups
            val foreign = all.associateWith { listOf(action("first"), action("last")) }
            foreign.forEach { (group, actions) -> actions.forEach(group::add) }
            SwingUtilities.invokeAndWait {
                f.split(f.header!!, groups[0], groups[1])
                f.split(groups[0], groups[2], groups[3])
                f.split(groups[3], groups[4])
            }
            f.drain()
            all.forEach { group ->
                assertEquals(listOf(f.save) + foreign.getValue(group), f.children(group))
                assertEquals(1, f.toolbar(group).updates)
                f.toolbar(group).visible = f.children(group)
            }
            val replacement = action("same ID new instance")
            f.manager.registerAction(LOGCAT_SAVE_LOGS_ACTION_ID, replacement)
            all.forEach { it.add(replacement) }
            val lookups = f.manager.lookups
            val idLookups = f.manager.idLookups
            Disposer.dispose(f.owner)
            assertEquals(lookups, f.manager.lookups)
            assertEquals(idLookups, f.manager.idLookups)
            all.forEach { group ->
                val toolbar = f.toolbar(group)
                assertEquals(foreign.getValue(group) + replacement, f.children(group))
                assertTrue(toolbar.visible.isEmpty())
                assertEquals(f.children(group), toolbar.pending)
                assertEquals(1, toolbar.resets)
                assertTrue(toolbar.container.containerListeners.isEmpty())
            }
        }
    }

    @Test
    fun `late split nested split join and reattachment have one action and release removed subtrees immediately`() {
        Fixture().use { f ->
            f.drain()
            val child = DefaultActionGroup()
            val nested = DefaultActionGroup()
            val foreign = action("child only")
            child.add(foreign)
            lateinit var splitter: JPanel
            lateinit var inner: JPanel
            SwingUtilities.invokeAndWait { splitter = f.split(f.header!!, child) }
            f.drain()
            assertEquals(listOf(f.save, foreign), f.children(child))
            SwingUtilities.invokeAndWait { inner = f.split(child, nested) }
            f.drain()
            assertEquals(listOf(f.save), f.children(nested))
            f.toolbar(child).visible = f.children(child)
            f.toolbar(nested).visible = f.children(nested)
            SwingUtilities.invokeAndWait { splitter.remove(f.toolbar(child).container) }
            assertEquals(listOf(foreign), f.children(child))
            assertTrue(f.children(nested).isEmpty())
            assertTrue(f.toolbar(child).visible.isEmpty())
            assertTrue(f.toolbar(nested).visible.isEmpty())
            assertTrue(inner.containerListeners.isEmpty())
            assertTrue(f.toolbar(child).container.containerListeners.isEmpty())
            assertTrue(f.toolbar(nested).container.containerListeners.isEmpty())
            f.drain()
            assertEquals(1, splitter.containerListeners.size, "An empty live split slot must still observe reattachment")
            SwingUtilities.invokeAndWait { splitter.add(f.toolbar(child).container) }
            f.drain()
            assertEquals(listOf(f.save, foreign), f.children(child))
            assertEquals(listOf(f.save), f.children(nested))
            SwingUtilities.invokeAndWait { f.toolbar(f.header!!).container.remove(splitter) }
            assertEquals(listOf(foreign), f.children(child))
            assertTrue(f.children(nested).isEmpty())
            assertTrue(splitter.containerListeners.isEmpty())
            assertTrue(inner.containerListeners.isEmpty())
            f.drain()
            assertEquals(listOf(f.save), f.children())
            Disposer.dispose(f.owner)
            assertTrue(f.children().isEmpty())
        }
    }

    @Test
    fun `already dispatched container events and pending split work cannot reattach after close or unload`() {
        listOf(false, true).forEach { closeProject ->
            Fixture().use { f ->
                f.drain()
                val child = DefaultActionGroup()
                lateinit var splitter: JPanel
                SwingUtilities.invokeAndWait { splitter = f.split(f.header!!, child) }
                f.drain()
                val root = f.toolbar().container
                val listeners = (root.containerListeners + splitter.containerListeners + f.toolbar(child).container.containerListeners).toList()
                f.binding.refresh()
                f.onFindGroup = { error("Expired work must not look up or generate UI") }
                if (closeProject) f.projectOpen.set(false)
                Disposer.dispose(f.owner)
                val queued = f.callbacks.size
                SwingUtilities.invokeAndWait {
                    listeners.forEach {
                        it.componentAdded(ContainerEvent(root, ContainerEvent.COMPONENT_ADDED, splitter))
                        it.componentRemoved(ContainerEvent(root, ContainerEvent.COMPONENT_REMOVED, splitter))
                    }
                }
                assertEquals(queued, f.callbacks.size)
                f.drain()
                assertTrue(f.children().isEmpty())
                assertTrue(f.children(child).isEmpty())
                assertTrue(root.containerListeners.isEmpty())
                assertTrue(splitter.containerListeners.isEmpty())
                assertTrue(f.toolbar(child).container.containerListeners.isEmpty())
                assertTrue(f.toolbar().pending.isEmpty())
                assertTrue(f.toolbar(child).pending.isEmpty())
            }
        }
    }

    @Test
    fun `an existing own action is tracked without moving it and removed on close or unload`() {
        listOf(false, true).forEach { closeProject ->
            Fixture().use { f ->
                val before = action("before")
                val after = action("after")
                f.header!!.addAll(before, f.save, after)
                f.drain()
                f.binding.refresh()
                f.drain()
                assertEquals(listOf(before, f.save, after), f.children())
                val toolbar = f.toolbar()
                toolbar.visible = f.children()
                toolbar.pending = f.children()
                val replacement = action("same ID new instance")
                f.manager.registerAction(LOGCAT_SAVE_LOGS_ACTION_ID, replacement)
                f.header!!.add(replacement)
                val lookups = f.manager.lookups
                if (closeProject) f.projectOpen.set(false)
                Disposer.dispose(f.owner)
                assertEquals(lookups, f.manager.lookups)
                assertEquals(listOf(before, after, replacement), f.children())
                assertEquals(1, toolbar.resets)
                assertTrue(toolbar.visible.isEmpty())
                assertEquals(f.children(), toolbar.pending)
                assertTrue(toolbar.container.containerListeners.isEmpty())
            }
        }
    }

    @Test
    fun `a split child inheriting the own action is tracked and detached synchronously`() {
        Fixture().use { f ->
            f.drain()
            val before = action("child before")
            val after = action("child after")
            val child = DefaultActionGroup().apply { addAll(before, f.save, after) }
            lateinit var splitter: JPanel
            SwingUtilities.invokeAndWait { splitter = f.split(f.header!!, child) }
            f.drain()
            f.binding.refresh()
            f.drain()
            assertEquals(listOf(before, f.save, after), f.children(child))
            val toolbar = f.toolbar(child)
            toolbar.visible = f.children(child)
            toolbar.pending = f.children(child)
            SwingUtilities.invokeAndWait { splitter.remove(toolbar.container) }
            assertEquals(listOf(before, after), f.children(child))
            assertEquals(1, toolbar.resets)
            assertTrue(toolbar.visible.isEmpty())
            assertEquals(f.children(child), toolbar.pending)
            assertTrue(toolbar.container.containerListeners.isEmpty())
            f.drain()
            SwingUtilities.invokeAndWait { splitter.add(toolbar.container) }
            f.drain()
            assertEquals(listOf(f.save, before, after), f.children(child))
            Disposer.dispose(f.owner)
            assertEquals(listOf(before, after), f.children(child))
            assertEquals(2, toolbar.resets)
        }
    }

    @Test
    fun `headers sharing an action group each clear their toolbar caches on close or unload`() {
        listOf(false, true).forEach { closeProject ->
            Fixture().use { f ->
                val foreign = action("foreign")
                f.header!!.add(foreign)
                f.drain()
                val root = f.toolbar()
                val child = f.newToolbar(f.header!!)
                SwingUtilities.invokeAndWait { root.container.add(child.container) }
                f.drain()
                f.binding.refresh()
                f.drain()
                assertEquals(listOf(f.save, foreign), f.children())
                listOf(root, child).forEach {
                    it.visible = f.children()
                    it.pending = f.children()
                }
                if (closeProject) f.projectOpen.set(false)
                Disposer.dispose(f.owner)
                assertEquals(listOf(foreign), f.children())
                listOf(root, child).forEach {
                    assertEquals(1, it.resets)
                    assertTrue(it.visible.isEmpty())
                    assertEquals(listOf(foreign), it.pending)
                    assertTrue(it.container.containerListeners.isEmpty())
                }
            }
        }
    }

    private class ManagerSpy {
        val foreign = object : ContentManagerListener {}
        val listeners = mutableListOf<ContentManagerListener>(foreign)
        var adds = 0
        var removes = 0
        val value = Proxy.newProxyInstance(ContentManager::class.java.classLoader, arrayOf(ContentManager::class.java)) { _, method, args ->
            check(SwingUtilities.isEventDispatchThread())
            when (method.name) {
                "addContentManagerListener" -> {
                    val listener = args!![0] as ContentManagerListener
                    check(listeners.none { it === listener })
                    listeners.add(listener); adds++; null
                }
                "removeContentManagerListener" -> {
                    val listener = args!![0] as ContentManagerListener
                    check(listeners.removeAll { it === listener })
                    removes++; null
                }
                else -> error("Unexpected ContentManager call: ${method.name}")
            }
        } as ContentManager

        fun event(): ContentManagerEvent {
            val content = Proxy.newProxyInstance(Content::class.java.classLoader, arrayOf(Content::class.java)) { _, _, _ ->
                error("Event content must not be inspected")
            } as Content
            return ContentManagerEvent(value, content, 0)
        }

        fun added() = SwingUtilities.invokeAndWait { val event = event(); listeners.toList().forEach { it.contentAdded(event) } }
        fun removed() = SwingUtilities.invokeAndWait { val event = event(); listeners.toList().forEach { it.contentRemoved(event) } }
    }

    private class ServiceFixture(initiallyCreated: Boolean) : AutoCloseable {
        private val environment = Fixture().also { it.header = null; it.drain() }
        val callbacks = environment.callbacks
        val project = MockProject(null, environment.lifetime)
        val toolWindows = TestToolWindowManager(callbacks)
        var manager = ManagerSpy()
        var created = initiallyCreated
        var windowDisposed = false
        var componentReads = 0
        var decoratorReads = 0
        var pendingListeners = 0
        private val component = JPanel()
        val window = Proxy.newProxyInstance(ToolWindowEx::class.java.classLoader, arrayOf(ToolWindowEx::class.java)) { _, method, _ ->
            when (method.name) {
                "getId" -> LOGCAT_TOOL_WINDOW_ID
                "isDisposed" -> windowDisposed
                "getContentManagerIfCreated" -> { check(SwingUtilities.isEventDispatchThread()); if (created) manager.value else null }
                "getComponent" -> { check(SwingUtilities.isEventDispatchThread()); componentReads++; created = true; component }
                "getDecorator" -> { decoratorReads++; null }
                "addContentManagerListener" -> { pendingListeners++; error("Pending listeners are forbidden") }
                else -> error("Unexpected ToolWindow call: ${method.name}")
            }
        } as ToolWindowEx
        val service: LogcatTitleActionService
        val observer = LogcatTitleActions(project)

        init {
            toolWindows.window = window
            project.registerService(ToolWindowManager::class.java, toolWindows)
            service = LogcatTitleActionService(project)
            project.registerService(LogcatTitleActionService::class.java, service)
            Disposer.register(environment.lifetime, service)
        }

        fun registered() = observer.toolWindowsRegistered(listOf(LOGCAT_TOOL_WINDOW_ID), toolWindows)
        fun shown() = observer.toolWindowShown(window)
        fun drain() = environment.drain()
        override fun close() = environment.close()
    }

    private class Fixture : AutoCloseable {
        val lifetime = Disposer.newDisposable("Logcat title actions test")
        val owner = Disposer.newDisposable("plugin project service")
        val projectOpen = AtomicBoolean(true)
        val manager = TestActionManager()
        val callbacks = ConcurrentLinkedQueue<Runnable>()
        var header: DefaultActionGroup? = DefaultActionGroup()
        var onFindGroup: (() -> Unit)? = null
        private val toolbars = mutableListOf<TestToolbar>()
        val save = action("save logs")
        val binding: LogcatTitleActionBinding

        init {
            val application = object : MockApplication(lifetime) {
                override fun invokeLater(runnable: Runnable, state: ModalityState) { callbacks.add(runnable) }
            }
            ApplicationManager.setApplication(application, lifetime)
            application.registerService(ActionManager::class.java, manager)
            manager.registerAction(LOGCAT_SAVE_LOGS_ACTION_ID, save)
            Disposer.register(lifetime, owner)
            binding = LogcatTitleActionBinding(
                alive = { projectOpen.get() },
                targets = {
                    onFindGroup?.invoke()
                    buildList {
                        fun visit(component: Component) {
                            toolbars.firstOrNull { it.container === component }?.let { add(it.target) }
                            if (component is Container) component.components.forEach(::visit)
                        }
                        header?.let { visit(toolbar(it).container) }
                    }
                },
                action = { manager.getAction(LOGCAT_SAVE_LOGS_ACTION_ID) },
                later = callbacks::add,
            )
            Disposer.register(owner, binding)
            binding.refresh()
        }

        fun children(group: DefaultActionGroup = header!!): List<AnAction> = group.getChildren(manager).toList()

        fun toolbar(group: DefaultActionGroup = header!!): TestToolbar =
            toolbars.firstOrNull { it.target.group === group } ?: newToolbar(group)

        fun newToolbar(group: DefaultActionGroup): TestToolbar = TestToolbar(group).also(toolbars::add)

        fun split(parent: DefaultActionGroup, vararg children: DefaultActionGroup): JPanel = JPanel().also { splitter ->
            children.forEach { splitter.add(toolbar(it).container) }
            toolbar(parent).container.add(splitter)
        }

        fun drain() {
            val drain = Runnable { while (true) (callbacks.poll() ?: break).run() }
            if (SwingUtilities.isEventDispatchThread()) drain.run() else SwingUtilities.invokeAndWait(drain)
        }

        override fun close() { Disposer.dispose(lifetime) }
    }

    private class TestToolbar(group: DefaultActionGroup) {
        val container = JPanel()
        var visible: List<AnAction> = emptyList()
        var pending: List<AnAction> = emptyList()
        var updates = 0
        var resets = 0
        private val toolbar = Proxy.newProxyInstance(ActionToolbar::class.java.classLoader, arrayOf(ActionToolbar::class.java)) { _, method, _ ->
            check(SwingUtilities.isEventDispatchThread())
            check(method.name == "updateActionsAsync") { "Unexpected toolbar call: ${method.name}" }
            updates++
            pending = group.childActionsOrStubs.toList()
            CompletableFuture.completedFuture(null)
        } as ActionToolbar
        val target = LogcatTitleActionTarget(container, group, toolbar) {
            check(SwingUtilities.isEventDispatchThread())
            resets++
            visible = emptyList()
            pending = emptyList()
        }
    }

    private class TestActionManager : ActionManager() {
        private val actions = linkedMapOf<String, AnAction>()
        var lookups = 0
        var idLookups = 0
        override fun getAction(id: String): AnAction? { lookups++; return actions[id] }
        override fun getId(action: AnAction): String? { idLookups++; return actions.entries.firstOrNull { it.value === action }?.key }
        override fun registerAction(id: String, action: AnAction) { actions[id] = action }
        override fun registerAction(id: String, action: AnAction, pluginId: PluginId?) = registerAction(id, action)
        override fun unregisterAction(id: String) { actions.remove(id) }
        override fun replaceAction(id: String, action: AnAction) = registerAction(id, action)
        override fun getActionIds(prefix: String): Array<String> = getActionIdList(prefix).toTypedArray()
        override fun getActionIdList(prefix: String): List<String> = actions.keys.filter { it.startsWith(prefix) }
        override fun isGroup(id: String): Boolean = actions[id] is ActionGroup
        override fun getActionOrStub(id: String): AnAction? = actions[id]
        override fun addTimerListener(listener: TimerListener) {}
        override fun removeTimerListener(listener: TimerListener) {}
        override fun getKeyboardShortcut(id: String): KeyboardShortcut? = null
        override fun createActionPopupMenu(place: String, group: ActionGroup): ActionPopupMenu = error("Unexpected popup")
        override fun createActionToolbar(place: String, group: ActionGroup, horizontal: Boolean): ActionToolbar = error("Unexpected toolbar")
        override fun tryToExecute(action: AnAction, event: InputEvent?, context: Component?, place: String?, now: Boolean): ActionCallback =
            error("Unexpected execution")
    }

    private class TestToolWindowManager(private val callbacks: ConcurrentLinkedQueue<Runnable>) : ToolWindowManager() {
        var window: ToolWindow? = null
        override val focusManager: IdeFocusManager get() = error("Unexpected focus lookup")
        override val isEditorComponentActive: Boolean = false
        override val toolWindowIds: Array<String> = emptyArray()
        override val toolWindowIdSet: Set<String> = emptySet()
        override val activeToolWindowId: String? = null
        override val lastActiveToolWindowId: String? = null
        override fun canShowNotification(id: String): Boolean = false
        override fun registerToolWindow(task: RegisterToolWindowTask): ToolWindow = error("Unexpected registration")
        override fun unregisterToolWindow(id: String) = error("Unexpected unregistration")
        override fun activateEditorComponent() = error("Unexpected activation")
        override fun getToolWindow(id: String?): ToolWindow? = if (id == LOGCAT_TOOL_WINDOW_ID) window else null
        override fun invokeLater(runnable: Runnable) { callbacks.add(runnable) }
        override fun notifyByBalloon(options: ToolWindowBalloonShowOptions) = error("Unexpected balloon")
        override fun getToolWindowBalloon(id: String): Balloon? = null
        override fun isMaximized(window: ToolWindow): Boolean = false
        override fun setMaximized(window: ToolWindow, maximized: Boolean) = error("Unexpected maximization")
    }

    companion object {
        private fun action(label: String): AnAction = object : AnAction(label) {
            override fun actionPerformed(event: AnActionEvent) {}
        }
    }
}
