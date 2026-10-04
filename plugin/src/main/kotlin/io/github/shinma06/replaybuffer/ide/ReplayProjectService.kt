package io.github.shinma06.replaybuffer.ide

import com.android.tools.idea.gradle.project.sync.GradleSyncListener
import com.android.tools.idea.gradle.project.sync.GradleSyncState
import com.android.tools.idea.projectsystem.PROJECT_SYSTEM_MODELS_UPDATED_TOPIC
import com.android.tools.idea.projectsystem.ProjectSystemSyncManager
import com.intellij.execution.RunManagerListener
import com.intellij.execution.RunnerAndConfigurationSettings
import com.intellij.notification.Notification
import com.intellij.notification.NotificationAction
import com.intellij.notification.NotificationGroupManager
import com.intellij.notification.NotificationType
import com.intellij.openapi.Disposable
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.application.ModalityState
import com.intellij.openapi.application.PathManager
import com.intellij.openapi.components.Service
import com.intellij.openapi.diagnostic.Logger
import com.intellij.openapi.ide.CopyPasteManager
import com.intellij.openapi.progress.ProcessCanceledException
import com.intellij.openapi.project.DumbAware
import com.intellij.openapi.project.DumbService
import com.intellij.openapi.project.Project
import com.intellij.openapi.roots.ModuleRootEvent
import com.intellij.openapi.roots.ModuleRootListener
import com.intellij.openapi.startup.ProjectActivity
import com.intellij.openapi.util.text.StringUtil
import com.intellij.openapi.wm.ToolWindowManager
import com.intellij.util.messages.Topic
import io.github.shinma06.replaybuffer.core.SavePhase
import io.github.shinma06.replaybuffer.core.SaveSnapshot
import io.github.shinma06.replaybuffer.settings.ReplaySettingsStore
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.future.await
import kotlinx.coroutines.launch
import java.awt.Desktop
import java.awt.datatransfer.StringSelection
import java.nio.file.Files
import java.nio.file.Path

/** Owns IDE subscriptions and background model resolution independently of the ToolWindow. */
@Service(Service.Level.PROJECT)
class ReplayProjectService(private val project: Project, private val scope: CoroutineScope) : Disposable {
    @Volatile
    var environment: AndroidReplayEnvironment? = null
        private set
    private var requestRevision = 0L
    @Volatile private var disposed = false
    private var resolution: Job? = null
    @Volatile var resolving = true
        private set
    private val notifications = mutableListOf<Notification>()
    private val projectBasePath = project.basePath
    private val capture = ReplayProjectCapture(scope, project.getService(ReplaySettingsStore::class.java), ::publishChanged, ::notifySave) {
        prepareCleanupDirectory(PathManager.getConfigDir(), projectBasePath)
    }
    internal val captureView: ReplayCaptureView get() = capture.view
    private val alive: Boolean get() = !disposed && !project.isDisposed

    init {
        val connection = project.messageBus.connect(this)
        connection.subscribe(ReplaySettingsStore.CHANGED, Runnable { refresh() })
        connection.subscribe(RunManagerListener.TOPIC, object : RunManagerListener {
            override fun runConfigurationSelected(settings: RunnerAndConfigurationSettings?) = refresh()
            override fun runConfigurationChanged(settings: RunnerAndConfigurationSettings) = refresh()
            override fun runConfigurationRemoved(settings: RunnerAndConfigurationSettings) = refresh()
            override fun stateLoaded(runManager: com.intellij.execution.RunManager, isFirstLoad: Boolean) = refresh()
        })
        connection.subscribe(ModuleRootListener.TOPIC, object : ModuleRootListener {
            override fun rootsChanged(event: ModuleRootEvent) = refresh()
        })
        connection.subscribe(PROJECT_SYSTEM_MODELS_UPDATED_TOPIC, object : ProjectSystemSyncManager.AndroidModelsUpdatedListener {
            override fun androidModelsUpdated() = refresh()
        })
        connection.subscribe(DumbService.DUMB_MODE, object : DumbService.DumbModeListener {
            override fun exitDumbMode() = refresh()
        })
        GradleSyncState.subscribe(project, object : GradleSyncListener {
            override fun syncStarted(project: Project) = refresh()
            override fun syncSucceeded(project: Project) = refresh()
            override fun syncFailed(project: Project, errorMessage: String) = refresh()
            override fun syncSkipped(project: Project) = refresh()
        }, this)
        refresh()
    }

    @Synchronized
    fun refresh() {
        if (disposed || project.isDisposed) return
        val revision = ++requestRevision
        capture.expectSettings(revision)
        resolution?.cancel()
        resolving = true
        publishChanged()
        val settings = project.getService(ReplaySettingsStore::class.java).settings()
        resolution = scope.launch(Dispatchers.IO) {
            try {
                val next = try {
                    ReplayAndroidEnvironment.resolve(project, settings)
                } catch (cancelled: CancellationException) {
                    throw cancelled
                } catch (cancelled: ProcessCanceledException) {
                    throw cancelled
                } catch (_: RuntimeException) {
                    AndroidReplayEnvironment(
                        null,
                        "projectのAndroid SDKとGradle同期を確認してください。",
                        ApplicationSelection(null, null, "対象アプリの情報を取得できません。 " + MANUAL_APPLICATION_GUIDANCE),
                    )
                }
                synchronized(this@ReplayProjectService) {
                    if (disposed || project.isDisposed || revision != requestRevision) return@launch
                    environment = next
                }
                publishChanged()
                capture.applySettings(settings, next, revision).await()
            } finally {
                val current = synchronized(this@ReplayProjectService) {
                    if (disposed || project.isDisposed || revision != requestRevision) false else {
                        resolving = false
                        true
                    }
                }
                if (current) publishChanged()
            }
        }
    }

    internal fun setEnabled(value: Boolean) { if (alive) capture.setEnabled(value) }
    internal fun save() {
        if (!alive) return
        if (resolving || capture.view.pending > 0) capture.showMessage("設定・操作を反映中です。完了後に保存してください。") else capture.save()
    }
    internal fun retry(id: String) { if (alive) capture.retry(id) }
    internal fun retryAtDirectory(id: String, path: Path) { if (alive) capture.retryAtDirectory(id, path) }
    internal fun discard(id: String) { if (alive) capture.discard(id) }

    private fun publishChanged() {
        if (alive) project.messageBus.syncPublisher(ENVIRONMENT_CHANGED).run()
    }

    private fun notifySave(save: SaveSnapshot) {
        onUi {
            if (save.phase == SavePhase.FAILED &&
                (capture.view.snapshot?.save?.requestId != save.requestId || capture.view.snapshot?.save?.phase != SavePhase.FAILED)) return@onUi
            val group = NotificationGroupManager.getInstance().getNotificationGroup("Android Replay Buffer")
            val notification = if (save.phase == SavePhase.COMPLETED && save.directory != null) {
                val missing = save.missingKinds.map(::streamName).joinToString("、")
                group.createNotification(
                    if (missing.isEmpty()) "保存しました" else "保存しました（一部の記録に欠落があります）",
                    if (missing.isEmpty()) "直前の記録を完成フォルダへ保存しました。" else "不足・欠落: $missing",
                    if (missing.isEmpty()) NotificationType.INFORMATION else NotificationType.WARNING,
                ).addAction(NotificationAction.create("フォルダを開く") { _, event ->
                    if (alive) openDirectory(save.directory) else event.expire()
                }).addAction(copyPathAction(save.directory))
            } else {
                group.createNotification("保存に失敗しました", StringUtil.escapeXmlEntities(save.error ?: "保存対象を再試行または破棄できます。"), NotificationType.ERROR)
                    .addAction(NotificationAction.create("保存対象を確認する") { _, event ->
                        if (alive) ToolWindowManager.getInstance(project).getToolWindow("Android Replay Buffer")?.show() else event.expire()
                    })
            }
            showNotification(notification)
        }
    }

    internal fun openDirectory(directory: Path) {
        if (!alive) return
        scope.launch(Dispatchers.IO) {
            val reason = try {
                when {
                    !Files.isDirectory(directory) -> "完成フォルダが見つからないか、アクセスできません。"
                    !Desktop.isDesktopSupported() || !Desktop.getDesktop().isSupported(Desktop.Action.OPEN) -> "この環境ではフォルダを開く操作を利用できません。"
                    !alive -> return@launch
                    else -> { Desktop.getDesktop().open(directory.toFile()); null }
                }
            } catch (_: Exception) {
                "完成フォルダを開けません。パスをコピーして開いてください。"
            }
            if (reason != null) onUi {
                showNotification(NotificationGroupManager.getInstance().getNotificationGroup("Android Replay Buffer")
                    .createNotification("フォルダを開けません", reason, NotificationType.WARNING).addAction(copyPathAction(directory)))
            }
        }
    }

    private fun copyPathAction(path: Path): NotificationAction = NotificationAction.create("パスをコピー") { _, notification ->
        if (alive) CopyPasteManager.getInstance().setContents(StringSelection(path.toString())) else notification.expire()
    }

    private fun showNotification(notification: Notification) {
        if (!alive) { notification.expire(); return }
        synchronized(notifications) { notifications += notification }
        notification.whenExpired { synchronized(notifications) { notifications.remove(notification) } }
        notification.notify(project)
    }

    private fun onUi(action: () -> Unit) {
        if (!alive) return
        ApplicationManager.getApplication().invokeLater({ if (alive) action() }, ModalityState.nonModal())
    }

    @Synchronized
    override fun dispose() {
        disposed = true
        requestRevision++
        resolution?.cancel()
        resolution = null
        capture.close()
        capture.termination.whenComplete { _, failure ->
            if (failure != null) Logger.getInstance(ReplayProjectService::class.java).warn("Replay Bufferの終了・cleanupを確認できません。一時領域の所有情報を保全します。")
        }
        val expired = synchronized(notifications) { notifications.toList().also { notifications.clear() } }
        expired.forEach(Notification::expire)
    }

    companion object {
        val ENVIRONMENT_CHANGED: Topic<Runnable> = Topic.create("Android Replay Buffer environment changed", Runnable::class.java)
    }
}

internal fun streamName(kind: String): String = when (kind) {
    "video" -> "動画"
    "device_log" -> "端末ログ"
    "app_log" -> "アプリログ"
    else -> "記録"
}

class ReplayProjectActivity : ProjectActivity, DumbAware {
    override suspend fun execute(project: Project) {
        project.getService(ReplayProjectService::class.java).refresh()
    }
}
