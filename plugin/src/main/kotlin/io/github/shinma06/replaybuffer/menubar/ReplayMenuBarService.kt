package io.github.shinma06.replaybuffer.menubar

import com.intellij.ide.IdeEventQueue
import com.intellij.openapi.Disposable
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.application.ModalityState
import com.intellij.openapi.components.Service
import com.intellij.openapi.options.ShowSettingsUtil
import com.intellij.openapi.project.Project
import com.intellij.openapi.project.ProjectManager
import com.intellij.openapi.project.ProjectManagerListener
import com.intellij.openapi.wm.ToolWindowManager
import com.intellij.openapi.wm.WindowManager
import com.intellij.util.messages.MessageBusConnection
import com.intellij.util.messages.Topic
import io.github.shinma06.replaybuffer.core.HostPlatform
import io.github.shinma06.replaybuffer.core.SavePhase
import io.github.shinma06.replaybuffer.core.SaveRequest
import io.github.shinma06.replaybuffer.ide.ReplayProjectService
import io.github.shinma06.replaybuffer.settings.ReplaySettingsConfigurable
import io.github.shinma06.replaybuffer.settings.ReplaySettingsStore
import io.github.shinma06.replaybuffer.ui.ScreenshotResult
import java.awt.Toolkit
import java.nio.file.Path
import java.time.LocalTime
import java.time.format.DateTimeFormatter
import java.util.Locale
import java.util.UUID
import java.util.concurrent.CompletableFuture
import java.util.concurrent.CompletionException
import java.util.concurrent.atomic.AtomicBoolean
import javax.swing.JFrame
import javax.swing.SwingUtilities

private val RESULT_TIME: DateTimeFormatter = DateTimeFormatter.ofPattern("HH:mm:ss", Locale.ROOT)
private const val TOOL_WINDOW_ID = "Android Replay Buffer"

internal fun restoredWindowState(state: Int): Int = state and JFrame.ICONIFIED.inv()

internal fun interface MenuBarAvailabilityListener {
    fun changed(reason: String?)
}

/**
 * Application-level owner of the tray icon: it aggregates every open project, rebuilds the shared menu on the EDT,
 * and only calls ReplayProjectService's existing operations, so the menu can never add a capture process.
 */
@Service(Service.Level.APP)
class ReplayMenuBarService : Disposable {
    private val surface = HostPlatform.traySurface()
    private val presenter = MenuBarPresenter(surface)
    /** EDT-confined, including registration, command lookup and disposal. */
    private val projects = LinkedHashMap<Project, ProjectMenu>()
    private var trayEventsRegistered = false
    private val queued = AtomicBoolean(false)
    @Volatile private var disposed = false

    init {
        val application = ApplicationManager.getApplication()
        // Opening is also announced from ReplayProjectActivity; the listener is only needed for close.
        application.messageBus.connect(this).subscribe(ProjectManager.TOPIC, object : ProjectManagerListener {
            override fun projectClosing(project: Project) = untrack(project)
        })
        application.messageBus.connect(this).subscribe(ReplayMenuBarStore.CHANGED, Runnable { refresh() })
        // Settings-form unit tests construct this service on a MockApplication that has no ProjectManager.
        application.getService(ProjectManager::class.java)?.openProjects?.forEach(::track)
    }

    private val store: ReplayMenuBarStore
        get() = ApplicationManager.getApplication().getService(ReplayMenuBarStore::class.java)

    /** Why the icon cannot appear in this environment, for the settings page (MB-02). */
    fun availabilityReason(): String? = presenter.unavailableReason()

    fun projectOpened(project: Project) = track(project)

    private fun track(project: Project) = onEdt {
        if (disposed || project.isDisposed || projects.containsKey(project)) return@onEdt
        val refresh = Runnable { refresh() }
        val connection = project.messageBus.connect(this).apply {
            subscribe(ReplayProjectService.ENVIRONMENT_CHANGED, refresh)
            subscribe(ReplaySettingsStore.CHANGED, refresh)
        }
        projects[project] = ProjectMenu(project, ::refresh, connection)
        refresh()
    }

    private fun untrack(project: Project) = onEdt {
        if (disposed) return@onEdt
        projects.remove(project)?.close()
        refresh()
    }

    fun refresh() {
        if (disposed || !queued.compareAndSet(false, true)) return
        ApplicationManager.getApplication().invokeLater({
            queued.set(false)
            if (!disposed) render()
        }, ModalityState.any())
    }

    private fun render() {
        val inputs = projects.values.filter { it.alive }.map { it.input() }
        val requested = store.showInMenuBar
        if (!trayEventsRegistered && requested && inputs.isNotEmpty() && surface.unavailableReason() == null) {
            val toolkit = Toolkit.getDefaultToolkit()
            IdeEventQueue.getInstance().addPostEventListener({ event ->
                !disposed && surface.interceptPostedEvent(event, toolkit)
            }, this)
            trayEventsRegistered = true
        }
        presenter.update(requested, inputs) { item ->
            if (!disposed) projects.values.firstOrNull { it.projectId == item.projectId }?.let(item::run)
        }
        ApplicationManager.getApplication().messageBus.syncPublisher(AVAILABILITY_CHANGED).changed(availabilityReason())
    }

    override fun dispose() {
        disposed = true
        onEdt {
            projects.values.forEach { it.close() }
            projects.clear()
            presenter.close()
        }
    }

    private fun onEdt(action: () -> Unit) {
        if (SwingUtilities.isEventDispatchThread()) action() else SwingUtilities.invokeLater(action)
    }

    companion object {
        internal val AVAILABILITY_CHANGED: Topic<MenuBarAvailabilityListener> =
            Topic.create("Android Replay Buffer menu bar availability changed", MenuBarAvailabilityListener::class.java)
    }
}

/**
 * One project's side of the menu: reads the snapshot for the model and adapts menu commands to the existing service.
 */
private class ProjectMenu(
    private val project: Project,
    private val changed: () -> Unit,
    private val connection: MessageBusConnection,
) : MenuBarOperations, AutoCloseable {
    private val service = project.getService(ReplayProjectService::class.java)
    private val settings = project.getService(ReplaySettingsStore::class.java)
    private var savedKey: Pair<String?, Long>? = null
    private var savedAt: String? = null
    private var trackedScreenshot: CompletableFuture<*>? = null
    private var screenshotResult: ScreenshotResult? = null

    val projectId: String = UUID.randomUUID().toString()
    val alive: Boolean get() = !project.isDisposed

    override fun close() = connection.disconnect()

    fun input(): MenuProjectInput {
        val view = service.captureView
        val snapshot = view.snapshot
        snapshot?.save?.takeIf { it.phase == SavePhase.COMPLETED }?.let { save ->
            val key = save.requestId to save.attempt
            if (key != savedKey) { savedKey = key; savedAt = LocalTime.now().format(RESULT_TIME) }
        }
        trackScreenshot()
        return MenuProjectInput(
            projectId = projectId, name = project.name, snapshot = snapshot,
            message = view.message, initializationError = view.initializationError,
            busy = service.resolving || view.pending > 0, enabled = settings.enabled, settings = settings.settings(),
            screenshotDisabledReason = service.screenshotDisabledReason, screenshotBusy = service.screenshotBusy,
            savedAt = savedAt.takeIf { snapshot?.save?.phase == SavePhase.COMPLETED },
            screenshot = screenshotResult,
            location = project.basePath ?: project.name,
        )
    }

    private fun trackScreenshot() {
        val latest = service.latestScreenshot ?: return
        if (latest === trackedScreenshot) return
        trackedScreenshot = latest
        latest.thenAccept { outcome ->
            val time = outcome.time.format(RESULT_TIME)
            val result = outcome.saved?.let { ScreenshotResult.Saved(time, it.file) } ?: ScreenshotResult.Failed(time,
                ((outcome.failure as? CompletionException)?.cause ?: outcome.failure)?.message
                    ?: "スクリーンショットを保存できませんでした。")
            SwingUtilities.invokeLater {
                if (alive && trackedScreenshot === latest) { screenshotResult = result; changed() }
            }
        }
    }

    override fun setEnabled(value: Boolean) { if (alive) service.setEnabled(value) }
    override fun save(request: SaveRequest) { if (alive) service.save(request) }
    override fun screenshot() { if (alive) service.screenshot() }
    override fun openDirectory(directory: Path) { if (alive) service.openDirectory(directory) }

    override fun showToolWindow() {
        if (!alive) return
        WindowManager.getInstance().getFrame(project)?.let { frame ->
            frame.extendedState = restoredWindowState(frame.extendedState)
            frame.isVisible = true
            frame.toFront()
        }
        ToolWindowManager.getInstance(project).getToolWindow(TOOL_WINDOW_ID)?.show()
    }

    override fun openSettings() {
        if (alive) ShowSettingsUtil.getInstance().showSettingsDialog(project, ReplaySettingsConfigurable::class.java)
    }
}
