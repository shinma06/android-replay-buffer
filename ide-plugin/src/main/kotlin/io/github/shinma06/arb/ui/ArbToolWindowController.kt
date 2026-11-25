package io.github.shinma06.arb.ui

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import com.intellij.ide.actions.RevealFileAction
import com.intellij.openapi.Disposable
import com.intellij.openapi.diagnostic.Logger
import com.intellij.openapi.fileChooser.FileChooser
import com.intellij.openapi.fileChooser.FileChooserDescriptorFactory
import com.intellij.openapi.project.Project
import io.github.shinma06.arb.ide.ArbDaemonStateListener
import io.github.shinma06.arb.ide.ArbNotifications
import io.github.shinma06.arb.ide.ArbScreenshotDestinationResolver
import io.github.shinma06.arb.ide.ArbSettingsConfigurable
import io.github.shinma06.arb.ide.ArbToolbarDaemonSnapshot
import io.github.shinma06.arb.ide.ArbdDaemonService
import io.github.shinma06.arb.model.AdbDeviceInfo
import io.github.shinma06.arb.model.AdbDeviceState
import io.github.shinma06.arb.model.ArbConfig
import io.github.shinma06.arb.model.ArbConfigPatch
import io.github.shinma06.arb.model.ArbdRpcException
import io.github.shinma06.arb.model.BundleDiskState
import io.github.shinma06.arb.model.BundleForgetResponse
import io.github.shinma06.arb.model.BundleGcResponse
import io.github.shinma06.arb.model.BundleKind
import io.github.shinma06.arb.model.CaptureMode
import io.github.shinma06.arb.model.CaptureStartParams
import io.github.shinma06.arb.model.IdeDefaultSaveAction
import io.github.shinma06.arb.model.SaveJobView
import io.github.shinma06.arb.model.SaveLogsParams
import io.github.shinma06.arb.model.SaveScreenshotParams
import io.github.shinma06.arb.model.SaveTriggerParams
import io.github.shinma06.arb.model.StringPatch
import io.github.shinma06.arb.state.ArbFormatters
import io.github.shinma06.arb.state.BundleCatalogItem
import io.github.shinma06.arb.state.BundleMetaScheduler
import io.github.shinma06.arb.state.BundleCatalogState
import io.github.shinma06.arb.state.BundleFilter
import io.github.shinma06.arb.state.CaptureBarReducer
import io.github.shinma06.arb.state.CaptureBarState
import io.github.shinma06.arb.state.DeviceSelectionOutcome
import io.github.shinma06.arb.state.DeviceSelectionPolicy
import io.github.shinma06.arb.state.FailedSaveRecoveryReducer
import io.github.shinma06.arb.state.FailedSaveRecoveryState
import java.nio.file.Path
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.Executors
import java.util.concurrent.RejectedExecutionException
import java.util.concurrent.TimeUnit
import javax.swing.SwingUtilities

/**
 * Explicit 3-state always-on capture UX state rendered by the Tool Window & Running Devices toolbar:
 * - [OFF]: capture disabled (`capture_enabled == false && !capture.active`).
 * - [ARMED_WAITING]: armed (`capture_enabled == true`) and waiting for a device / selection.
 * - [RECORDING]: actively capturing (`capture.active == true`).
 */
enum class CaptureToggleState {
    OFF,
    ARMED_WAITING,
    RECORDING;

    companion object {
        /** The one 3-state rule shared by the Tool Window and the toolbar / Tools menu action. */
        fun of(active: Boolean, armed: Boolean, connected: Boolean): CaptureToggleState = when {
            active && connected -> RECORDING
            active || armed -> ARMED_WAITING
            else -> OFF
        }
    }
}

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
    /**
     * Resolves the `save.screenshot` `output_dir` (Android Studio Logcat screenshot destination
     * inheritance or the IDE-local custom override); `null` lets `arbd` apply its own
     * `screenshot_dir_override` -> `output_dir_override` -> default chain. Called off the EDT.
     */
    private val screenshotDirResolver: () -> String? = {
        ArbScreenshotDestinationResolver.resolveOutputDir(project)
    },
    private val onNotifyInfo: (String) -> Unit = { ArbNotifications.notifyInfo(project, it) },
    private val onNotifyWarning: (String) -> Unit = { ArbNotifications.notifyWarning(project, it) },
    private val onNotifyActionFailed: (String, String?) -> Unit = { op, detail ->
        ArbNotifications.notifyActionFailed(project, op, detail)
    },
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
    private var rawCaptureEnabled = false

    @Volatile
    private var rawCatalogState = BundleCatalogState()

    @Volatile
    private var activeFilterBacking = BundleFilter.ALL

    /** [crashed] is `null` while `timeline.crashes` has not succeeded yet (Unknown, retried). */
    private data class BundleMeta(val kind: BundleKind, val durationSecs: Long?, val crashed: Boolean?)

    private val metaCache = ConcurrentHashMap<String, BundleMeta>()
    private val metaScheduler = BundleMetaScheduler(budgetPerTick = META_BUDGET_PER_POLL)
    private val metaTick = AtomicLong(0L)

    // One in-flight guard per user action (acquired on the calling thread before anything is
    // queued, so two clicks in the same EDT turn can never both submit).
    private val saveInFlight = AtomicBoolean(false)
    private val logSaveInFlight = AtomicBoolean(false)
    private val deviceChangeBusy: Boolean get() = daemonService.deviceChangeBusy
    private val screenshotInFlight = AtomicBoolean(false)
    private val captureModeInFlight = AtomicBoolean(false)
    private val restartInFlight = AtomicBoolean(false)
    private val recoveryInFlight = AtomicBoolean(false)

    var captureState: CaptureBarState by mutableStateOf(CaptureBarState())
        private set

    var captureEnabled: Boolean by mutableStateOf(false)
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

    var captureModeBusy: Boolean by mutableStateOf(false)
        private set

    /** `save.trigger` in flight; further save clicks are ignored (not queued) until it settles. */
    var saveBusy: Boolean by mutableStateOf(false)
        private set

    var logSaveBusy: Boolean by mutableStateOf(false)
        private set

    var screenshotBusy: Boolean by mutableStateOf(false)
        private set

    var gcBusy: Boolean by mutableStateOf(false)
        private set

    var confirmEmptyTrashVisible: Boolean by mutableStateOf(false)
        private set

    var forgetBusy: Boolean by mutableStateOf(false)
        private set

    private val daemonStateListener = object : ArbDaemonStateListener {
        override fun onConfigUpdated(config: ArbConfig) {
            val next = synchronized(captureStateLock) {
                rawCaptureEnabled = config.captureEnabled
                val previous = rawCaptureState
                val configured = CaptureBarReducer.withConfig(previous, config)
                val next = if (config.deviceSerial != previous.config.deviceSerial) {
                    configured.copy(
                        selectedSerial = config.deviceSerial,
                        autoSelectedSerial = null,
                        deviceSelection = DeviceSelectionPolicy.evaluate(config.deviceSerial, configured.devices),
                    )
                } else configured
                next.also { rawCaptureState = it }
            }
            publishOnEdt {
                captureState = next
                captureEnabled = config.captureEnabled
            }
            runAsync { refreshAll() }
        }

        override fun onDaemonConnectionChanged() {
            runAsync { refreshAll() }
        }

        override fun onCatalogChanged() {
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
        get() = hasRealDevices && !deviceChangeBusy

    /**
     * Device dropdown entries derived strictly from `client-core`'s [DeviceSelectionPolicy] outcome:
     * - 0 devices -> single disabled placeholder ([NO_DEVICES_PLACEHOLDER]);
     * - 2+ online and nothing chosen -> leading prompt ([MULTIPLE_DEVICES_PROMPT], never auto-select);
     * - pinned device gone while others are online -> the pinned serial stays selected and is
     *   labelled `切断中（再接続待ち）` (never silently switched to another device).
     */
    val deviceDropdownEntries: List<DeviceDropdownEntry>
        get() = buildDeviceDropdownEntries(captureState)

    val deviceDropdownLabels: List<String>
        get() = deviceDropdownEntries.map { it.label }

    val selectedDeviceIndex: Int
        get() {
            val entries = deviceDropdownEntries
            val selected = captureState.selectedSerial ?: return 0
            val idx = entries.indexOfFirst { it.serial == selected }
            return if (idx >= 0) idx else 0
        }

    val selectedDeviceSerial: String?
        get() = if (!hasRealDevices) null else captureState.selectedDevice?.serial

    /** Inline prompt shown under the device row when the user must pick or reconnect a device. */
    val deviceSelectionNotice: String?
        get() = when (val sel = captureState.deviceSelection) {
            is DeviceSelectionOutcome.MultipleDevicesNeedSelection ->
                if (captureState.selectedSerial == null) MULTIPLE_DEVICES_PROMPT else null
            is DeviceSelectionOutcome.WaitingForPreferredDevice ->
                if (captureState.devices.none { it.serial == sel.preferredSerial }) {
                    "${sel.preferredSerial} は$WAITING_FOR_PINNED_SUFFIX。別の端末へは自動で切り替えません。"
                } else {
                    null
                }
            else -> null
        }

    /** Current capture mode (`video_and_logs` / `logs_only`) — the running mode while capturing. */
    val captureMode: CaptureMode
        get() = captureState.activeCaptureMode

    val isLogsOnlyMode: Boolean
        get() = captureState.isLogsOnlyMode

    /** Quiet inline status chip text, shown only in `logs_only` mode. */
    val captureModeChipText: String?
        get() = if (isLogsOnlyMode) LOGS_ONLY_CHIP_TEXT else null

    val captureModeToggleEnabled: Boolean
        get() = captureState.daemonConnected && !deviceChangeBusy && !captureModeBusy

    val canUseSelectedDevice: Boolean
        get() = captureState.daemonConnected && captureState.isSelectedDeviceOnline

    val isCaptureActive: Boolean
        get() = captureState.capture?.active == true

    /** Whether always-on background capture is armed (`capture_enabled == true`). */
    val isCaptureArmed: Boolean
        get() = captureEnabled || captureState.config.captureEnabled

    /** `true` when either actively recording or armed and waiting for a device to connect. */
    val isCaptureOn: Boolean
        get() = isCaptureActive || isCaptureArmed

    val captureToggleState: CaptureToggleState
        get() = CaptureToggleState.of(
            active = isCaptureActive,
            armed = isCaptureArmed,
            connected = captureState.capture?.connected == true,
        )

    /** Compact status indicator text clearly distinguishing OFF / ON・待機中 / ON・REC. */
    val captureStatusBadgeText: String
        get() = when {
            !captureState.daemonConnected -> "未接続"
            captureState.capture?.storagePressureStopped == true -> "容量停止"
            captureToggleState == CaptureToggleState.RECORDING -> when {
                isLogsOnlyMode -> "ON・REC (ログのみ)"
                else -> "ON・REC"
            }
            captureToggleState == CaptureToggleState.ARMED_WAITING -> "ON・待機中"
            else -> "OFF"
        }

    /** Shown under the device row while armed and waiting for a device connection / selection. */
    val armedWaitingNotice: String?
        get() = if (captureState.daemonConnected && captureToggleState == CaptureToggleState.ARMED_WAITING) {
            ARMED_WAITING_DETAIL_TEXT
        } else {
            null
        }

    /** Always enabled while `arbd` is connected so Capture ON can be armed even with 0 devices. */
    val startStopEnabled: Boolean
        get() = captureState.daemonConnected && !deviceChangeBusy

    val screenshotEnabled: Boolean
        get() = !deviceChangeBusy && (if (isCaptureActive) captureState.daemonConnected else canUseSelectedDevice)

    val startStopLabel: String
        get() = if (isCaptureOn) ArbFormatters.SECONDARY_STOP_CAPTURE_LABEL else ArbFormatters.SECONDARY_START_CAPTURE_LABEL

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
        get() = when (captureToggleState) {
            CaptureToggleState.RECORDING -> "ON・REC（取得中） — クリックでキャプチャを OFF にします"
            CaptureToggleState.ARMED_WAITING -> "$ARMED_WAITING_DETAIL_TEXT — クリックでキャプチャを OFF にします"
            CaptureToggleState.OFF -> if (canUseSelectedDevice) {
                "OFF（停止中） — ライブキャプチャを開始"
            } else {
                val hint = disabledDeviceHint()?.let { " · $it" }.orEmpty()
                "OFF（停止中） — クリックで $ARMED_WAITING_DETAIL_TEXT にします$hint"
            }
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

    val trashCount: Int
        get() = catalogState.countFor(BundleFilter.TRASH)

    val missingBundleCount: Int
        get() = catalogState.missingCount

    val emptyTrashEnabled: Boolean
        get() = captureState.daemonConnected && trashCount > 0 && !gcBusy

    val showEmptyTrashConfirmation: Boolean
        get() = activeFilter == BundleFilter.TRASH && confirmEmptyTrashVisible && emptyTrashEnabled

    private fun disabledDeviceHint(): String? {
        deviceSelectionNotice?.let { return it }
        return when (captureState.selectedDevice?.state) {
            AdbDeviceState.Unauthorized -> "端末が未承認です。端末画面で USB デバッグを許可してください。"
            AdbDeviceState.Offline -> "端末がオフラインです。USB / ワイヤレス ADB 接続を確認してください。"
            null -> NO_DEVICES_PLACEHOLDER
            else -> null
        }
    }

    // ---- Commands invoked from Jewel UI & tests ----

    /**
     * Explicit user choice: pins the serial (clears the auto-selected marker so the reducer never
     * re-evaluates it away) and persists it as `config.device_serial`, so the Running Devices
     * toolbar, `arb-tray`, CLI and MCP all target the same device.
     */
    fun selectDeviceByIndex(index: Int) {
        val serial = deviceDropdownEntries.getOrNull(index)?.serial ?: return
        // Re-choosing the sole auto-selected device is a no-op: it must stay "auto" so plugging
        // in a second device still prompts instead of silently keeping a pin nobody asked for.
        val current = captureState
        if (serial == current.selectedSerial && serial == current.autoSelectedSerial) return
        if (!daemonService.beginDeviceChange()) return
        val release = { SwingUtilities.invokeLater { daemonService.finishDeviceChange() } }
        runAsync(operation = "録画対象端末の変更", onDropped = release) {
            try {
                val config = if (rawCaptureState.config.deviceSerial == serial) rawCaptureState.config else {
                    daemonService.withClient {
                        it.configUpdate(ArbConfigPatch(deviceSerial = StringPatch.Set(serial))).config
                    }
                }
                val next = synchronized(captureStateLock) {
                    rawCaptureState.copy(
                        selectedSerial = serial,
                        autoSelectedSerial = null,
                        deviceSelection = DeviceSelectionPolicy.evaluate(serial, rawCaptureState.devices),
                        config = config,
                    ).also { rawCaptureState = it }
                }
                publishOnEdt { captureState = next }
                daemonService.updateCachedToolbarSnapshot(next.toToolbarSnapshot(captureEnabled = rawCaptureEnabled))
                daemonService.notifyConfigUpdated(config)
            } finally {
                release()
            }
        }
    }

    /**
     * Switches between `映像＋ログ` and `ログのみ` via `config.update(capture_mode)`. A running capture
     * is restarted on the same device (`capture.stop` + `capture.start(capture_mode)`) because the
     * mode only takes effect when a capture starts.
     */
    fun setCaptureMode(mode: CaptureMode) {
        if (deviceChangeBusy) return
        val snapshot = captureState
        val wasActive = snapshot.capture?.active == true
        val runningMode = snapshot.capture?.captureMode
        if (snapshot.config.captureMode == mode && (!wasActive || runningMode == mode)) return
        val serial = if (wasActive) snapshot.capture?.serial ?: selectedDeviceSerial else selectedDeviceSerial
        runGuarded(captureModeInFlight, { captureModeBusy = it }) {
            val label = captureModeLabel(mode)
            try {
                val updated = daemonService.withClient {
                    it.configUpdate(ArbConfigPatch(captureMode = mode)).config
                }
                daemonService.notifyConfigUpdated(updated)
                if (wasActive && runningMode != mode) {
                    daemonService.withClient { it.captureStop() }
                    daemonService.withClient {
                        it.captureStart(CaptureStartParams(serial = serial, captureMode = mode))
                    }
                    ArbNotifications.notifyInfo(project, "キャプチャモードを「$label」に切り替え、録画を再開しました")
                } else {
                    ArbNotifications.notifyInfo(project, "キャプチャモードを「$label」に設定しました")
                }
            } catch (t: Throwable) {
                logger.warn("ARB capture mode switch to ${mode.wire} failed", t)
                ArbNotifications.notifyWarning(
                    project,
                    "キャプチャモードを「$label」に切り替えられませんでした: ${t.message ?: "詳細不明"}",
                )
            }
            refreshAll()
        }
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
        if (deviceChangeBusy) return
        val currentlyOn = isCaptureOn
        val currentlyActive = isCaptureActive
        val canStartNow = canUseSelectedDevice
        val serial = selectedDeviceSerial
        runAsync(operation = "録画の切り替え") {
            setCaptureEnabled(
                daemonService,
                enable = !currentlyOn,
                currentlyActive = currentlyActive,
                serial = serial.takeIf { canStartNow },
            )
            refreshAll()
        }
    }

    fun captureScreenshot() {
        if (deviceChangeBusy) return
        val serial = selectedDeviceSerial
        runGuarded(screenshotInFlight, { screenshotBusy = it }) {
            try {
                val outputDir = runCatching { screenshotDirResolver() }
                    .onFailure { logger.debug("ARB screenshot dir resolution failed", it) }
                    .getOrNull()
                val res = daemonService.withClient {
                    it.saveScreenshot(SaveScreenshotParams(serial = serial, outputDir = outputDir))
                }
                ArbNotifications.notifyImageSaved(
                    project = project,
                    imagePath = res.path,
                    width = res.width,
                    height = res.height,
                    kindLabel = "スクリーンショット",
                )
                refreshAll()
            } catch (t: Throwable) {
                logger.warn("ARB screenshot failed", t)
                onNotifyActionFailed("スクリーンショットの保存", t.message)
                handleDaemonFailure(t)
            }
        }
    }

    fun saveLogsOnly() {
        val durationSec = captureState.saveWindowSecs.toLong()
        runGuarded(logSaveInFlight, { logSaveBusy = it }) {
            try {
                val res = daemonService.withClient {
                    it.saveLogs(SaveLogsParams(windowSecs = durationSec))
                }
                val savedId = res.savedBundleId
                if (savedId != null) {
                    ArbNotifications.notifyLogsOnlySaved(
                        project = project,
                        daemonService = daemonService,
                        bundleId = savedId,
                        bundlePath = res.bundlePath,
                        windowSecs = durationSec,
                        warning = res.warning,
                    )
                } else {
                    ArbNotifications.notifyWarning(
                        project = project,
                        message = "ログ保存ジョブ状態: ${res.job.state.wire} (${res.job.lastError ?: "詳細を確認してください"})",
                        includeOpenPanel = true,
                    )
                }
                refreshAll()
            } catch (t: Throwable) {
                logger.warn("ARB save.logs failed", t)
                onNotifyActionFailed("ログのみの保存", t.message)
                handleDaemonFailure(t)
            }
        }
    }

    fun launchTray() {
        runAsync {
            val bin = daemonService.launchBundledTray()
            ArbNotifications.notifyInfo(project, "メニューバー / トレイ (arb-tray) を起動しました: ${bin.fileName}")
        }
    }

    fun openSettings() {
        onOpenSettings()
        runAsync { refreshAll() }
    }

    fun restartDaemon() {
        runGuarded(restartInFlight, { daemonRestartBusy = it }) {
            try {
                daemonService.restartDaemon()
                publishOnEdt { staleDaemonMessage = null }
                refreshAll()
            } catch (t: Throwable) {
                logger.warn("ARB daemon restart failed", t)
                publishOnEdt {
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
            if (filter != BundleFilter.TRASH) {
                confirmEmptyTrashVisible = false
            }
        }
    }

    fun openBundle(item: BundleCatalogItem) {
        when {
            item.isMissingOnDisk -> {
                val msg = "フォルダが見つかりません (${item.bundleId})。手動削除または移動された可能性があります。一覧から削除できます。"
                publishOnEdt { catalogNote = msg }
                onNotifyWarning(msg)
            }
            item.isStorageUnavailable -> {
                val msg = "保存先にアクセスできません (${item.bundleId})。外付けドライブの接続やアクセス権限を確認してください。"
                publishOnEdt { catalogNote = msg }
                onNotifyWarning(msg)
            }
            item.canOpen -> onOpenBundle(item.bundleId, item.displayTitle, item.isLegacyReadOnly)
        }
    }

    fun revealInFinder(path: String) {
        runCatching { RevealFileAction.openFile(Path.of(path)) }
            .onFailure { logger.warn("Failed to reveal $path", it) }
    }

    fun trashBundle(bundleId: String) {
        runAsync(operation = "ゴミ箱へ移動") {
            daemonService.withClient { it.bundleTrash(bundleId) }
            refreshAll(note = "ゴミ箱へ移動しました（ゴミ箱タブから復元可）")
        }
    }

    fun restoreBundle(bundleId: String) {
        runAsync(operation = "バンドルの復元") {
            daemonService.withClient { it.bundleRestore(bundleId) }
            refreshAll(note = "バンドルを復元しました")
        }
    }

    fun requestEmptyTrash() {
        if (!showGcAction || !emptyTrashEnabled) return
        publishOnEdt { confirmEmptyTrashVisible = true }
    }

    fun cancelEmptyTrash() {
        publishOnEdt { confirmEmptyTrashVisible = false }
    }

    fun confirmEmptyTrash() {
        publishOnEdt {
            if (!showEmptyTrashConfirmation) return@publishOnEdt
            confirmEmptyTrashVisible = false
            gcBusy = true
            executeEmptyTrash()
        }
    }

    private fun executeEmptyTrash() {
        runAsync {
            try {
                val gc = daemonService.withClient { it.bundleGc(ignoreGrace = true) }
                val note = formatGcSummary(gc)
                val (isWarning, notificationText) = formatGcNotification(gc)
                if (isWarning) {
                    onNotifyWarning(notificationText)
                } else {
                    onNotifyInfo(notificationText)
                }
                refreshAll(note = note)
            } catch (t: Throwable) {
                logger.warn("ARB bundle.gc(ignore_grace=true) failed", t)
                val msg = "ゴミ箱を空にできませんでした: ${t.message ?: "詳細不明"}"
                publishOnEdt { catalogNote = msg }
                onNotifyActionFailed("ゴミ箱を空にする", t.message)
                handleDaemonFailure(t)
            } finally {
                publishOnEdt { gcBusy = false }
            }
        }
    }

    fun forgetBundle(bundleId: String) {
        if (forgetBusy) return
        publishOnEdt { forgetBusy = true }
        runAsync {
            try {
                val res = daemonService.withClient { it.bundleForget(bundleId = bundleId) }
                val note = formatForgetSummary(res)
                if (forgetNeedsWarning(res)) {
                    onNotifyWarning(note)
                } else {
                    onNotifyInfo(note)
                }
                refreshAll(note = note)
            } catch (t: Throwable) {
                logger.warn("ARB bundle.forget($bundleId) failed", t)
                val msg = "一覧から削除できませんでした: ${t.message ?: "詳細不明"}"
                publishOnEdt { catalogNote = msg }
                onNotifyActionFailed("一覧からの削除", t.message)
                handleDaemonFailure(t)
            } finally {
                publishOnEdt { forgetBusy = false }
            }
        }
    }

    fun forgetAllMissingBundles() {
        if (forgetBusy) return
        publishOnEdt { forgetBusy = true }
        runAsync {
            try {
                val res = daemonService.withClient { it.bundleForget(allMissing = true) }
                val note = formatForgetSummary(res)
                if (forgetNeedsWarning(res)) {
                    onNotifyWarning(note)
                } else {
                    onNotifyInfo(note)
                }
                refreshAll(note = note)
            } catch (t: Throwable) {
                logger.warn("ARB bundle.forget(all_missing=true) failed", t)
                val msg = "見つからないバンドルの一括整理に失敗しました: ${t.message ?: "詳細不明"}"
                publishOnEdt { catalogNote = msg }
                onNotifyActionFailed("見つからないバンドルの一括整理", t.message)
                handleDaemonFailure(t)
            } finally {
                publishOnEdt { forgetBusy = false }
            }
        }
    }

    fun retryFailedJob(jobId: String) {
        runRecoveryAction(jobId, "保存の再試行") {
            val res = daemonService.withClient { it.saveRetry(jobId) }
            "再試行結果: ${res.job.state.wire}" to res.job
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
        runRecoveryAction(jobId, "保存先の変更") {
            val res = daemonService.withClient { it.saveRetarget(jobId, newDestDir) }
            "保存先変更後の結果: ${res.job.state.wire}" to res.job
        }
    }

    fun discardFailedJob(jobId: String) {
        runRecoveryAction(jobId, "保存ジョブの破棄") {
            val job = daemonService.withClient { it.saveDiscard(jobId) }
            "保存ジョブを破棄し、ピン留めチャンクを解放しました。" to job
        }
    }

    /**
     * Failed-save recovery (retry / retarget / discard). The busy marker is taken atomically on the
     * calling thread, so a second click on the same job (or any job) while one is in flight is
     * dropped instead of re-submitting; every outcome (success, business error, transport
     * failure) clears it and a failure is reported instead of vanishing.
     */
    private fun runRecoveryAction(jobId: String, operation: String, call: () -> Pair<String, SaveJobView>) {
        runGuarded(recoveryInFlight, { busy ->
            recoveryState = recoveryState.copy(busyJobId = if (busy) jobId else null)
        }) {
            try {
                val (message, job) = call()
                publishOnEdt {
                    recoveryState = FailedSaveRecoveryReducer.applyJobUpdated(recoveryState, job, message).copy(busyJobId = jobId)
                }
                refreshAll()
            } catch (t: Throwable) {
                logger.warn("ARB $operation failed", t)
                publishOnEdt {
                    recoveryState = recoveryState.copy(lastMessage = "$operation に失敗しました: ${t.message ?: "詳細不明"}")
                }
                onNotifyActionFailed(operation, t.message)
                handleDaemonFailure(t)
            }
        }
    }

    internal fun awaitWorkerForTest() {
        executor.submit {}.get(10, TimeUnit.SECONDS)
    }

    fun refreshNow() {
        runAsync { refreshAll() }
    }

    /** Test hook: one full poll on the calling thread (deterministic, no worker involved). */
    internal fun pollOnceForTest() {
        refreshAll()
    }

    /** Direct state injection helper for deterministic headless tests. */
    internal fun applyStateForTest(
        capture: CaptureBarState = captureState,
        recovery: FailedSaveRecoveryState = recoveryState,
        catalog: BundleCatalogState = catalogState,
        staleMessage: String? = staleDaemonMessage,
        captureEnabled: Boolean = capture.config.captureEnabled,
    ) {
        synchronized(captureStateLock) {
            rawCaptureState = capture
            rawCaptureEnabled = captureEnabled
        }
        rawCatalogState = catalog
        activeFilterBacking = catalog.activeFilter
        captureState = capture
        this.captureEnabled = captureEnabled
        recoveryState = recovery
        catalogState = catalog
        activeFilter = catalog.activeFilter
        staleDaemonMessage = staleMessage
    }

    private fun triggerSave(durationSec: Long, openEditor: Boolean) {
        runGuarded(saveInFlight, { saveBusy = it }) {
            try {
                val res = daemonService.withClient {
                    it.saveTrigger(SaveTriggerParams(durationSec = durationSec))
                }
                refreshAll()
                val savedId = res.savedBundleId
                if (savedId != null) {
                    if (openEditor) {
                        publishOnEdt { onOpenBundle(savedId, savedId, false) }
                    }
                    ArbNotifications.notifyReplaySaved(
                        project = project,
                        daemonService = daemonService,
                        bundleId = savedId,
                        bundlePath = res.bundlePath,
                        windowSecs = durationSec,
                        openedInStudio = openEditor,
                        warning = res.warning,
                    )
                } else {
                    ArbNotifications.notifyWarning(
                        project = project,
                        message = "保存ジョブ状態: ${res.job.state.wire} (${res.job.lastError ?: "詳細を確認してください"})",
                        includeOpenPanel = true,
                    )
                }
            } catch (t: Throwable) {
                logger.warn("ARB save.trigger failed", t)
                onNotifyActionFailed("直前の記録の保存", t.message)
                handleDaemonFailure(t)
            }
        }
    }

    /**
     * Queues [action] on the worker. Returns `false` when it was dropped (disposed / shut down),
     * after running [onDropped] so callers holding a guard can release it.
     */
    private fun runAsync(operation: String? = null, onDropped: () -> Unit = {}, action: () -> Unit): Boolean {
        if (disposed) {
            onDropped()
            return false
        }
        return try {
            executor.submit {
                if (disposed) {
                    onDropped()
                    return@submit
                }
                try {
                    action()
                } catch (t: Throwable) {
                    logger.warn("ARB toolwindow action failed", t)
                    operation?.let { onNotifyActionFailed(it, t.message) }
                    handleDaemonFailure(t)
                }
            }
            true
        } catch (_: RejectedExecutionException) {
            // Disposed concurrently; nothing to do.
            onDropped()
            false
        }
    }

    /**
     * Single-flight user action. [inFlight] is claimed with a CAS on the calling thread (the EDT
     * click handler) *before* anything is queued, so a double click in one EDT turn submits once.
     * The guard and the observable busy flag are always released afterwards, whatever [action]
     * throws, and also when the worker drops the task.
     */
    private fun runGuarded(inFlight: AtomicBoolean, setBusy: (Boolean) -> Unit, action: () -> Unit) {
        if (!inFlight.compareAndSet(false, true)) return
        publishOnEdt { setBusy(true) }
        val release = {
            publishOnEdt {
                setBusy(false)
                inFlight.set(false)
            }
        }
        runAsync(onDropped = release) {
            try {
                action()
            } finally {
                release()
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

            val presentIds = bundles.mapTo(mutableSetOf()) { it.bundleId }
            metaCache.keys.retainAll(presentIds)
            metaScheduler.retainOnly(presentIds)

            val tick = metaTick.incrementAndGet()
            // Candidates still missing data: never fetched, or crash status still Unknown.
            val pending = bundles
                .filter { it.diskState == BundleDiskState.PRESENT }
                .map { it.bundleId }
                .filter { id -> metaCache[id]?.crashed == null }
            for (id in metaScheduler.select(pending, tick)) {
                fetchBundleMeta(id, config.videoMaxFps, tick)
            }
            val crashIds = mutableSetOf<String>()
            val crashUnknownIds = mutableSetOf<String>()
            val durations = mutableMapOf<String, Long>()
            val kinds = mutableMapOf<String, BundleKind>()
            for (entry in bundles) {
                val meta = metaCache[entry.bundleId]
                if (meta == null) {
                    // Attempted and failing: crash status is unknown, never "no crash".
                    if (metaScheduler.failureCount(entry.bundleId) > 0) crashUnknownIds.add(entry.bundleId)
                    continue
                }
                kinds[entry.bundleId] = meta.kind
                meta.durationSecs?.let { durations[entry.bundleId] = it }
                when (meta.crashed) {
                    true -> crashIds.add(entry.bundleId)
                    null -> crashUnknownIds.add(entry.bundleId)
                    false -> Unit
                }
            }

            val armed = status.captureEnabled || config.captureEnabled
            daemonService.updateCachedCaptureState(active = status.capture.active, enabled = armed)
            val nextCapture = synchronized(captureStateLock) {
                rawCaptureEnabled = armed
                CaptureBarReducer.fromStatus(
                    previous = rawCaptureState,
                    status = status,
                    devices = devices,
                    config = config,
                ).also { rawCaptureState = it }
            }
            daemonService.updateCachedToolbarSnapshot(nextCapture.toToolbarSnapshot(captureEnabled = armed))
            val nextCatalog = BundleCatalogState.fromEntries(
                entries = bundles,
                activeFilter = activeFilterBacking,
                crashBundleIds = crashIds,
                crashUnknownBundleIds = crashUnknownIds,
                durationsByBundleId = durations,
                kindsByBundleId = kinds,
            )
            rawCatalogState = nextCatalog

            publishOnEdt {
                captureState = nextCapture
                captureEnabled = armed
                recoveryState = recoveryState.copy(jobs = jobs)
                catalogState = nextCatalog.copy(activeFilter = activeFilter)
                if (trashCount == 0) confirmEmptyTrashVisible = false
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

    /**
     * Fetches `timeline.summary` / `timeline.crashes` for one bundle. A failure of either query
     * leaves the affected part Unknown and schedules a back-off retry; it is never cached as a
     * successful "no crash" (`false`).
     */
    private fun fetchBundleMeta(id: String, videoMaxFps: Int, tick: Long) {
        val cached = metaCache[id]
        val base = cached ?: run {
            val summary = runCatching { daemonService.withClient { it.timelineSummary(id) } }.getOrNull()
            if (summary == null) {
                metaScheduler.markFailure(id, tick)
                return
            }
            val spanNs = summary.epochs.firstOrNull()?.let { ep ->
                (ep.endBoottimeNs - ep.startBoottimeNs).coerceAtLeast(0L)
            }
            val secs = when {
                spanNs != null && spanNs > 0L -> (spanNs / 1_000_000_000L).coerceAtLeast(1L)
                summary.frameCount > 0 -> (summary.frameCount / videoMaxFps.coerceAtLeast(1)).toLong().coerceAtLeast(1L)
                else -> null
            }
            BundleMeta(summary.bundleKind, secs, crashed = null)
        }
        val crashes = runCatching {
            daemonService.withClient { it.timelineCrashes(id).crashes }
        }.getOrNull()
        metaCache[id] = base.copy(crashed = crashes?.isNotEmpty())
        if (crashes == null) metaScheduler.markFailure(id, tick) else metaScheduler.markSuccess(id)
    }

    /**
     * An `arbd` that answered with a JSON-RPC error is reachable: the request failed for a
     * business reason, not because the transport is gone. Only genuine transport failures
     * (I/O, timeouts, no daemon) may flip the UI to `disconnected`.
     */
    private fun isDaemonBusinessError(t: Throwable): Boolean =
        generateSequence(t) { it.cause }.any { it is ArbdRpcException }

    private fun handleDaemonFailure(t: Throwable) {
        val diag = ArbSettingsConfigurable.classifyDaemonError(t)
        val stale = if (diag.kind == ArbSettingsConfigurable.DaemonDiagnosticKind.STALE_OR_MISMATCHED) {
            diag.message
        } else {
            null
        }
        if (stale == null && isDaemonBusinessError(t)) {
            logger.debug("ARB daemon business error (still connected): ${t.message}")
            return
        }
        val next = synchronized(captureStateLock) {
            CaptureBarReducer.disconnected(rawCaptureState, t.message ?: "arbd 未起動").also { rawCaptureState = it }
        }
        daemonService.updateCachedToolbarSnapshot(
            ArbToolbarDaemonSnapshot(
                probed = true,
                daemonConnected = false,
                captureActive = false,
            ),
        )
        publishOnEdt {
            captureState = next
            confirmEmptyTrashVisible = false
            if (stale != null) {
                staleDaemonMessage = stale
            }
        }
    }

    private fun publishOnEdt(block: () -> Unit) {
        if (disposed) return
        if (SwingUtilities.isEventDispatchThread()) {
            if (!disposed) block()
        } else {
            SwingUtilities.invokeLater { if (!disposed) block() }
        }
    }

    override fun dispose() {
        disposed = true
        daemonService.removeStateListener(daemonStateListener)
        // Never `shutdownNow()`: interrupting a worker blocked in the shared app-level IPC client
        // closes its interruptible `SocketChannel` (ClosedByInterruptException) and breaks the
        // in-flight call for every other ARB view. Queued tasks bail out on `disposed` instead.
        executor.shutdown()
    }

    /** Test hook: whether the worker executor has been shut down without interrupting tasks. */
    internal val isWorkerShutdown: Boolean
        get() = executor.isShutdown

    /** One device dropdown row; [serial] is `null` for non-selectable prompts/placeholders. */
    data class DeviceDropdownEntry(val label: String, val serial: String?)

    companion object {
        const val NO_DEVICES_PLACEHOLDER: String = "端末未検出（USB / エミュレータを接続）"
        const val MULTIPLE_DEVICES_PROMPT: String = "複数端末が接続中です — 対象を選択してください"
        const val WAITING_FOR_PINNED_SUFFIX: String = "切断中（再接続待ち）"
        const val CAPTURE_MODE_VIDEO_LABEL: String = "映像＋ログ"
        const val CAPTURE_MODE_LOGS_LABEL: String = "ログのみ"
        const val LOGS_ONLY_CHIP_TEXT: String = "ログのみ常時"
        const val ARMED_WAITING_DETAIL_TEXT: String = "ON・待機中（端末接続で自動開始）"

        fun captureModeLabel(mode: CaptureMode): String = when (mode) {
            CaptureMode.VIDEO_AND_LOGS -> CAPTURE_MODE_VIDEO_LABEL
            CaptureMode.LOGS_ONLY -> CAPTURE_MODE_LOGS_LABEL
        }

        /** Pure dropdown builder (see [deviceDropdownEntries]). */
        fun buildDeviceDropdownEntries(state: CaptureBarState): List<DeviceDropdownEntry> {
            val devices = state.devices
            val selection = state.deviceSelection
            val entries = mutableListOf<DeviceDropdownEntry>()
            val pinnedMissing = (selection as? DeviceSelectionOutcome.WaitingForPreferredDevice)
                ?.preferredSerial
                ?.takeIf { pinned -> devices.none { it.serial == pinned } }
            when {
                pinnedMissing != null ->
                    entries += DeviceDropdownEntry("$pinnedMissing — $WAITING_FOR_PINNED_SUFFIX", pinnedMissing)
                selection is DeviceSelectionOutcome.MultipleDevicesNeedSelection && state.selectedSerial == null ->
                    entries += DeviceDropdownEntry(MULTIPLE_DEVICES_PROMPT, null)
                devices.isEmpty() ->
                    entries += DeviceDropdownEntry(NO_DEVICES_PLACEHOLDER, null)
            }
            devices.mapTo(entries) { DeviceDropdownEntry(formatDeviceLabel(it), it.serial) }
            return entries
        }
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

        /** Formats a concise 1-line summary of all non-zero `bundle.gc` outcome buckets. */
        fun formatGcSummary(gc: BundleGcResponse): String {
            val parts = buildList {
                if (gc.purgedBundleIds.isNotEmpty()) add("削除=${gc.purgedBundleIds.size}")
                if (gc.skippedLeasedBundleIds.isNotEmpty()) add("使用中(Lease)保護=${gc.skippedLeasedBundleIds.size}")
                if (gc.skippedInGraceBundleIds.isNotEmpty()) add("猶予中=${gc.skippedInGraceBundleIds.size}")
                if (gc.skippedUnmanagedOrLegacyBundleIds.isNotEmpty()) {
                    add("外部保存先/旧形式保護=${gc.skippedUnmanagedOrLegacyBundleIds.size}")
                }
                if (gc.skippedActiveJobBundleIds.isNotEmpty()) {
                    add("保存ジョブ保護=${gc.skippedActiveJobBundleIds.size}")
                }
                if (gc.failed.isNotEmpty()) add("失敗=${gc.failed.size}")
                if (gc.reapedLeaseIds.isNotEmpty()) add("期限切れLease解放=${gc.reapedLeaseIds.size}")
            }
            return if (parts.isEmpty()) {
                "ゴミ箱に削除対象はありませんでした"
            } else {
                "ゴミ箱整理完了: ${parts.joinToString(", ")}"
            }
        }

        /**
         * Returns `(isWarning, message)` for the IDE balloon notification after emptying trash.
         * Explains clearly why any trashed items remain protected or failed.
         */
        fun formatGcNotification(gc: BundleGcResponse): Pair<Boolean, String> {
            val summary = formatGcSummary(gc)
            val hasSkipsOrFailures = gc.skippedLeasedBundleIds.isNotEmpty() ||
                gc.skippedInGraceBundleIds.isNotEmpty() ||
                gc.skippedUnmanagedOrLegacyBundleIds.isNotEmpty() ||
                gc.skippedActiveJobBundleIds.isNotEmpty() ||
                gc.failed.isNotEmpty()
            if (!hasSkipsOrFailures) {
                return false to summary
            }
            val hints = buildList {
                if (gc.skippedLeasedBundleIds.isNotEmpty()) {
                    add("ビューワで開いているバンドルはタブを閉じてから削除してください")
                }
                if (gc.skippedUnmanagedOrLegacyBundleIds.isNotEmpty()) {
                    add("外部保存先・旧形式バンドルは安全のため自動削除していません")
                }
                if (gc.skippedInGraceBundleIds.isNotEmpty()) {
                    add("猶予期間内の項目が残っています")
                }
                if (gc.skippedActiveJobBundleIds.isNotEmpty()) {
                    add("保存処理中の項目は保護されています")
                }
                if (gc.failed.isNotEmpty()) {
                    add("削除失敗: ${gc.failed.first().error}")
                }
            }
            return true to "$summary（${hints.joinToString(" / ")}）"
        }

        /** `true` when any requested entry was kept (present, unmounted, leased or saving). */
        fun forgetNeedsWarning(res: BundleForgetResponse): Boolean =
            res.skippedPresentBundleIds.isNotEmpty() ||
                res.skippedStorageUnavailableBundleIds.isNotEmpty() ||
                res.skippedLeasedBundleIds.isNotEmpty() ||
                res.skippedActiveJobBundleIds.isNotEmpty()

        /** Formats a concise summary of a non-destructive `bundle.forget` response. */
        fun formatForgetSummary(res: BundleForgetResponse): String {
            val parts = buildList {
                if (res.forgottenBundleIds.isNotEmpty()) add("一覧から削除=${res.forgottenBundleIds.size}")
                if (res.reapedLeaseIds.isNotEmpty()) add("期限切れLease解放=${res.reapedLeaseIds.size}")
                if (res.skippedPresentBundleIds.isNotEmpty()) add("実在のためスキップ=${res.skippedPresentBundleIds.size}")
                if (res.skippedStorageUnavailableBundleIds.isNotEmpty()) {
                    add("保存先未接続のため保護=${res.skippedStorageUnavailableBundleIds.size}")
                }
                if (res.skippedLeasedBundleIds.isNotEmpty()) add("使用中(Lease)保護=${res.skippedLeasedBundleIds.size}")
                if (res.skippedActiveJobBundleIds.isNotEmpty()) {
                    add("保存ジョブ保護=${res.skippedActiveJobBundleIds.size}")
                }
            }
            return if (parts.isEmpty()) {
                "一覧から整理する対象はありませんでした"
            } else {
                "一覧整理完了: ${parts.joinToString(", ")}"
            }
        }
    }
}

internal fun CaptureBarState.toToolbarSnapshot(
    captureEnabled: Boolean = this.config.captureEnabled,
): ArbToolbarDaemonSnapshot {
    val hint = when (val sel = this.deviceSelection) {
        is DeviceSelectionOutcome.MultipleDevicesNeedSelection ->
            if (this.selectedSerial == null) ArbToolWindowController.MULTIPLE_DEVICES_PROMPT else null
        is DeviceSelectionOutcome.WaitingForPreferredDevice ->
            "${sel.preferredSerial} は${ArbToolWindowController.WAITING_FOR_PINNED_SUFFIX}"
        else -> when (this.selectedDevice?.state) {
            AdbDeviceState.Unauthorized -> "端末が未承認です。USB デバッグを許可してください。"
            AdbDeviceState.Offline -> "端末がオフラインです。接続を確認してください。"
            null -> ArbToolWindowController.NO_DEVICES_PLACEHOLDER
            AdbDeviceState.Device -> null
            else -> "端末が録画できる状態ではありません（${this.selectedDevice?.state?.wireName}）。"
        }
    }
    return ArbToolbarDaemonSnapshot(
        probed = true,
        daemonConnected = this.daemonConnected,
        captureActive = this.capture?.active == true,
        captureConnected = this.capture?.connected == true,
        captureEnabled = captureEnabled,
        hasBufferedData = this.ringChunkCount > 0L,
        hasOnlineDevice = this.isSelectedDeviceOnline,
        selectedDeviceSerial = this.selectedDevice?.serial,
        disabledDeviceHint = hint,
    )
}

/**
 * Persists `capture_enabled` (always-on capture) through `config.update`. Returns the daemon's
 * stored value, or `null` when the running `arbd` predates `capture_enabled` and rejects the patch
 * field; callers then fall back to a plain `capture.start` / `capture.stop` toggle.
 */
internal fun persistCaptureEnabled(daemonService: ArbdDaemonService, enabled: Boolean): Boolean? {
    val updated = try {
        daemonService.withClient { it.configUpdate(ArbConfigPatch(captureEnabled = enabled)).config }
    } catch (e: ArbdRpcException) {
        if (!e.isUnknownCaptureEnabledField) throw e
        return null
    }
    daemonService.notifyConfigUpdated(updated)
    return updated.captureEnabled
}

/** Result of [setCaptureEnabled]: the new state and the device a capture was started on. */
internal data class CaptureToggleResult(val active: Boolean, val armed: Boolean, val startedSerial: String?)

/**
 * The capture ON/OFF toggle shared by the Tool Window and the toolbar / Tools menu action.
 * OFF persists `capture_enabled = false` and stops a [currentlyActive] capture. ON persists
 * `capture_enabled = true` and, when [serial] is given and nothing records yet, starts on it now;
 * without always-on support (`armed == false`) that start is the whole toggle, so its error is
 * thrown. The resulting state is published to the toolbar snapshot.
 */
internal fun setCaptureEnabled(
    daemonService: ArbdDaemonService,
    enable: Boolean,
    currentlyActive: Boolean,
    serial: String?,
): CaptureToggleResult {
    if (!enable) {
        persistCaptureEnabled(daemonService, enabled = false)
        if (currentlyActive) {
            daemonService.withClient { it.captureStop() }
        }
        daemonService.updateCachedCaptureState(active = false, enabled = false)
        return CaptureToggleResult(active = false, armed = false, startedSerial = null)
    }
    val armed = persistCaptureEnabled(daemonService, enabled = true) == true
    val started = if (!currentlyActive && !serial.isNullOrBlank()) {
        val attempt = runCatching {
            daemonService.withClient { it.captureStart(CaptureStartParams(serial = serial)) }
        }
        if (!armed) attempt.getOrThrow()
        attempt.getOrNull()?.takeIf { it.started }?.serial
    } else {
        null
    }
    val active = currentlyActive || started != null
    daemonService.updateCachedCaptureState(active = active, enabled = armed)
    return CaptureToggleResult(active = active, armed = armed, startedSerial = started)
}
