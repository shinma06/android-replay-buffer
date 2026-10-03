package io.github.shinma06.replaybuffer.ide

import com.android.tools.idea.gradle.project.sync.GradleSyncListener
import com.android.tools.idea.gradle.project.sync.GradleSyncState
import com.intellij.execution.RunManagerListener
import com.intellij.execution.RunnerAndConfigurationSettings
import com.intellij.openapi.Disposable
import com.intellij.openapi.components.Service
import com.intellij.openapi.progress.ProcessCanceledException
import com.intellij.openapi.project.DumbAware
import com.intellij.openapi.project.DumbService
import com.intellij.openapi.project.Project
import com.intellij.openapi.roots.ModuleRootEvent
import com.intellij.openapi.roots.ModuleRootListener
import com.intellij.openapi.startup.ProjectActivity
import com.intellij.util.messages.Topic
import io.github.shinma06.replaybuffer.settings.ReplaySettingsStore
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch

/** Owns IDE subscriptions and background model resolution independently of the ToolWindow. */
@Service(Service.Level.PROJECT)
class ReplayProjectService(private val project: Project, private val scope: CoroutineScope) : Disposable {
    @Volatile
    var environment: AndroidReplayEnvironment? = null
        private set
    private var requestRevision = 0L
    private var disposed = false
    private var resolution: Job? = null

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
        connection.subscribe(DumbService.DUMB_MODE, object : DumbService.DumbModeListener {
            override fun exitDumbMode() = refresh()
        })
        GradleSyncState.subscribe(project, object : GradleSyncListener {
            override fun syncStarted(project: Project) = refresh()
            override fun syncSucceeded(project: Project) = refresh()
            override fun syncFailed(project: Project, errorMessage: String) = refresh()
            override fun syncSkipped(project: Project) = refresh()
        }, this)
    }

    @Synchronized
    fun refresh() {
        if (disposed || project.isDisposed) return
        val revision = ++requestRevision
        resolution?.cancel()
        val settings = project.getService(ReplaySettingsStore::class.java).settings()
        resolution = scope.launch {
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
                    ApplicationSelection(null, null, "対象アプリの情報を取得できません。設定でpackage名を指定できます。"),
                )
            }
            synchronized(this@ReplayProjectService) {
                if (disposed || project.isDisposed || revision != requestRevision) return@launch
                environment = next
                project.messageBus.syncPublisher(ENVIRONMENT_CHANGED).run()
            }
        }
    }

    @Synchronized
    override fun dispose() {
        disposed = true
        requestRevision++
        resolution?.cancel()
        resolution = null
    }

    companion object {
        val ENVIRONMENT_CHANGED: Topic<Runnable> = Topic.create("Android Replay Buffer environment changed", Runnable::class.java)
    }
}

class ReplayProjectActivity : ProjectActivity, DumbAware {
    override suspend fun execute(project: Project) {
        project.getService(ReplayProjectService::class.java).refresh()
    }
}
