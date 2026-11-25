package io.github.shinma06.arb.ide

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.text.input.TextFieldState
import androidx.compose.foundation.text.input.setTextAndPlaceCursorAtEnd
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import com.intellij.openapi.fileChooser.FileChooser
import com.intellij.openapi.fileChooser.FileChooserDescriptor
import com.intellij.openapi.fileChooser.FileChooserDescriptorFactory
import com.intellij.openapi.ide.CopyPasteManager
import com.intellij.openapi.options.Configurable
import com.intellij.openapi.options.ConfigurationException
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.LocalFileSystem
import io.github.shinma06.arb.model.ArbConfig
import io.github.shinma06.arb.model.ArbdRpcException
import io.github.shinma06.arb.model.CaptureMode
import io.github.shinma06.arb.model.IdeDefaultSaveAction
import io.github.shinma06.arb.state.ArbSettingsDraft
import io.github.shinma06.arb.state.StorageSizeUnit
import io.github.shinma06.arb.state.StorageSizeValue
import io.github.shinma06.arb.ui.ArbJewelPalette
import io.github.shinma06.arb.ui.ArbLabeledCheckbox
import io.github.shinma06.arb.ui.ArbScrollableColumn
import io.github.shinma06.arb.ui.ArbSoftBanner
import io.github.shinma06.arb.ui.ArbSubtleAction
import org.jetbrains.jewel.bridge.JewelComposePanel
import org.jetbrains.jewel.ui.component.GroupHeader
import org.jetbrains.jewel.ui.component.ListComboBox
import org.jetbrains.jewel.ui.component.OutlinedButton
import org.jetbrains.jewel.ui.component.Text
import org.jetbrains.jewel.ui.component.TextField
import org.jetbrains.jewel.ui.icons.AllIconsKeys
import java.awt.Toolkit
import java.awt.datatransfer.StringSelection
import java.nio.file.Path
import java.util.Locale
import javax.swing.JComponent

/**
 * Pure Kotlin + Compose state holder for `Settings -> Tools -> Android Replay Buffer`.
 *
 * Fully headless-testable without Skiko or a live IDE frame: owns daemon settings draft,
 * IDE-local overrides (custom `arbd` path, Logcat screenshot destination inheritance + custom
 * screenshot override), and 1-click CLI / MCP path & JSON copy state.
 */
class ArbSettingsFormModel(
    bundledCliPath: String?,
) {
    var bundledCliPath by mutableStateOf(bundledCliPath)
        private set

    fun updateCliPath(path: String?) {
        bundledCliPath = path
        cliPathReadOnlyText.setTextAndPlaceCursorAtEnd(path ?: CLI_PATH_UNAVAILABLE_TEXT)
    }
    val saveWindowText = TextFieldState(ArbConfig.DEFAULT_SAVE_WINDOW_SECS.toString())
    val ringBudgetAmountText = TextFieldState("1.5")
    var ringBudgetUnit by mutableStateOf(StorageSizeUnit.GIB)

    val minFreeDiskAmountText = TextFieldState("3.0")
    var minFreeDiskUnit by mutableStateOf(StorageSizeUnit.GIB)

    var videoMaxSizeOptions by mutableStateOf(DEFAULT_VIDEO_MAX_SIZES)
        private set
    var videoMaxSize by mutableStateOf(ArbConfig.DEFAULT_VIDEO_MAX_SIZE)

    var videoMaxFpsOptions by mutableStateOf(DEFAULT_VIDEO_MAX_FPS)
        private set
    var videoMaxFps by mutableStateOf(ArbConfig.DEFAULT_VIDEO_MAX_FPS)

    var audioEnabled by mutableStateOf(true)
    var captureMode by mutableStateOf(CaptureMode.VIDEO_AND_LOGS)
    var crashAutosaveEnabled by mutableStateOf(true)
    val crashCooldownText = TextFieldState(ArbConfig.DEFAULT_CRASH_AUTOSAVE_COOLDOWN_SECS.toString())

    val outputDirText = TextFieldState("")
    var ideDefaultSaveAction by mutableStateOf(IdeDefaultSaveAction.SAVE_AND_OPEN)

    // Screenshot destination inheritance (IDE-local, safe reflection + fallback to output_dir)
    var screenshotInheritLogcat by mutableStateOf(true)
    val customScreenshotDirText = TextFieldState("")
    var screenshotStatusText by mutableStateOf("")

    // Local binary path override & OS tray icon toggle (IDE-local)
    val customArbdPathText = TextFieldState("")
    var trayEnabled by mutableStateOf(false)

    // Daemon fields this page does not edit (pinned device from the Tool Window / CLI, and the
    // CLI/MCP `screenshot_dir_override`). Carried through unchanged so the draft never reports a
    // phantom modification and Apply never clears them.
    private var passthroughDeviceSerial: String = ""
    private var passthroughScreenshotDirOverride: String = ""

    // CLI / MCP copy section
    val cliPathReadOnlyText = TextFieldState(bundledCliPath ?: CLI_PATH_UNAVAILABLE_TEXT)
    var copyFeedback by mutableStateOf("")

    // Daemon connectivity / stale-arbd banner
    var daemonOnline by mutableStateOf(true)
    var daemonBannerMessage by mutableStateOf("")
    var showRestartAction by mutableStateOf(false)

    val isBannerVisible: Boolean
        get() = daemonBannerMessage.isNotBlank()

    val isDaemonControlsEnabled: Boolean
        get() = daemonOnline

    val mcpConfigJson: String?
        get() = bundledCliPath?.takeIf { it.isNotBlank() }?.let(ArbCliMcpPathResolver::formatMcpConfigJson)

    fun setDaemonStatus(
        online: Boolean,
        detailMessage: String? = null,
        showRestart: Boolean = !online,
    ) {
        daemonOnline = online
        if (!online || detailMessage != null) {
            daemonBannerMessage = detailMessage
                ?: "arbd デーモンに接続できません（オフライン）。ローカルの arbd バイナリパス設定を変更するか、arbd を再起動してください。"
            showRestartAction = showRestart
        } else {
            daemonBannerMessage = ""
            showRestartAction = false
        }
    }

    fun loadFromDraft(
        draft: ArbSettingsDraft,
        screenshotInheritLogcat: Boolean = this.screenshotInheritLogcat,
        customScreenshotDir: String = this.customScreenshotDirText.text.toString(),
        trayEnabled: Boolean = this.trayEnabled,
    ) {
        saveWindowText.setTextAndPlaceCursorAtEnd(
            draft.saveWindowSecs.coerceIn(
                ArbConfig.MIN_SAVE_WINDOW_SECS,
                ArbConfig.MAX_SAVE_WINDOW_SECS,
            ).toString(),
        )

        val ringVal = StorageSizeValue.fromBytes(draft.ringBudgetBytes)
        ringBudgetAmountText.setTextAndPlaceCursorAtEnd(formatDecimal(ringVal.amount))
        ringBudgetUnit = ringVal.unit

        val diskVal = StorageSizeValue.fromBytes(draft.minFreeDiskBytes)
        minFreeDiskAmountText.setTextAndPlaceCursorAtEnd(formatDecimal(diskVal.amount))
        minFreeDiskUnit = diskVal.unit

        videoMaxSizeOptions = (DEFAULT_VIDEO_MAX_SIZES + draft.videoMaxSize).distinct().sorted()
        videoMaxSize = draft.videoMaxSize

        videoMaxFpsOptions = (DEFAULT_VIDEO_MAX_FPS + draft.videoMaxFps).distinct().sorted()
        videoMaxFps = draft.videoMaxFps

        audioEnabled = draft.audioEnabled
        captureMode = draft.captureMode
        passthroughDeviceSerial = draft.deviceSerial
        passthroughScreenshotDirOverride = draft.screenshotDirOverride
        crashAutosaveEnabled = draft.crashAutosaveEnabled
        crashCooldownText.setTextAndPlaceCursorAtEnd(
            draft.crashAutosaveCooldownSecs.coerceIn(
                ArbConfig.MIN_CRASH_AUTOSAVE_COOLDOWN_SECS,
                ArbConfig.MAX_CRASH_AUTOSAVE_COOLDOWN_SECS,
            ).toString(),
        )
        outputDirText.setTextAndPlaceCursorAtEnd(draft.outputDirOverride)
        ideDefaultSaveAction = draft.ideDefaultSaveAction
        customArbdPathText.setTextAndPlaceCursorAtEnd(draft.customArbdBinaryPath)
        this.screenshotInheritLogcat = screenshotInheritLogcat
        this.customScreenshotDirText.setTextAndPlaceCursorAtEnd(customScreenshotDir.trim())
        this.trayEnabled = trayEnabled
    }

    fun currentDraft(): ArbSettingsDraft {
        val windowSecs = saveWindowText.text.toString().trim().toIntOrNull() ?: -1
        val ringAmount = ringBudgetAmountText.text.toString().trim().toDoubleOrNull() ?: -1.0
        val diskAmount = minFreeDiskAmountText.text.toString().trim().toDoubleOrNull() ?: -1.0
        val cooldownSecs = crashCooldownText.text.toString().trim().toIntOrNull() ?: -1
        return ArbSettingsDraft(
            saveWindowSecs = windowSecs,
            ringBudgetBytes = if (ringAmount > 0.0) StorageSizeValue(ringAmount, ringBudgetUnit).toBytes() else -1L,
            minFreeDiskBytes = if (diskAmount > 0.0) StorageSizeValue(diskAmount, minFreeDiskUnit).toBytes() else -1L,
            videoMaxSize = videoMaxSize,
            videoMaxFps = videoMaxFps,
            audioEnabled = audioEnabled,
            crashAutosaveEnabled = crashAutosaveEnabled,
            crashAutosaveCooldownSecs = cooldownSecs,
            outputDirOverride = outputDirText.text.toString().trim(),
            ideDefaultSaveAction = ideDefaultSaveAction,
            customArbdBinaryPath = customArbdPathText.text.toString().trim(),
            captureMode = captureMode,
            deviceSerial = passthroughDeviceSerial,
            screenshotDirOverride = passthroughScreenshotDirOverride,
        )
    }

    fun currentLocalScreenshotSettings(): Pair<Boolean, String> =
        screenshotInheritLogcat to customScreenshotDirText.text.toString().trim()

    companion object {
        val DEFAULT_VIDEO_MAX_SIZES: List<Int> = listOf(1024, 1280, 1920, 2560, 3840)
        val DEFAULT_VIDEO_MAX_FPS: List<Int> = listOf(15, 30, 60)
        const val CLI_PATH_UNAVAILABLE_TEXT: String = "arb-cli が見つかりません（プラグイン再インストールまたはビルドが必要）"

        private fun formatDecimal(value: Double): String =
            if (value % 1.0 == 0.0) String.format(Locale.ROOT, "%.0f", value) else String.format(Locale.ROOT, "%.1f", value)
    }
}

/**
 * Android Studio `Settings -> Tools -> Android Replay Buffer` (`id = io.github.shinma06.arb.settings`).
 *
 * Implements [Configurable.NoScroll] so Compose handles its own vertical scrolling cleanly inside
 * Android Studio's Settings pane without unbounded preferred-size measurement or horizontal scroll.
 */
class ArbSettingsConfigurable internal constructor(
    private val project: Project?,
    private val daemonConfigReader: () -> ArbConfig,
    private val daemonConfigWriter: (ArbConfig, ArbSettingsDraft) -> ArbConfig,
    private val localBinaryReader: () -> String,
    private val localBinaryWriter: (String) -> Unit,
    private val localScreenshotReader: () -> Pair<Boolean, String> = { true to "" },
    private val localScreenshotWriter: (Boolean, String) -> Unit = { _, _ -> },
    private val localTrayReader: () -> Boolean = { false },
    private val localTrayWriter: (Boolean) -> Unit = {},
    private val screenshotStatusResolver: (Boolean, String) -> String = { inherit, custom ->
        ArbScreenshotDestinationResolver.resolveDirectory(project, inherit, custom).statusText
    },
    private val cliPathProvider: () -> String? = {
        runCatching { ArbdDaemonService.getInstance().resolveBundledCliPath()?.toString() }.getOrNull()
    },
    private val daemonRestarter: () -> Unit = {},
    private val clipboardWriter: (String) -> Unit = ::copyTextToClipboard,
    private val blockingRunner: DaemonCallRunner = DaemonCallRunner.Inline,
    /**
     * Runs slow, possibly I/O-bound lookups (Studio's Logcat screenshot settings, VFS) off the
     * EDT and hands the result back on the EDT; see [BackgroundUiRunner].
     */
    private val backgroundUi: BackgroundUiRunner = BackgroundUiRunner.Pooled,
) : Configurable, Configurable.NoScroll {
    // Constructor used by IntelliJ Platform `projectConfigurable` extension instantiation.
    constructor(project: Project) : this(
        project = project,
        daemonConfigReader = {
            val service = ArbdDaemonService.getInstance()
            val cfg = service.withClient { it.configGet().config }
            service.updateCachedConfig(cfg)
            cfg
        },
        daemonConfigWriter = { baseline, draft ->
            val service = ArbdDaemonService.getInstance()
            val updated = service.withClient { it.configUpdate(draft.toConfigPatch(baseline)).config }
            service.notifyConfigUpdated(updated)
            updated
        },
        localBinaryReader = {
            ArbLocalSettingsState.getInstanceOrNull()?.customArbdBinaryPath.orEmpty()
        },
        localBinaryWriter = { newPath ->
            checkNotNull(ArbLocalSettingsState.getInstanceOrNull()) { "IDE 設定を取得できません" }.customArbdBinaryPath = newPath
            runCatching { ArbdDaemonService.getInstance().notifyBinaryPathChanged() }
        },
        localScreenshotReader = {
            val st = ArbLocalSettingsState.getInstanceOrNull()
            (st?.screenshotInheritLogcat ?: true) to st?.customScreenshotDir.orEmpty()
        },
        localScreenshotWriter = { inherit, customDir ->
            val st = checkNotNull(ArbLocalSettingsState.getInstanceOrNull()) { "IDE 設定を取得できません" }
            st.screenshotInheritLogcat = inherit
            st.customScreenshotDir = customDir
        },
        localTrayReader = {
            ArbLocalSettingsState.getInstanceOrNull()?.trayEnabled ?: false
        },
        localTrayWriter = { enabled ->
            checkNotNull(ArbLocalSettingsState.getInstanceOrNull()) { "IDE 設定を取得できません" }.trayEnabled = enabled
            runCatching { ArbdDaemonService.getInstance().syncTrayStateAsync(enabled) }
        },
        daemonRestarter = {
            ArbdDaemonService.getInstance().restartDaemon()
        },
        blockingRunner = ProgressDaemonCallRunner(project),
    )

    private var form: ArbSettingsFormModel? = null
    private var baselineConfig: ArbConfig = ArbConfig()
    private var baselineCustomBinary: String = ""
    private var baselineScreenshotInherit: Boolean = true
    private var baselineCustomScreenshotDir: String = ""
    private var baselineTrayEnabled: Boolean = false
    private var hasDaemonBaseline: Boolean = false

    override fun getDisplayName(): String = "Android Replay Buffer"

    /** Creates the pure form model and runs initial load without instantiating Compose/Skiko. */
    internal fun createFormModel(): ArbSettingsFormModel {
        val created = ArbSettingsFormModel(bundledCliPath = null)
        form = created
        backgroundUi.run(cliPathProvider) { path -> if (form === created) created.updateCliPath(path) }
        reset()
        return created
    }

    override fun createComponent(): JComponent {
        val created = createFormModel()
        return JewelComposePanel {
            ArbSettingsContent(
                form = created,
                onRestartDaemon = { restartDaemonAndReload() },
                onBrowseOutputDir = { chooseFolder("保存先フォルダを選択", created.outputDirText) },
                onBrowseScreenshotDir = {
                    chooseFolder("スクリーンショット保存先フォルダを選択", created.customScreenshotDirText) {
                        refreshScreenshotStatus(created)
                    }
                },
                onBrowseBinary = { chooseBinary("arbd バイナリを選択", created.customArbdPathText) },
                onScreenshotOptionChanged = { refreshScreenshotStatus(created) },
                onCopyCliPath = { copyCliPath(created) },
                onCopyMcpJson = { copyMcpJson(created) },
            )
        }
    }

    internal fun copyCliPath(model: ArbSettingsFormModel = requireNotNull(form)): Boolean {
        val path = model.bundledCliPath?.takeIf { it.isNotBlank() } ?: return false
        clipboardWriter(path)
        model.copyFeedback = "CLI パスをコピーしました"
        return true
    }

    internal fun copyMcpJson(model: ArbSettingsFormModel = requireNotNull(form)): Boolean {
        val json = model.mcpConfigJson ?: return false
        clipboardWriter(json)
        model.copyFeedback = "MCP 設定 JSON をコピーしました"
        return true
    }

    internal fun restartDaemonAndReload() {
        val currentForm = form ?: return
        val draftPath = currentForm.currentDraft().normalizedCustomArbdPath()
        var localBinarySavedForRestart = false
        // An emptied path is a change too: it switches back to the bundled arbd on restart.
        if (draftPath != baselineCustomBinary) {
            val usable = draftPath.isEmpty() || runCatching { Path.of(draftPath).isAbsolute }.getOrDefault(false)
            if (usable) {
                val wrote = runCatching { blockingRunner.run { localBinaryWriter(draftPath) } }
                if (wrote.isSuccess) {
                    baselineCustomBinary = draftPath
                    localBinarySavedForRestart = true
                } else {
                    val why = wrote.exceptionOrNull()?.message?.takeIf { it.isNotBlank() } ?: "詳細不明"
                    currentForm.setDaemonStatus(
                        online = currentForm.daemonOnline,
                        detailMessage = "arbd バイナリパスを保存できませんでした（$why）。再起動は行っていません。",
                        showRestart = true,
                    )
                    return
                }
            }
        }
        val restarted = runCatching { blockingRunner.run { daemonRestarter() } }
        if (restarted.isSuccess) {
            // Keep the baseline that the user edited: Apply sends only its differences.
            // Replacing it with the restarted daemon's config would drop the pending draft.
            if (hasDaemonBaseline && currentForm.currentDraft().isDaemonModifiedAgainst(baselineConfig)) {
                currentForm.setDaemonStatus(online = true)
            } else {
                val screenshot = currentForm.currentLocalScreenshotSettings()
                val tray = currentForm.trayEnabled
                val screenshotChanged = screenshot != (baselineScreenshotInherit to baselineCustomScreenshotDir)
                val trayChanged = tray != baselineTrayEnabled
                reset()
                if (screenshotChanged || trayChanged) {
                    currentForm.loadFromDraft(
                        currentForm.currentDraft(),
                        if (screenshotChanged) screenshot.first else baselineScreenshotInherit,
                        if (screenshotChanged) screenshot.second else baselineCustomScreenshotDir,
                        if (trayChanged) tray else baselineTrayEnabled,
                    )
                    refreshScreenshotStatus(currentForm)
                }
            }
        } else {
            val err = restarted.exceptionOrNull()?.message?.takeIf { it.isNotBlank() } ?: "詳細不明"
            currentForm.setDaemonStatus(
                online = false,
                detailMessage = "arbd の再起動に失敗しました（$err）。ローカルの arbd バイナリパスを確認してください。" +
                    if (localBinarySavedForRestart) "（バイナリパスの変更は保存済みです）" else "",
                showRestart = true,
            )
        }
    }

    override fun isModified(): Boolean {
        val currentForm = form ?: return false
        val draft = currentForm.currentDraft()
        val (inherit, customShot) = currentForm.currentLocalScreenshotSettings()
        val localBinaryChanged = draft.normalizedCustomArbdPath() != baselineCustomBinary
        val localScreenshotChanged = inherit != baselineScreenshotInherit || customShot != baselineCustomScreenshotDir
        val localTrayChanged = currentForm.trayEnabled != baselineTrayEnabled
        val daemonChanged = hasDaemonBaseline && draft.isDaemonModifiedAgainst(baselineConfig)
        return localBinaryChanged || localScreenshotChanged || localTrayChanged || daemonChanged
    }

    @Throws(ConfigurationException::class)
    override fun apply() {
        val currentForm = form ?: return
        val draft = currentForm.currentDraft()
        val (inherit, customShot) = currentForm.currentLocalScreenshotSettings()
        val trayEnabled = currentForm.trayEnabled

        val newCustomPath = draft.normalizedCustomArbdPath()
        if (newCustomPath.isNotEmpty()) {
            val isAbsolute = runCatching { Path.of(newCustomPath).isAbsolute }.getOrDefault(false)
            if (!isAbsolute) {
                throw ConfigurationException("arbd バイナリパスには絶対パスを指定してください。")
            }
        }

        if (!inherit && customShot.isNotEmpty()) {
            val isAbsolute = runCatching { Path.of(customShot).isAbsolute }.getOrDefault(false)
            if (!isAbsolute) {
                throw ConfigurationException("スクリーンショットの保存先には絶対パスを指定してください。")
            }
        }

        val saved = mutableListOf<String>()
        val failed = mutableListOf<String>()

        if (hasDaemonBaseline && draft.isDaemonModifiedAgainst(baselineConfig)) {
            val validationError = draft.validateDaemonFields()
            if (validationError != null) {
                throw ConfigurationException(validationError)
            }
            try {
                baselineConfig = blockingRunner.run { daemonConfigWriter(baselineConfig, draft) }
                currentForm.setDaemonStatus(online = true)
                saved += DAEMON_SETTINGS_LABEL
            } catch (t: Throwable) {
                // The daemon is the first writer: nothing local was written yet, and the baselines
                // are untouched, so the form keeps the unsaved values and stays "modified".
                val diagnostic = classifyDaemonError(t)
                if (generateSequence(t) { it.cause }.none { it is ArbdRpcException } ||
                    diagnostic.kind == DaemonDiagnosticKind.STALE_OR_MISMATCHED
                ) {
                    currentForm.setDaemonStatus(online = false, detailMessage = diagnostic.message, showRestart = true)
                }
                throw ConfigurationException(
                    "arbd への設定保存に失敗しました: ${t.message ?: "詳細不明"}。" +
                        "arbd への保存を確認できません。ローカル設定は未保存です。再適用できます。",
                )
            }
        }

        if (newCustomPath != baselineCustomBinary) {
            if (applyLocal("arbd バイナリパス", saved, failed) { blockingRunner.run { localBinaryWriter(newCustomPath) } }) {
                baselineCustomBinary = newCustomPath
            }
        }

        if (inherit != baselineScreenshotInherit || customShot != baselineCustomScreenshotDir) {
            if (applyLocal("スクリーンショット保存先", saved, failed) { localScreenshotWriter(inherit, customShot) }) {
                baselineScreenshotInherit = inherit
                baselineCustomScreenshotDir = customShot
            }
        }

        if (trayEnabled != baselineTrayEnabled) {
            if (applyLocal("トレイアイコン", saved, failed) { localTrayWriter(trayEnabled) }) {
                baselineTrayEnabled = trayEnabled
            }
        }

        if (failed.isNotEmpty()) {
            // Keep the typed values (they differ from the baselines, so the page stays modified and a
            // second Apply retries only what failed); never show a failed value as saved.
            throw ConfigurationException(partialApplyMessage(saved, failed))
        }

        currentForm.loadFromDraft(
            draft = ArbSettingsDraft.fromConfig(baselineConfig, baselineCustomBinary),
            screenshotInheritLogcat = baselineScreenshotInherit,
            customScreenshotDir = baselineCustomScreenshotDir,
            trayEnabled = baselineTrayEnabled,
        )
        refreshScreenshotStatus(currentForm)
    }

    private fun applyLocal(label: String, saved: MutableList<String>, failed: MutableList<String>, write: () -> Unit): Boolean =
        try {
            write()
            saved += label
            true
        } catch (t: Throwable) {
            failed += "$label（${t.message?.takeIf { it.isNotBlank() } ?: "詳細不明"}）"
            false
        }

    override fun reset() {
        val currentForm = form ?: return
        baselineCustomBinary = localBinaryReader().trim()
        val (inherit, customShot) = localScreenshotReader()
        baselineScreenshotInherit = inherit
        baselineCustomScreenshotDir = customShot.trim()
        baselineTrayEnabled = localTrayReader()

        val loaded = runCatching { blockingRunner.run { daemonConfigReader() } }
        if (loaded.isSuccess) {
            hasDaemonBaseline = true
            baselineConfig = loaded.getOrThrow()
            currentForm.setDaemonStatus(online = true)
        } else {
            hasDaemonBaseline = false
            baselineConfig = ArbConfig()
            val diagnostic = classifyDaemonError(loaded.exceptionOrNull())
            currentForm.setDaemonStatus(
                online = false,
                detailMessage = diagnostic.message,
                showRestart = true,
            )
        }
        currentForm.loadFromDraft(
            draft = ArbSettingsDraft.fromConfig(baselineConfig, baselineCustomBinary),
            screenshotInheritLogcat = baselineScreenshotInherit,
            customScreenshotDir = baselineCustomScreenshotDir,
            trayEnabled = baselineTrayEnabled,
        )
        refreshScreenshotStatus(currentForm)
    }

    override fun disposeUIResources() {
        form = null
    }

    internal fun currentFormForTest(): ArbSettingsFormModel? = form

    /**
     * Resolves the screenshot destination text off the EDT (Studio's Logcat screenshot settings
     * are read reflectively and may touch the file system) and applies it on the EDT, unless the
     * form was replaced or the selection changed meanwhile.
     */
    private fun refreshScreenshotStatus(model: ArbSettingsFormModel) {
        val selection = model.currentLocalScreenshotSettings()
        backgroundUi.run(
            work = { screenshotStatusResolver(selection.first, selection.second) },
            onEdt = { text ->
                if (form === model && model.currentLocalScreenshotSettings() == selection) {
                    model.screenshotStatusText = text
                }
            },
        )
    }

    private fun chooseFolder(titleText: String, target: TextFieldState, onChosen: () -> Unit = {}) {
        val descriptor = FileChooserDescriptorFactory.createSingleFolderDescriptor().apply {
            title = titleText
        }
        chooseFile(descriptor, target, onChosen)
    }

    private fun chooseBinary(titleText: String, target: TextFieldState) {
        val descriptor = FileChooserDescriptorFactory.createSingleFileNoJarsDescriptor().apply {
            title = titleText
        }
        chooseFile(descriptor, target) {}
    }

    /** Opens the chooser at the current path, resolving that VFS entry off the EDT first. */
    private fun chooseFile(descriptor: FileChooserDescriptor, target: TextFieldState, onChosen: () -> Unit) {
        val owner = form ?: return
        val current = target.text.toString().trim()
        backgroundUi.run(
            work = { current.takeIf { it.isNotEmpty() }?.let { LocalFileSystem.getInstance().findFileByPath(it) } },
            onEdt = { initial ->
                if (form !== owner || project?.isDisposed == true) return@run
                FileChooser.chooseFile(descriptor, project, initial) { selected ->
                    if (form !== owner) return@chooseFile
                    target.setTextAndPlaceCursorAtEnd(selected.toNioPath().toAbsolutePath().normalize().toString())
                    onChosen()
                }
            },
        )
    }

    enum class DaemonDiagnosticKind {
        STALE_OR_MISMATCHED,
        OFFLINE,
    }

    data class DaemonDiagnostic(
        val kind: DaemonDiagnosticKind,
        val message: String,
    )

    companion object {
        const val DAEMON_SETTINGS_LABEL: String = "arbd 設定"

        /** Wording of a partially applied Apply: what is persisted, what is not. */
        fun partialApplyMessage(saved: List<String>, failed: List<String>): String {
            val savedText = if (saved.isEmpty()) "なし" else saved.joinToString("、")
            return "一部の設定を保存できませんでした。保存済み: $savedText ／ 未保存: ${failed.joinToString("、")}。" +
                "未保存の値は画面に残していますが、まだ反映されていません。もう一度「適用」してください。"
        }

        fun classifyDaemonError(error: Throwable?): DaemonDiagnostic {
            var cur: Throwable? = error
            while (cur != null) {
                if (cur is ArbdRpcException && cur.isMethodNotFound) {
                    return DaemonDiagnostic(
                        kind = DaemonDiagnosticKind.STALE_OR_MISMATCHED,
                        message = "古いバージョンの arbd が起動中です（config.get 非対応）。新しい arbd へ再起動してください。",
                    )
                }
                val msg = cur.message.orEmpty()
                if (msg.contains("method not found", ignoreCase = true)) {
                    return DaemonDiagnostic(
                        kind = DaemonDiagnosticKind.STALE_OR_MISMATCHED,
                        message = "古いバージョンの arbd が起動中です（config.get 非対応）。新しい arbd へ再起動してください。",
                    )
                }
                if (msg.contains("protocol version", ignoreCase = true) ||
                    msg.contains("Incompatible arbd IPC", ignoreCase = true)
                ) {
                    return DaemonDiagnostic(
                        kind = DaemonDiagnosticKind.STALE_OR_MISMATCHED,
                        message = "互換性のないバージョンの arbd が起動中です（$msg）。新しい arbd へ再起動してください。",
                    )
                }
                cur = cur.cause
            }

            val reason = error?.message?.takeIf { it.isNotBlank() }
            val message = if (reason != null) {
                "arbd デーモンに接続できません（$reason）。ローカルの arbd バイナリパスを設定するか、arbd を再起動してください。"
            } else {
                "arbd デーモンに接続できません（オフライン）。ローカルの arbd バイナリパスを設定するか、arbd を再起動してください。"
            }
            return DaemonDiagnostic(kind = DaemonDiagnosticKind.OFFLINE, message = message)
        }
    }
}

private fun copyTextToClipboard(text: String) {
    runCatching {
        CopyPasteManager.getInstance().setContents(StringSelection(text))
    }.onFailure {
        runCatching {
            Toolkit.getDefaultToolkit().systemClipboard.setContents(StringSelection(text), null)
        }
    }
}

// ---- Native JetBrains Jewel Compose Settings UI (fits cleanly inside 520px width!) ----

private val SETTINGS_LABEL_WIDTH = 156.dp
private val SETTINGS_PATH_MAX_WIDTH = 280.dp

@Composable
private fun SettingsFormRow(
    label: String,
    content: @Composable RowScope.() -> Unit,
) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(
            text = label,
            style = ArbJewelPalette.bodyStyle(sizeSp = 12f, color = ArbJewelPalette.primaryText()),
            modifier = Modifier.width(SETTINGS_LABEL_WIDTH),
        )
        content()
    }
}

private fun RowScope.settingsFlexWidth(max: Dp): Modifier =
    Modifier.weight(1f, fill = false).widthIn(max = max).fillMaxWidth()

@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun ArbSettingsContent(
    form: ArbSettingsFormModel,
    onRestartDaemon: () -> Unit,
    onBrowseOutputDir: () -> Unit,
    onBrowseScreenshotDir: () -> Unit,
    onBrowseBinary: () -> Unit,
    onScreenshotOptionChanged: () -> Unit,
    onCopyCliPath: () -> Unit,
    onCopyMcpJson: () -> Unit,
) {
    LaunchedEffect(form) {
        snapshotFlow { form.screenshotInheritLogcat to form.customScreenshotDirText.text.toString() }
            .collect { onScreenshotOptionChanged() }
    }
    val daemonEnabled = form.isDaemonControlsEnabled
    ArbScrollableColumn(
        modifier = Modifier
            .fillMaxSize()
            .background(ArbJewelPalette.panelBackground()),
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(start = 6.dp, end = 16.dp, top = 6.dp, bottom = 16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            // Wrapped stale-arbd / offline warning banner with 1-click restart
            if (form.isBannerVisible) {
                ArbSoftBanner(
                    background = ArbJewelPalette.warningAmberSoft(),
                    accent = ArbJewelPalette.warningAmber(),
                ) {
                    Text(
                        text = form.daemonBannerMessage,
                        style = ArbJewelPalette.bodyStyle(sizeSp = 11.5f, color = ArbJewelPalette.primaryText()),
                        modifier = Modifier.weight(1f),
                    )
                    if (form.showRestartAction) {
                        OutlinedButton(onClick = onRestartDaemon) {
                            Text("arbd を再起動", maxLines = 1, softWrap = false)
                        }
                    }
                }
            }

            // 1. Capture & Ring Buffer
            GroupHeader("録画・リングバッファ設定（arbd 永続設定）", modifier = Modifier.semantics { heading() })
            SettingsFormRow("保持ウィンドウ:") {
                TextField(
                    state = form.saveWindowText,
                    enabled = daemonEnabled,
                    modifier = Modifier.width(88.dp).semantics { contentDescription = "保持ウィンドウ秒数" },
                )
                Text("秒 (5〜600s)", style = ArbJewelPalette.bodyStyle(11.5f, color = ArbJewelPalette.mutedText()))
            }
            SettingsFormRow("リングバッファ上限:") {
                TextField(
                    state = form.ringBudgetAmountText,
                    enabled = daemonEnabled,
                    modifier = Modifier.width(88.dp).semantics { contentDescription = "リングバッファ上限数値" },
                )
                val units = StorageSizeUnit.entries
                key(form.ringBudgetUnit) {
                    ListComboBox(
                        items = units.map { it.label },
                        selectedIndex = units.indexOf(form.ringBudgetUnit).coerceAtLeast(0),
                        onSelectedItemChange = { form.ringBudgetUnit = units[it] },
                        enabled = daemonEnabled,
                        modifier = Modifier.width(84.dp),
                    )
                }
            }
            SettingsFormRow("最低空き容量フロア:") {
                TextField(
                    state = form.minFreeDiskAmountText,
                    enabled = daemonEnabled,
                    modifier = Modifier.width(88.dp).semantics { contentDescription = "最低空き容量フロア数値" },
                )
                val units = StorageSizeUnit.entries
                key(form.minFreeDiskUnit) {
                    ListComboBox(
                        items = units.map { it.label },
                        selectedIndex = units.indexOf(form.minFreeDiskUnit).coerceAtLeast(0),
                        onSelectedItemChange = { form.minFreeDiskUnit = units[it] },
                        enabled = daemonEnabled,
                        modifier = Modifier.width(84.dp),
                    )
                }
            }
            SettingsFormRow("映像最大長辺 (px):") {
                val opts = form.videoMaxSizeOptions
                key(opts, form.videoMaxSize) {
                    ListComboBox(
                        items = opts.map { it.toString() },
                        selectedIndex = opts.indexOf(form.videoMaxSize).coerceAtLeast(0),
                        onSelectedItemChange = { form.videoMaxSize = opts[it] },
                        enabled = daemonEnabled,
                        modifier = Modifier.width(104.dp),
                    )
                }
            }
            SettingsFormRow("映像最大 fps:") {
                val opts = form.videoMaxFpsOptions
                key(opts, form.videoMaxFps) {
                    ListComboBox(
                        items = opts.map { it.toString() },
                        selectedIndex = opts.indexOf(form.videoMaxFps).coerceAtLeast(0),
                        onSelectedItemChange = { form.videoMaxFps = opts[it] },
                        enabled = daemonEnabled,
                        modifier = Modifier.width(104.dp),
                    )
                }
            }
            SettingsFormRow("キャプチャモード:") {
                val modes = CaptureMode.entries
                key(form.captureMode) {
                    ListComboBox(
                        items = modes.map(::captureModeOptionLabel),
                        selectedIndex = modes.indexOf(form.captureMode).coerceAtLeast(0),
                        onSelectedItemChange = { form.captureMode = modes[it] },
                        enabled = daemonEnabled,
                        modifier = Modifier.width(236.dp).semantics { contentDescription = "キャプチャモード" },
                    )
                }
            }
            ArbLabeledCheckbox(
                text = "端末の再生音を録音する (API 33+)",
                checked = form.audioEnabled,
                onCheckedChange = { form.audioEnabled = it },
                enabled = daemonEnabled,
            )

            // 2. Save & Crash Auto-Save
            GroupHeader("保存・クラッシュ自動保存", modifier = Modifier.semantics { heading() })
            SettingsFormRow("IDE の既定保存アクション:") {
                val actions = listOf(
                    IdeDefaultSaveAction.SAVE_AND_OPEN to "保存して開く",
                    IdeDefaultSaveAction.SAVE_ONLY to "保存のみ",
                )
                key(form.ideDefaultSaveAction) {
                    ListComboBox(
                        items = actions.map { it.second },
                        selectedIndex = actions.indexOfFirst { it.first == form.ideDefaultSaveAction }.coerceAtLeast(0),
                        onSelectedItemChange = { form.ideDefaultSaveAction = actions[it].first },
                        enabled = daemonEnabled,
                        modifier = Modifier.width(156.dp),
                    )
                }
            }
            ArbLabeledCheckbox(
                text = "アプリクラッシュ (am_crash) 時に自動保存する",
                checked = form.crashAutosaveEnabled,
                onCheckedChange = { form.crashAutosaveEnabled = it },
                enabled = daemonEnabled,
            )
            SettingsFormRow("自動保存クールダウン:") {
                TextField(
                    state = form.crashCooldownText,
                    enabled = daemonEnabled,
                    modifier = Modifier.width(88.dp),
                )
                Text("秒", style = ArbJewelPalette.bodyStyle(11.5f, color = ArbJewelPalette.mutedText()))
            }
            SettingsFormRow("保存先フォルダ上書き:") {
                TextField(
                    state = form.outputDirText,
                    enabled = daemonEnabled,
                    modifier = settingsFlexWidth(SETTINGS_PATH_MAX_WIDTH),
                )
                OutlinedButton(onClick = onBrowseOutputDir, enabled = daemonEnabled) {
                    Text("参照...", maxLines = 1, softWrap = false)
                }
                ArbSubtleAction(
                    label = "既定",
                    enabled = daemonEnabled,
                    compact = true,
                    tooltip = "既定の保存先へ戻す",
                    onClick = { form.outputDirText.setTextAndPlaceCursorAtEnd("") },
                )
            }

            // 3. Screenshot Destination Inheritance (Owner-confirmed T-6B feature!)
            GroupHeader("スクリーンショット保存先", modifier = Modifier.semantics { heading() })
            ArbLabeledCheckbox(
                text = "Android Studio の Logcat スクリーンショット保存先を継承する",
                checked = form.screenshotInheritLogcat,
                onCheckedChange = { checked ->
                    form.screenshotInheritLogcat = checked
                    onScreenshotOptionChanged()
                },
            )
            if (!form.screenshotInheritLogcat) {
                SettingsFormRow("専用保存先フォルダ:") {
                    TextField(
                        state = form.customScreenshotDirText,
                        modifier = settingsFlexWidth(SETTINGS_PATH_MAX_WIDTH),
                    )
                    OutlinedButton(onClick = onBrowseScreenshotDir) {
                        Text("参照...", maxLines = 1, softWrap = false)
                    }
                    ArbSubtleAction(
                        label = "クリア",
                        compact = true,
                        onClick = {
                            form.customScreenshotDirText.setTextAndPlaceCursorAtEnd("")
                            onScreenshotOptionChanged()
                        },
                    )
                }
            }
            if (form.screenshotStatusText.isNotBlank()) {
                Text(
                    text = form.screenshotStatusText,
                    style = ArbJewelPalette.bodyStyle(11f, color = ArbJewelPalette.mutedText()),
                )
            }

            // 4. CLI / MCP 1-Click Copy Section (Owner-confirmed T-6B feature!)
            GroupHeader("CLI / MCP パスをコピー", modifier = Modifier.semantics { heading() })
            SettingsFormRow("同梱 arb-cli パス:") {
                TextField(
                    state = form.cliPathReadOnlyText,
                    readOnly = true,
                    modifier = settingsFlexWidth(SETTINGS_PATH_MAX_WIDTH),
                )
            }
            FlowRow(
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalArrangement = Arrangement.spacedBy(6.dp),
                modifier = Modifier.padding(start = SETTINGS_LABEL_WIDTH + 8.dp),
            ) {
                OutlinedButton(
                    onClick = onCopyCliPath,
                    enabled = !form.bundledCliPath.isNullOrBlank(),
                ) {
                    Text("CLI パスをコピー", maxLines = 1, softWrap = false)
                }
                OutlinedButton(
                    onClick = onCopyMcpJson,
                    enabled = form.mcpConfigJson != null,
                ) {
                    Text("MCP 設定 JSON をコピー", maxLines = 1, softWrap = false)
                }
            }
            if (form.copyFeedback.isNotBlank()) {
                Text(
                    text = form.copyFeedback,
                    style = ArbJewelPalette.bodyStyle(11f, color = ArbJewelPalette.successGreen()),
                    modifier = Modifier.padding(start = SETTINGS_LABEL_WIDTH + 8.dp),
                )
            } else {
                Text(
                    text = "MCP 設定 JSON 例: {\"command\": \"<path>/arb-cli\", \"args\": [\"mcp\"]}",
                    style = ArbJewelPalette.tabularMonoStyle(10.5f, color = ArbJewelPalette.mutedText()),
                    modifier = Modifier.padding(start = SETTINGS_LABEL_WIDTH + 8.dp),
                )
            }

            // 5. Local Execution Override & OS Menu Bar / System Tray
            GroupHeader("ローカル実行設定（IDE ローカル保存）", modifier = Modifier.semantics { heading() })
            ArbLabeledCheckbox(
                text = "メニューバー / システムトレイアイコンを表示する",
                checked = form.trayEnabled,
                onCheckedChange = { form.trayEnabled = it },
            )
            SettingsFormRow("arbd バイナリパス (任意):") {
                TextField(
                    state = form.customArbdPathText,
                    modifier = settingsFlexWidth(SETTINGS_PATH_MAX_WIDTH),
                )
                OutlinedButton(onClick = onBrowseBinary) {
                    Text("参照...", maxLines = 1, softWrap = false)
                }
                ArbSubtleAction(
                    label = "自動検出",
                    iconKey = AllIconsKeys.Actions.Refresh,
                    compact = true,
                    tooltip = "自動検出に戻す",
                    onClick = { form.customArbdPathText.setTextAndPlaceCursorAtEnd("") },
                )
            }
        }
    }
}

internal fun captureModeOptionLabel(mode: CaptureMode): String = when (mode) {
    CaptureMode.VIDEO_AND_LOGS -> "映像＋ログ（画面録画あり）"
    CaptureMode.LOGS_ONLY -> "ログのみ（低負荷・常時記録）"
}

/**
 * Runs [work] off the EDT and [onEdt] with its result on the EDT (in the modality the request
 * came from, so it also reaches a modal Settings dialog). Without an IntelliJ application
 * (headless unit tests) both run inline.
 */
internal interface BackgroundUiRunner {
    fun <T> run(work: () -> T, onEdt: (T) -> Unit)

    companion object {
        val Pooled: BackgroundUiRunner = object : BackgroundUiRunner {
            override fun <T> run(work: () -> T, onEdt: (T) -> Unit) {
                val app = com.intellij.openapi.application.ApplicationManager.getApplication()
                if (app == null) {
                    onEdt(work())
                    return
                }
                val modality = com.intellij.openapi.application.ModalityState.current()
                app.executeOnPooledThread {
                    val result = work()
                    app.invokeLater({ onEdt(result) }, modality)
                }
            }
        }
    }
}

/** Runs a potentially blocking arbd call; see [ArbSettingsConfigurable]. */
internal interface DaemonCallRunner {
    fun <T> run(block: () -> T): T

    object Inline : DaemonCallRunner {
        override fun <T> run(block: () -> T): T = block()
    }
}

internal class ProgressDaemonCallRunner(private val project: Project?) : DaemonCallRunner {
    override fun <T> run(block: () -> T): T {
        val app = com.intellij.openapi.application.ApplicationManager.getApplication()
        if (app == null || !app.isDispatchThread) return block()
        return com.intellij.openapi.progress.ProgressManager.getInstance().runProcessWithProgressSynchronously(
            com.intellij.openapi.util.ThrowableComputable<T, Exception> { block() },
            "arbd と通信中...",
            false,
            project,
        )
    }
}
