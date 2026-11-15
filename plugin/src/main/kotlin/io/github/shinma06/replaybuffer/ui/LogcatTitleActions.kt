package io.github.shinma06.replaybuffer.ui

import com.intellij.ide.DataManager
import com.intellij.openapi.Disposable
import com.intellij.openapi.actionSystem.AnAction
import com.intellij.openapi.actionSystem.ActionManager
import com.intellij.openapi.actionSystem.ActionToolbar
import com.intellij.openapi.actionSystem.Constraints
import com.intellij.openapi.actionSystem.DefaultActionGroup
import com.intellij.openapi.actionSystem.PlatformDataKeys
import com.intellij.openapi.actionSystem.impl.ActionToolbarImpl
import com.intellij.openapi.components.Service
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.Disposer
import com.intellij.openapi.wm.ToolWindow
import com.intellij.openapi.wm.ToolWindowManager
import com.intellij.openapi.wm.ex.ToolWindowEx
import com.intellij.openapi.wm.ex.ToolWindowManagerListener
import com.intellij.toolWindow.InternalDecoratorImpl
import java.awt.Component
import java.awt.Container
import java.awt.event.ContainerAdapter
import java.awt.event.ContainerEvent
import java.util.IdentityHashMap
import java.util.concurrent.atomic.AtomicBoolean
import javax.swing.SwingUtilities

internal const val LOGCAT_TOOL_WINDOW_ID = "Logcat"
internal const val LOGCAT_SAVE_LOGS_ACTION_ID = "io.github.shinma06.replaybuffer.Logcat.SaveLogs"

class LogcatTitleActions(private val project: Project) : ToolWindowManagerListener {
    override fun toolWindowsRegistered(ids: List<String>, toolWindowManager: ToolWindowManager) {
        if (LOGCAT_TOOL_WINDOW_ID in ids && !project.isDisposed)
            project.getService(LogcatTitleActionService::class.java).refresh()
    }
}

@Service(Service.Level.PROJECT)
internal class LogcatTitleActionService(project: Project) : Disposable {
    private val toolWindows = ToolWindowManager.getInstance(project)
    private val binding = LogcatTitleActionBinding(
        alive = { !project.isDisposed },
        targets = { logcatHeaderTargets(toolWindows.getToolWindow(LOGCAT_TOOL_WINDOW_ID)) },
        action = { ActionManager.getInstance().getAction(LOGCAT_SAVE_LOGS_ACTION_ID) },
        later = toolWindows::invokeLater,
    )

    init {
        Disposer.register(this, binding)
        binding.refresh()
    }

    fun refresh() = binding.refresh()

    override fun dispose() {}
}

// Verified with AS AI-262.10968.63.2622.16730706: InternalDecoratorImpl is @ApiStatus.Internal; reset is an impl API too.
// ToolWindow has only a replacing setter. Unsupported SDK implementations must leave the header untouched.
internal fun logcatHeaderTargets(window: ToolWindow?): List<LogcatTitleActionTarget> {
    if (window == null || window.isDisposed) return emptyList()
    return try {
        val extended = window as? ToolWindowEx ?: return emptyList()
        // The decorator getter itself does not initialize the content manager.
        extended.component
        val root = extended.decorator as? InternalDecoratorImpl ?: return emptyList()
        buildList {
            fun visit(component: Component) {
                if (component is InternalDecoratorImpl) {
                    if (PlatformDataKeys.TOOL_WINDOW.getData(DataManager.getInstance().getDataContext(component)) !== window) return
                    val group = component.headerToolbarActions as? DefaultActionGroup ?: return
                    val toolbar = component.headerToolbar as? ActionToolbarImpl ?: return
                    add(LogcatTitleActionTarget(component, group, toolbar, toolbar::reset))
                }
                if (component is Container) component.components.forEach(::visit)
            }
            visit(root)
        }
    } catch (_: LinkageError) {
        emptyList()
    }
}

internal class LogcatTitleActionTarget(
    val container: Container,
    val group: DefaultActionGroup,
    val toolbar: ActionToolbar,
    val reset: () -> Unit,
)

internal class LogcatTitleActionBinding(
    private val alive: () -> Boolean,
    private val targets: () -> List<LogcatTitleActionTarget>,
    private val action: () -> AnAction?,
    private val later: (Runnable) -> Unit,
) : Disposable {
    @Volatile private var disposed = false
    private val queued = AtomicBoolean()
    private val watched = IdentityHashMap<Container, Boolean>()
    private val installed = IdentityHashMap<Container, Pair<LogcatTitleActionTarget, AnAction>>()
    private var root: Container? = null
    private val listener = object : ContainerAdapter() {
        override fun componentAdded(event: ContainerEvent) = refresh()
        override fun componentRemoved(event: ContainerEvent) {
            synchronized(this@LogcatTitleActionBinding) {
                if (!disposed) removeSubtree(event.child)
            }
            refresh()
        }
    }

    fun refresh() {
        if (!disposed && alive() && queued.compareAndSet(false, true)) later(Runnable {
            queued.set(false)
            render()
        })
    }

    @Synchronized
    private fun render() {
        if (disposed || !alive()) return
        val currentTargets = targets()
        val currentAction = if (currentTargets.isNotEmpty()) action() else null
        if (disposed || !alive()) return
        val currentRoot = currentTargets.firstOrNull()?.container
        if (root !== currentRoot) clear()
        root = currentRoot
        for (container in watched.keys.toList()) {
            if (currentRoot == null || !container.inside(currentRoot)) unwatch(container)
        }
        for (container in installed.keys.toList()) {
            if (currentTargets.none { it.container === container }) detach(container)
        }
        for (target in currentTargets) {
            // Watch only the decorator-to-root split structure, not Logcat's editor/UI subtree.
            var container: Container? = target.container
            while (container != null) {
                if (watched.put(container, true) == null) container.addContainerListener(listener)
                if (container === currentRoot) break
                container = container.parent
            }
            val old = installed[target.container]
            if (old != null && old.first.group === target.group && old.first.toolbar === target.toolbar &&
                old.second === currentAction && target.group.containsInstance(currentAction)) continue
            detach(target.container)
            if (currentAction != null && !target.group.containsInstance(currentAction)) {
                target.group.add(currentAction, Constraints.FIRST)
                installed[target.container] = target to currentAction
                target.toolbar.updateActionsAsync()
            }
        }
    }

    private fun detach(container: Container) {
        // IDs may already be unregistered or reused during unload.
        val (target, action) = installed.remove(container) ?: return
        target.group.remove(action, null as String?)
        target.reset()
        target.toolbar.updateActionsAsync()
    }

    private fun unwatch(container: Container) {
        watched.remove(container)
        container.removeContainerListener(listener)
    }

    private fun removeSubtree(component: Component) {
        watched.keys.filter { it.inside(component) }.forEach(::unwatch)
        installed.keys.filter { it.inside(component) }.forEach(::detach)
    }

    private fun clear() {
        watched.keys.toList().forEach(::unwatch)
        installed.keys.toList().forEach(::detach)
        root = null
    }

    override fun dispose() {
        disposed = true
        val cleanup = Runnable { synchronized(this) { queued.set(false); clear() } }
        if (SwingUtilities.isEventDispatchThread()) cleanup.run() else SwingUtilities.invokeAndWait(cleanup)
    }

    private fun DefaultActionGroup.containsInstance(action: AnAction?): Boolean =
        childActionsOrStubs.any { it === action }

    private fun Component.inside(component: Component): Boolean =
        this === component || SwingUtilities.isDescendingFrom(this, component)
}
