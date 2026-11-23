package io.github.shinma06.arb.ui

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import com.intellij.ide.actions.RevealFileAction
import com.intellij.notification.NotificationGroupManager
import com.intellij.notification.NotificationType
import com.intellij.openapi.Disposable
import com.intellij.openapi.diagnostic.Logger
import com.intellij.openapi.fileChooser.FileChooser
import com.intellij.openapi.fileChooser.FileChooserDescriptorFactory
import com.intellij.openapi.project.Project
import io.github.shinma06.arb.ide.ArbDaemonStateListener
import io.github.shinma06.arb.ide.ArbScreenshotDestinationResolver
import io.github.shinma06.arb.ide.ArbSettingsConfigurable
import io.github.shinma06.arb.ide.ArbdDaemonService
import io.github.shinma06.arb.model.AdbDeviceInfo
import io.github.shinma06.arb.model.AdbDeviceState
import io.github.shinma06.arb.model.ArbConfig
import io.github.shinma06.arb.model.BundleKind
import io.github.shinma06.arb.model.CaptureStartParams
import io.github.shinma06.arb.model.IdeDefaultSaveAction
import io.github.shinma06.arb.model.SaveLogsParams
import io.github.shinma06.arb.model.SaveScreenshotParams
import io.github.shinma06.arb.model.SaveTriggerParams
import io.github.shinma06.arb.state.ArbFormatters
import io.github.shinma06.arb.state.BundleCatalogItem
import io.github.shinma06.arb.state.BundleCatalogState
import io.github.shinma06.arb.state.BundleFilter
import io.github.shinma06.arb.state.CaptureBarReducer
import io.github.shinma06.arb.state.CaptureBarState
import io.github.shinma06.arb.state.FailedSaveRecoveryReducer
import io.github.shinma06.arb.state.FailedSaveRecoveryState
import java.nio.file.Path
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import javax.swing.SwingUtilities

/**
 * Pure Kotlin + Compose state controller for the `ARB` Tool Window.
 *
 * Owns 2-second live background polling (`system.status`, `config.get`, `device.list`,
 * `save.list_jobs`, `bundle.list`), `ArbDaemonStateListener` updates, stale-`arbd` detection
 * and 1-click restart, bundled `arb-tray` launching, and screenshot destination resolution.
 * Fully headless-testable with JUnit 5 without requiring Skiko or a live IDE frame.
 */
class ArbToolWindowController(
    private val project: Project?,
    private val daemonService: ArbdDaemonService,
    private val onOpenBundle: (bundleIdOrPath: String, title: String, isLegacy: Boolean) -> Unit,
    private val onOpenSettings: () -> Unit = {},
    private val pollIntervalMs: Long = DEFAULT_POLL_INTERVAL_MS,
) : Disposable {
    private val logger = Logger.getInstance(ArbToolWindowController::class.java)
    private val executor = Executors.newSingleThreadScheduledExecutor { r ->
        Thread(r, "arb-toolwindow-worker").apply { isDaemon = true }
    }
    private val captureStateLock = Any()

    @Volatile
    private var disposed = false

    @Volatile
    private var rawCaptureState = CaptureBarState()

    @Volatile
    private var rawRecoveryState = FailedSaveRecoveryState()

    @Volatile
    private var rawCatalogState = BundleCatalogState()

    @Volatile
    private var activeFilterBacking = BundleFilter.ALL

    private data class BundleMeta(val kind: BundleKind, val durationSecs: Long?, val crashed: Boolean)

    private val metaCache = ConcurrentHashMap<String, BundleMeta>()

    var captureState: CaptureBarState by mutableStateOf(CaptureBarState())
        private set

    var recoveryState: FailedSaveRecoveryState by mutableStateOf(FailedSaveRecoveryState())
        private set

    var catalogState: BundleCatalogState by mutableStateOf(BundleCatalogState())
        private set

    var activeFilter: BundleFilter by mutableStateOf(BundleFilter.ALL)
        private set

    var catalogNote: String? by mutableStateOf(null)
        private set

    var staleDaemonMessage: String? by mutableStateOf(null)
        private set

    var daemonRestartBusy: Boolean by mutableStateOf(false)
        private set

    private val daemonStateListener = object : ArbDaemonStateListener {
        override fun onConfigUpdated(config: ArbConfig) {
            val next = synchronized(captureStateLock) {
                CaptureBarReducer.withConfig(rawCaptureState, config).also { rawCaptureState = it }
            }
            publishOnEdt { captureState = next }
            runAsync { refreshAll() }
        }

        override fun onDaemonConnectionChanged() {
            runAsync { refreshAll() }
        }
    }

    init {
        daemonService.addStateListener(daemonStateListener)
        if (pollIntervalMs > 0L) {
            executor.scheduleWithFixedDelay(
                { refreshAll() },
                0L,
                pollIntervalMs,
                TimeUnit.MILLISECONDS,
            )
        }
    }

    // ---- Derived UI properties (tested directly in headless JUnit tests) ----

    val hasRealDevices: Boolean
        get() = captureState.devices.isNotEmpty()

    val deviceDropdownEnabled: Boolean
        get() = hasRealDevices

    val deviceDropdownLabels: List<String>
        get() = if (captureState.devices.isEmpty()) {
            listOf(NO_DEVICES_PLACEHOLDER)
        } else {
            captureState.devices.map(::formatDeviceLabel)
        }

    val selectedDeviceIndex: Int
        get() {
            if (captureState.devices.isEmpty()) return 0
            val idx = captureState.devices.indexOfFirst { it.serial == captureState.selectedSerial }
            return if (idx >= 0) idx else 0
        }

    val selectedDeviceSerial: String?
        get() = if (!hasRealDevices) null else captureState.selectedDevice?.serial

    val canUseSelectedDevice: Boolean
        get() = captureState.daemonConnected && captureState.isSelectedDeviceOnline

    val isCaptureActive: Boolean
        get() = captureState.capture?.active == true

    val startStopEnabled: Boolean
        get() = if (isCaptureActive) captureState.daemonConnected else canUseSelectedDevice

    val screenshotEnabled: Boolean
        get() = if (isCaptureActive) captureState.daemonConnected else canUseSelectedDevice

    val startStopLabel: String
        get() = if (isCaptureActive) ArbFormatters.SECONDARY_STOP_CAPTURE_LABEL else ArbFormatters.SECONDARY_START_CAPTURE_LABEL

    val deviceTooltip: String
        get() = when (val info = captureState.selectedDevice) {
            null -> NO_DEVICES_PLACEHOLDER
            else -> when (info.state) {
                AdbDeviceState.Unauthorized -> "端末で「USB デバッグを許可」をタップしてください (${info.serial})"
                AdbDeviceState.Offline -> "端末がオフラインです (${info.serial})"
                else -> "録画・スクリーンショット対象端末 (${info.serial})"
            }
        }

    val startStopTooltip: String
        get() = if (isCaptureActive) {
            "ライブキャプチャを停止"
        } else {
            disabledDeviceHint() ?: "ライブキャプチャを開始"
        }

    val screenshotTooltip: String
        get() = if (isCaptureActive) {
            "対象端末の現在画面を PNG 保存"
        } else {
            disabledDeviceHint() ?: "対象端末の現在画面を PNG 保存"
        }

    val displayedBundles: List<BundleCatalogItem>
        get() = catalogState.copy(activeFilter = activeFilter).filteredItems

    val showGcAction: Boolean
        get() = activeFilter == BundleFilter.TRASH

    private fun disabledDeviceHint(): String? = when (captureState.selectedDevice?.state) {
        AdbDeviceState.Unauthorized -> "端末が未承認です。端末画面で USB デバッグを許可してください。"
        AdbDeviceState.Offline -> "端末がオフラインです。USB / ワイヤレス ADB 接続を確認してください。"
        null -> NO_DEVICES_PLACEHOLDER
        else -> null
    }

    // ---- Commands invoked from Jewel UI & tests ----

    fun selectDeviceByIndex(index: Int) {
        val device = captureState.devices.getOrNull(index) ?: return
        val next = synchronized(captureStateLock) {
            rawCaptureState.copy(selectedSerial = device.serial).also { rawCaptureState = it }
        }
        publishOnEdt { captureState = next }
    }

    fun triggerPrimarySave() {
        val sec = captureState.saveWindowSecs.toLong()
        when (captureState.defaultSaveAction) {
            IdeDefaultSaveAction.SAVE_AND_OPEN -> triggerSave(sec, openEditor = true)
            IdeDefaultSaveAction.SAVE_ONLY -> triggerSave(sec, openEditor = false)
        }
    }

    fun triggerSecondarySave() {
        val sec = captureState.saveWindowSecs.toLong()
        when (captureState.defaultSaveAction) {
            IdeDefaultSaveAction.SAVE_AND_OPEN -> triggerSave(sec, openEditor = false)
            IdeDefaultSaveAction.SAVE_ONLY -> triggerSave(sec, openEditor = true)
        }
    }

    fun toggleCapture() {
        val active = isCaptureActive
        val serial = selectedDeviceSerial
        runAsync {
            if (active) {
                daemonService.withClient { it.captureStop() }
            } else {
                daemonService.withClient { it.captureStart(CaptureStartParams(serial = serial)) }
            }
            refreshAll()
        }
    }

    fun captureScreenshot() {
        val serial = selectedDeviceSerial
        runAsync {
            val outputPath = ArbScreenshotDestinationResolver.resolveOutputFilePath(project)
            val res = daemonService.withClient {
                it.saveScreenshot(SaveScreenshotParams(serial = serial, outputPath = outputPath))
            }
            val fileName = runCatching { Path.of(res.path).fileName.toString() }.getOrDefault(res.path)
            notifyBalloon("スクリーンショットを保存しました: $fileName (${res.width}x${res.height})")
            refreshAll()
        }
    }

    fun saveLogsOnly() {
        val durationSec = captureState.saveWindowSecs.toLong()
        runAsync {
            val res = daemonService.withClient {
                it.saveLogs(SaveLogsParams(windowSecs = durationSec))
            }
            val detail = res.bundleId ?: res.job.state.wire
            notifyBalloon("直前 ${ArbFormatters.formatSeconds(durationSec)} のログを保存しました: $detail")
            refreshAll()
        }
    }

    fun launchTray() {
        runAsync {
            val bin = daemonService.launchBundledTray()
            notifyBalloon("メニューバー / トレイ (arb-tray) を起動しました: ${bin.fileName}")
        }
    }

    fun openSettings() {
        onOpenSettings()
        runAsync { refreshAll() }
    }

    fun restartDaemon() {
        publishOnEdt { daemonRestartBusy = true }
        runAsync {
            try {
                daemonService.restartDaemon()
                publishOnEdt {
                    staleDaemonMessage = null
                    daemonRestartBusy = false
                }
                refreshAll()
            } catch (t: Throwable) {
                logger.warn("ARB daemon restart failed", t)
                publishOnEdt {
                    daemonRestartBusy = false
                    staleDaemonMessage = "arbd の再起動に失敗しました: ${t.message ?: "詳細不明"}"
                }
            }
        }
    }

    fun selectFilter(filter: BundleFilter) {
        activeFilterBacking = filter
        publishOnEdt {
            activeFilter = filter
            catalogState = rawCatalogState.copy(activeFilter = filter)
        }
    }

    fun openBundle(item: BundleCatalogItem) {
        onOpenBundle(item.bundleId, item.displayTitle, item.isLegacyReadOnly)
    }

    fun revealInFinder(path: String) {
        runCatching { RevealFileAction.openFile(Path.of(path)) }
            .onFailure { logger.warn("Failed to reveal $path", it) }
    }

    fun trashBundle(bundleId: String) {
        runAsync {
            daemonService.withClient { it.bundleTrash(bundleId) }
            refreshAll(note = "ゴミ箱へ移動しました（ゴミ箱タブから復元可）")
        }
    }

    fun restoreBundle(bundleId: String) {
        runAsync {
            daemonService.withClient { it.bundleRestore(bundleId) }
            refreshAll(note = "バンドルを復元しました")
        }
    }

    fun runGc() {
        runAsync {
            val gc = daemonService.withClient { it.bundleGc() }
            val note = "GC 完了: 削除=${gc.purgedBundleIds.size}, Lease 保護=${gc.skippedLeasedBundleIds.size}, Legacy 保護=${gc.skippedUnmanagedOrLegacyBundleIds.size}"
            refreshAll(note = note)
        }
    }

    fun retryFailedJob(jobId: String) {
        runAsync {
            updateRecovery(FailedSaveRecoveryReducer.markBusy(rawRecoveryState, jobId))
            val res = daemonService.withClient { it.saveRetry(jobId) }
            updateRecovery(
                FailedSaveRecoveryReducer.applyJobUpdated(
                    rawRecoveryState,
                    res.job,
                    "再試行結果: ${res.job.state.wire}",
                ),
            )
            refreshAll()
        }
    }

    fun retargetFailedJobInteractive(jobId: String) {
        val descriptor = FileChooserDescriptorFactory.createSingleFolderDescriptor().apply {
            title = "新しい保存先ディレクトリを選択"
        }
        FileChooser.chooseFile(descriptor, project, null) { selected ->
            retargetFailedJob(jobId, selected.toNioPath().toAbsolutePath().normalize().toString())
        }
    }

    fun retargetFailedJob(jobId: String, newDestDir: String) {
        runAsync {
            updateRecovery(FailedSaveRecoveryReducer.markBusy(rawRecoveryState, jobId))
            val res = daemonService.withClient { it.saveRetarget(jobId, newDestDir) }
            updateRecovery(
                FailedSaveRecoveryReducer.applyJobUpdated(
                    rawRecoveryState,
                    res.job,
                    "保存先変更後の結果: ${res.job.state.wire}",
                ),
            )
            refreshAll()
        }
    }

    fun discardFailedJob(jobId: String) {
        runAsync {
            updateRecovery(FailedSaveRecoveryReducer.markBusy(rawRecoveryState, jobId))
            val job = daemonService.withClient { it.saveDiscard(jobId) }
            updateRecovery(
                FailedSaveRecoveryReducer.applyJobUpdated(
                    rawRecoveryState,
                    job,
                    "保存ジョブを破棄し、ピン留めチャンクを解放しました。",
                ),
            )
            refreshAll()
        }
    }

    fun refreshNow() {
        runAsync { refreshAll() }
    }

    /** Direct state injection helper for deterministic headless tests. */
    internal fun applyStateForTest(
        capture: CaptureBarState = captureState,
        recovery: FailedSaveRecoveryState = recoveryState,
        catalog: BundleCatalogState = catalogState,
        staleMessage: String? = staleDaemonMessage,
    ) {
        synchronized(captureStateLock) { rawCaptureState = capture }
        rawRecoveryState = recovery
        rawCatalogState = catalog
        activeFilterBacking = catalog.activeFilter
        captureState = capture
        recoveryState = recovery
        catalogState = catalog
        activeFilter = catalog.activeFilter
        staleDaemonMessage = staleMessage
    }

    private fun triggerSave(durationSec: Long, openEditor: Boolean) {
        runAsync {
            val res = daemonService.withClient {
                it.saveTrigger(SaveTriggerParams(durationSec = durationSec))
            }
            refreshAll()
            val savedId = res.bundleId
            if (savedId != null) {
                if (openEditor) {
                    publishOnEdt { onOpenBundle(savedId, savedId, false) }
                } else {
                    notifyBalloon("直前 ${ArbFormatters.formatSeconds(durationSec)} を保存しました: $savedId")
                }
            } else {
                notifyBalloon(
                    "保存ジョブ状態: ${res.job.state.wire} (${res.job.lastError ?: "詳細を確認してください"})",
                    NotificationType.WARNING,
                )
            }
        }
    }

    private fun runAsync(action: () -> Unit) {
        if (disposed) return
        executor.submit {
            try {
                action()
            } catch (t: Throwable) {
                logger.warn("ARB toolwindow action failed", t)
                handleDaemonFailure(t)
            }
        }
    }

    private fun refreshAll(note: String? = null) {
        if (disposed) return
        try {
            val status = daemonService.withClient { it.systemStatus() }
            daemonService.updateCachedCaptureActive(status.capture.active)
            var staleWarning: String? = null
            val config = try {
                daemonService.withClient { it.configGet().config }.also(daemonService::updateCachedConfig)
            } catch (t: Throwable) {
                val diag = ArbSettingsConfigurable.classifyDaemonError(t)
                if (diag.kind == ArbSettingsConfigurable.DaemonDiagnosticKind.STALE_OR_MISMATCHED) {
                    staleWarning = diag.message
                }
                rawCaptureState.config
            }
            val devices = runCatching {
                daemonService.withClient { it.deviceList().devices }
            }.onFailure { err ->
                logger.warn("ARB device.list poll failed: ${err.message}")
            }.getOrDefault(rawCaptureState.devices)
            val jobs = daemonService.withClient { it.saveListJobs().jobs }
            val bundles = daemonService.withClient { it.bundleList().bundles }

            var budget = META_BUDGET_PER_POLL
            for (entry in bundles) {
                if (metaCache.containsKey(entry.bundleId)) continue
                if (budget-- <= 0) break
                val summary = runCatching {
                    daemonService.withClient { it.timelineSummary(entry.bundleId) }
                }.getOrNull() ?: continue
                val spanNs = summary.epochs.firstOrNull()?.let { ep ->
                    (ep.endBoottimeNs - ep.startBoottimeNs).coerceAtLeast(0L)
                }
                val secs = when {
                    spanNs != null && spanNs > 0L -> (spanNs / 1_000_000_000L).coerceAtLeast(1L)
                    summary.frameCount > 0 -> (summary.frameCount / config.videoMaxFps.coerceAtLeast(1)).toLong().coerceAtLeast(1L)
                    else -> null
                }
                val crashed = runCatching {
                    daemonService.withClient { it.timelineCrashes(entry.bundleId).crashes }.isNotEmpty()
                }.getOrDefault(false)
                metaCache[entry.bundleId] = BundleMeta(summary.bundleKind, secs, crashed)
            }
            val crashIds = mutableSetOf<String>()
            val durations = mutableMapOf<String, Long>()
            val kinds = mutableMapOf<String, BundleKind>()
            for (entry in bundles) {
                val meta = metaCache[entry.bundleId] ?: continue
                kinds[entry.bundleId] = meta.kind
                meta.durationSecs?.let { durations[entry.bundleId] = it }
                if (meta.crashed) crashIds.add(entry.bundleId)
            }

            val nextCapture = synchronized(captureStateLock) {
                CaptureBarReducer.fromStatus(
                    previous = rawCaptureState,
                    status = status,
                    devices = devices,
                    config = config,
                ).also { rawCaptureState = it }
            }
            val nextRecovery = FailedSaveRecoveryReducer.withJobs(rawRecoveryState, jobs)
            val nextCatalog = BundleCatalogState.fromEntries(
                entries = bundles,
                activeFilter = activeFilterBacking,
                crashBundleIds = crashIds,
                durationsByBundleId = durations,
                kindsByBundleId = kinds,
            )
            rawRecoveryState = nextRecovery
            rawCatalogState = nextCatalog

            publishOnEdt {
                captureState = nextCapture
                recoveryState = nextRecovery
                catalogState = nextCatalog
                staleDaemonMessage = staleWarning
                if (note != null) {
                    catalogNote = note
                }
            }
        } catch (t: Throwable) {
            logger.debug("ARB status poll failed: ${t.message}")
            handleDaemonFailure(t)
        }
    }

    private fun handleDaemonFailure(t: Throwable) {
        val diag = ArbSettingsConfigurable.classifyDaemonError(t)
        val stale = if (diag.kind == ArbSettingsConfigurable.DaemonDiagnosticKind.STALE_OR_MISMATCHED) {
            diag.message
        } else {
            null
        }
        val next = synchronized(captureStateLock) {
            CaptureBarReducer.disconnected(rawCaptureState, t.message ?: "arbd 未起動").also { rawCaptureState = it }
        }
        publishOnEdt {
            captureState = next
            if (stale != null) {
                staleDaemonMessage = stale
            }
        }
    }

    private fun updateRecovery(next: FailedSaveRecoveryState) {
        rawRecoveryState = next
        publishOnEdt { recoveryState = next }
    }

    private fun publishOnEdt(block: () -> Unit) {
        if (disposed) return
        if (SwingUtilities.isEventDispatchThread()) {
            if (!disposed) block()
        } else {
            SwingUtilities.invokeLater { if (!disposed) block() }
        }
    }

    private fun notifyBalloon(content: String, type: NotificationType = NotificationType.INFORMATION) {
        runCatching {
            NotificationGroupManager.getInstance()
                .getNotificationGroup("ARB v2")
                .createNotification("Android Replay Buffer", content, type)
                .notify(project)
        }.onFailure {
            logger.info("ARB notification: $content")
        }
    }

    override fun dispose() {
        disposed = true
        daemonService.removeStateListener(daemonStateListener)
        executor.shutdownNow()
    }

    companion object {
        const val NO_DEVICES_PLACEHOLDER: String = "端末未検出（USB / エミュレータを接続）"
        private const val META_BUDGET_PER_POLL = 24
        const val DEFAULT_POLL_INTERVAL_MS: Long = 2_000L

        fun formatDeviceLabel(device: AdbDeviceInfo): String = when (val st = device.state) {
            AdbDeviceState.Device -> device.serial
            AdbDeviceState.Unauthorized -> "${device.serial} (未承認: USB デバッグを許可)"
            AdbDeviceState.Offline -> "${device.serial} (オフライン)"
            AdbDeviceState.Recovery -> "${device.serial} (recovery)"
            AdbDeviceState.Bootloader -> "${device.serial} (bootloader)"
            is AdbDeviceState.Unknown -> "${device.serial} (${st.value})"
        }
    }
}
