package io.github.shinma06.replaybuffer.core

import java.nio.file.Files
import java.nio.file.Path
import java.util.UUID
import java.util.concurrent.CompletableFuture
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.Executors
import java.util.concurrent.Future
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

/** Project-owned concrete core. All device/disk work and notifications run outside EDT. */
class ReplayCore(initialSettings: ReplaySettings, private val workspace: Path, cleanupDirectory: Path = workspace) : AutoCloseable {
    private val control = Executors.newSingleThreadScheduledExecutor { r -> Thread(r, "replay-control").apply { isDaemon = true } }
    private val writer = Executors.newSingleThreadExecutor { r -> Thread(r, "replay-save").apply { isDaemon = true } }
    private val listeners = CopyOnWriteArrayList<(ReplaySnapshot) -> Unit>()
    private val closed = AtomicBoolean()
    private val closeCompletion = CompletableFuture<Void>()
    private val publishing = Any()
    private var settings = initialSettings.validate()
    private var settingsRevision = 0L
    private var revision = 0L
    private var generation = 0L
    private var enabled = false
    private var device: ReplayDevice? = null
    private var store: CaptureStore? = null
    private var resources: CaptureResources? = null
    private var ownedRoot: Path? = null
    private var backend: DeviceCapture? = null
    private var backendStarted = 0L
    private var probe: OwnedAdb? = null
    private val cleanup = RemoteCleanupJournal(cleanupDirectory)
    private var captureState = CaptureState.DISABLED
    private var error: String? = null
    private var saveState = SaveSnapshot()
    private var pending: FrozenCapture? = null
    private var saveTask: Future<*>? = null
    private var cancelSave = AtomicBoolean()
    @Volatile private var current = ReplaySnapshot(0, 0, false, CaptureState.DISABLED, settings, 0,
        saveDisabledReason = "取得を有効にしてください")

    init {
        require(workspace.isAbsolute) { "一時領域は絶対パスで指定してください" }
        control.execute { if (!closed.get()) runCatching { cleanup.recover(); publish() }.onFailure {
            error = "端末側の終了情報を読み戻せません（記録を保全しました）"; publish()
        } }
        control.scheduleWithFixedDelay({ if (!closed.get()) runCatching { poll() }.onFailure {
            error = "取得状態の確認に失敗しました"; publish()
        } }, 1, 1, TimeUnit.SECONDS)
    }

    fun snapshot(): ReplaySnapshot = current

    fun subscribe(listener: (ReplaySnapshot) -> Unit): AutoCloseable {
        if (closed.get()) return AutoCloseable { }
        listeners += listener
        runCatching { control.execute { if (!closed.get() && listener in listeners) runCatching { listener(current) } } }
            .onFailure { listeners -= listener }
        return AutoCloseable { listeners -= listener }
    }

    fun setEnabled(value: Boolean): CompletableFuture<ReplayOperation> {
        if (!value) synchronized(publishing) { cancelSave.set(true) }
        return operation {
            if (value == enabled) return@operation ReplayOperation(true)
            if (value) {
                cleanup.recover()
                val root = Files.createDirectories(workspace).resolve("replay-${UUID.randomUUID()}")
                Files.createDirectory(root)
                runCatching { Files.setPosixFilePermissions(root, java.nio.file.attribute.PosixFilePermissions.fromString("rwx------")) }
                ownedRoot = root
                try {
                    resources = CaptureResources(root)
                    store = CaptureStore(root.resolve("ring"))
                } catch (e: Exception) {
                    resources?.close(); resources = null
                    // Extraction writes only these exact new names; never clean unrelated entries.
                    Files.deleteIfExists(root.resolve("scrcpy-server-v4.0"))
                    Files.deleteIfExists(root.resolve("replay-clock.jar"))
                    Files.delete(root); ownedRoot = null
                    throw e
                }
                enabled = true
                captureState = CaptureState.WAITING
                error = null
                saveState = SaveSnapshot()
                probe = settings.adbPath?.let { OwnedAdb(it) }
                poll()
            } else disable()
            publish()
            ReplayOperation(true)
        }
    }

    fun applySettings(value: ReplaySettings): CompletableFuture<ReplayOperation> = operation {
        val applied = value.validate()
        val changedAdb = applied.adbPath != settings.adbPath
        val changedApp = applied.application != settings.application
        settings = applied
        settingsRevision++
        if (changedAdb) {
            stopBackend()
            probe?.close(); probe = if (enabled) settings.adbPath?.let { OwnedAdb(it) } else null
        }
        if (changedApp) backend?.application(applied.application)
        store?.prune(settings.replaySeconds)
        poll()
        ReplayOperation(true)
    }

    /** Run selection updates AUTO only; manual override remains authoritative until explicitly applied. */
    fun updateApplication(value: ApplicationTarget): CompletableFuture<ReplayOperation> = operation {
        value.validate()
        if (settings.application.mode == ApplicationMode.AUTO) {
            settings = settings.copy(application = value.copy(mode = ApplicationMode.AUTO))
            settingsRevision++
            backend?.application(settings.application)
            publish()
        }
        ReplayOperation(true)
    }

    fun save(): CompletableFuture<ReplayOperation> = operation {
        publish()
        if (!current.canSave) return@operation ReplayOperation(false, current.saveDisabledReason)
        val capture = store?.capture(settings)?.copy(device = device) ?: return@operation ReplayOperation(false, "保存できる取得データがありません")
        pending = capture
        startSave(capture, settings.saveDirectory!!)
        ReplayOperation(true, requestId = capture.id)
    }

    fun retry(requestId: String): CompletableFuture<ReplayOperation> = operation {
        val capture = failedRequest(requestId) ?: return@operation ReplayOperation(false, "再試行できる保存対象がありません")
        startSave(capture, saveState.directory ?: settings.saveDirectory ?: return@operation ReplayOperation(false, "保存先を指定してください"))
        ReplayOperation(true, requestId = capture.id)
    }

    fun retryAtDirectory(requestId: String, directory: Path): CompletableFuture<ReplayOperation> = operation {
        require(directory.isAbsolute) { "保存先は絶対パスで指定してください" }
        val capture = failedRequest(requestId) ?: return@operation ReplayOperation(false, "再試行できる保存対象がありません")
        startSave(capture, directory.normalize())
        ReplayOperation(true, requestId = capture.id)
    }

    fun discard(requestId: String): CompletableFuture<ReplayOperation> = operation {
        val capture = failedRequest(requestId) ?: return@operation ReplayOperation(false, "破棄できる保存対象がありません")
        store?.release(capture.id)
        pending = null
        saveState = SaveSnapshot()
        publish()
        ReplayOperation(true, requestId = requestId)
    }

    private fun failedRequest(id: String): FrozenCapture? = pending?.takeIf { enabled && it.id == id && saveState.phase == SavePhase.FAILED }

    private fun startSave(capture: FrozenCapture, directory: Path) {
        cancelSave = AtomicBoolean()
        val cancellation = cancelSave
        val missing = (capture.states.filterValues { it.state != StreamState.CAPTURING || it.reason != null }.keys +
            capture.gaps.filter { gap -> !capture.windowKnown || gap.intersects(capture.start, capture.end, capture.endUncertainty) }.flatMap { if (it.stream == "clock") listOf("video", "device_log", "app_log") else listOf(it.stream) } +
            if (capture.logs.any { it.app == null }) listOf("app_log") else emptyList()).distinct().let { java.util.List.copyOf(it) }
        saveState = SaveSnapshot(SavePhase.WRITING, capture.id, capture.sequence, capture.start, capture.end,
            capture.seconds, directory, missingKinds = missing, device = capture.device,
            application = capture.settings.application, applicationHistory = capture.applicationHistory())
        publish()
        saveTask = writer.submit {
            val result = runCatching { SaveWriter().write(capture, directory, { cancellation.get() || closed.get() }) { partial, complete ->
                synchronized(publishing) {
                    check(!cancellation.get() && !closed.get()) { "保存が取消されました" }
                    check(!Files.exists(complete)) { "完成先が既に存在します" }
                    publishCapture(partial, complete)
                }
            } }
            if (!closed.get()) runCatching { control.execute {
                if (!cancellation.get() && pending?.id == capture.id && enabled) {
                    if (result.isSuccess) {
                        store?.release(capture.id)
                        pending = null
                        val output = result.getOrThrow()
                        saveState = saveState.copy(phase = SavePhase.COMPLETED, directory = output.directory, error = null,
                            missingKinds = java.util.List.copyOf((saveState.missingKinds + output.missingKinds).distinct()))
                    } else {
                        saveState = saveState.copy(phase = SavePhase.FAILED,
                            error = (result.exceptionOrNull() as? SaveFailure)?.message ?: "保存に失敗しました")
                    }
                    publish()
                }
            } }
        }
    }

    private fun poll() {
        if (closed.get()) return
        if (!enabled) {
            cleanup.recover()
            val executable = settings.adbPath
            // OFF never starts acquisition. No verified owned pending record means no adb invocation at all.
            if (cleanup.hasRecoverable && executable != null) {
                val devices = runCatching { OwnedAdb(executable).use { parseDevices(it.command("devices", "-l")) } }
                if (closed.get()) return
                devices.onSuccess { cleanup.clean(executable, it.map { device -> device.serial }.toSet(), closed::get) }
            }
            if (cleanup.pendingCount > 0) error = "端末側の終了・cleanupを再接続時に確認します（所有情報を保全しています）"
            else if (current.cleanupPendingCount > 0) error = null
            publish()
            return
        }
        val adb = probe
        if (adb == null) {
            captureState = CaptureState.WAITING
            error = "Android SDKのadbを解決できません"
            publish(); return
        }
        val devices = runCatching { parseDevices(adb.command("devices", "-l")) }.getOrElse {
            captureState = if (backend != null) CaptureState.PARTIAL else CaptureState.RECOVERING
            error = "SDK adbから端末状態を取得できません"
            publish(); return
        }
        if (closed.get()) return
        cleanup.recover()
        val selected = device?.let { current -> devices.firstOrNull { it.serial == current.serial } }
        if (device == null && devices.size != 1 || device != null && selected == null) {
            stopBackend()
            device = device?.copy(connected = false)
            captureState = if (devices.size > 1) CaptureState.MULTIPLE_DEVICES else if (device != null) CaptureState.RECOVERING else CaptureState.WAITING
            error = if (devices.size > 1) "端末を1台だけ接続してください" else if (devices.size == 1) "最初に選択した端末の再接続を待っています" else null
            cleanup.clean(settings.adbPath!!, devices.map { it.serial }.toSet(), closed::get)
            publish(); return
        }
        if (device == null) device = devices.single() else device = device?.copy(connected = true)
        if (backend == null) {
            val data = store ?: return
            generation++
            data.generation(generation)
            backendStarted = System.nanoTime()
            backend = DeviceCapture(settings.adbPath!!, device!!.serial, resources!!, data, generation, settings.application).also { it.start() }
        }
        val data = store!!
        if (data.clock.snapshot().lastOrNull()?.received?.let { it >= backendStarted } == true) data.resume()
        backend?.reportVideoHealth()
        data.prune(settings.replaySeconds)
        val states = data.streams(settings.replaySeconds)
        captureState = when {
            devices.size > 1 -> CaptureState.MULTIPLE_DEVICES
            states.values.all { it.state == StreamState.CAPTURING && it.reason == null } -> CaptureState.CAPTURING
            states["video"]?.state == StreamState.CAPTURING || states["device_log"]?.state == StreamState.CAPTURING -> CaptureState.PARTIAL
            else -> CaptureState.RECOVERING
        }
        error = if (devices.size > 1) "初期版は1台に対応しています。ほかの端末を切断してください（既存の取得は継続）" else null
        publish()
        val pendingBeforeCleanup = cleanup.pendingCount
        cleanup.clean(settings.adbPath!!, devices.map { it.serial }.toSet(), closed::get)
        if (cleanup.pendingCount != pendingBeforeCleanup) publish()
    }

    private fun stopBackend() {
        val capture = backend ?: return
        store?.freeze()
        listOf("video", "device_log", "app_log").forEach { store?.status(it, StreamState.RECOVERING, "端末との取得接続が中断しています", generation) }
        capture.close()
        if (capture.cleanupPending) cleanup.retain(capture.cleanupRecord())
        backend = null
    }

    private fun disable() {
        synchronized(publishing) { cancelSave.set(true) }
        stopBackend()
        probe?.close(); probe = null
        // Packet files are removed only after their pinned reader has really finished.
        saveTask?.get(10, TimeUnit.SECONDS)
        saveTask = null
        pending = null
        store?.close(); store = null
        resources?.close(); resources = null
        ownedRoot?.let { Files.delete(it) }; ownedRoot = null
        device = null
        enabled = false
        generation++
        captureState = CaptureState.DISABLED
        saveState = SaveSnapshot()
        error = if (cleanup.pendingCount == 0) null else "端末側の終了・cleanupは再接続時に確認します（所有情報を保全しました）"
    }

    private fun publish() {
        if (closed.get()) return
        val data = store
        val streams = data?.streams(settings.replaySeconds).orEmpty()
        val disabled = when {
            !enabled -> "取得を有効にしてください"
            pending != null -> if (saveState.phase == SavePhase.WRITING) "保存中です" else "固定した保存対象を再試行または破棄してください"
            settings.saveDirectory == null -> "保存先を設定してください"
            data?.hasData() != true -> "保存できる取得データがありません"
            else -> null
        }
        current = ReplaySnapshot(++revision, generation, enabled, captureState, settings, settingsRevision,
            device, data?.sequence, data?.end(), data?.frozen() ?: false, streams["video"] ?: StreamSnapshot(),
            streams["device_log"] ?: StreamSnapshot(), streams["app_log"] ?: StreamSnapshot(), saveState,
            disabled == null, disabled, error, windowStartNs = data?.windowStart(settings.replaySeconds),
            cleanupPendingCount = cleanup.pendingCount)
        listeners.forEach { if (!closed.get()) runCatching { it(current) } }
    }

    private fun operation(action: () -> ReplayOperation): CompletableFuture<ReplayOperation> {
        if (closed.get()) return CompletableFuture.completedFuture(ReplayOperation(false, "projectは終了しています"))
        val result = CompletableFuture<ReplayOperation>()
        try { control.execute {
            if (closed.get()) result.complete(ReplayOperation(false, "projectは終了しています")) else {
                runCatching(action).onSuccess { result.complete(it) }.onFailure {
                    error = if (it is IllegalArgumentException || it is IllegalStateException) it.message else "取得/保存操作に失敗しました"
                    publish()
                    result.complete(ReplayOperation(false, error))
                }
            }
        }
        } catch (_: java.util.concurrent.RejectedExecutionException) { result.complete(ReplayOperation(false, "projectは終了しています")) }
        return result
    }

    /** Returns immediately; actual process termination/deletion continues on replay-control. */
    fun closeAsync(): CompletableFuture<Void> {
        if (!closed.compareAndSet(false, true)) return closeCompletion
        synchronized(publishing) { cancelSave.set(true) }
        listeners.clear()
        val done = closeCompletion
        control.execute {
            runCatching { try { disable() } finally { cleanup.close() } }.onSuccess {
                current = current.copy(enabled = false, captureState = CaptureState.DISABLED, canSave = false,
                    saveDisabledReason = "projectは終了しています", cleanupPendingCount = cleanup.pendingCount, error = error)
                done.complete(null)
            }.onFailure { done.completeExceptionally(it) }
            writer.shutdown()
            control.shutdown()
        }
        return done
    }

    override fun close() { closeAsync() }
}
