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
import io.github.shinma06.arb.model.IdeDefaultSaveAction
import io.github.shinma06.arb.state.ArbSettingsDraft
import io.github.shinma06.arb.state.StorageSizeUnit
import io.github.shinma06.arb.state.StorageSizeValue
import io.github.shinma06.arb.ui.ArbTheme
import java.awt.BorderLayout
import java.awt.Component
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
) : JPanel(BorderLayout(0, 8)) {
    private val statusBannerLabel = JLabel("").apply {
        font = ArbTheme.uiFont(11.5f)
        foreground = ArbTheme.warningAmber
        icon = AllIcons.General.Warning
        border = BorderFactory.createCompoundBorder(
            BorderFactory.createMatteBorder(0, JBUI.scale(3), 0, 0, ArbTheme.warningAmber),
            BorderFactory.createCompoundBorder(
                BorderFactory.createLineBorder(ArbTheme.hairlineBorder, 1),
                JBUI.Borders.empty(6, 10),
            ),
        )
        background = ArbTheme.warningAmberSoft
        isOpaque = true
        isVisible = false
    }

    private val saveWindowSpinner = JSpinner(
        SpinnerNumberModel(
            ArbConfig.DEFAULT_SAVE_WINDOW_SECS,
            ArbConfig.MIN_SAVE_WINDOW_SECS,
            ArbConfig.MAX_SAVE_WINDOW_SECS,
            5,
        ),
    )

    private val ringBudgetAmountSpinner = JSpinner(SpinnerNumberModel(1.5, 1.0, 65_536.0, 0.5))
    private val ringBudgetUnitCombo = ComboBox(StorageSizeUnit.entries.toTypedArray()).apply {
        selectedItem = StorageSizeUnit.GIB
    }

    private val minFreeDiskAmountSpinner = JSpinner(SpinnerNumberModel(3.0, 1.0, 1_048_576.0, 0.5))
    private val minFreeDiskUnitCombo = ComboBox(StorageSizeUnit.entries.toTypedArray()).apply {
        selectedItem = StorageSizeUnit.GIB
    }

    private val videoMaxSizeCombo = ComboBox(arrayOf(1024, 1280, 1920, 2560, 3840)).apply {
        selectedItem = ArbConfig.DEFAULT_VIDEO_MAX_SIZE
    }

    private val videoMaxFpsCombo = ComboBox(arrayOf(15, 30, 60)).apply {
        selectedItem = ArbConfig.DEFAULT_VIDEO_MAX_FPS
    }

    private val audioEnabledCheckBox = JCheckBox("端末の再生音を録音する (API 33+)", true).apply {
        isOpaque = false
        font = ArbTheme.uiFont(12f)
    }

    private val crashAutosaveCheckBox = JCheckBox("アプリクラッシュ (am_crash) 時に自動保存する", true).apply {
        isOpaque = false
        font = ArbTheme.uiFont(12f)
    }

    private val crashCooldownSpinner = JSpinner(
        SpinnerNumberModel(
            ArbConfig.DEFAULT_CRASH_AUTOSAVE_COOLDOWN_SECS,
            ArbConfig.MIN_CRASH_AUTOSAVE_COOLDOWN_SECS,
            ArbConfig.MAX_CRASH_AUTOSAVE_COOLDOWN_SECS,
            5,
        ),
    )

    private val outputDirField = JTextField(28).apply {
        toolTipText = "空欄の場合は arbd 既定のデータディレクトリ配下へ保存します"
    }
    private val outputDirBrowseButton = ArbTheme.styleSecondaryButton(JButton("参照...", AllIcons.Nodes.Folder), compact = true)
    private val outputDirClearButton = ArbTheme.styleSecondaryButton(JButton("既定に戻す"), compact = true)

    private val defaultSaveActionCombo = ComboBox(
        arrayOf(
            SaveActionItem(IdeDefaultSaveAction.SAVE_AND_OPEN, "保存して開く"),
            SaveActionItem(IdeDefaultSaveAction.SAVE_ONLY, "保存のみ"),
        ),
    )

    private val customArbdPathField = JTextField(28).apply {
        toolTipText = "空欄の場合は環境変数・同梱バイナリ・PATH から自動検出します"
    }
    private val customArbdBrowseButton = ArbTheme.styleSecondaryButton(JButton("参照...", AllIcons.Nodes.Folder), compact = true)
    private val customArbdClearButton = ArbTheme.styleSecondaryButton(JButton("自動検出に戻す"), compact = true)

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
        border = JBUI.Borders.empty(8, 12)

        val form = JPanel(GridBagLayout()).apply { isOpaque = false }
        val gbc = GridBagConstraints().apply {
            anchor = GridBagConstraints.WEST
            fill = GridBagConstraints.HORIZONTAL
            insets = JBUI.insets(5, 4)
        }

        var row = 0
        fun addSection(title: String) {
            gbc.gridx = 0
            gbc.gridy = row++
            gbc.gridwidth = 2
            gbc.weightx = 1.0
            val header = JLabel(title).apply {
                font = ArbTheme.uiFont(12f, bold = true)
                foreground = ArbTheme.primaryText
                border = BorderFactory.createCompoundBorder(
                    BorderFactory.createMatteBorder(0, 0, 1, 0, ArbTheme.hairlineBorder),
                    JBUI.Borders.empty(8, 0, 3, 0),
                )
            }
            form.add(header, gbc)
            gbc.gridwidth = 1
        }

        fun addRow(labelText: String, field: Component) {
            gbc.gridx = 0
            gbc.gridy = row
            gbc.weightx = 0.0
            form.add(
                JLabel(labelText).apply {
                    font = ArbTheme.uiFont(12f)
                    foreground = ArbTheme.primaryText
                },
                gbc,
            )
            gbc.gridx = 1
            gbc.weightx = 1.0
            form.add(field, gbc)
            row++
        }

        fun addFullWidth(comp: Component) {
            gbc.gridx = 0
            gbc.gridy = row++
            gbc.gridwidth = 2
            gbc.weightx = 1.0
            form.add(comp, gbc)
            gbc.gridwidth = 1
        }

        addSection("録画・リングバッファ設定（arbd 永続設定）")
        addRow("保持ウィンドウ（秒: 5〜600s）:", saveWindowSpinner)
        addRow("リングバッファ上限:", sizeRow(ringBudgetAmountSpinner, ringBudgetUnitCombo))
        addRow("最低空き容量フロア:", sizeRow(minFreeDiskAmountSpinner, minFreeDiskUnitCombo))
        addRow("映像最大長辺 (px):", videoMaxSizeCombo)
        addRow("映像最大フレームレート (fps):", videoMaxFpsCombo)
        addFullWidth(audioEnabledCheckBox)

        addSection("保存・クラッシュ自動保存")
        addRow("IDE の既定保存アクション:", defaultSaveActionCombo)
        addFullWidth(crashAutosaveCheckBox)
        addRow("クラッシュ自動保存クールダウン（秒: 1〜3600s）:", crashCooldownSpinner)
        addRow("保存先フォルダ上書き（任意）:", pathRow(outputDirField, outputDirBrowseButton, outputDirClearButton))

        addSection("ローカル実行設定（IDE ローカル保存）")
        addRow("arbd バイナリパス上書き（任意）:", pathRow(customArbdPathField, customArbdBrowseButton, customArbdClearButton))

        // Vertical glue to keep form anchored to top
        gbc.gridx = 0
        gbc.gridy = row
        gbc.gridwidth = 2
        gbc.weighty = 1.0
        gbc.fill = GridBagConstraints.BOTH
        form.add(JPanel().apply { isOpaque = false }, gbc)

        add(statusBannerLabel, BorderLayout.NORTH)
        add(form, BorderLayout.CENTER)

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

    private fun sizeRow(spinner: JSpinner, unitCombo: ComboBox<StorageSizeUnit>): JPanel =
        JPanel(FlowLayout(FlowLayout.LEFT, 6, 0)).apply {
            isOpaque = false
            add(spinner)
            add(unitCombo)
        }

    private fun pathRow(field: JTextField, browse: JButton, clear: JButton): JPanel =
        JPanel(BorderLayout(6, 0)).apply {
            isOpaque = false
            add(field, BorderLayout.CENTER)
            val buttons = JPanel(FlowLayout(FlowLayout.RIGHT, 4, 0)).apply {
                isOpaque = false
                add(browse)
                add(clear)
            }
            add(buttons, BorderLayout.EAST)
        }

    fun setDaemonStatus(online: Boolean, detailMessage: String? = null) {
        for (control in daemonControls) {
            control.isEnabled = online
        }
        if (!online) {
            statusBannerLabel.text = detailMessage
                ?: "arbd デーモンに接続できません（オフライン）。ローカルの arbd バイナリパス設定のみ変更できます。"
            statusBannerLabel.isVisible = true
        } else {
            statusBannerLabel.isVisible = false
        }
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

    fun isOfflineBannerVisible(): Boolean = statusBannerLabel.isVisible

    fun offlineBannerText(): String = statusBannerLabel.text

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
            service.updateCachedConfig(updated)
            updated
        },
        localBinaryReader = {
            ArbLocalSettingsState.getInstanceOrNull()?.customArbdBinaryPath.orEmpty()
        },
        localBinaryWriter = { newPath ->
            ArbLocalSettingsState.getInstanceOrNull()?.customArbdBinaryPath = newPath
            runCatching { ArbdDaemonService.getInstance().disconnect() }
        },
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
        )
        panel = created
        reset()
        return created
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
                baselineConfig = daemonConfigWriter(baselineConfig, draft)
            } catch (t: Throwable) {
                throw ConfigurationException("arbd への設定保存に失敗しました: ${t.message}")
            }
        }

        if (newCustomPath != baselineCustomBinary) {
            localBinaryWriter(newCustomPath)
            baselineCustomBinary = newCustomPath
        }

        currentPanel.loadFromDraft(ArbSettingsDraft.fromConfig(baselineConfig, baselineCustomBinary))
    }

    override fun reset() {
        val currentPanel = panel ?: return
        baselineCustomBinary = localBinaryReader().trim()
        val loaded = runCatching { daemonConfigReader() }
        if (loaded.isSuccess) {
            daemonOnline = true
            baselineConfig = loaded.getOrThrow()
            currentPanel.setDaemonStatus(online = true)
        } else {
            daemonOnline = false
            baselineConfig = ArbConfig()
            val reason = loaded.exceptionOrNull()?.message?.takeIf { it.isNotBlank() }
            val msg = if (reason != null) {
                "arbd デーモンに接続できません（$reason）。ローカルの arbd バイナリパスのみ変更できます。"
            } else {
                "arbd デーモンに接続できません（オフライン）。ローカルの arbd バイナリパスのみ変更できます。"
            }
            currentPanel.setDaemonStatus(online = false, detailMessage = msg)
        }
        currentPanel.loadFromDraft(ArbSettingsDraft.fromConfig(baselineConfig, baselineCustomBinary))
    }

    override fun disposeUIResources() {
        panel = null
    }

    internal fun currentPanelForTest(): ArbSettingsPanel? = panel
}
