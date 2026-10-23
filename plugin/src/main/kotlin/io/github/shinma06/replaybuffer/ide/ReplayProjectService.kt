package io.github.shinma06.replaybuffer.ide

import com.android.tools.idea.gradle.project.sync.GradleSyncListener
import com.android.tools.idea.gradle.project.sync.GradleSyncState
import com.android.tools.idea.projectsystem.PROJECT_SYSTEM_MODELS_UPDATED_TOPIC
import com.android.tools.idea.projectsystem.ProjectSystemSyncManager
import com.google.gson.Gson
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
import io.github.shinma06.replaybuffer.cli.CliCommand
import io.github.shinma06.replaybuffer.cli.CliRequest
import io.github.shinma06.replaybuffer.cli.CliResponse
import io.github.shinma06.replaybuffer.cli.LegacyCliConfig
import io.github.shinma06.replaybuffer.cli.ReplayCliRegistry
import io.github.shinma06.replaybuffer.cli.ReplayCliServer
import io.github.shinma06.replaybuffer.cli.cliSaveResponse
import io.github.shinma06.replaybuffer.core.ReplayOperation
import io.github.shinma06.replaybuffer.core.ReplaySnapshot
import io.github.shinma06.replaybuffer.core.SavePhase
import io.github.shinma06.replaybuffer.core.SaveSnapshot
import io.github.shinma06.replaybuffer.core.SavedScreenshot
import io.github.shinma06.replaybuffer.core.ScreenshotSaveFailure
import io.github.shinma06.replaybuffer.core.saveScreenshot
import io.github.shinma06.replaybuffer.settings.ReplaySettings
import io.github.shinma06.replaybuffer.settings.ReplaySettingsStore
import io.github.shinma06.replaybuffer.settings.validateDestination
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.future.await
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.runInterruptible
import java.awt.Desktop
import java.awt.datatransfer.StringSelection
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.CompletableFuture
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException

/** Owns IDE subscriptions and background model resolution independently of the ToolWindow. */
@Service(Service.Level.PROJECT)
class ReplayProjectService(private val project: Project, internal val scope: CoroutineScope) : Disposable {
    @Volatile
    var environment: AndroidReplayEnvironment? = null
        private set
    private var requestRevision = 0L
    @Volatile private var disposed = false
    private var resolution: Job? = null
    private var cliServer: ReplayCliServer? = null
    private var screenshotJob: Job? = null
    private var screenshotRevision = -1L
    private var screenshotSettings: ReplaySettings? = null
    @Volatile internal var screenshotBusy = false
        private set
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
        scope.launch(Dispatchers.IO) {
            var server: ReplayCliServer? = null
            try {
                val base = projectBasePath ?: return@launch
                server = ReplayCliServer(ReplayCliRegistry.directory(), Path.of(base), ReplayCliRegistry.buildIdentity(), ::executeCli)
                synchronized(this@ReplayProjectService) {
                    if (alive && coroutineContext.isActive) { cliServer = server; server = null }
                }
            } catch (_: Exception) {
                if (alive) capture.showMessage("CLI入口を開けません。利用者専用の接続情報とprojectの保存場所を確認してください。")
            } finally {
                server?.let(::closeCliServer)
            }
        }
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
                    screenshotSettings = settings
                    screenshotRevision = revision
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

    @Synchronized
    internal fun setEnabled(value: Boolean): CompletableFuture<ReplayOperation> {
        if (!alive) return rejected("projectは終了しています。")
        return if (value && (resolving || capture.view.pending > 0)) rejected("設定・操作を反映中です。完了後に取得をONにしてください。")
        else capture.setEnabled(value)
    }

    @Synchronized
    internal fun save(): CompletableFuture<ReplayOperation> {
        if (!alive) return rejected("projectは終了しています。")
        return if (resolving || capture.view.pending > 0) rejected("設定・操作を反映中です。完了後に保存してください。") else capture.save()
    }

    internal val screenshotDisabledReason: String?
        @Synchronized get() = screenshotDisabledReason(capture.view.snapshot)

    private fun screenshotDisabledReason(snapshot: ReplaySnapshot?): String? = when {
        !alive -> "projectは終了しています。"
        screenshotBusy -> "スクリーンショットを保存中です。"
        screenshotRevision != requestRevision -> "対象アプリ・設定を確認中です。"
        environment?.adb == null -> environment?.adbReason ?: "Android SDKを確認してください。"
        environment?.application?.packageName == null -> environment?.application?.reason ?: "対象アプリを選択してください。"
        screenshotSettings?.destination.isNullOrEmpty() -> "設定で保存先を指定してください。"
        else -> runCatching { screenshotDeviceSerial(snapshot) }.exceptionOrNull()?.message
    }

    private fun screenshotDeviceSerial(snapshot: ReplaySnapshot?): String? {
        if (snapshot?.enabled != true) {
            check(!project.getService(ReplaySettingsStore::class.java).enabled) { "取得対象の反映を待っています。完了後に保存してください。" }
            return null
        }
        check(snapshot.settings.adbPath == environment?.adb?.normalize()) { "取得対象のSDK設定を反映中です。完了後に保存してください。" }
        return checkNotNull(snapshot.device) { "録画対象の端末が未確定です。端末の接続状態を確認してください。" }.serial
    }

    @Synchronized
    internal fun screenshot() {
        // Capture publishes under a different lock; validate and execute against this one snapshot.
        val snapshot = capture.view.snapshot
        screenshotDisabledReason(snapshot)?.let { screenshotError(it); return }
        val selected = checkNotNull(environment)
        val destination = checkNotNull(screenshotSettings).destination
        val serial = runCatching { screenshotDeviceSerial(snapshot) }.getOrElse {
            screenshotError(it.message ?: "録画対象の端末を確認できません。")
            return
        }
        screenshotBusy = true
        val job = scope.launch(Dispatchers.IO, start = CoroutineStart.LAZY) {
            try {
                val saved = runInterruptible {
                    saveScreenshot(checkNotNull(selected.adb), checkNotNull(selected.application.packageName), Path.of(destination), serial)
                }
                notifyScreenshot(saved)
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: Exception) {
                screenshotError(failure.message ?: "端末接続または保存に失敗しました。", (failure as? ScreenshotSaveFailure)?.retained)
            }
        }
        screenshotJob = job
        job.invokeOnCompletion {
            synchronized(this) {
                if (screenshotJob === job) { screenshotJob = null; screenshotBusy = false }
            }
            publishChanged()
        }
        publishChanged()
        job.start()
    }

    private fun notifyScreenshot(saved: SavedScreenshot) = onUi {
        val deleting = java.util.concurrent.atomic.AtomicBoolean()
        val notification = NotificationGroupManager.getInstance().getNotificationGroup("Android Replay Buffer")
            .createNotification("スクリーンショットを保存しました",
                "端末の表示画面をPNGで保存しました。選択アプリ: ${StringUtil.escapeXmlEntities(saved.application)}。保護領域は空白になる場合があります。",
                NotificationType.INFORMATION)
            .addAction(NotificationAction.create("保存場所を開く") { _, event ->
                if (alive) openDirectory(saved.file.parent) else event.expire()
            })
            .addAction(NotificationAction.create("保存したファイルを削除") { _, event ->
                if (!alive) { event.expire(); return@create }
                if (!deleting.compareAndSet(false, true)) return@create
                scope.launch(Dispatchers.IO) {
                    try {
                        saved.delete()
                        onUi {
                            event.expire()
                            showNotification(NotificationGroupManager.getInstance().getNotificationGroup("Android Replay Buffer")
                                .createNotification("スクリーンショットを削除しました", "保存フォルダと他のファイルは残しています。", NotificationType.INFORMATION))
                        }
                    } catch (failure: Exception) {
                        deleting.set(false)
                        screenshotError("保存したファイルを削除できません。消失・変更・アクセス権を確認してください。",
                            (failure as? ScreenshotSaveFailure)?.retained ?: saved.file.parent)
                    }
                }
            })
        showNotification(notification)
    }

    private fun screenshotError(reason: String, retained: Path? = null) = onUi {
        val notification = NotificationGroupManager.getInstance().getNotificationGroup("Android Replay Buffer")
            .createNotification("スクリーンショットの操作を完了できません", StringUtil.escapeXmlEntities(reason), NotificationType.ERROR)
        if (retained != null) notification.addAction(copyPathAction(retained))
        showNotification(notification)
    }

    private fun rejected(reason: String): CompletableFuture<ReplayOperation> {
        if (alive) capture.showMessage(reason)
        return CompletableFuture.completedFuture(ReplayOperation(false, reason))
    }

    private fun importLegacy(config: LegacyCliConfig): CompletableFuture<ReplayOperation> {
        val validated = validateDestination(config.destination)
        return synchronized(this) {
            if (!alive) return rejected("projectは終了しています。")
            if (resolving || capture.view.pending > 0) return rejected("設定・操作の反映後に旧設定を移行してください。")
            val store = project.getService(ReplaySettingsStore::class.java)
            store.importLegacy(config.seconds, config.destination, validated)
            refresh()
            val completed = CompletableFuture<ReplayOperation>()
            val revision = requestRevision
            resolution!!.invokeOnCompletion { failure ->
                synchronized(this) {
                    val current = capture.view.snapshot?.settings
                    completed.complete(if (failure == null && alive && revision == requestRevision &&
                        current?.replaySeconds == config.seconds && current.saveDirectory?.toString() == config.destination) ReplayOperation(true)
                    else ReplayOperation(false, "旧設定は保存しましたが反映を確認できません。GUIの設定・状態を確認してください。"))
                }
            }
            completed
        }
    }

    /** Runs only on the CLI worker, never EDT; GUI and CLI enter the same command queue. */
    private fun executeCli(request: CliRequest): CliResponse {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(120)
        var requestId: String? = null
        fun await(operation: CompletableFuture<ReplayOperation>): ReplayOperation = operation.get(
            (deadline - System.nanoTime()).coerceAtLeast(1), TimeUnit.NANOSECONDS)
        try {
            if (!alive) return CliResponse(1, error = "projectは終了しています。")
            var warning = ""
            if (request.configText != null) {
                val config = LegacyCliConfig.parse(request.configText, request.yaml, Path.of(request.workingDirectory))
                val imported = await(importLegacy(config))
                if (!imported.accepted) return CliResponse(1, error = imported.reason.orEmpty())
                warning = config.warning
            }
            if (request.command == CliCommand.STATUS) {
                val view = capture.view
                val snapshot = view.snapshot
                return CliResponse(0, Gson().toJson(linkedMapOf(
                    "project" to projectBasePath, "running" to (snapshot?.enabled == true),
                    "capture_state" to snapshot?.captureState?.name, "capture_mode" to snapshot?.settings?.captureMode?.name,
                    "device_name" to snapshot?.device?.name, "device_serial" to snapshot?.device?.serial,
                    "error" to (view.initializationError ?: snapshot?.error),
                    "pending" to view.pending, "resolving" to resolving, "save_state" to snapshot?.save?.phase?.name,
                    "request_id" to snapshot?.save?.requestId, "replay_seconds" to snapshot?.settings?.replaySeconds,
                    "output_directory" to snapshot?.settings?.saveDirectory?.toString(),
                    "last_save" to snapshot?.lastSave?.let { last -> linkedMapOf(
                        "phase" to last.phase.name, "request_id" to last.requestId, "attempt" to last.attempt,
                        "directory" to last.directory?.takeIf { last.phase == SavePhase.COMPLETED }?.toString(),
                        "retained_path" to last.retainedPath?.toString(),
                        "error" to last.error, "missing_kinds" to last.missingKinds,
                    ) },
                    "reason" to (view.message ?: view.initializationError ?: snapshot?.saveDisabledReason),
                )))
            }
            if (request.command == CliCommand.SAVE && capture.view.snapshot?.enabled != true) {
                val enabled = await(setEnabled(true))
                if (!enabled.accepted) return CliResponse(1, error = enabled.reason.orEmpty())
            }
            val operation = await(when (request.command) {
                CliCommand.SAVE -> save()
                CliCommand.DAEMON -> setEnabled(true)
                CliCommand.STOP -> setEnabled(false)
                CliCommand.STATUS -> error("status already handled")
            })
            if (!operation.accepted) return CliResponse(1, error = listOf(warning, operation.reason.orEmpty()).filter(String::isNotEmpty).joinToString("\n"))
            if (request.command != CliCommand.SAVE) return CliResponse(0,
                if (request.command == CliCommand.STOP) "stopped" else "started", warning)
            requestId = operation.requestId
            return cliSaveResponse(operation.requestId, checkNotNull(operation.saveResult), deadline, warning)
        } catch (_: TimeoutException) {
            return CliResponse(1, error = "操作結果の待機が時間切れになりました${requestId?.let { "（request: $it）" }.orEmpty()}。GUIの状態・保存結果を確認してください。自動再試行はしません。")
        } catch (failure: IllegalArgumentException) {
            return CliResponse(2, error = failure.message ?: "入力内容を確認してください。")
        } catch (_: Exception) {
            return CliResponse(1, error = "projectの終了または操作中のエラーで結果を確認できません。GUIの状態・保存結果を確認してください。")
        }
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
                (capture.view.snapshot?.save?.requestId != save.requestId || capture.view.snapshot?.save?.attempt != save.attempt ||
                    capture.view.snapshot?.save?.phase != SavePhase.FAILED)) return@onUi
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

    private fun closeCliServer(server: ReplayCliServer) {
        runCatching { server.close() }.onFailure {
            Logger.getInstance(ReplayProjectService::class.java).warn("CLI接続の停止を確認できません。取得本体の終了処理を続けます。")
        }
        server.termination.whenComplete { _, failure ->
            if (failure != null) Logger.getInstance(ReplayProjectService::class.java).warn("CLI入口の終了・接続情報の削除を確認できません。所有情報を保全します。")
        }
    }

    @Synchronized
    override fun dispose() {
        disposed = true
        cliServer?.let(::closeCliServer)
        cliServer = null
        requestRevision++
        resolution?.cancel()
        resolution = null
        screenshotJob?.cancel()
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
