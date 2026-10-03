package io.github.shinma06.replaybuffer.ide

import io.github.shinma06.replaybuffer.core.ApplicationMode
import io.github.shinma06.replaybuffer.core.ApplicationTarget
import io.github.shinma06.replaybuffer.core.ReplayCore
import io.github.shinma06.replaybuffer.core.ReplayOperation
import io.github.shinma06.replaybuffer.core.ReplaySnapshot
import io.github.shinma06.replaybuffer.core.ReplaySettings as CoreSettings
import io.github.shinma06.replaybuffer.core.SavePhase
import io.github.shinma06.replaybuffer.core.SaveSnapshot
import io.github.shinma06.replaybuffer.settings.AppSelectionMode
import io.github.shinma06.replaybuffer.settings.ReplaySettings
import io.github.shinma06.replaybuffer.settings.ReplaySettingsStore
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.future.await
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.CompletableFuture

internal data class ReplayCaptureView(
    val snapshot: ReplaySnapshot? = null,
    val pending: Int = 0,
    val message: String? = null,
    val initializationError: String? = null,
)

/** One project's concrete core owner. No Swing, SDK lookup, or device selection is performed here. */
internal class ReplayProjectCapture(
    private val scope: CoroutineScope,
    private val store: ReplaySettingsStore,
    private val changed: () -> Unit,
    private val saved: (SaveSnapshot) -> Unit,
    cleanupDirectory: (() -> Path)? = null,
) : AutoCloseable {
    @Volatile var view = ReplayCaptureView()
        private set
    private val ready = CompletableDeferred<ReplayCore>()
    private val commands = Mutex()
    private var core: ReplayCore? = null
    private var workspace: Path? = null
    private var subscription: AutoCloseable? = null
    private var closed = false
    private var closingStarted = false
    private var initializationFinished = false
    private var enabledIntent = 0L
    private var operationIntent = 0L
    private var settingsRevision = 0L
    val termination = CompletableFuture<Void>()

    init {
        val initialization = scope.launch(Dispatchers.IO) {
            var created: ReplayCore? = null
            var directory: Path? = null
            try {
                // The default temp directory is private to this new project session (0700 on Unix).
                directory = Files.createTempDirectory("android-replay-buffer-")
                val engine = ReplayCore(CoreSettings(), directory, cleanupDirectory?.invoke() ?: directory)
                created = engine
                val retained = synchronized(this@ReplayProjectCapture) {
                    if (closed) false else {
                        workspace = directory
                        core = engine
                        subscription = engine.subscribe(::acceptSnapshot)
                        ready.complete(engine)
                        true
                    }
                }
                if (!retained) finishClose(engine, directory)
            } catch (failure: Throwable) {
                if (created != null) finishClose(created, directory) else directory?.let { Files.deleteIfExists(it) }
                val message = "取得の初期化に失敗しました。projectの保存場所とIDE設定領域へのアクセス権を確認し、projectを開き直してください。"
                synchronized(this@ReplayProjectCapture) {
                    if (!closed) {
                        view = view.copy(initializationError = message)
                        if (enabledIntent == 0L) store.setEnabled(false)
                    }
                }
                showMessage(message)
                ready.completeExceptionally(failure)
            }
        }
        initialization.invokeOnCompletion { failure ->
            if (failure != null) ready.completeExceptionally(failure)
            synchronized(this) {
                initializationFinished = true
                if (closed && core == null && !closingStarted && !termination.isDone) termination.complete(null)
            }
        }
    }

    fun setEnabled(value: Boolean): CompletableFuture<ReplayOperation> {
        val intent = synchronized(this) {
            if (closed) return CompletableFuture.completedFuture(ReplayOperation(false, "projectは終了しています。"))
            store.setEnabled(value)
            ++enabledIntent
        }
        return submit { engine -> engine.setEnabled(value).await() }.whenComplete { result, _ ->
            val notify = synchronized(this) {
                if (!closed && intent == enabledIntent && result?.accepted == false) {
                    store.setEnabled(core?.snapshot()?.enabled == true)
                    true
                } else false
            }
            if (notify) changed()
        }
    }

    @Synchronized
    fun expectSettings(revision: Long) {
        settingsRevision = revision
    }

    fun applySettings(settings: ReplaySettings, environment: AndroidReplayEnvironment, revision: Long): CompletableFuture<ReplayOperation> = submit(revision) { engine ->
        settings.validationError()?.let { return@submit settingsFailure(engine, revision, it) }
        val target = settings.toCoreSettings(environment)
        // Only the nonblocking core enqueue is inside this boundary. No callback, IO, or await
        // can reenter refresh between checking the generation and submitting its settings.
        val operation = synchronized(this) {
            if (closed || revision != settingsRevision) return@submit obsoleteSettings()
            val current = engine.snapshot().settings
            when {
                target == current -> CompletableFuture.completedFuture(ReplayOperation(true))
                current.application.mode == ApplicationMode.AUTO && target.application.mode == ApplicationMode.AUTO &&
                    current.copy(application = target.application) == target -> engine.updateApplication(target.application)
                else -> engine.applySettings(target)
            }
        }
        val result = operation.await()
        if (synchronized(this) { closed || revision != settingsRevision }) return@submit obsoleteSettings()
        if (!result.accepted) return@submit settingsFailure(engine, revision, result.reason ?: "旧設定で稼働しています。")
        // Startup restoration is independent of ToolWindow creation. Later refreshes honor the latest toggle.
        val restored = if (engine.snapshot().enabled != store.enabled) engine.setEnabled(store.enabled).await() else result
        if (restored.accepted) restored else settingsFailure(engine, revision, restored.reason ?: "取得を復元できません。")
    }

    private fun settingsFailure(engine: ReplayCore, revision: Long, reason: String): ReplayOperation = synchronized(this) {
        if (closed || revision != settingsRevision) return@synchronized obsoleteSettings()
        // Only reconcile the persisted startup intent. A later user toggle owns its own result.
        if (enabledIntent == 0L) store.setEnabled(engine.snapshot().enabled)
        ReplayOperation(false, "保存済み設定を反映できません: $reason")
    }

    private fun obsoleteSettings() = ReplayOperation(false, "新しい設定の解決が開始されたため、旧設定の反映を取り消しました。")

    fun save(): CompletableFuture<ReplayOperation> = submit { it.save().await() }
    fun retry(id: String): CompletableFuture<ReplayOperation> = submit { it.retry(id).await() }
    fun retryAtDirectory(id: String, directory: Path): CompletableFuture<ReplayOperation> = submit { engine ->
        ReplaySettings(destination = directory.toString()).validationError()?.let { return@submit ReplayOperation(false, it) }
        engine.retryAtDirectory(id, directory).await()
    }
    fun discard(id: String): CompletableFuture<ReplayOperation> = submit { it.discard(id).await() }

    private fun submit(revision: Long? = null, action: suspend (ReplayCore) -> ReplayOperation): CompletableFuture<ReplayOperation> {
        val result = CompletableFuture<ReplayOperation>()
        val intent = synchronized(this) {
            if (closed) return CompletableFuture.completedFuture(ReplayOperation(false, "projectは終了しています。"))
            if (revision != null && revision != settingsRevision) return CompletableFuture.completedFuture(obsoleteSettings())
            view = view.copy(pending = view.pending + 1, message = null)
            ++operationIntent
        }
        changed()
        // Enter the FIFO mutex immediately, but run filesystem validation and all waits on IO.
        scope.launch(start = CoroutineStart.UNDISPATCHED) {
            var outcome: ReplayOperation? = null
            try {
                outcome = commands.withLock {
                    val engine = ready.await()
                    withContext(Dispatchers.IO) {
                        if (synchronized(this@ReplayProjectCapture) { closed }) ReplayOperation(false, "projectは終了しています。")
                        else action(engine)
                    }
                }
                if (!outcome.accepted) showMessage(outcome.reason ?: "操作を実行できません。", intent, revision)
            } catch (cancelled: CancellationException) {
                result.completeExceptionally(cancelled)
                throw cancelled
            } catch (_: Exception) {
                val message = view.initializationError ?: "取得・保存の操作を実行できません。"
                showMessage(message, intent, revision)
                outcome = ReplayOperation(false, message)
            } finally {
                val notify = synchronized(this@ReplayProjectCapture) {
                    view = view.copy(pending = (view.pending - 1).coerceAtLeast(0))
                    !closed
                }
                if (notify) changed()
                outcome?.let(result::complete)
            }
        }
        return result
    }

    internal fun acceptSnapshot(next: ReplaySnapshot) {
        val terminal = synchronized(this) {
            val previous = view.snapshot
            if (closed || previous != null && next.revision <= previous.revision) return
            view = view.copy(snapshot = next)
            next.save.takeIf {
                it.phase in setOf(SavePhase.COMPLETED, SavePhase.FAILED) &&
                    (previous?.save?.requestId != it.requestId || previous?.save?.phase != it.phase)
            }
        }
        changed()
        terminal?.let(saved)
    }

    fun showMessage(message: String) = showMessage(message, null)

    private fun showMessage(message: String, intent: Long?, revision: Long? = null) {
        synchronized(this) {
            if (closed || intent != null && intent != operationIntent || revision != null && revision != settingsRevision) return
            if (intent == null) operationIntent++
            view = view.copy(message = message)
        }
        changed()
    }

    override fun close() {
        val active = synchronized(this) {
            if (closed) return
            closed = true
            subscription?.close()
            subscription = null
            ready.cancel()
            if (core == null && initializationFinished && !closingStarted) termination.complete(null)
            core
        }
        if (active != null) finishClose(active, workspace)
    }

    private fun finishClose(engine: ReplayCore, directory: Path?) {
        synchronized(this) { closingStarted = true }
        engine.closeAsync().whenComplete { _, failure ->
            if (failure != null) termination.completeExceptionally(failure) else {
                try {
                    directory?.let { Files.deleteIfExists(it) }
                    termination.complete(null)
                } catch (cleanup: Exception) {
                    // Retain pending remote-cleanup ownership and never delete unconfirmed data recursively.
                    termination.completeExceptionally(cleanup)
                }
            }
        }
    }
}

internal fun ReplaySettings.toCoreSettings(environment: AndroidReplayEnvironment): CoreSettings = CoreSettings(
    adbPath = environment.adb,
    saveDirectory = destination.takeIf(String::isNotEmpty)?.let(Path::of),
    replaySeconds = retentionSeconds,
    application = if (appSelection == AppSelectionMode.MANUAL) {
        ApplicationTarget(manualPackage, ApplicationMode.MANUAL)
    } else {
        ApplicationTarget(environment.application.packageName, ApplicationMode.AUTO, environment.application.reason)
    },
)
