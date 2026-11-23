package io.github.shinma06.arb.ide

import com.intellij.icons.AllIcons
import com.intellij.openapi.fileChooser.FileChooser
import com.intellij.openapi.fileChooser.FileChooserDescriptorFactory
import com.intellij.openapi.options.Configurable
import com.intellij.openapi.options.ConfigurationException
import com.intellij.openapi.project.Project
import com.intellij.openapi.ui.ComboBox
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.util.ui.JBUI
import io.github.shinma06.arb.model.ArbConfig
import io.github.shinma06.arb.model.ArbdRpcException
import io.github.shinma06.arb.model.IdeDefaultSaveAction
import io.github.shinma06.arb.state.ArbSettingsDraft
import io.github.shinma06.arb.state.StorageSizeUnit
import io.github.shinma06.arb.state.StorageSizeValue
import io.github.shinma06.arb.ui.ArbTheme
import java.awt.BorderLayout
import java.awt.Component
import java.awt.Dimension
import java.awt.FlowLayout
import java.awt.GridBagConstraints
import java.awt.GridBagLayout
import java.nio.file.Path
import javax.swing.BorderFactory
import javax.swing.JButton
import javax.swing.JCheckBox
import javax.swing.JComponent
import javax.swing.JLabel
import javax.swing.JPanel
import javax.swing.JSpinner
import javax.swing.JTextArea
import javax.swing.JTextField
import javax.swing.SpinnerNumberModel

/**
 * Native IntUI Settings form for `Settings -> Tools -> Android Replay Buffer`.
 *
 * Pure Swing / `JBUI` component so headless JUnit tests can instantiate, edit, validate,
 * and verify `config.get` / `config.update` round-trips without a full IDE frame.
 */
class ArbSettingsPanel(
    private val onBrowseDirectory: (current: String, onSelected: (String) -> Unit) -> Unit = { _, _ -> },
    private val onBrowseBinary: (current: String, onSelected: (String) -> Unit) -> Unit = { _, _ -> },
    private val onRestartDaemon: () -> Unit = {},
) : JPanel(BorderLayout(0, 6)) {
    private val statusBannerTextArea = JTextArea("").apply {
        font = ArbTheme.uiFont(11.5f)
        foreground = ArbTheme.warningAmber
        isEditable = false
        isOpaque = false
        isFocusable = false
        lineWrap = true
        wrapStyleWord = false
        rows = 0
        columns = 1
        border = null
    }

    private val restartDaemonButton = ArbTheme.styleSecondaryButton(
        JButton("arbd を再起動", AllIcons.Actions.Restart),
        compact = true,
    ).apply {
        toolTipText = "古いまたは停止中の arbd を終了し、設定済みの arbd バイナリで再起動します"
    }

    private val statusBannerPanel = JPanel(BorderLayout(8, 0)).apply {
        background = ArbTheme.warningAmberSoft
        isOpaque = true
        border = BorderFactory.createCompoundBorder(
            BorderFactory.createMatteBorder(0, JBUI.scale(3), 0, 0, ArbTheme.warningAmber),
            BorderFactory.createCompoundBorder(
                BorderFactory.createLineBorder(ArbTheme.hairlineBorder, 1),
                JBUI.Borders.empty(6, 10),
            ),
        )
        val iconWrap = JPanel(BorderLayout()).apply {
            isOpaque = false
            add(JLabel(AllIcons.General.Warning), BorderLayout.NORTH)
        }
        val actionWrap = JPanel(BorderLayout()).apply {
            isOpaque = false
            add(restartDaemonButton, BorderLayout.NORTH)
        }
        add(iconWrap, BorderLayout.WEST)
        add(statusBannerTextArea, BorderLayout.CENTER)
        add(actionWrap, BorderLayout.EAST)
        isVisible = false
    }

    private val saveWindowSpinner = compactSpinner(
        JSpinner(
            SpinnerNumberModel(
                ArbConfig.DEFAULT_SAVE_WINDOW_SECS,
                ArbConfig.MIN_SAVE_WINDOW_SECS,
                ArbConfig.MAX_SAVE_WINDOW_SECS,
                5,
            ),
        ),
        widthPx = 108,
    )

    private val ringBudgetAmountSpinner = compactSpinner(
        JSpinner(SpinnerNumberModel(1.5, 1.0, 65_536.0, 0.5)),
        widthPx = 96,
    )
    private val ringBudgetUnitCombo = compactCombo(
        ComboBox(StorageSizeUnit.entries.toTypedArray()).apply {
            selectedItem = StorageSizeUnit.GIB
        },
        widthPx = 76,
    )

    private val minFreeDiskAmountSpinner = compactSpinner(
        JSpinner(SpinnerNumberModel(3.0, 1.0, 1_048_576.0, 0.5)),
        widthPx = 96,
    )
    private val minFreeDiskUnitCombo = compactCombo(
        ComboBox(StorageSizeUnit.entries.toTypedArray()).apply {
            selectedItem = StorageSizeUnit.GIB
        },
        widthPx = 76,
    )

    private val videoMaxSizeCombo = compactCombo(
        ComboBox(arrayOf(1024, 1280, 1920, 2560, 3840)).apply {
            selectedItem = ArbConfig.DEFAULT_VIDEO_MAX_SIZE
        },
        widthPx = 108,
    )

    private val videoMaxFpsCombo = compactCombo(
        ComboBox(arrayOf(15, 30, 60)).apply {
            selectedItem = ArbConfig.DEFAULT_VIDEO_MAX_FPS
        },
        widthPx = 108,
    )

    private val audioEnabledCheckBox = JCheckBox("端末の再生音を録音する (API 33+)", true).apply {
        isOpaque = false
        font = ArbTheme.uiFont(12f)
    }

    private val crashAutosaveCheckBox = JCheckBox("アプリクラッシュ (am_crash) 時に自動保存する", true).apply {
        isOpaque = false
        font = ArbTheme.uiFont(12f)
    }

    private val crashCooldownSpinner = compactSpinner(
        JSpinner(
            SpinnerNumberModel(
                ArbConfig.DEFAULT_CRASH_AUTOSAVE_COOLDOWN_SECS,
                ArbConfig.MIN_CRASH_AUTOSAVE_COOLDOWN_SECS,
                ArbConfig.MAX_CRASH_AUTOSAVE_COOLDOWN_SECS,
                5,
            ),
        ),
        widthPx = 108,
    )

    private val outputDirField = JTextField(12).apply {
        toolTipText = "空欄の場合は arbd 既定のデータディレクトリ配下へ保存します"
        preferredSize = Dimension(JBUI.scale(130), JBUI.scale(28))
        minimumSize = Dimension(JBUI.scale(64), JBUI.scale(28))
    }
    private val outputDirBrowseButton = ArbTheme.styleSecondaryButton(JButton("参照...", AllIcons.Nodes.Folder), compact = true)
    private val outputDirClearButton = ArbTheme.styleSecondaryButton(JButton("既定"), compact = true).apply {
        toolTipText = "既定の保存先へ戻す"
    }

    private val defaultSaveActionCombo = compactCombo(
        ComboBox(
            arrayOf(
                SaveActionItem(IdeDefaultSaveAction.SAVE_AND_OPEN, "保存して開く"),
                SaveActionItem(IdeDefaultSaveAction.SAVE_ONLY, "保存のみ"),
            ),
        ),
        widthPx = 144,
    )

    private val customArbdPathField = JTextField(12).apply {
        toolTipText = "空欄の場合は環境変数・同梱バイナリ・PATH から自動検出します"
        preferredSize = Dimension(JBUI.scale(130), JBUI.scale(28))
        minimumSize = Dimension(JBUI.scale(64), JBUI.scale(28))
    }
    private val customArbdBrowseButton = ArbTheme.styleSecondaryButton(JButton("参照...", AllIcons.Nodes.Folder), compact = true)
    private val customArbdClearButton = ArbTheme.styleSecondaryButton(JButton("自動検出"), compact = true).apply {
        toolTipText = "自動検出に戻す"
    }

    private val daemonControls: List<JComponent> = listOf(
        saveWindowSpinner,
        ringBudgetAmountSpinner,
        ringBudgetUnitCombo,
        minFreeDiskAmountSpinner,
        minFreeDiskUnitCombo,
        videoMaxSizeCombo,
        videoMaxFpsCombo,
        audioEnabledCheckBox,
        crashAutosaveCheckBox,
        crashCooldownSpinner,
        outputDirField,
        outputDirBrowseButton,
        outputDirClearButton,
        defaultSaveActionCombo,
    )

    init {
        isOpaque = false
        border = JBUI.Borders.empty(6, 10)

        val form = JPanel(GridBagLayout()).apply { isOpaque = false }
        val gbc = GridBagConstraints().apply {
            anchor = GridBagConstraints.WEST
            fill = GridBagConstraints.HORIZONTAL
            insets = JBUI.insets(3, 4)
        }

        var row = 0
        fun addSection(title: String) {
            gbc.gridx = 0
            gbc.gridy = row++
            gbc.gridwidth = 2
            gbc.weightx = 1.0
            gbc.fill = GridBagConstraints.HORIZONTAL
            val header = JLabel(title).apply {
                font = ArbTheme.uiFont(12f, bold = true)
                foreground = ArbTheme.primaryText
                border = BorderFactory.createCompoundBorder(
                    BorderFactory.createMatteBorder(0, 0, 1, 0, ArbTheme.hairlineBorder),
                    JBUI.Borders.empty(6, 0, 2, 0),
                )
            }
            form.add(header, gbc)
            gbc.gridwidth = 1
        }

        fun addRow(labelText: String, field: Component, stretchField: Boolean = true) {
            gbc.gridx = 0
            gbc.gridy = row
            gbc.weightx = 0.0
            gbc.fill = GridBagConstraints.NONE
            form.add(
                JLabel(labelText).apply {
                    font = ArbTheme.uiFont(11.5f)
                    foreground = ArbTheme.primaryText
                },
                gbc,
            )
            gbc.gridx = 1
            gbc.weightx = 1.0
            gbc.fill = if (stretchField) GridBagConstraints.HORIZONTAL else GridBagConstraints.NONE
            form.add(field, gbc)
            row++
        }

        fun addFullWidth(comp: Component) {
            gbc.gridx = 0
            gbc.gridy = row++
            gbc.gridwidth = 2
            gbc.weightx = 1.0
            gbc.fill = GridBagConstraints.HORIZONTAL
            form.add(comp, gbc)
            gbc.gridwidth = 1
        }

        addSection("録画・リングバッファ設定（arbd 永続設定）")
        addRow("保持ウィンドウ (秒: 5〜600s):", leftAlignedRow(saveWindowSpinner))
        addRow("リングバッファ上限:", sizeRow(ringBudgetAmountSpinner, ringBudgetUnitCombo))
        addRow("最低空き容量フロア:", sizeRow(minFreeDiskAmountSpinner, minFreeDiskUnitCombo))
        addRow("映像最大長辺 (px):", leftAlignedRow(videoMaxSizeCombo))
        addRow("映像最大フレームレート (fps):", leftAlignedRow(videoMaxFpsCombo))
        addFullWidth(audioEnabledCheckBox)

        addSection("保存・クラッシュ自動保存")
        addRow("IDE の既定保存アクション:", leftAlignedRow(defaultSaveActionCombo))
        addFullWidth(crashAutosaveCheckBox)
        addRow("クラッシュ自動保存クールダウン (秒):", leftAlignedRow(crashCooldownSpinner))
        addRow("保存先フォルダ上書き (任意):", pathRow(outputDirField, outputDirBrowseButton, outputDirClearButton))

        addSection("ローカル実行設定（IDE ローカル保存）")
        addRow("arbd バイナリパス (任意):", pathRow(customArbdPathField, customArbdBrowseButton, customArbdClearButton))

        // Vertical glue to keep form anchored to top
        gbc.gridx = 0
        gbc.gridy = row
        gbc.gridwidth = 2
        gbc.weighty = 1.0
        gbc.fill = GridBagConstraints.BOTH
        form.add(JPanel().apply { isOpaque = false }, gbc)

        add(statusBannerPanel, BorderLayout.NORTH)
        add(form, BorderLayout.CENTER)

        restartDaemonButton.addActionListener { onRestartDaemon() }
        outputDirBrowseButton.addActionListener {
            onBrowseDirectory(outputDirField.text.trim()) { chosen ->
                outputDirField.text = chosen
            }
        }
        outputDirClearButton.addActionListener { outputDirField.text = "" }
        customArbdBrowseButton.addActionListener {
            onBrowseBinary(customArbdPathField.text.trim()) { chosen ->
                customArbdPathField.text = chosen
            }
        }
        customArbdClearButton.addActionListener { customArbdPathField.text = "" }
    }

    private fun compactSpinner(spinner: JSpinner, widthPx: Int): JSpinner = spinner.apply {
        val h = JBUI.scale(28)
        val dim = Dimension(JBUI.scale(widthPx), h)
        preferredSize = dim
        minimumSize = Dimension(JBUI.scale(76), h)
        maximumSize = dim
    }

    private fun <T> compactCombo(combo: ComboBox<T>, widthPx: Int): ComboBox<T> = combo.apply {
        val h = JBUI.scale(28)
        val dim = Dimension(JBUI.scale(widthPx), h)
        preferredSize = dim
        minimumSize = Dimension(JBUI.scale(68), h)
        maximumSize = dim
    }

    private fun leftAlignedRow(comp: Component): JPanel =
        JPanel(FlowLayout(FlowLayout.LEFT, 0, 0)).apply {
            isOpaque = false
            add(comp)
        }

    private fun sizeRow(spinner: JSpinner, unitCombo: ComboBox<StorageSizeUnit>): JPanel =
        JPanel(FlowLayout(FlowLayout.LEFT, 0, 0)).apply {
            isOpaque = false
            add(spinner)
            add(javax.swing.Box.createHorizontalStrut(JBUI.scale(6)))
            add(unitCombo)
        }

    private fun pathRow(field: JTextField, browse: JButton, clear: JButton): JPanel =
        JPanel(BorderLayout(4, 0)).apply {
            isOpaque = false
            add(field, BorderLayout.CENTER)
            val buttons = JPanel(FlowLayout(FlowLayout.RIGHT, 4, 0)).apply {
                isOpaque = false
                add(browse)
                add(clear)
            }
            add(buttons, BorderLayout.EAST)
        }

    fun setDaemonStatus(
        online: Boolean,
        detailMessage: String? = null,
        showRestartAction: Boolean = !online,
    ) {
        for (control in daemonControls) {
            control.isEnabled = online
        }
        if (!online) {
            statusBannerTextArea.text = detailMessage
                ?: "arbd デーモンに接続できません（オフライン）。ローカルの arbd バイナリパス設定を変更するか、arbd を再起動してください。"
            restartDaemonButton.isVisible = showRestartAction
            statusBannerPanel.isVisible = true
        } else {
            statusBannerPanel.isVisible = false
        }
        revalidate()
        repaint()
    }

    fun loadFromDraft(draft: ArbSettingsDraft) {
        saveWindowSpinner.value = draft.saveWindowSecs.coerceIn(
            ArbConfig.MIN_SAVE_WINDOW_SECS,
            ArbConfig.MAX_SAVE_WINDOW_SECS,
        )

        val ringVal = StorageSizeValue.fromBytes(draft.ringBudgetBytes)
        ringBudgetAmountSpinner.value = ringVal.amount
        ringBudgetUnitCombo.selectedItem = ringVal.unit

        val diskVal = StorageSizeValue.fromBytes(draft.minFreeDiskBytes)
        minFreeDiskAmountSpinner.value = diskVal.amount
        minFreeDiskUnitCombo.selectedItem = diskVal.unit

        ensureComboContains(videoMaxSizeCombo, draft.videoMaxSize)
        videoMaxSizeCombo.selectedItem = draft.videoMaxSize

        ensureComboContains(videoMaxFpsCombo, draft.videoMaxFps)
        videoMaxFpsCombo.selectedItem = draft.videoMaxFps

        audioEnabledCheckBox.isSelected = draft.audioEnabled
        crashAutosaveCheckBox.isSelected = draft.crashAutosaveEnabled
        crashCooldownSpinner.value = draft.crashAutosaveCooldownSecs.coerceIn(
            ArbConfig.MIN_CRASH_AUTOSAVE_COOLDOWN_SECS,
            ArbConfig.MAX_CRASH_AUTOSAVE_COOLDOWN_SECS,
        )
        outputDirField.text = draft.outputDirOverride
        selectSaveAction(draft.ideDefaultSaveAction)
        customArbdPathField.text = draft.customArbdBinaryPath
    }

    fun currentDraft(): ArbSettingsDraft {
        val ringUnit = (ringBudgetUnitCombo.selectedItem as? StorageSizeUnit) ?: StorageSizeUnit.GIB
        val diskUnit = (minFreeDiskUnitCombo.selectedItem as? StorageSizeUnit) ?: StorageSizeUnit.GIB
        val action = (defaultSaveActionCombo.selectedItem as? SaveActionItem)?.action
            ?: IdeDefaultSaveAction.SAVE_AND_OPEN
        return ArbSettingsDraft(
            saveWindowSecs = (saveWindowSpinner.value as Number).toInt(),
            ringBudgetBytes = StorageSizeValue((ringBudgetAmountSpinner.value as Number).toDouble(), ringUnit).toBytes(),
            minFreeDiskBytes = StorageSizeValue((minFreeDiskAmountSpinner.value as Number).toDouble(), diskUnit).toBytes(),
            videoMaxSize = (videoMaxSizeCombo.selectedItem as? Int) ?: ArbConfig.DEFAULT_VIDEO_MAX_SIZE,
            videoMaxFps = (videoMaxFpsCombo.selectedItem as? Int) ?: ArbConfig.DEFAULT_VIDEO_MAX_FPS,
            audioEnabled = audioEnabledCheckBox.isSelected,
            crashAutosaveEnabled = crashAutosaveCheckBox.isSelected,
            crashAutosaveCooldownSecs = (crashCooldownSpinner.value as Number).toInt(),
            outputDirOverride = outputDirField.text.trim(),
            ideDefaultSaveAction = action,
            customArbdBinaryPath = customArbdPathField.text.trim(),
        )
    }

    fun isOfflineBannerVisible(): Boolean = statusBannerPanel.isVisible

    fun offlineBannerText(): String = statusBannerTextArea.text

    fun isRestartButtonVisible(): Boolean = statusBannerPanel.isVisible && restartDaemonButton.isVisible

    fun clickRestartDaemon() = restartDaemonButton.doClick()

    fun bannerWrapsLines(): Boolean = statusBannerTextArea.lineWrap

    fun spinnersForLayoutTest(): List<JSpinner> = listOf(
        saveWindowSpinner,
        ringBudgetAmountSpinner,
        minFreeDiskAmountSpinner,
        crashCooldownSpinner,
    )

    fun isDaemonControlsEnabled(): Boolean = daemonControls.all { it.isEnabled }

    private fun selectSaveAction(action: IdeDefaultSaveAction) {
        for (i in 0 until defaultSaveActionCombo.itemCount) {
            val item = defaultSaveActionCombo.getItemAt(i)
            if (item.action == action) {
                defaultSaveActionCombo.selectedItem = item
                return
            }
        }
    }

    private fun ensureComboContains(combo: ComboBox<Int>, value: Int) {
        val existing = (0 until combo.itemCount).map { combo.getItemAt(it) }
        if (value !in existing) {
            combo.removeAllItems()
            (existing + value).distinct().sorted().forEach { combo.addItem(it) }
        }
    }

    private data class SaveActionItem(
        val action: IdeDefaultSaveAction,
        val label: String,
    ) {
        override fun toString(): String = label
    }
}

/**
 * Android Studio `Settings -> Tools -> Android Replay Buffer` (`id = io.github.shinma06.arb.settings`).
 *
 * Persists daemon settings via `arbd`'s `config.get` / `config.update`, and persists the optional
 * custom `arbd` binary path override locally via [ArbLocalSettingsState].
 */
class ArbSettingsConfigurable internal constructor(
    private val project: Project?,
    private val daemonConfigReader: () -> ArbConfig,
    private val daemonConfigWriter: (ArbConfig, ArbSettingsDraft) -> ArbConfig,
    private val localBinaryReader: () -> String,
    private val localBinaryWriter: (String) -> Unit,
    private val daemonRestarter: () -> Unit = {},
    // Runs a blocking arbd call. Production wraps it in a modal progress so the EDT keeps pumping
    // events (no IDE freeze while arbd spawns or another RPC holds the daemon lock); tests run inline.
    private val blockingRunner: DaemonCallRunner = DaemonCallRunner.Inline,
) : Configurable {
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
            ArbLocalSettingsState.getInstanceOrNull()?.customArbdBinaryPath = newPath
            runCatching { ArbdDaemonService.getInstance().notifyBinaryPathChanged() }
        },
        daemonRestarter = {
            ArbdDaemonService.getInstance().restartDaemon()
        },
        blockingRunner = ProgressDaemonCallRunner(project),
    )

    private var panel: ArbSettingsPanel? = null
    private var baselineConfig: ArbConfig = ArbConfig()
    private var baselineCustomBinary: String = ""
    private var daemonOnline: Boolean = false

    override fun getDisplayName(): String = "Android Replay Buffer"

    override fun createComponent(): JComponent {
        val created = ArbSettingsPanel(
            onBrowseDirectory = { current, onSelected ->
                val descriptor = FileChooserDescriptorFactory.createSingleFolderDescriptor().apply {
                    title = "保存先フォルダを選択"
                }
                val initial = current.takeIf { it.isNotBlank() }?.let { LocalFileSystem.getInstance().findFileByPath(it) }
                FileChooser.chooseFile(descriptor, project, initial) { selected ->
                    onSelected(selected.toNioPath().toAbsolutePath().normalize().toString())
                }
            },
            onBrowseBinary = { current, onSelected ->
                val descriptor = FileChooserDescriptorFactory.createSingleFileNoJarsDescriptor().apply {
                    title = "arbd バイナリを選択"
                }
                val initial = current.takeIf { it.isNotBlank() }?.let { LocalFileSystem.getInstance().findFileByPath(it) }
                FileChooser.chooseFile(descriptor, project, initial) { selected ->
                    onSelected(selected.toNioPath().toAbsolutePath().normalize().toString())
                }
            },
            onRestartDaemon = {
                restartDaemonAndReload()
            },
        )
        panel = created
        reset()
        return created
    }

    private fun restartDaemonAndReload() {
        val currentPanel = panel ?: return
        val draftPath = currentPanel.currentDraft().normalizedCustomArbdPath()
        if (draftPath.isNotEmpty() && draftPath != baselineCustomBinary) {
            val isAbsolute = runCatching { Path.of(draftPath).isAbsolute }.getOrDefault(false)
            if (isAbsolute) {
                blockingRunner.run { localBinaryWriter(draftPath) }
                baselineCustomBinary = draftPath
            }
        }
        val restarted = runCatching { blockingRunner.run { daemonRestarter() } }
        if (restarted.isSuccess) {
            reset()
        } else {
            daemonOnline = false
            val err = restarted.exceptionOrNull()?.message?.takeIf { it.isNotBlank() } ?: "詳細不明"
            currentPanel.setDaemonStatus(
                online = false,
                detailMessage = "arbd の再起動に失敗しました（$err）。ローカルの arbd バイナリパスを確認してください。",
                showRestartAction = true,
            )
        }
    }

    override fun isModified(): Boolean {
        val currentPanel = panel ?: return false
        val draft = currentPanel.currentDraft()
        val localChanged = draft.normalizedCustomArbdPath() != baselineCustomBinary
        val daemonChanged = daemonOnline && draft.isDaemonModifiedAgainst(baselineConfig)
        return localChanged || daemonChanged
    }

    @Throws(ConfigurationException::class)
    override fun apply() {
        val currentPanel = panel ?: return
        val draft = currentPanel.currentDraft()

        val newCustomPath = draft.normalizedCustomArbdPath()
        if (newCustomPath.isNotEmpty()) {
            val isAbsolute = runCatching { Path.of(newCustomPath).isAbsolute }.getOrDefault(false)
            if (!isAbsolute) {
                throw ConfigurationException("arbd バイナリパスには絶対パスを指定してください。")
            }
        }

        if (daemonOnline && draft.isDaemonModifiedAgainst(baselineConfig)) {
            val validationError = draft.validateDaemonFields()
            if (validationError != null) {
                throw ConfigurationException(validationError)
            }
            try {
                baselineConfig = blockingRunner.run { daemonConfigWriter(baselineConfig, draft) }
            } catch (t: Throwable) {
                throw ConfigurationException("arbd への設定保存に失敗しました: ${t.message}")
            }
        }

        if (newCustomPath != baselineCustomBinary) {
            blockingRunner.run { localBinaryWriter(newCustomPath) }
            baselineCustomBinary = newCustomPath
        }

        currentPanel.loadFromDraft(ArbSettingsDraft.fromConfig(baselineConfig, baselineCustomBinary))
    }

    override fun reset() {
        val currentPanel = panel ?: return
        baselineCustomBinary = localBinaryReader().trim()
        val loaded = runCatching { blockingRunner.run { daemonConfigReader() } }
        if (loaded.isSuccess) {
            daemonOnline = true
            baselineConfig = loaded.getOrThrow()
            currentPanel.setDaemonStatus(online = true)
        } else {
            daemonOnline = false
            baselineConfig = ArbConfig()
            val diagnostic = classifyDaemonError(loaded.exceptionOrNull())
            currentPanel.setDaemonStatus(
                online = false,
                detailMessage = diagnostic.message,
                showRestartAction = true,
            )
        }
        currentPanel.loadFromDraft(ArbSettingsDraft.fromConfig(baselineConfig, baselineCustomBinary))
    }

    override fun disposeUIResources() {
        panel = null
    }

    internal fun currentPanelForTest(): ArbSettingsPanel? = panel

    enum class DaemonDiagnosticKind {
        STALE_OR_MISMATCHED,
        OFFLINE,
    }

    data class DaemonDiagnostic(
        val kind: DaemonDiagnosticKind,
        val message: String,
    )

    companion object {
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
